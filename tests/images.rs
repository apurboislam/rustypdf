//! Static assets, manifest, schema and dynamic (URL / data: / file) images.
//! URL tests run against a local `wiremock` server.

mod common;

use std::io::Cursor;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::{StatusCode, header};
use base64::Engine;
use common::{get, json_body, post_json, send, state_with, state_with_mock};
use rustypdf::api::{self, AppState};
use rustypdf::images;
use rustypdf::registry;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The fetcher maps this host to the mock server.
const HOST: &str = "img.test";

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::DynamicImage::new_rgb8(w, h)
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

fn png_response() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "image/png")
        .set_body_bytes(png(120, 80))
}

async fn mock_app(vars: &[(&str, &str)]) -> (MockServer, Router, Arc<AppState>) {
    let server = MockServer::start().await;
    let state = state_with_mock(vars, HOST, *server.address());
    (server, api::router(state.clone()), state)
}

fn url(server: &MockServer, p: &str) -> String {
    format!("http://{HOST}:{}{p}", server.address().port())
}

fn photos(items: Value) -> Value {
    json!({ "title": "Test", "items": items })
}

fn assert_pdf(status: StatusCode, body: &[u8]) {
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(body));
    assert!(body.starts_with(b"%PDF-"));
}

fn image_error(status: StatusCode, body: &[u8]) -> Value {
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "{}",
        String::from_utf8_lossy(body)
    );
    let body = json_body(body);
    assert_eq!(body["error"], "image", "{body}");
    body["details"][0].clone()
}

// ---- P4: static assets, manifest, schema ----

/// Counts embedded images in the laid-out document.
fn count_images(frame: &typst::layout::Frame) -> usize {
    use typst::layout::FrameItem;
    frame
        .items()
        .map(|(_, item)| match item {
            FrameItem::Group(g) => count_images(&g.frame),
            FrameItem::Image(..) => 1,
            _ => 0,
        })
        .sum()
}

#[test]
fn static_logo_and_file_images_render() {
    let templates = registry::load(
        &common::repo().join("tests/fixtures/templates"),
        common::shared(),
    )
    .unwrap();
    let t = templates["photos"].clone();
    assert!(t.startup_errors.is_empty(), "{:?}", t.startup_errors);
    assert_eq!(t.name, "photo-test");
    assert_eq!(t.title, "Photo test");

    let mut data = t.sample.clone().unwrap();
    let resolved = images::resolve_offline(&t, &mut data).unwrap();
    // rice.png, no-photo.png (fallback for the missing photo), manager.png.
    assert_eq!(resolved.images.len(), 3);
    assert!(
        data["items"][0]["photo"]
            .as_str()
            .unwrap()
            .starts_with("/__img/")
    );
    assert!(
        data["items"][1]["photo"]
            .as_str()
            .unwrap()
            .starts_with("/__img/")
    );
    assert!(resolved.warnings.is_empty());

    let doc = common::compile_in(&t.project, &t.main, &data, &resolved.images).unwrap();
    // Logo + 2 item photos + signature + stamp.
    assert_eq!(count_images(&doc.pages()[0].frame), 5);
}

#[tokio::test]
async fn sample_renders_over_http() {
    let app = api::router(state_with(&[]));
    let sample: Value = serde_json::from_slice(
        &std::fs::read(common::repo().join("tests/fixtures/templates/photo-test/sample.json"))
            .unwrap(),
    )
    .unwrap();
    let (status, headers, body) = send(&app, post_json("/render/photos", &sample)).await;
    assert_pdf(status, &body);
    assert!(headers.get("x-render-warnings").is_none());
    assert!(
        headers["x-fetch-time-ms"]
            .to_str()
            .unwrap()
            .parse::<u64>()
            .is_ok()
    );
}

#[tokio::test]
async fn missing_static_asset_is_a_clear_compile_error() {
    let app = api::router(state_with(&[]));
    let (status, _, body) = send(&app, post_json("/render/missing-asset", &json!({}))).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let body = json_body(&body);
    assert_eq!(body["error"], "compile");
    let d = &body["details"][0];
    assert_eq!(d["file"], "/missing-asset/main.typ");
    assert_eq!(d["line"], 2);
    assert!(d["message"].as_str().unwrap().contains("not found"), "{d}");
}

#[tokio::test]
async fn missing_file_image_is_422() {
    let app = api::router(state_with(&[]));
    let body = json!({ "title": "x", "signature": "assets/sign/nobody.png" });
    let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
    let d = image_error(status, &body);
    assert_eq!(d["path"], "/signature");
    assert!(d["msg"].as_str().unwrap().contains("no image asset"), "{d}");
}

