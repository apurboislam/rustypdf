//! Golden-image tests: page 1 of each template's `sample.json`, rendered to
//! PNG and compared with `tests/golden/<template>.png`.
//!
//! Samples go through the service pipeline (schema, NFC, images). Their photo
//! URLs (`http://cdn.rustypdf.test/...`) are served from
//! `tests/fixtures/photos` by a mock server.
//!
//! Approve intended changes with `just golden-update` (sets UPDATE_GOLDEN=1),
//! and say why in the PR.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use rustypdf::images::{self, fetch::Fetcher};
use rustypdf::{registry, render};
use typst_layout::PagedDocument;
use wiremock::{MockServer, Request, Respond, ResponseTemplate};

const PIXEL_PER_PT: f32 = 2.0;
/// A channel differing by more than this counts as a changed pixel.
const CHANNEL_TOLERANCE: u8 = 16;
/// Fraction of pixels allowed to change (anti-aliasing noise).
const MAX_CHANGED_FRACTION: f64 = 0.001;

/// Serves `tests/fixtures/photos/<path>`, or 404.
struct PhotoFiles;

impl Respond for PhotoFiles {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let rel = req.url.path().trim_start_matches('/');
        if rel.split('/').any(|s| s == "..") {
            return ResponseTemplate::new(400);
        }
        match std::fs::read(common::repo().join("tests/fixtures/photos").join(rel)) {
            Ok(bytes) => ResponseTemplate::new(200).set_body_bytes(bytes),
            Err(_) => ResponseTemplate::new(404),
        }
    }
}

/// Renders a repo template's sample the way the service does. Returns the
/// document and the image warnings.
async fn render_sample(template: &str) -> (PagedDocument, Vec<images::ImageIssue>) {
    let server = MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(PhotoFiles)
        .mount(&server)
        .await;
    let templates = common::repo().join("templates");
    let config = common::config_with(&[("TEMPLATES_DIR", &templates.display().to_string())]);
    let fetcher = Fetcher::with_dns_overrides(&config, &[("cdn.rustypdf.test", *server.address())]);
    let t = Arc::new(registry::load_one(&templates, template, common::shared()).unwrap());
    assert!(
        t.startup_errors.is_empty(),
        "{template}: {:?}",
        t.startup_errors
    );
    let mut data = t.sample.clone().expect("template has a sample.json");
    t.validate(&data).expect("sample matches the schema");
    render::normalize_nfc(&mut data);
    let limits = images::Limits {
        max_per_request: config.image_max_per_request,
        fetch_concurrency: config.image_fetch_concurrency,
        max_bytes: config.image_max_bytes,
    };
    let (data, resolved) = images::resolve(t.clone(), data, &fetcher, limits)
        .await
        .expect("sample images resolve");
    let doc =
        common::compile_in(&t.project, &t.main, &data, &resolved.images).expect("sample compiles");
    (doc, resolved.warnings)
}

async fn check_golden(template: &str) -> Vec<images::ImageIssue> {
    let (doc, warnings) = render_sample(template).await;
    check_png(template, &doc);
    warnings
}

fn check_png(template: &str, doc: &PagedDocument) {
    let png = rustypdf::render::page_png(doc, 0, PIXEL_PER_PT).expect("page 1 renders");
    let path: PathBuf = common::repo()
        .join("tests/golden")
        .join(format!("{template}.png"));

    if std::env::var_os("UPDATE_GOLDEN").is_some() || !path.exists() {
        if std::env::var_os("CI").is_some() && !path.exists() {
            panic!(
                "missing golden image {}; run `just golden-update`",
                path.display()
            );
        }
        std::fs::write(&path, &png).unwrap();
        eprintln!("updated {}", path.display());
        return;
    }

    let expected = tiny_skia::Pixmap::decode_png(&std::fs::read(&path).unwrap()).unwrap();
    let actual = tiny_skia::Pixmap::decode_png(&png).unwrap();
    let actual_path = path.with_extension("actual.png");
    if (expected.width(), expected.height()) != (actual.width(), actual.height()) {
        std::fs::write(&actual_path, &png).unwrap();
        panic!(
            "{template}: size changed from {}x{} to {}x{} (see {})",
            expected.width(),
            expected.height(),
            actual.width(),
            actual.height(),
            actual_path.display()
        );
    }
    let changed = expected
        .data()
        .chunks_exact(4)
        .zip(actual.data().chunks_exact(4))
        .filter(|(a, b)| {
            a.iter()
                .zip(b.iter())
                .any(|(x, y)| x.abs_diff(*y) > CHANNEL_TOLERANCE)
        })
        .count();
    let fraction = changed as f64 / (actual.width() * actual.height()) as f64;
    if fraction > MAX_CHANGED_FRACTION {
        std::fs::write(&actual_path, &png).unwrap();
        panic!(
            "{template}: {changed} pixels ({:.3}%) differ from {} (see {})",
            fraction * 100.0,
            path.display(),
            actual_path.display()
        );
    }
    let _ = std::fs::remove_file(&actual_path);
}

#[tokio::test]
async fn golden_bilingual_test() {
    check_golden("bilingual-test").await;
}

#[tokio::test]
async fn golden_hello() {
    check_golden("hello").await;
}

#[tokio::test]
async fn golden_invoice_retail() {
    let warnings = check_golden("invoice-retail").await;
    // The sample has one deliberately broken photo link, which falls back to
    // no-photo.png; the item without a photo falls back silently.
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].path, "/items/5/photo");
    assert_eq!(warnings[0].msg, "HTTP 404");
}

#[tokio::test]
async fn golden_invoice_healthcare() {
    assert!(check_golden("invoice-healthcare").await.is_empty());
}

#[tokio::test]
async fn golden_invoice_logistics() {
    assert!(check_golden("invoice-logistics").await.is_empty());
}

#[tokio::test]
async fn golden_invoice_construction() {
    assert!(check_golden("invoice-construction").await.is_empty());
}

/// Every template with a sample has a golden test above.
#[test]
fn every_sample_has_a_golden_test() {
    let source = std::fs::read_to_string(common::repo().join("tests/golden.rs")).unwrap();
    for entry in std::fs::read_dir(common::repo().join("templates")).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        if name.starts_with('_')
            || !common::repo()
                .join("templates")
                .join(&name)
                .join("sample.json")
                .exists()
        {
            continue;
        }
        assert!(
            source.contains(&format!("check_golden(\"{name}\")")),
            "templates/{name} has no golden test"
        );
    }
}
