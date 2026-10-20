//! HTTP API integration tests: every endpoint and every error code.

mod common;

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::{get, json_body, post_json, send, state_with};
use rustypdf::api::{self, AppState};
use rustypdf::config::Config;
use serde_json::json;

fn app(vars: &[(&str, &str)]) -> Router {
    api::router(state_with(vars))
}

/// A loop count no other test uses, so comemo cannot serve a cached result.
fn unique_n(base: u64) -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    base + NEXT.fetch_add(1, Ordering::SeqCst)
}

#[tokio::test]
async fn render_returns_pdf() {
    let app = app(&[]);
    let (status, headers, body) = send(
        &app,
        post_json("/render/greet", &json!({ "name": "Rahim" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(headers[header::CONTENT_TYPE], "application/pdf");
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        "attachment; filename=\"greet.pdf\""
    );
    assert!(
        headers["x-render-time-ms"]
            .to_str()
            .unwrap()
            .parse::<u64>()
            .is_ok()
    );
    assert!(body.starts_with(b"%PDF-"));
    assert!(String::from_utf8_lossy(&body).contains("<pdfaid:part>3</pdfaid:part>"));
}

#[tokio::test]
async fn filename_and_inline_options() {
    let app = app(&[]);
    let req = post_json("/render/greet?filename=R-1001.pdf&inline=true", &json!({}));
    let (status, headers, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        "inline; filename=\"R-1001.pdf\""
    );

    let req = post_json("/render/greet?filename=..%2F..%2Fx%22y", &json!({}));
    let (_, headers, _) = send(&app, req).await;
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        "attachment; filename=\"x_y.pdf\""
    );
}

#[tokio::test]
async fn unknown_template_is_404() {
    let app = app(&[]);
    for name in ["nope", "_private", "no-main"] {
        let (status, _, body) = send(&app, post_json(&format!("/render/{name}"), &json!({}))).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{name}");
        assert_eq!(json_body(&body)["error"], "not_found");
    }
    let (status, _, body) = send(&app, get("/no/such/route")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(json_body(&body)["error"], "not_found");
}

#[tokio::test]
async fn oversized_body_is_413() {
    let app = app(&[("MAX_BODY_KB", "1")]);
    let big = json!({ "name": "x".repeat(2000) });
    let (status, _, body) = send(&app, post_json("/render/greet", &big)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(json_body(&body)["error"], "payload_too_large");

    // Without Content-Length (chunked), the limit applies while reading.
    let stream = futures_body(vec![
        b"{\"name\":\"".to_vec(),
        vec![b'x'; 2000],
        b"\"}".to_vec(),
    ]);
    let req = Request::post("/render/greet").body(stream).unwrap();
    let (status, _, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

fn futures_body(chunks: Vec<Vec<u8>>) -> Body {
    Body::from_stream(futures_util::stream::iter(
        chunks.into_iter().map(Ok::<_, std::io::Error>),
    ))
}

#[tokio::test]
async fn invalid_json_is_422() {
    let app = app(&[]);
    let req = Request::post("/render/greet")
        .body(Body::from("{not json"))
        .unwrap();
    let (status, _, body) = send(&app, req).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(&body);
    assert_eq!(body["error"], "validation");
    assert_eq!(body["details"][0]["path"], "");
}

#[tokio::test]
async fn compile_error_is_500_with_location() {
    let app = app(&[]);
    let (status, _, body) = send(&app, post_json("/render/broken", &json!({}))).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let body = json_body(&body);
    assert_eq!(body["error"], "compile");
    assert_eq!(body["details"][0]["file"], "/broken/main.typ");
    assert_eq!(body["details"][0]["line"], 3);
    assert!(body["details"][0]["column"].is_number());
}

#[tokio::test]
async fn data_error_is_500_with_location() {
    let app = app(&[]);
    // `slow` reads `d.n`; without it the template fails at that access.
    let (status, _, body) = send(&app, post_json("/render/slow", &json!({}))).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let body = json_body(&body);
    assert_eq!(body["details"][0]["line"], 4, "{body}");
}

#[tokio::test]
async fn queue_timeout_is_503() {
    let state = state_with(&[("MAX_CONCURRENCY", "1"), ("QUEUE_TIMEOUT_MS", "50")]);
    let app = api::router(state.clone());

    let busy = {
        let app = app.clone();
        let n = unique_n(1_000_000);
        tokio::spawn(async move { send(&app, post_json("/render/slow", &json!({ "n": n }))).await })
    };
    // Wait until the slow render holds the only slot.
    while state.compile_slots.available_permits() > 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let (status, headers, body) = send(&app, post_json("/render/greet", &json!({}))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(json_body(&body)["error"], "overloaded");
    assert_eq!(headers[header::RETRY_AFTER], "1");

    let (status, _, _) = busy.await.unwrap();
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn render_timeout_is_500_and_keeps_the_slot() {
    let state = state_with(&[("MAX_CONCURRENCY", "1"), ("RENDER_TIMEOUT_MS", "20")]);
    let app = api::router(state.clone());
    let n = unique_n(2_000_000);
    let (status, _, body) = send(&app, post_json("/render/slow", &json!({ "n": n }))).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(json_body(&body)["error"], "render_timeout");
    // The abandoned render still runs, so its slot stays taken until it ends.
    assert_eq!(state.compile_slots.available_permits(), 0);
}

#[tokio::test]
async fn fifty_parallel_requests_succeed() {
    let app = app(&[("MAX_CONCURRENCY", "4"), ("QUEUE_TIMEOUT_MS", "60000")]);
    let tasks: Vec<_> = (0..50)
        .map(|i| {
            let app = app.clone();
            tokio::spawn(async move {
                send(
                    &app,
                    post_json("/render/greet", &json!({ "name": format!("n{i}") })),
                )
                .await
            })
        })
        .collect();
    for task in tasks {
        let (status, _, body) = task.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        assert!(body.starts_with(b"%PDF-"));
    }
}

#[tokio::test]
async fn templates_listing() {
    let app = app(&[]);
    let (status, _, body) = send(&app, get("/templates")).await;
    assert_eq!(status, StatusCode::OK);
    let body = json_body(&body);
    let names: Vec<&str> = body["templates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    // `bad-manifest` is skipped; `photo-test` is served at its manifest route.
    assert_eq!(
        names,
        ["broken", "greet", "missing-asset", "photos", "slow"]
    );

    let (status, _, body) = send(&app, get("/templates/broken")).await;
    assert_eq!(status, StatusCode::OK);
    let body = json_body(&body);
    assert_eq!(body["route"], "/render/broken");
    assert_eq!(body["ok"], false);
    assert_eq!(body["errors"][0]["line"], 3);

    let (status, _, body) = send(&app, get("/templates/slow")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_body(&body)["sample"], json!({ "n": 1 }));

    let (status, _, _) = send(&app, get("/templates/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn health_and_readiness() {
    let state = state_with(&[]);
    let app = api::router(state.clone());
    let (status, _, body) = send(&app, get("/healthz")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_body(&body)["status"], "ok");

    let (status, _, body) = send(&app, get("/readyz")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json_body(&body)["templates"], 5);

    state.ready.store(false, Ordering::SeqCst);
    let (status, _, _) = send(&app, get("/readyz")).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
}

#[test]
fn missing_required_font_refuses_to_start() {
    let fixtures = common::repo().join("tests/fixtures/templates");
    let fonts = common::repo().join("fonts");
    let config = Config::from_lookup(|k| match k {
        "TEMPLATES_DIR" => Some(fixtures.display().to_string()),
        "FONTS_DIR" => Some(fonts.display().to_string()),
        "REQUIRED_FONTS" => Some("Inter,Comic Sans MS".into()),
        _ => None,
    })
    .unwrap();
    let err = AppState::load(config).err().expect("must refuse to start");
    assert!(err.to_string().contains("Comic Sans MS"), "{err}");
}
