//! `POST /render/{template}`: JSON in, PDF/A-3b out.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;

use super::AppState;
use crate::error::{AppError, FieldError};
use crate::render::{self, RenderInput};

#[derive(Debug, Default, Deserialize)]
pub struct RenderQuery {
    /// Sets the `Content-Disposition` filename.
    pub filename: Option<String>,
    /// `true` returns the PDF inline instead of as an attachment.
    pub inline: Option<String>,
}

pub async fn render(
    State(state): State<Arc<AppState>>,
    Path(name): Path<String>,
    Query(query): Query<RenderQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, AppError> {
    let template = state
        .registry
        .get(&name)
        .ok_or_else(|| AppError::TemplateNotFound(name.clone()))?;

    // 1. Body-size limit: reject early on Content-Length, and again while
    //    reading for chunked bodies.
    let limit = state.config.max_body_bytes;
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|len| len > limit as u64) {
        return Err(AppError::PayloadTooLarge { limit });
    }
    let bytes = axum::body::to_bytes(body, limit)
        .await
        .map_err(|_| AppError::PayloadTooLarge { limit })?;

    // 2. Parse and NFC-normalize.
    let mut data: Value = serde_json::from_slice(&bytes).map_err(|err| {
        AppError::Validation(vec![FieldError {
            path: String::new(),
            msg: format!("body is not valid JSON: {err}"),
        }])
    })?;
    render::normalize_nfc(&mut data);

    // 3. Take a compile slot, or give up with a 503.
    let permit = tokio::time::timeout(
        state.config.queue_timeout,
        state.compile_slots.clone().acquire_owned(),
    )
    .await
    .map_err(|_| AppError::Overloaded)?
    .map_err(|_| AppError::Overloaded)?;

    // 4. Compile on the blocking pool. The permit moves into the task, so a
    //    render that outlives its deadline still holds its slot until done.
    let start = Instant::now();
    let shared = state.shared.clone();
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let images = HashMap::new();
        render::render(
            &shared,
            &RenderInput {
                project: &template.project,
                main: &template.main,
                data: &data,
                images: &images,
                now: chrono::Utc::now(),
            },
        )
    });
    let deadline = state.config.render_timeout;
    let rendered = tokio::time::timeout(deadline, task)
        .await
        .map_err(|_| AppError::RenderTimeout(deadline))?
        .map_err(|err| AppError::Internal(format!("render task failed: {err}")))??;
    let elapsed = start.elapsed();
    tracing::info!(template = %name, ms = elapsed.as_millis() as u64, bytes = rendered.pdf.len(), "rendered");

    let filename = sanitize_filename(query.filename.as_deref().unwrap_or(&name));
    let disposition = if is_true(query.inline.as_deref()) {
        "inline"
    } else {
        "attachment"
    };
    let mut response = rendered.pdf.into_response();
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/pdf"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("{disposition}; filename=\"{filename}\"")) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        "x-render-time-ms",
        HeaderValue::from(elapsed.as_millis() as u64),
    );
    Ok(response)
}

fn is_true(v: Option<&str>) -> bool {
    matches!(v, Some("true" | "1" | "yes"))
}

/// Keeps the filename to safe printable ASCII and ensures a `.pdf` suffix.
fn sanitize_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut name: String = base
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => c,
            _ => '_',
        })
        .take(120)
        .collect();
    let name_trimmed = name.trim_matches('.');
    if name_trimmed.is_empty() {
        name = "document".into();
    } else {
        name = name_trimmed.to_owned();
    }
    if !name.to_ascii_lowercase().ends_with(".pdf") {
        name.push_str(".pdf");
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_are_sanitized() {
        assert_eq!(sanitize_filename("R-1001.pdf"), "R-1001.pdf");
        assert_eq!(sanitize_filename("R-1001"), "R-1001.pdf");
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd.pdf");
        assert_eq!(sanitize_filename("a\"b\r\nc.PDF"), "a_b__c.PDF");
        assert_eq!(sanitize_filename("চালান"), "_____.pdf");
        assert_eq!(sanitize_filename(".."), "document.pdf");
    }
}
