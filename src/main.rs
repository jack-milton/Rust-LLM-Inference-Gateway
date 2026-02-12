mod auth;
mod backend;
mod batcher;
mod cache;
mod coalescing;
mod errors;
mod handlers;
mod limits;
mod metrics;
mod models;
mod router;
mod scheduler;
mod state;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    routing::{get, post},
    Router,
};
use backend::{mock::MockBackend, openai::OpenAiAdapter, InferenceBackend};
use router::BackendRouter;
use state::AppState;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,rust_llm_inference_gateway=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let mut backends: Vec<Arc<dyn InferenceBackend>> = Vec::new();
    if let Some(openai) = OpenAiAdapter::from_env()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::Other, error))?
    {
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
    let state = AppState::new(router);

    let app = Router::new()
        .route("/healthz", get(handlers::healthz))
        .route("/metrics", get(handlers::metrics))
        .route("/v1/chat/completions", post(handlers::chat_completions))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "gateway listening");

    axum::serve(listener, app).await?;
    Ok(())
}
