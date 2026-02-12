mod auth;
mod backend;
mod coalescing;
mod errors;
mod handlers;
mod limits;
mod models;
mod router;
mod scheduler;
mod state;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    routing::{get, post},
    Router,
};
use backend::{mock::MockBackend, InferenceBackend};
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

    let backend_a: Arc<dyn InferenceBackend> = Arc::new(MockBackend::named("mock-a"));
    let backend_b: Arc<dyn InferenceBackend> = Arc::new(MockBackend::named("mock-b"));
    let router = Arc::new(BackendRouter::new(vec![backend_a, backend_b]));
    router
        .clone()
        .spawn_health_checks(Duration::from_secs(15));
    info!(backend = router.name(), "backend router configured");
    let state = AppState::new(router);

    let app = Router::new()
        .route("/healthz", get(handlers::healthz))
        .route("/v1/chat/completions", post(handlers::chat_completions))
        .with_state(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "gateway listening");

    axum::serve(listener, app).await?;
    Ok(())
}
