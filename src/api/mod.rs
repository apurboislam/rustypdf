//! HTTP API: router, shared state and the server loop.

pub mod ops;
pub mod render;
pub mod templates;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use std::time::Instant;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Uri, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{get, post};
use tokio::sync::Semaphore;
use tracing::Instrument;

use crate::config::Config;
use crate::error::{AppError, ErrorKind};
use crate::fonts::FontStore;
use crate::images::fetch::Fetcher;
use crate::memo::{self, MemoEvictor};
use crate::metrics::Metrics;
use crate::registry::{self, ReloadSummary, TemplateRegistry};
use crate::world::Shared;

pub struct AppState {
    pub config: Config,
    pub metrics: Metrics,
    pub shared: Arc<Shared>,
    pub registry: TemplateRegistry,
    /// One permit per concurrent compile (`MAX_CONCURRENCY`).
    pub compile_slots: Arc<Semaphore>,
    /// URL image fetcher and its cache, shared by all requests.
    pub fetcher: Fetcher,
    /// Bounds Typst's memo cache: evicts every `MEMO_EVICT_EVERY` renders.
    pub memo: Arc<MemoEvictor>,
    /// Set once startup finished; cleared when shutdown begins.
    pub ready: AtomicBool,
}

impl AppState {
    /// Loads fonts (failing if a required one is missing) and templates.
    pub fn load(config: Config) -> Result<Arc<Self>> {
        let fetcher = Fetcher::new(&config);
        Self::load_with(config, fetcher)
    }

    /// Like [`AppState::load`], with a given image fetcher (tests use one
    /// that points at a mock server).
    pub fn load_with(config: Config, fetcher: Fetcher) -> Result<Arc<Self>> {
        let fonts = FontStore::load(&config.fonts_dir, &config.required_fonts)?;
        tracing::info!(faces = fonts.len(), families = ?fonts.families(), "fonts loaded");
        let shared = Arc::new(Shared::new(Arc::new(fonts)));
        let templates = registry::load(&config.templates_dir, &shared)?;
        tracing::info!(count = templates.len(), dir = %config.templates_dir.display(), "templates loaded");
        let metrics = Metrics::new();
        metrics.register_fetch(&fetcher.metrics);
        Ok(Arc::new(Self {
            metrics,
            compile_slots: Arc::new(Semaphore::new(config.max_concurrency)),
            fetcher,
            memo: Arc::new(MemoEvictor::new(config.memo_evict_every)),
            registry: TemplateRegistry::new(templates),
            shared,
            config,
            ready: AtomicBool::new(true),
        }))
    }
}

