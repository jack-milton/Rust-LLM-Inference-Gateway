use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use async_trait::async_trait;
use tokio::{
    sync::Mutex,
    time::{sleep, Instant},
};
use tracing::{debug, warn};

use crate::{
    backend::{BackendError, BackendStream, InferenceBackend},
    models::{BackendChatResponse, NormalizedChatRequest},
};

#[derive(Clone)]
pub struct BackendRouter {
    endpoints: Arc<Vec<Endpoint>>,
    next_index: Arc<AtomicUsize>,
    failure_threshold: u32,
    cooldown: Duration,
}

#[derive(Clone)]
struct Endpoint {
    backend: Arc<dyn InferenceBackend>,
    health: Arc<Mutex<EndpointHealth>>,
}

#[derive(Debug, Default)]
struct EndpointHealth {
    consecutive_failures: u32,
    circuit_open_until: Option<Instant>,
    last_latency_ms: Option<u64>,
}

impl BackendRouter {
    pub fn new(backends: Vec<Arc<dyn InferenceBackend>>) -> Self {
        assert!(
            !backends.is_empty(),
            "at least one backend must be configured"
        );

        let endpoints = backends
            .into_iter()
            .map(|backend| Endpoint {
                backend,
                health: Arc::new(Mutex::new(EndpointHealth::default())),
            })
            .collect::<Vec<_>>();

        Self {
            endpoints: Arc::new(endpoints),
            next_index: Arc::new(AtomicUsize::new(0)),
            failure_threshold: 3,
            cooldown: Duration::from_secs(20),
        }
    }

    pub fn spawn_health_checks(self: Arc<Self>, interval: Duration) {
        tokio::spawn(async move {
            loop {
                self.check_once().await;
                sleep(interval).await;
            }
        });
    }

    async fn check_once(&self) {
        let probe_request = health_probe_request();
        for endpoint in self.endpoints.iter() {
            let started = Instant::now();
            let result = endpoint.backend.execute_chat(probe_request.clone()).await;
            let elapsed = started.elapsed().as_millis() as u64;
            let mut health = endpoint.health.lock().await;
            match result {
                Ok(_) => {
                    health.consecutive_failures = 0;
                    health.circuit_open_until = None;
                    health.last_latency_ms = Some(elapsed);
                }
                Err(error) => {
                    health.consecutive_failures = health.consecutive_failures.saturating_add(1);
                    health.last_latency_ms = Some(elapsed);
                    if health.consecutive_failures >= self.failure_threshold {
                        health.circuit_open_until = Some(Instant::now() + self.cooldown);
                    }
                    warn!(
                        backend = %endpoint.backend.name(),
                        error = %error,
                        failures = health.consecutive_failures,
                        "health check failed"
                    );
                }
            }
        }
    }

    async fn select_endpoint(&self) -> Result<Endpoint, BackendError> {
        let total = self.endpoints.len();
        let start = self.next_index.fetch_add(1, Ordering::Relaxed);
        let now = Instant::now();

        for offset in 0..total {
            let index = (start + offset) % total;
            let endpoint = self.endpoints[index].clone();
            let mut health = endpoint.health.lock().await;

            if let Some(until) = health.circuit_open_until {
                if until > now {
                    continue;
                }
                health.circuit_open_until = None;
                health.consecutive_failures = 0;
            }

            return Ok(endpoint);
        }

        Err(BackendError::Unavailable(
            "all backends are currently unhealthy".to_owned(),
        ))
    }

    async fn mark_success(&self, endpoint: &Endpoint, latency_ms: u64) {
        let mut health = endpoint.health.lock().await;
        health.consecutive_failures = 0;
        health.circuit_open_until = None;
        health.last_latency_ms = Some(latency_ms);
    }

    async fn mark_failure(&self, endpoint: &Endpoint, latency_ms: u64) {
        let mut health = endpoint.health.lock().await;
        health.consecutive_failures = health.consecutive_failures.saturating_add(1);
        health.last_latency_ms = Some(latency_ms);
        if health.consecutive_failures >= self.failure_threshold {
            health.circuit_open_until = Some(Instant::now() + self.cooldown);
            warn!(
                backend = %endpoint.backend.name(),
                failures = health.consecutive_failures,
                cooldown_secs = self.cooldown.as_secs(),
                "circuit opened for backend"
            );
        }
    }
}

#[async_trait]
impl InferenceBackend for BackendRouter {
    fn name(&self) -> &str {
        "backend-router"
    }

    async fn execute_chat(
        &self,
        request: NormalizedChatRequest,
    ) -> Result<BackendChatResponse, BackendError> {
        let endpoint = self.select_endpoint().await?;
        let started = Instant::now();
        let result = endpoint.backend.execute_chat(request).await;
        let latency_ms = started.elapsed().as_millis() as u64;
        match &result {
            Ok(_) => self.mark_success(&endpoint, latency_ms).await,
            Err(_) => self.mark_failure(&endpoint, latency_ms).await,
        }

        debug!(
            router = self.name(),
            backend = %endpoint.backend.name(),
            latency_ms,
            "execute_chat completed"
        );

        result
    }

    async fn stream_chat(&self, request: NormalizedChatRequest) -> Result<BackendStream, BackendError> {
        let endpoint = self.select_endpoint().await?;
        let started = Instant::now();
        let result = endpoint.backend.stream_chat(request).await;
        let latency_ms = started.elapsed().as_millis() as u64;
        match &result {
            Ok(_) => self.mark_success(&endpoint, latency_ms).await,
            Err(_) => self.mark_failure(&endpoint, latency_ms).await,
        }

        debug!(
            router = self.name(),
            backend = %endpoint.backend.name(),
            latency_ms,
            "stream_chat routed"
        );

        result
    }
}

fn health_probe_request() -> NormalizedChatRequest {
    use crate::models::{GenerationParams, MessageRole, NormalizedMessage};

    NormalizedChatRequest {
        request_id: "health-probe".to_owned(),
        user_id: "system".to_owned(),
        model: "health-probe".to_owned(),
        messages: vec![NormalizedMessage {
            role: MessageRole::User,
            content: "healthcheck".to_owned(),
        }],
        generation: GenerationParams {
            max_tokens: Some(1),
            temperature: None,
            top_p: None,
        },
        stream: false,
    }
}
