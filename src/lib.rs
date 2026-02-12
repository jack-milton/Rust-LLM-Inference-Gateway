pub mod auth;
pub mod backend;
pub mod batcher;
pub mod cache;
pub mod coalescing;
pub mod errors;
pub mod handlers;
pub mod limits;
pub mod metrics;
pub mod models;
pub mod router;
pub mod scheduler;
pub mod state;

use std::{sync::Arc, time::Duration};

use axum::{
    routing::{get, post},
    Router,
};
use backend::{mock::MockBackend, openai::OpenAiAdapter, InferenceBackend};
use router::BackendRouter;
use tracing::info;

pub fn build_state() -> Result<state::AppState, std::io::Error> {
    let mut backends: Vec<Arc<dyn InferenceBackend>> = Vec::new();
    if let Some(openai) = OpenAiAdapter::from_env().map_err(std::io::Error::other)? {
        backends.push(Arc::new(openai));
    }

    if backends.is_empty() {
        let backend_a: Arc<dyn InferenceBackend> = Arc::new(MockBackend::named("mock-a"));
        let backend_b: Arc<dyn InferenceBackend> = Arc::new(MockBackend::named("mock-b"));
        backends.push(backend_a);
        backends.push(backend_b);
    }

    let backend_names = backends
        .iter()
        .map(|backend| backend.name().to_owned())
        .collect::<Vec<_>>()
        .join(",");
    let router = Arc::new(BackendRouter::new(backends));
    router.clone().spawn_health_checks(Duration::from_secs(15));
    info!(backend = router.name(), endpoints = %backend_names, "backend router configured");
    Ok(state::AppState::new(router))
}

pub fn build_app(state: state::AppState) -> Router {
    Router::new()
        .route("/healthz", get(handlers::healthz))
        .route("/metrics", get(handlers::metrics))
        .route("/v1/chat/completions", post(handlers::chat_completions))
        .with_state(state)
}
