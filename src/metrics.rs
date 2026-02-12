use std::time::Duration;

use prometheus::{
    opts, Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, Registry, TextEncoder,
};

use crate::models::Usage;

#[derive(Clone)]
pub struct AppMetrics {
    registry: Registry,
    request_total: IntCounterVec,
    request_duration_seconds: HistogramVec,
    inflight_requests: IntGauge,
    backend_errors_total: IntCounterVec,
    tokens_total: IntCounterVec,
}

pub struct InflightGuard<'a> {
    metrics: &'a AppMetrics,
}

impl AppMetrics {
    pub fn new() -> Self {
        let registry = Registry::new();

        let request_total = IntCounterVec::new(
            opts!(
                "gateway_http_requests_total",
                "Total HTTP requests processed by gateway"
            ),
            &["path", "method", "status", "stream"],
        )
        .expect("valid request_total metric");

        let request_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "gateway_http_request_duration_seconds",
                "HTTP request latency in seconds",
            ),
            &["path", "method", "stream"],
        )
        .expect("valid request_duration_seconds metric");

        let inflight_requests = IntGauge::new(
            "gateway_inflight_requests",
            "Current in-flight requests at gateway",
        )
        .expect("valid inflight_requests metric");

        let backend_errors_total = IntCounterVec::new(
            opts!(
                "gateway_backend_errors_total",
                "Total backend-related errors by stage"
            ),
            &["stage"],
        )
        .expect("valid backend_errors_total metric");

        let tokens_total = IntCounterVec::new(
            opts!(
                "gateway_tokens_total",
                "Token accounting aggregated by type"
            ),
            &["kind"],
        )
        .expect("valid tokens_total metric");

        registry
            .register(Box::new(request_total.clone()))
            .expect("register request_total");
        registry
            .register(Box::new(request_duration_seconds.clone()))
            .expect("register request_duration_seconds");
        registry
            .register(Box::new(inflight_requests.clone()))
            .expect("register inflight_requests");
        registry
            .register(Box::new(backend_errors_total.clone()))
            .expect("register backend_errors_total");
        registry
            .register(Box::new(tokens_total.clone()))
            .expect("register tokens_total");

        Self {
            registry,
            request_total,
            request_duration_seconds,
            inflight_requests,
            backend_errors_total,
            tokens_total,
        }
    }

    pub fn inflight_guard(&self) -> InflightGuard<'_> {
        self.inflight_requests.inc();
        InflightGuard { metrics: self }
    }

    pub fn observe_request(
        &self,
        path: &str,
        method: &str,
        stream: bool,
        status: u16,
        duration: Duration,
    ) {
        let stream_label = if stream { "true" } else { "false" };
        let status_label = status.to_string();
        self.request_total
            .with_label_values(&[path, method, &status_label, stream_label])
            .inc();
        self.request_duration_seconds
            .with_label_values(&[path, method, stream_label])
            .observe(duration.as_secs_f64());
    }

    pub fn observe_backend_error(&self, stage: &str) {
        self.backend_errors_total.with_label_values(&[stage]).inc();
    }

    pub fn observe_usage(&self, usage: &Usage) {
        self.tokens_total
            .with_label_values(&["prompt"])
            .inc_by(usage.prompt_tokens as u64);
        self.tokens_total
            .with_label_values(&["completion"])
            .inc_by(usage.completion_tokens as u64);
        self.tokens_total
            .with_label_values(&["total"])
            .inc_by(usage.total_tokens as u64);
    }

    pub fn render(&self) -> Result<String, String> {
        let mut buffer = Vec::new();
        let encoder = TextEncoder::new();
        let families = self.registry.gather();
        encoder
            .encode(&families, &mut buffer)
            .map_err(|error| error.to_string())?;
        String::from_utf8(buffer).map_err(|error| error.to_string())
    }
}

impl Default for AppMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        self.metrics.inflight_requests.dec();
    }
}