#[tokio::test]
async fn invalid_asset_files_are_rejected_at_load() {
    let app = api::router(state_with(&[]));
    // assets/fake.png is text: it is dropped at load, so it cannot be used.
    let body = json!({ "title": "x", "signature": "assets/fake.png" });
    let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
    let d = image_error(status, &body);
    assert!(d["msg"].as_str().unwrap().contains("no image asset"), "{d}");
}

#[tokio::test]
async fn path_traversal_in_image_field_is_rejected() {
    let app = api::router(state_with(&[]));
    for evil in [
        "../greet/main.typ",
        "assets/../../greet/main.typ",
        "/greet/main.typ",
        "/etc/passwd",
        "file:///etc/passwd",
        "assets\\..\\main.typ",
    ] {
        let body = json!({ "title": "x", "signature": evil });
        let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
        let d = image_error(status, &body);
        assert_eq!(d["path"], "/signature", "{evil}");
    }
}

#[tokio::test]
async fn schema_errors_list_every_field() {
    let app = api::router(state_with(&[]));
    let body = json!({ "items": [{ "qty": 0 }, { "name": 5 }] });
    let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let body = json_body(&body);
    assert_eq!(body["error"], "validation");
    let mut paths: Vec<&str> = body["details"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["path"].as_str().unwrap())
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        ["", "/items/0", "/items/0/qty", "/items/1/name"],
        "{body}"
    );
}

