//! Helpers shared by integration tests.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use http_body_util::BodyExt;
use rustypdf::api::AppState;
use rustypdf::config::Config;
use rustypdf::fonts::FontStore;
use rustypdf::images::fetch::Fetcher;
use rustypdf::render::{self, RenderError, RenderInput, Rendered};
use rustypdf::world::{ProjectFiles, Shared};
use serde_json::Value;
use tower::ServiceExt;
use typst::foundations::Bytes;
use typst_layout::PagedDocument;

pub fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

pub fn shared() -> &'static Shared {
    static SHARED: OnceLock<Shared> = OnceLock::new();
    SHARED.get_or_init(|| {
        let fonts = FontStore::load(
            &repo().join("fonts"),
            &["Noto Sans Bengali".into(), "Inter".into()],
        )
        .expect("bundled fonts load");
        Shared::new(Arc::new(fonts))
    })
}

/// The repo's `templates/` folder.
pub fn templates() -> &'static ProjectFiles {
    static FILES: OnceLock<ProjectFiles> = OnceLock::new();
    FILES.get_or_init(|| ProjectFiles::load(&repo().join("templates")).unwrap())
}

pub fn sample(template: &str) -> Value {
    let path: PathBuf = repo().join("templates").join(template).join("sample.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// A fixed timestamp, so outputs are reproducible.
pub fn fixed_now() -> chrono::DateTime<chrono::Utc> {
    "2026-01-01T00:00:00Z".parse().unwrap()
}

pub fn compile_in(
    project: &ProjectFiles,
    main: &str,
    data: &Value,
    images: &HashMap<String, Bytes>,
) -> Result<PagedDocument, RenderError> {
    let mut data = data.clone();
    render::normalize_nfc(&mut data);
    render::compile(
        shared(),
        &RenderInput {
            project,
            main,
            data: &data,
            images,
            now: fixed_now(),
        },
    )
    .map(|(doc, _)| doc)
}

pub fn render_in(
    project: &ProjectFiles,
    main: &str,
    data: &Value,
) -> Result<Rendered, RenderError> {
    let mut data = data.clone();
    render::normalize_nfc(&mut data);
    let images = HashMap::new();
    render::render(
        shared(),
        &RenderInput {
            project,
            main,
            data: &data,
            images: &images,
            now: fixed_now(),
        },
    )
}

/// Compiles a repo template with `data`.
pub fn compile(template: &str, data: &Value) -> Result<PagedDocument, RenderError> {
    compile_in(
        templates(),
        &format!("/{template}/main.typ"),
        data,
        &HashMap::new(),
    )
}

/// Service config for the fixture templates, with `vars` on top.
pub fn config_with(vars: &[(&str, &str)]) -> Config {
    let fixtures = repo().join("tests/fixtures/templates");
    let fonts = repo().join("fonts");
    let mut all: Vec<(String, String)> = vec![
        ("TEMPLATES_DIR".into(), fixtures.display().to_string()),
        ("FONTS_DIR".into(), fonts.display().to_string()),
    ];
    all.extend(vars.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    Config::from_lookup(|k| {
        all.iter()
            .rev()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
    })
    .unwrap()
}

pub fn state_with(vars: &[(&str, &str)]) -> Arc<AppState> {
    AppState::load(config_with(vars)).unwrap()
}

/// State whose fetcher sends `host` to `addr` (a local mock server) without
/// the DNS guard; every other host stays guarded.
pub fn state_with_mock(
    vars: &[(&str, &str)],
    host: &str,
    addr: std::net::SocketAddr,
) -> Arc<AppState> {
    let config = config_with(vars);
    let fetcher = Fetcher::with_dns_overrides(&config, &[(host, addr)]);
    AppState::load_with(config, fetcher).unwrap()
}

pub fn post_json(uri: &str, body: &Value) -> Request<Body> {
    Request::post(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap()
}

pub fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

pub async fn send(app: &Router, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, body)
}

pub fn json_body(body: &[u8]) -> Value {
    serde_json::from_slice(body)
        .unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(body)))
}
