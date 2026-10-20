//! Auth, metrics, request IDs and template reload (manual and hot).

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{config_with, get, json_body, post_json, send, state_with};
use rustypdf::api::{self, AppState};
use serde_json::json;

fn with_auth(mut req: Request<Body>, value: &str) -> Request<Body> {
    req.headers_mut()
        .insert(header::AUTHORIZATION, value.parse().unwrap());
    req
}

#[tokio::test]
async fn auth_is_off_without_keys() {
    let app = api::router(state_with(&[]));
    let (status, _, _) = send(&app, post_json("/render/greet", &json!({}))).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn auth_requires_a_valid_bearer_key() {
    let app = api::router(state_with(&[("API_KEYS", "k1, k2")]));
    for req in [
        post_json("/render/greet", &json!({})),
        with_auth(post_json("/render/greet", &json!({})), "Bearer nope"),
        with_auth(post_json("/render/greet", &json!({})), "Basic k1"),
        with_auth(post_json("/render/greet", &json!({})), "Bearer k"),
        get("/templates"),
        get("/metrics"),
        Request::post("/admin/reload").body(Body::empty()).unwrap(),
    ] {
        let path = req.uri().path().to_owned();
        let (status, headers, body) = send(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(headers[header::WWW_AUTHENTICATE], "Bearer");
        assert_eq!(json_body(&body)["error"], "unauthorized");
    }
    for key in ["Bearer k1", "bearer k2"] {
        let req = with_auth(post_json("/render/greet", &json!({})), key);
        let (status, _, _) = send(&app, req).await;
        assert_eq!(status, StatusCode::OK, "{key}");
    }
    // Probes stay open for orchestrators.
    for probe in ["/healthz", "/readyz"] {
        let (status, _, _) = send(&app, get(probe)).await;
        assert_eq!(status, StatusCode::OK, "{probe}");
    }
}

#[tokio::test]
async fn request_ids_are_echoed_or_generated() {
    let app = api::router(state_with(&[]));
    let mut req = get("/healthz");
    req.headers_mut()
        .insert("x-request-id", "abc-123".parse().unwrap());
    let (_, headers, _) = send(&app, req).await;
    assert_eq!(headers["x-request-id"], "abc-123");

    let (_, headers, _) = send(&app, get("/healthz")).await;
    let id = headers["x-request-id"].to_str().unwrap();
    assert_eq!(id.len(), 32, "{id}");
}

#[tokio::test]
async fn metrics_count_renders_and_errors() {
    let app = api::router(state_with(&[]));
    send(&app, post_json("/render/greet", &json!({ "name": "m" }))).await;
    send(&app, post_json("/render/nope", &json!({}))).await;
    send(&app, post_json("/render/photos", &json!({}))).await;
    let (status, headers, body) = send(&app, get("/metrics")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/plain")
    );
    let text = String::from_utf8(body).unwrap();
    for needle in [
        "rustypdf_render_seconds_count{template=\"greet\"} 1",
        "rustypdf_renders_in_flight 0",
        "rustypdf_errors_total{kind=\"not_found\"} 1",
        "rustypdf_errors_total{kind=\"validation\"} 1",
        "rustypdf_http_requests_total{status=\"200\"} 1",
        "rustypdf_image_cache_hits_total 0",
        "rustypdf_image_cache_misses_total 0",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
    }
}

// ---- reload ----

/// A fresh copy of the fixture templates `greet` and `photo-test` (plus
/// `_shared`), so a test can edit files freely.
fn temp_templates() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "rustypdf-ops-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let fixtures = common::repo().join("tests/fixtures/templates");
    for t in ["greet", "photo-test", "_shared"] {
        copy_dir(&fixtures.join(t), &dir.join(t));
    }
    dir
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn state_in(dir: &Path, vars: &[(&str, &str)]) -> Arc<AppState> {
    let dir = dir.display().to_string();
    let mut all = vec![("TEMPLATES_DIR", dir.as_str())];
    all.extend_from_slice(vars);
    AppState::load(config_with(&all)).unwrap()
}

fn main_typ(state: &AppState, route: &str) -> String {
    let t = state.registry.get(route).unwrap();
    t.project.get(&t.main).unwrap().as_str().unwrap().to_owned()
}

#[tokio::test]
async fn admin_reload_picks_up_changes_and_keeps_broken_templates() {
    let dir = temp_templates();
    let state = state_in(&dir, &[]);
    let app = api::router(state.clone());

    // A new template appears, and an edit is picked up.
    std::fs::create_dir_all(dir.join("added")).unwrap();
    std::fs::write(dir.join("added/main.typ"), "Added").unwrap();
    std::fs::write(
        dir.join("greet/main.typ"),
        "#set text(font: \"Inter\")\nVersion 2",
    )
    .unwrap();
    let (status, _, body) = send(
        &app,
        Request::post("/admin/reload").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    let summary = json_body(&body);
    assert_eq!(summary["templates"], json!(["added", "greet", "photos"]));
    assert!(main_typ(&state, "greet").contains("Version 2"));

    // A syntax error keeps the previous version; so does a broken manifest.
    std::fs::write(dir.join("greet/main.typ"), "#let x = (").unwrap();
    std::fs::write(dir.join("photo-test/manifest.toml"), "nonsense = true").unwrap();
    std::fs::remove_dir_all(dir.join("added")).unwrap();
    let (status, _, body) = send(
        &app,
        Request::post("/admin/reload").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let summary = json_body(&body);
    let kept: Vec<&str> = summary["kept_previous"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["folder"].as_str().unwrap())
        .collect();
    assert_eq!(kept, ["greet", "photo-test"], "{summary}");
    assert_eq!(summary["removed"], json!(["added"]));
    assert!(main_typ(&state, "greet").contains("Version 2"));
    let (status, _, _) = send(&app, post_json("/render/greet", &json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, _) = send(&app, get("/preview/photos")).await;
    assert_eq!(status, StatusCode::OK);

    let metrics = String::from_utf8(send(&app, get("/metrics")).await.2).unwrap();
    assert!(
        metrics.contains("rustypdf_template_reloads_total{result=\"partial\"} 1"),
        "{metrics}"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn hot_reload_applies_edits_without_restart() {
    let dir = temp_templates();
    let state = state_in(&dir, &[]);
    let _watcher = api::watch_templates(state.clone()).unwrap();

    std::fs::write(
        dir.join("greet/main.typ"),
        "#set text(font: \"Inter\")\nHot edit",
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !main_typ(&state, "greet").contains("Hot edit") {
        assert!(Instant::now() < deadline, "edit was not picked up");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