#[tokio::test]
async fn manifest_body_limit_overrides_the_default() {
    let app = api::router(state_with(&[("MAX_BODY_KB", "1024")]));
    // `pad` is ignored by both templates; only the body size matters.
    let big = json!({ "title": "x", "pad": "x".repeat(70 * 1024) });
    let (status, _, body) = send(&app, post_json("/render/photos", &big)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        json_body(&body)["message"],
        "request body exceeds 65536 bytes"
    );
    // Other templates keep the service default.
    let (status, _, _) = send(&app, post_json("/render/greet", &big)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn template_info_shows_schema_and_image_fields() {
    let app = api::router(state_with(&[]));
    let (status, _, body) = send(&app, get("/templates/photos")).await;
    assert_eq!(status, StatusCode::OK);
    let body = json_body(&body);
    assert_eq!(body["route"], "/render/photos");
    assert_eq!(body["folder"], "photo-test");
    assert_eq!(body["title"], "Photo test");
    assert_eq!(body["ok"], true, "{body}");
    assert_eq!(body["schema"]["required"], json!(["title"]));
    assert_eq!(body["images"]["/items/[]/photo"]["max_px"], 64);
    assert_eq!(body["images"]["/signature"]["on_error"], "fail");
    // The folder name is not a route when the manifest sets one.
    let (status, _, _) = send(&app, get("/templates/photo-test")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn preview_renders_the_sample() {
    let app = api::router(state_with(&[]));
    let (status, headers, body) = send(&app, get("/preview/photos")).await;
    assert_pdf(status, &body);
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        "inline; filename=\"photos.pdf\""
    );

    let (status, _, body) = send(&app, get("/preview/greet")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        json_body(&body)["message"]
            .as_str()
            .unwrap()
            .contains("sample.json")
    );

    let app = api::router(state_with(&[("PREVIEW", "false")]));
    let (status, _, _) = send(&app, get("/preview/photos")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---- P5: dynamic images ----

#[tokio::test]
async fn url_image_is_fetched_and_resized() {
    let (server, app, _) = mock_app(&[]).await;
    Mock::given(method("GET"))
        .and(path("/rice.png"))
        .respond_with(png_response())
        .expect(1)
        .mount(&server)
        .await;
    let body = photos(json!([{ "name": "Rice", "photo": url(&server, "/rice.png") }]));
    let (status, headers, body) = send(&app, post_json("/render/photos", &body)).await;
    assert_pdf(status, &body);
    assert!(headers.get("x-render-warnings").is_none());
}

#[tokio::test]
async fn second_identical_request_makes_no_network_call() {
    let (server, app, state) = mock_app(&[]).await;
    Mock::given(method("GET"))
        .and(path("/cached.png"))
        .respond_with(png_response())
        .expect(1)
        .mount(&server)
        .await;
    let body = photos(json!([
        { "name": "A", "photo": url(&server, "/cached.png") },
        { "name": "B", "photo": url(&server, "/cached.png") }
    ]));
    for _ in 0..2 {
        let (status, _, b) = send(&app, post_json("/render/photos", &body)).await;
        assert_pdf(status, &b);
    }
    // `expect(1)` is verified when the server drops.
    assert!(state.fetcher.metrics.cache_hits.get() >= 1);
}

#[tokio::test]
async fn broken_link_uses_fallback_with_warning() {
    let (server, app, _) = mock_app(&[]).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let photo = url(&server, "/gone.png");
    let body = photos(json!([{ "name": "Gone", "photo": photo }]));
    let (status, headers, body) = send(&app, post_json("/render/photos", &body)).await;
    assert_pdf(status, &body);
    let warnings: Value =
        serde_json::from_str(headers["x-render-warnings"].to_str().unwrap()).unwrap();
    assert_eq!(
        warnings,
        json!([{ "path": "/items/0/photo", "url": photo, "msg": "HTTP 404" }])
    );
}

#[tokio::test]
async fn broken_link_with_on_error_fail_is_422() {
    let (server, app, _) = mock_app(&[]).await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let sig = url(&server, "/sign.png");
    let body = json!({ "title": "x", "signature": sig });
    let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
    let d = image_error(status, &body);
    assert_eq!(
        d,
        json!({ "path": "/signature", "url": sig, "msg": "HTTP 404" })
    );
}

#[tokio::test]
async fn slow_image_times_out() {
    let (server, app, _) = mock_app(&[("IMAGE_FETCH_TIMEOUT_MS", "200")]).await;
    Mock::given(method("GET"))
        .respond_with(png_response().set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;
    let body = json!({ "title": "x", "signature": url(&server, "/slow.png") });
    let start = Instant::now();
    let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
    let d = image_error(status, &body);
    assert_eq!(d["msg"], "timeout");
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn oversized_image_is_rejected() {
    let (server, app, _) = mock_app(&[("IMAGE_MAX_BYTES", "1000")]).await;
    let mut big = png(8, 8);
    big.resize(5000, 0);
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(big))
        .mount(&server)
        .await;
    let body = json!({ "title": "x", "signature": url(&server, "/big.png") });
    let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
    let d = image_error(status, &body);
    assert!(
        d["msg"].as_str().unwrap().contains("exceeds 1000 bytes"),
        "{d}"
    );
}

#[tokio::test]
async fn non_image_response_is_rejected() {
    let (server, app, _) = mock_app(&[]).await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "image/png")
                .set_body_string("<html>not an image</html>"),
        )
        .mount(&server)
        .await;
    let body = json!({ "title": "x", "signature": url(&server, "/fake.png") });
    let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
    let d = image_error(status, &body);
    assert!(d["msg"].as_str().unwrap().contains("not a PNG"), "{d}");
}

#[tokio::test]
async fn disallowed_host_is_rejected() {
    let (server, app, _) =
        mock_app(&[("IMAGE_ALLOWED_HOSTS", "cdn.example.com,*.s3.amazonaws.com")]).await;
    Mock::given(method("GET"))
        .respond_with(png_response())
        .expect(0)
        .mount(&server)
        .await;
    let body = json!({ "title": "x", "signature": url(&server, "/a.png") });
    let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
    let d = image_error(status, &body);
    assert!(
        d["msg"].as_str().unwrap().contains("IMAGE_ALLOWED_HOSTS"),
        "{d}"
    );
}

#[tokio::test]
async fn allowlisted_host_is_fetched() {
    let (server, app, _) = mock_app(&[("IMAGE_ALLOWED_HOSTS", "*.example.com, IMG.test")]).await;
    Mock::given(method("GET"))
        .respond_with(png_response())
        .expect(1)
        .mount(&server)
        .await;
    let body = json!({ "title": "x", "signature": url(&server, "/a.png") });
    let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
    assert_pdf(status, &body);
}

#[tokio::test]
async fn private_addresses_are_blocked() {
    let (server, app, _) = mock_app(&[]).await;
    Mock::given(method("GET"))
        .respond_with(png_response())
        .expect(0)
        .mount(&server)
        .await;
    let port = server.address().port();
    for target in [
        format!("http://127.0.0.1:{port}/a.png"),
        format!("http://localhost:{port}/a.png"),
        format!("http://[::1]:{port}/a.png"),
        "http://169.254.169.254/latest/meta-data/".to_owned(),
        "http://10.0.0.1/a.png".to_owned(),
    ] {
        let body = json!({ "title": "x", "signature": target });
        let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
        let d = image_error(status, &body);
        assert!(
            d["msg"].as_str().unwrap().contains("blocked"),
            "{target}: {d}"
        );
    }
}

#[tokio::test]
async fn redirect_to_private_address_is_blocked() {
    let (server, app, _) = mock_app(&[]).await;
    let port = server.address().port();
    Mock::given(method("GET"))
        .and(path("/to-ip"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("http://127.0.0.1:{port}/secret")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/to-name"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("location", format!("http://localhost:{port}/secret")),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/secret"))
        .respond_with(png_response())
        .expect(0)
        .mount(&server)
        .await;
    for hop in ["/to-ip", "/to-name"] {
        let body = json!({ "title": "x", "signature": url(&server, hop) });
        let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
        let d = image_error(status, &body);
        assert!(d["msg"].as_str().unwrap().contains("blocked"), "{hop}: {d}");
    }
}

#[tokio::test]
async fn redirect_to_disallowed_host_is_blocked() {
    let (server, app, _) = mock_app(&[("IMAGE_ALLOWED_HOSTS", "img.test")]).await;
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", "https://evil.example.org/a.png"),
        )
        .mount(&server)
        .await;
    let body = json!({ "title": "x", "signature": url(&server, "/hop") });
    let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
    let d = image_error(status, &body);
    assert!(
        d["msg"].as_str().unwrap().contains("IMAGE_ALLOWED_HOSTS"),
        "{d}"
    );
}

#[tokio::test]
async fn too_many_redirects_fail() {
    let (server, app, _) = mock_app(&[]).await;
    for i in 0..5 {
        Mock::given(method("GET"))
            .and(path(format!("/r{i}")))
            .respond_with(
                ResponseTemplate::new(302)
                    .insert_header("location", url(&server, &format!("/r{}", i + 1))),
            )
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/r5"))
        .respond_with(png_response())
        .mount(&server)
        .await;
    let body = json!({ "title": "x", "signature": url(&server, "/r0") });
    let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
    let d = image_error(status, &body);
    assert!(d["msg"].as_str().unwrap().contains("redirects"), "{d}");

    // Three hops are fine.
    let body = json!({ "title": "x", "signature": url(&server, "/r2") });
    let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
    assert_pdf(status, &body);
}

#[tokio::test]
async fn fifty_items_fetch_in_parallel() {
    let (server, app, _) = mock_app(&[("IMAGE_FETCH_CONCURRENCY", "16")]).await;
    Mock::given(method("GET"))
        .respond_with(png_response().set_delay(Duration::from_millis(200)))
        .expect(50)
        .mount(&server)
        .await;
    let items: Vec<Value> = (0..50)
        .map(|i| json!({ "name": format!("p{i}"), "photo": url(&server, &format!("/p{i}.png")) }))
        .collect();
    let start = Instant::now();
    let (status, headers, body) =
        send(&app, post_json("/render/photos", &photos(json!(items)))).await;
    assert_pdf(status, &body);
    let fetch_ms: u64 = headers["x-fetch-time-ms"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    // Sequentially this would take 10 s; 16 at a time takes about 4 rounds.
    assert!(fetch_ms < 3000, "fetch took {fetch_ms} ms");
    assert!(start.elapsed() < Duration::from_secs(20));
}

#[tokio::test]
async fn image_count_is_capped() {
    let app = api::router(state_with(&[("IMAGE_MAX_PER_REQUEST", "2")]));
    let items: Vec<Value> = (0..3)
        .map(|i| json!({ "name": format!("p{i}"), "photo": "assets/rice.png" }))
        .collect();
    let (status, _, body) = send(&app, post_json("/render/photos", &photos(json!(items)))).await;
    let d = image_error(status, &body);
    assert!(
        d["msg"].as_str().unwrap().contains("IMAGE_MAX_PER_REQUEST"),
        "{d}"
    );
}

#[tokio::test]
async fn data_uri_images_are_decoded() {
    let app = api::router(state_with(&[]));
    let b64 = base64::engine::general_purpose::STANDARD.encode(png(300, 100));
    let body =
        photos(json!([{ "name": "Inline", "photo": format!("data:image/png;base64,{b64}") }]));
    let (status, headers, body) = send(&app, post_json("/render/photos", &body)).await;
    assert_pdf(status, &body);
    assert!(headers.get("x-render-warnings").is_none());

    let body = json!({ "title": "x", "signature": "data:image/png;base64,!!!" });
    let (status, _, body) = send(&app, post_json("/render/photos", &body)).await;
    let d = image_error(status, &body);
    assert!(d["msg"].as_str().unwrap().contains("base64"), "{d}");
}

#[tokio::test]
async fn empty_and_wrong_typed_values() {
    let app = api::router(state_with(&[]));
    // Empty photo uses the fallback silently; empty optional signature is left.
    let body = json!({ "title": "x", "items": [{ "name": "a", "photo": "" }], "signature": null });
    let (status, headers, body) = send(&app, post_json("/render/photos", &body)).await;
    assert_pdf(status, &body);
    assert!(headers.get("x-render-warnings").is_none());

    // A number is not an image; the photo field falls back with a warning.
    let body = json!({ "title": "x", "items": [{ "name": "a", "photo": 7 }] });
    let (status, headers, body) = send(&app, post_json("/render/photos", &body)).await;
    assert_pdf(status, &body);
    assert!(
        headers["x-render-warnings"]
            .to_str()
            .unwrap()
            .contains("must be a string")
    );
}
