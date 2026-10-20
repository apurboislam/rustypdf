//! Template listing, metadata and preview.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::response::Response;
use serde_json::{Value, json};

use super::{AppState, render};
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

/// `GET /preview/{template}`: renders the template's `sample.json` inline.
/// Disabled (404) when `PREVIEW=false`.
pub async fn preview(
    State(state): State<Arc<AppState>>,
    Path(route): Path<String>,
) -> Result<Response, AppError> {
    if !state.config.preview {
        return Err(AppError::RouteNotFound(format!("/preview/{route}")));
    }
    let template = state
        .registry
        .get(&route)
        .ok_or_else(|| AppError::TemplateNotFound(route.clone()))?;
    let sample = template
        .sample
        .clone()
        .ok_or_else(|| AppError::NoSample(route.clone()))?;
    let output = render::run(&state, template, sample).await?;
    Ok(render::pdf_response(output, &route, true))
}

fn describe(t: &Template) -> Value {
    let images: serde_json::Map<String, Value> = t
        .image_fields
        .iter()
        .map(|f| (f.pointer.clone(), json!(f.field)))
        .collect();
    json!({
        "name": t.route,
        "title": t.title,
        "folder": t.name,
        "route": format!("/render/{}", t.route),
        "schema": t.schema,
        "images": images,
        "sample": t.sample,
        "ok": t.startup_errors.is_empty(),
        "errors": t.startup_errors,
    })
}
