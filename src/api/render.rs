//! `POST /render/{template}`: JSON in, PDF/A-3b out.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;

use super::AppState;
use crate::error::{AppError, FieldError};
use crate::images::{self, ImageIssue};
use crate::metrics::InFlight;
use crate::registry::Template;
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
    Path(route): Path<String>,
    Query(query): Query<RenderQuery>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, AppError> {
    let template = state
        .registry
        .get(&route)
        .ok_or_else(|| AppError::TemplateNotFound(route.clone()))?;

    // 1. Body-size limit: reject early on Content-Length, and again while
    //    reading for chunked bodies.
    let limit = template.max_body_bytes(state.config.max_body_bytes);
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
    let data: Value = serde_json::from_slice(&bytes).map_err(|err| {
        AppError::Validation(vec![FieldError {
            path: String::new(),
            msg: format!("body is not valid JSON: {err}"),
        }])
    })?;

    let output = run(&state, template, data).await?;
    let filename = query.filename.as_deref().unwrap_or(&route);
    Ok(pdf_response(
        output,
        filename,
        is_true(query.inline.as_deref()),
    ))
}

/// A finished render and its timings.
pub struct Output {
    pub pdf: Vec<u8>,
    pub render_time: Duration,
    pub fetch_time: Duration,
    /// Image fields that fell back to their fallback file.
    pub warnings: Vec<ImageIssue>,
}

/// Validates, normalizes, resolves images, then compiles in a slot.
pub async fn run(
    state: &AppState,
    template: Arc<Template>,
    data: Value,
) -> Result<Output, AppError> {
    // 2. Schema validation, then NFC normalization.
    template.validate(&data).map_err(AppError::Validation)?;
    let mut data = data;
    render::normalize_nfc(&mut data);

    // 3. Resolve images before taking a compile slot, so slow networks never
    //    hold a CPU worker.
    let fetch_start = Instant::now();
    let limits = images::Limits {
        max_per_request: state.config.image_max_per_request,
        fetch_concurrency: state.config.image_fetch_concurrency,
        max_bytes: state.config.image_max_bytes,
    };
    let (data, resolved) = images::resolve(template.clone(), data, &state.fetcher, limits)
        .await
        .map_err(AppError::Image)?;
    let fetch_time = fetch_start.elapsed();

    // 4. Take a compile slot, or give up with a 503.
    let permit = tokio::time::timeout(
        state.config.queue_timeout,
        state.compile_slots.clone().acquire_owned(),
    )
    .await
    .map_err(|_| AppError::Overloaded)?
    .map_err(|_| AppError::Overloaded)?;

    // 5. Compile on the blocking pool. The permit moves into the task, so a
    //    render that outlives its deadline still holds its slot until done.
    let start = Instant::now();
    let shared = state.shared.clone();
    let name = template.route.clone();
    let deadline = template.render_timeout(state.config.render_timeout);
    let images = resolved.images;
    let memo = state.memo.clone();
    let in_flight = InFlight::start(&state.metrics.renders_in_flight);
    let histogram = state
        .metrics
        .render_seconds
        .with_label_values(&[name.as_str()]);
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _in_flight = in_flight;
        let start = Instant::now();
        let result = render::render(
            &shared,
            &RenderInput {
                project: &template.project,
                main: &template.main,
                data: &data,
                images: &images,
                now: chrono::Utc::now(),
            },
        );
        histogram.observe(start.elapsed().as_secs_f64());
        memo.after_render();
        result
    });
    let rendered = tokio::time::timeout(deadline, task)
        .await
        .map_err(|_| AppError::RenderTimeout(deadline))?
        .map_err(|err| AppError::Internal(format!("render task failed: {err}")))??;
    let render_time = start.elapsed();
    tracing::info!(
        template = %name,
        ms = render_time.as_millis() as u64,
        fetch_ms = fetch_time.as_millis() as u64,
        bytes = rendered.pdf.len(),
        image_warnings = resolved.warnings.len(),
        "rendered"
    );
    Ok(Output {
        pdf: rendered.pdf,
        render_time,
        fetch_time,
        warnings: resolved.warnings,
    })
}

/// Longest `X-Render-Warnings` value; later entries are summarized.
const MAX_WARNINGS_HEADER: usize = 4096;

pub fn pdf_response(output: Output, filename: &str, inline: bool) -> Response {
    let filename = sanitize_filename(filename);
    let disposition = if inline { "inline" } else { "attachment" };
    let mut response = output.pdf.into_response();
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
        HeaderValue::from(output.render_time.as_millis() as u64),
    );
    h.insert(
        "x-fetch-time-ms",
        HeaderValue::from(output.fetch_time.as_millis() as u64),
    );
    if !output.warnings.is_empty()
        && let Ok(v) = HeaderValue::from_str(&images::warnings_header(
            &output.warnings,
            MAX_WARNINGS_HEADER,
        ))
    {
        h.insert("x-render-warnings", v);
    }
    response
}

fn is_true(v: Option<&str>) -> bool {
    matches!(v, Some("true" | "1" | "yes"))
}

/// Keeps the filename to safe printable ASCII and ensures a `.pdf` suffix.
pub fn sanitize_filename(raw: &str) -> String {
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
