//! Render benchmarks, without HTTP (BUILD.md P8).
//!
//! Each sector template's `sample.json` goes through the service pipeline
//! once (schema, NFC, images, with photo URLs served from
//! `tests/fixtures/photos` by a mock server), then `render::render` (compile +
//! PDF/A-3b export) is measured with the images already resolved, evicting the
//! memo cache like the service does (`MemoEvictor`):
//! - `warm/<template>`: the same data every time (comemo replays the most);
//! - `new-data/<template>`: a different invoice number every iteration, the
//!   typical production case (template warm, data new);
//! - `images/warm`, `images/cold`: resolving the retail sample's photo URLs
//!   from the fetcher's cache, and with cache misses (download from a local
//!   mock server, decode, resize to `max_px`);
//! - `startup/*`: the cold-start steps (font scan; template scan + sample
//!   test compiles with an empty memo cache).
//!
//! Run with `just bench`. CI compares each run with the last `main` run
//! (`scripts/bench-ci.sh`) and warns on a regression above 20%.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use rustypdf::config::Config;
use rustypdf::fonts::FontStore;
use rustypdf::images::{self, fetch::Fetcher};
use rustypdf::memo::{DEFAULT_EVICT_EVERY, MemoEvictor};
use rustypdf::registry::{self, Template};
use rustypdf::render::{self, RenderInput};
use rustypdf::world::Shared;
use serde_json::Value;
use typst::foundations::Bytes;
use wiremock::{MockServer, Request, Respond, ResponseTemplate};

const TEMPLATES: [&str; 4] = [
    "invoice-retail",
    "invoice-healthcare",
    "invoice-logistics",
    "invoice-construction",
];

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn required_fonts() -> Vec<String> {
    vec!["Noto Sans Bengali".into(), "Inter".into()]
}

/// Serves `tests/fixtures/photos/<path>`, or 404.
struct PhotoFiles;

impl Respond for PhotoFiles {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let rel = req.url.path().trim_start_matches('/');
        if rel.split('/').any(|s| s == "..") {
            return ResponseTemplate::new(400);
        }
        match std::fs::read(repo().join("tests/fixtures/photos").join(rel)) {
            Ok(bytes) => ResponseTemplate::new(200).set_body_bytes(bytes),
            Err(_) => ResponseTemplate::new(404),
        }
    }
}

/// A template's sample, validated, normalized and with images resolved.
struct Prepared {
    template: Arc<Template>,
    data: Value,
    images: HashMap<String, Bytes>,
}

/// A fetcher whose `cdn.rustypdf.test` is a local mock photo server.
struct Photos {
    _server: MockServer,
    fetcher: Fetcher,
    limits: images::Limits,
}

impl Photos {
    async fn start() -> Self {
        let config = Config::from_lookup(|_| None).unwrap();
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
        Self {
            _server: server,
            fetcher,
            limits,
        }
    }

    async fn resolve(&self, template: &Arc<Template>, data: Value) -> (Value, images::Resolved) {
        images::resolve(template.clone(), data, &self.fetcher, self.limits)
            .await
            .expect("sample images resolve")
    }
}

/// Validates and normalizes each sector sample and resolves its images.
fn prepare(shared: &Shared, runtime: &tokio::runtime::Runtime, photos: &Photos) -> Vec<Prepared> {
    let templates = repo().join("templates");
    runtime.block_on(async {
        let mut out = Vec::new();
        for name in TEMPLATES {
            let template = Arc::new(registry::load_one(&templates, name, shared).unwrap());
            let mut data = template.sample.clone().expect("template has a sample");
            template.validate(&data).expect("sample matches the schema");
            render::normalize_nfc(&mut data);
            let (data, resolved) = photos.resolve(&template, data).await;
            out.push(Prepared {
                template,
                data,
                images: resolved.images,
            });
        }
        out
    })
}

fn render_once(shared: &Shared, memo: &MemoEvictor, p: &Prepared, data: &Value) -> usize {
    let len = render::render(
        shared,
        &RenderInput {
            project: &p.template.project,
            main: &p.template.main,
            data,
            images: &p.images,
            now: "2026-01-01T00:00:00Z".parse().unwrap(),
        },
    )
    .expect("sample renders")
    .pdf
    .len();
    memo.after_render();
    len
}

/// The invoice number field of a sector sample.
fn number_field(data: &Value) -> &'static str {
    ["invoice_no", "bill_no"]
        .into_iter()
        .find(|k| data.get(k).is_some_and(Value::is_string))
        .expect("sample has an invoice or bill number")
}

fn benches(c: &mut Criterion) {
    let fonts_dir: PathBuf = repo().join("fonts");
    let shared = Shared::new(Arc::new(
        FontStore::load(&fonts_dir, &required_fonts()).unwrap(),
    ));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let photos = runtime.block_on(Photos::start());
    let prepared = prepare(&shared, &runtime, &photos);
    let memo = MemoEvictor::new(DEFAULT_EVICT_EVERY);

    let mut warm = c.benchmark_group("warm");
    for p in &prepared {
        warm.bench_function(p.template.name.as_str(), |b| {
            b.iter(|| render_once(&shared, &memo, p, &p.data))
        });
    }
    warm.finish();

    let mut fresh = c.benchmark_group("new-data");
    for p in &prepared {
        let field = number_field(&p.data);
        let base = p.data[field].as_str().unwrap().to_owned();
        let mut data = p.data.clone();
        let mut seq = 0u64;
        fresh.bench_function(p.template.name.as_str(), |b| {
            b.iter(|| {
                seq += 1;
                data[field] = Value::String(format!("{base}-{seq}"));
                render_once(&shared, &memo, p, &data)
            })
        });
    }
    fresh.finish();

    let retail = prepared
        .iter()
        .find(|p| p.template.name == "invoice-retail")
        .unwrap()
        .template
        .clone();
    let mut sample = retail.sample.clone().unwrap();
    render::normalize_nfc(&mut sample);
    let mut imgs = c.benchmark_group("images");
    imgs.bench_function("warm", |b| {
        b.iter(|| {
            runtime
                .block_on(photos.resolve(&retail, sample.clone()))
                .1
                .images
                .len()
        })
    });
    let mut seq = 0u64;
    imgs.bench_function("cold", |b| {
        b.iter(|| {
            // A new query string per iteration: every URL misses the cache.
            seq += 1;
            let mut data = sample.clone();
            for item in data["items"].as_array_mut().unwrap() {
                if let Some(Value::String(url)) = item.get_mut("photo") {
                    *url = format!("{url}?v={seq}");
                }
            }
            runtime
                .block_on(photos.resolve(&retail, data))
                .1
                .images
                .len()
        })
    });
    imgs.finish();

    let mut startup = c.benchmark_group("startup");
    startup
        .sample_size(10)
        .measurement_time(Duration::from_secs(10));
    startup.bench_function("fonts", |b| {
        b.iter(|| {
            FontStore::load(&fonts_dir, &required_fonts())
                .unwrap()
                .len()
        })
    });
    // An empty memo cache and a fresh font store each time, so nothing
    // carries over from earlier iterations.
    startup.bench_function("templates", |b| {
        b.iter(|| {
            typst::comemo::evict(0);
            let shared = Shared::new(Arc::new(
                FontStore::load(&fonts_dir, &required_fonts()).unwrap(),
            ));
            registry::load(&repo().join("templates"), &shared)
                .unwrap()
                .len()
        })
    });
    startup.finish();
}

criterion_group!(render_benches, benches);
criterion_main!(render_benches);
