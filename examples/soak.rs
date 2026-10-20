//! In-process soak test: renders the sector samples with a new invoice number
//! every time (the production case) on N threads, evicting comemo the way the
//! service does (`MemoEvictor`), and prints throughput, latency and RSS every
//! few seconds.
//!
//! ```text
//! cargo run --release --example soak -- [seconds] [threads]
//! MEMO_EVICT_EVERY=64 cargo run --release --example soak -- 60 1
//! SOAK_COMPILE_ONLY=1 cargo run --release --example soak   # skip PDF export
//! SOAK_VARY_ALL=1 cargo run --release --example soak       # change every text field
//! ```
//!
//! Photo URLs in the retail sample are served from `tests/fixtures/photos`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustypdf::config::Config;
use rustypdf::fonts::FontStore;
use rustypdf::images::{self, fetch::Fetcher};
use rustypdf::memo::{DEFAULT_EVICT_EVERY, MemoEvictor};
use rustypdf::registry;
use rustypdf::render::{self, RenderInput};
use rustypdf::world::Shared;
use serde_json::Value;
use wiremock::{MockServer, Request, Respond, ResponseTemplate};

const TEMPLATES: [&str; 4] = [
    "invoice-retail",
    "invoice-healthcare",
    "invoice-logistics",
    "invoice-construction",
];

struct PhotoFiles;

impl Respond for PhotoFiles {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let rel = req.url.path().trim_start_matches('/');
        if rel.split('/').any(|s| s == "..") {
            return ResponseTemplate::new(400);
        }
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/photos");
        match std::fs::read(dir.join(rel)) {
            Ok(bytes) => ResponseTemplate::new(200).set_body_bytes(bytes),
            Err(_) => ResponseTemplate::new(404),
        }
    }
}

fn env_num(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn rss_mb() -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<f64>().ok())
        .map_or(0.0, |kb| kb / 1024.0)
}

fn percentile(sorted: &[Duration], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() as f64 * p).ceil() as usize).clamp(1, sorted.len()) - 1;
    sorted[i].as_secs_f64() * 1000.0
}

/// Appends `n` to every string that contains a letter (names, addresses,
/// item names), so no paragraph repeats; numbers and paths stay valid.
fn vary_text(value: &mut Value, n: u64) {
    match value {
        Value::String(s) if !s.starts_with('/') && s.chars().any(char::is_alphabetic) => {
            s.push_str(&format!(" {n}"));
        }
        Value::Array(items) => items.iter_mut().for_each(|v| vary_text(v, n)),
        Value::Object(map) => map.values_mut().for_each(|v| vary_text(v, n)),
        _ => {}
    }
}

fn main() {
    let args: Vec<u64> = std::env::args()
        .skip(1)
        .filter_map(|a| a.parse().ok())
        .collect();
    let seconds = args.first().copied().unwrap_or(60);
    let threads = args.get(1).copied().unwrap_or(1) as usize;
    let evict_every = env_num("MEMO_EVICT_EVERY", DEFAULT_EVICT_EVERY);
    let evictor = Arc::new(MemoEvictor::new(evict_every));
    let compile_only = std::env::var_os("SOAK_COMPILE_ONLY").is_some();
    let vary_all = std::env::var_os("SOAK_VARY_ALL").is_some();

    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let config = Config::from_lookup(|_| None).unwrap();
    let fonts = FontStore::load(&repo.join("fonts"), &config.required_fonts).unwrap();
    let shared = Arc::new(Shared::new(Arc::new(fonts)));

    let runtime = tokio::runtime::Runtime::new().unwrap();
    let prepared: Vec<_> = runtime.block_on(async {
        let server = MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::any())
            .respond_with(PhotoFiles)
            .mount(&server)
            .await;
        let fetcher =
            Fetcher::with_dns_overrides(&config, &[("cdn.rustypdf.test", *server.address())]);
        let limits = images::Limits {
            max_per_request: config.image_max_per_request,
            fetch_concurrency: config.image_fetch_concurrency,
            max_bytes: config.image_max_bytes,
        };
        let mut out = Vec::new();
        for name in TEMPLATES {
            let t = Arc::new(registry::load_one(&repo.join("templates"), name, &shared).unwrap());
            let mut data = t.sample.clone().unwrap();
            render::normalize_nfc(&mut data);
            let (data, resolved) = images::resolve(t.clone(), data, &fetcher, limits)
                .await
                .unwrap();
            out.push((t, data, Arc::new(resolved.images)));
        }
        out
    });
    let prepared = Arc::new(prepared);

    eprintln!(
        "soak: {seconds}s, {threads} thread(s), evict every {evict_every} renders, rss at start {:.0} MB",
        rss_mb()
    );
    let stop = Arc::new(AtomicBool::new(false));
    let seq = Arc::new(AtomicU64::new(0));
    let samples = Arc::new(Mutex::new(Vec::<Duration>::new()));

    let workers: Vec<_> = (0..threads)
        .map(|_| {
            let (shared, prepared, stop, seq, samples, evictor) = (
                shared.clone(),
                prepared.clone(),
                stop.clone(),
                seq.clone(),
                samples.clone(),
                evictor.clone(),
            );
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let n = seq.fetch_add(1, Ordering::Relaxed);
                    let (t, data, imgs) = &prepared[n as usize % prepared.len()];
                    let mut data: Value = data.clone();
                    let field = if data.get("invoice_no").is_some() {
                        "invoice_no"
                    } else {
                        "bill_no"
                    };
                    data[field] = Value::String(format!("SOAK-{n}"));
                    if vary_all {
                        vary_text(&mut data, n);
                    }
                    let start = Instant::now();
                    let input = RenderInput {
                        project: &t.project,
                        main: &t.main,
                        data: &data,
                        images: imgs,
                        now: chrono::Utc::now(),
                    };
                    if compile_only {
                        render::compile(&shared, &input).unwrap();
                    } else {
                        render::render(&shared, &input).unwrap();
                    }
                    evictor.after_render();
                    samples.lock().unwrap().push(start.elapsed());
                }
            })
        })
        .collect();

    let start = Instant::now();
    let report_every = Duration::from_secs(5);
    let mut next_report = start + report_every;
    println!("t_s,renders,per_s,p50_ms,p99_ms,max_ms,rss_mb");
    let mut all = Vec::new();
    while start.elapsed() < Duration::from_secs(seconds) {
        std::thread::sleep(Duration::from_millis(5));
        if Instant::now() >= next_report {
            let mut batch = std::mem::take(&mut *samples.lock().unwrap());
            batch.sort();
            println!(
                "{},{},{:.0},{:.1},{:.1},{:.1},{:.0}",
                start.elapsed().as_secs(),
                batch.len(),
                batch.len() as f64 / report_every.as_secs_f64(),
                percentile(&batch, 0.50),
                percentile(&batch, 0.99),
                percentile(&batch, 1.0),
                rss_mb()
            );
            all.extend(batch);
            next_report += report_every;
        }
    }
    stop.store(true, Ordering::Relaxed);
    for w in workers {
        w.join().unwrap();
    }
    // The first report includes the first-ever compiles; leave it in, so the
    // overall numbers are honest.
    all.sort();
    eprintln!(
        "overall: {} renders, p50 {:.1} ms, p99 {:.1} ms, max {:.1} ms, rss {:.0} MB",
        all.len(),
        percentile(&all, 0.50),
        percentile(&all, 0.99),
        percentile(&all, 1.0),
        rss_mb()
    );
}
