use std::net::SocketAddr;

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

    let state = rust_llm_inference_gateway::build_state()?;
    let app = rust_llm_inference_gateway::build_app(state);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!(%addr, "gateway listening");

    axum::serve(listener, app).await?;
    Ok(())
}
