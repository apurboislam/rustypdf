//! Prometheus metrics, served at `GET /metrics`.

use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounter, IntCounterVec, IntGauge, Opts, Registry,
    TextEncoder,
};

/// Latency buckets in seconds, from 1 ms to 10 s.
const BUCKETS: &[f64] = &[
    0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];

pub struct Metrics {
    registry: Registry,
    /// Compile + PDF export time per template.
    pub render_seconds: HistogramVec,
    /// Renders currently holding a compile slot.
    pub renders_in_flight: IntGauge,
    /// Error responses by kind (`validation`, `image`, `compile`, ...).
    pub errors: IntCounterVec,
    /// HTTP responses by status code.
    pub requests: IntCounterVec,
    /// Template reloads by result (`ok`, `failed`).
    pub reloads: IntCounterVec,
}

/// The image fetcher's metrics. They are created by the fetcher and
/// registered by [`Metrics::register_fetch`].
#[derive(Clone)]
pub struct FetchMetrics {
    /// Network fetch time (cache misses only).
    pub fetch_seconds: HistogramVec,
    pub cache_hits: IntCounter,
    pub cache_misses: IntCounter,
}

impl Default for FetchMetrics {
    fn default() -> Self {
        Self {
            fetch_seconds: HistogramVec::new(
                HistogramOpts::new(
                    "rustypdf_image_fetch_seconds",
                    "Image download time, by result",
                )
                .buckets(BUCKETS.to_vec()),
                &["result"],
            )
            .expect("valid metric"),
            cache_hits: IntCounter::new("rustypdf_image_cache_hits_total", "Image cache hits")
                .expect("valid metric"),
            cache_misses: IntCounter::new(
                "rustypdf_image_cache_misses_total",
                "Image cache misses (each one is a download or joins one)",
            )
            .expect("valid metric"),
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Registry::new();
        let render_seconds = HistogramVec::new(
            HistogramOpts::new(
                "rustypdf_render_seconds",
                "Compile and PDF export time, by template",
            )
            .buckets(BUCKETS.to_vec()),
            &["template"],
        )
        .expect("valid metric");
        let renders_in_flight = IntGauge::new(
            "rustypdf_renders_in_flight",
            "Renders holding a compile slot",
        )
        .expect("valid metric");
        let errors = IntCounterVec::new(
            Opts::new("rustypdf_errors_total", "Error responses, by kind"),
            &["kind"],
        )
        .expect("valid metric");
        let requests = IntCounterVec::new(
            Opts::new("rustypdf_http_requests_total", "HTTP responses, by status"),
            &["status"],
        )
        .expect("valid metric");
        let reloads = IntCounterVec::new(
            Opts::new(
                "rustypdf_template_reloads_total",
                "Template reloads, by result",
            ),
            &["result"],
        )
        .expect("valid metric");
        for c in [
            Box::new(render_seconds.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(renders_in_flight.clone()),
            Box::new(errors.clone()),
            Box::new(requests.clone()),
            Box::new(reloads.clone()),
        ] {
            registry.register(c).expect("metric names are unique");
        }
        Self {
            registry,
            render_seconds,
            renders_in_flight,
            errors,
            requests,
            reloads,
        }
    }

    pub fn register_fetch(&self, fetch: &FetchMetrics) {
        for c in [
            Box::new(fetch.fetch_seconds.clone()) as Box<dyn prometheus::core::Collector>,
            Box::new(fetch.cache_hits.clone()),
            Box::new(fetch.cache_misses.clone()),
        ] {
            self.registry.register(c).expect("metric names are unique");
        }
    }

    /// The Prometheus text exposition of every metric.
    pub fn encode(&self) -> String {
        let mut out = Vec::new();
        TextEncoder::new()
            .encode(&self.registry.gather(), &mut out)
            .expect("text encoding never fails");
        String::from_utf8(out).expect("text encoding is UTF-8")
    }
}

/// Decrements the in-flight gauge when dropped, so abandoned renders are
/// counted until they really finish.
pub struct InFlight(IntGauge);

impl InFlight {
    pub fn start(gauge: &IntGauge) -> Self {
        gauge.inc();
        Self(gauge.clone())
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.dec();
    }
}