impl AppState {
    /// Re-scans `TEMPLATES_DIR` and swaps the registry (blocking: it
    /// compiles every template's sample).
    pub fn reload(&self) -> Result<ReloadSummary> {
        let start = Instant::now();
        let result = self
            .registry
            .reload(&self.config.templates_dir, &self.shared);
        match &result {
            Ok(summary) => {
                let outcome = if summary.kept_previous.is_empty() && summary.failed.is_empty() {
                    "ok"
                } else {
                    "partial"
                };
                self.metrics.reloads.with_label_values(&[outcome]).inc();
                tracing::info!(
                    templates = summary.templates.len(),
                    kept_previous = summary.kept_previous.len(),
                    failed = summary.failed.len(),
                    removed = ?summary.removed,
                    ms = start.elapsed().as_millis() as u64,
                    "templates reloaded"
                );
            }
            Err(err) => {
                self.metrics.reloads.with_label_values(&["failed"]).inc();
                tracing::error!(error = %err, "template reload failed; keeping every template");
            }
        }
        result
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/render/{template}", post(render::render))
        .route("/templates", get(templates::list))
        .route("/templates/{template}", get(templates::info))
        .route("/preview/{template}", get(templates::preview))
        .route("/admin/reload", post(ops::reload))
        .route("/metrics", get(ops::metrics))
        .route("/healthz", get(ops::healthz))
        .route("/readyz", get(ops::readyz))
        .fallback(|uri: Uri| async move { AppError::RouteNotFound(uri.path().to_owned()) })
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .layer(middleware::from_fn_with_state(state.clone(), observe))
        .with_state(state)
}

/// Paths that never need an API key (probes).
fn is_public(path: &str) -> bool {
    matches!(path, "/healthz" | "/readyz")
}

/// `Authorization: Bearer <key>` against `API_KEYS`. Off when no key is set.
async fn auth(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let keys = &state.config.api_keys;
    if keys.is_empty() || is_public(req.uri().path()) {
        return Ok(next.run(req).await);
    }
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            let (scheme, token) = v.split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
        });
    match token {
        // Compare against every key so timing does not reveal which matched.
        Some(token)
            if keys
                .iter()
                .fold(false, |ok, key| ok | constant_time_eq(key, token)) =>
        {
            Ok(next.run(req).await)
        }
        _ => Err(AppError::Unauthorized),
    }
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Request ID, span, access log and request/error metrics.
async fn observe(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_graphic()))
        .map_or_else(|| uuid::Uuid::new_v4().simple().to_string(), str::to_owned);
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let span = tracing::info_span!("request", request_id = %id, %method, %path);
    let start = Instant::now();
    let mut response = next.run(req).instrument(span.clone()).await;

    let status = response.status();
    state
        .metrics
        .requests
        .with_label_values(&[status.as_str()])
        .inc();
    if let Some(ErrorKind(kind)) = response.extensions().get::<ErrorKind>() {
        state.metrics.errors.with_label_values(&[kind]).inc();
    }
    if let Ok(v) = HeaderValue::from_str(&id) {
        response.headers_mut().insert(REQUEST_ID, v);
    }
    let ms = start.elapsed().as_millis() as u64;
    span.in_scope(|| {
        if matches!(path.as_str(), "/healthz" | "/readyz" | "/metrics") {
            tracing::debug!(status = status.as_u16(), ms, "request");
        } else {
            tracing::info!(status = status.as_u16(), ms, "request");
        }
    });
    response
}

const REQUEST_ID: &str = "x-request-id";

/// How long the templates folder must be quiet before a hot reload runs.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(300);

/// Watches `TEMPLATES_DIR` and reloads after changes settle. The returned
/// watcher must be kept alive.
pub fn watch_templates(state: Arc<AppState>) -> Result<notify::RecommendedWatcher> {
    use notify::{EventKind, RecursiveMode, Watcher};
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && !matches!(event.kind, EventKind::Access(_))
        {
            let _ = tx.send(());
        }
    })?;
    let dir = state.config.templates_dir.clone();
    watcher
        .watch(&dir, RecursiveMode::Recursive)
        .with_context(|| format!("cannot watch {}", dir.display()))?;
    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            // Wait until no event arrived for RELOAD_DEBOUNCE.
            loop {
                match tokio::time::timeout(RELOAD_DEBOUNCE, rx.recv()).await {
                    Ok(Some(())) => continue,
                    Ok(None) => return,
                    Err(_) => break,
                }
            }
            let state = state.clone();
            let _ = tokio::task::spawn_blocking(move || state.reload()).await;
        }
    });
    tracing::info!(dir = %dir.display(), "watching templates for changes");
    Ok(watcher)
}

/// Runs the service until SIGTERM or Ctrl-C, then drains in-flight requests.
pub async fn serve(config: Config) -> Result<()> {
    tracing::info!(?config, "starting rustypdf");
    let port = config.port;
    let state = AppState::load(config)?;

    // Renders evict as they go (`MemoEvictor::after_render`); this releases
    // memory once traffic stops.
    let idle_state = state.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(memo::IDLE_EVICT_INTERVAL);
        let mut last = 0;
        loop {
            tick.tick().await;
            let memo = idle_state.memo.clone();
            last = tokio::task::spawn_blocking(move || {
                memo.idle_tick(&mut last);
                last
            })
            .await
            .unwrap_or(last);
        }
    });

    // Kept alive until the server stops.
    let _watcher = if state.config.hot_reload {
        Some(watch_templates(state.clone())?)
    } else {
        None
    };

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port))
        .await
        .with_context(|| format!("cannot listen on port {port}"))?;
    tracing::info!(port, "listening");
    let shutdown_state = state.clone();
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            shutdown_state.ready.store(false, Ordering::SeqCst);
            tracing::info!("shutting down, draining in-flight requests");
        })
        .await?;
    tracing::info!("stopped");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
}
