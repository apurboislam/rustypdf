//! Probes, metrics and template reload.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::AppState;
use crate::error::AppError;

pub async fn healthz() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// Ready once fonts are loaded and templates compiled; not ready while
/// shutting down, so load balancers stop sending traffic.
pub async fn readyz(State(state): State<Arc<AppState>>) -> (StatusCode, Json<Value>) {
    if state.ready.load(Ordering::SeqCst) {
        let templates = state.registry.list().len();
        (
            StatusCode::OK,
            Json(json!({ "status": "ready", "templates": templates })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "status": "not_ready" })),
        )
    }
}

/// Prometheus text exposition.
pub async fn metrics(State(state): State<Arc<AppState>>) -> Response {
    let mut response = state.metrics.encode().into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    response
}

/// `POST /admin/reload`: re-scans the templates folder now.
pub async fn reload(State(state): State<Arc<AppState>>) -> Result<Json<Value>, AppError> {
    let summary = tokio::task::spawn_blocking(move || state.reload())
        .await
        .map_err(|e| AppError::Internal(format!("reload task failed: {e}")))?
        .map_err(|e| AppError::Internal(format!("{e:#}")))?;
    Ok(Json(json!(summary)))
}
