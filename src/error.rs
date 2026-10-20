//! One JSON error format for every HTTP error (PLAN.md §7):
//! `{"error": "<kind>", "message": "...", "details": [...]}`.

use std::time::Duration;

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::json;

use crate::images::ImageIssue;
use crate::render::RenderError;

/// One invalid input field.
#[derive(Debug, Clone, Serialize)]
pub struct FieldError {
    /// JSON pointer to the field (`""` for the whole body).
    pub path: String,
    pub msg: String,
}

/// Response extension naming the error kind, read by the metrics layer.
#[derive(Debug, Clone, Copy)]
pub struct ErrorKind(pub &'static str);

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("missing or invalid API key")]
    Unauthorized,
    #[error("unknown template {0:?}")]
    TemplateNotFound(String),
    #[error("template {0:?} has no sample.json")]
    NoSample(String),
    #[error("no route for {0}")]
    RouteNotFound(String),
    #[error("request body exceeds {limit} bytes")]
    PayloadTooLarge { limit: usize },
    #[error("invalid input")]
    Validation(Vec<FieldError>),
    #[error("an image could not be loaded")]
    Image(Vec<ImageIssue>),
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error("render exceeded its {0:?} deadline")]
    RenderTimeout(Duration),
    #[error("server is busy, retry later")]
    Overloaded,
    #[error("internal error: {0}")]
    Internal(String),
}

impl AppError {
    pub fn status(&self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::TemplateNotFound(_) | Self::NoSample(_) | Self::RouteNotFound(_) => {
                StatusCode::NOT_FOUND
            }
            Self::PayloadTooLarge { .. } => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Validation(_) | Self::Image(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Render(_) | Self::RenderTimeout(_) | Self::Internal(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::Overloaded => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::TemplateNotFound(_) | Self::NoSample(_) | Self::RouteNotFound(_) => "not_found",
            Self::PayloadTooLarge { .. } => "payload_too_large",
            Self::Validation(_) => "validation",
            Self::Image(_) => "image",
            Self::Render(RenderError::MainNotFound(_)) => "internal",
            Self::Render(_) => "compile",
            Self::RenderTimeout(_) => "render_timeout",
            Self::Overloaded => "overloaded",
            Self::Internal(_) => "internal",
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let details = match &self {
            Self::Validation(fields) => json!(fields),
            Self::Image(issues) => json!(issues),
            Self::Render(err) => json!(err.diagnostics()),
            _ => json!([]),
        };
        if status.is_server_error() {
            tracing::warn!(kind = self.kind(), error = %self, "request failed");
        }
        let body = json!({ "error": self.kind(), "message": self.to_string(), "details": details });
        let mut response = (status, axum::Json(body)).into_response();
        match self {
            Self::Overloaded => {
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            }
            Self::Unauthorized => {
                response
                    .headers_mut()
                    .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            }
            _ => {}
        }
        response.extensions_mut().insert(ErrorKind(self.kind()));
        response
    }
}
