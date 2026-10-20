//! HTTP API: router, shared state and the server loop.

pub mod ops;
pub mod render;
pub mod templates;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use axum::http::Uri;
use axum::routing::{get, post};
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::error::AppError;
use crate::fonts::FontStore;
use crate::images::fetch::Fetcher;
use crate::registry::{self, TemplateRegistry};
use crate::world::Shared;

/// How often `comemo::evict` runs. Memoized results unused for
/// `EVICT_MAX_AGE` consecutive runs are dropped.
const EVICT_INTERVAL: Duration = Duration::from_secs(10);
const EVICT_MAX_AGE: usize = 30;

pub struct AppState {
    pub config: Config,
    pub shared: Arc<Shared>,
    pub registry: TemplateRegistry,
    /// One permit per concurrent compile (`MAX_CONCURRENCY`).
    pub compile_slots: Arc<Semaphore>,
    /// URL image fetcher and its cache, shared by all requests.
    pub fetcher: Fetcher,
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
        Ok(Arc::new(Self {
            compile_slots: Arc::new(Semaphore::new(config.max_concurrency)),
            fetcher,
            registry: TemplateRegistry::new(templates),
            shared,
            config,
            ready: AtomicBool::new(true),
        }))
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/render/{template}", post(render::render))
        .route("/templates", get(templates::list))
        .route("/templates/{template}", get(templates::info))
        .route("/preview/{template}", get(templates::preview))
        .route("/healthz", get(ops::healthz))
        .route("/readyz", get(ops::readyz))
        .fallback(|uri: Uri| async move { AppError::RouteNotFound(uri.path().to_owned()) })
        .with_state(state)
}

/// Runs the service until SIGTERM or Ctrl-C, then drains in-flight requests.
pub async fn serve(config: Config) -> Result<()> {
    tracing::info!(?config, "starting rustypdf");
    let port = config.port;
    let state = AppState::load(config)?;

    tokio::spawn(async {
        let mut tick = tokio::time::interval(EVICT_INTERVAL);
        loop {
            tick.tick().await;
            typst::comemo::evict(EVICT_MAX_AGE);
        }
    });

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
