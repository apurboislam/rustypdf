//! Liveness and readiness probes.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde_json::{Value, json};

use super::AppState;

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
