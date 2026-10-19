//! Template listing and metadata.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use serde_json::{Value, json};

use super::AppState;
use crate::error::AppError;
use crate::registry::Template;

pub async fn list(State(state): State<Arc<AppState>>) -> Json<Value> {
    let templates: Vec<Value> = state.registry.list().iter().map(|t| describe(t)).collect();
    Json(json!({ "templates": templates }))
}

pub async fn info(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> Result<Json<Value>, AppError> {
    let template = state
        .registry
        .get(&name)
        .ok_or(AppError::TemplateNotFound(name))?;
    Ok(Json(describe(&template)))
}

fn describe(t: &Template) -> Value {
    json!({
        "name": t.name,
        "route": format!("/render/{}", t.name),
        "sample": t.sample,
        "ok": t.startup_errors.is_empty(),
        "errors": t.startup_errors,
    })
}
