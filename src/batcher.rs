use std::{
    collections::VecDeque,
    env,
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

use crate::{
    backend::{BackendError, BackendStream, InferenceBackend},
    models::NormalizedChatRequest,
};

#[derive(Clone)]
pub struct Batcher {
    backend: Arc<dyn InferenceBackend>,
    tx: mpsc::Sender<BatchItem>,
}

#[derive(Debug, Clone, Copy)]
pub struct BatchConfig {
    pub enabled: bool,
    pub max_batch_size: usize,
    pub max_wait: Duration,
}

impl BatchConfig {
    pub fn from_env() -> Self {
        let enabled = env::var("GATEWAY_BATCH_ENABLED")
            .ok()
            .map(|value| value != "0" && !value.eq_ignore_ascii_case("false"))
            .unwrap_or(true);
        let max_batch_size = env::var("GATEWAY_BATCH_MAX_SIZE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(8);
        let max_wait_ms = env::var("GATEWAY_BATCH_MAX_WAIT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(10);

        Self {
            enabled,
            max_batch_size,
            max_wait: Duration::from_millis(max_wait_ms),
        }
    }
}

struct BatchItem {
    class: BatchClass,
    request: NormalizedChatRequest,
    response_tx: oneshot::Sender<Result<crate::models::BackendChatResponse, BackendError>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BatchClass {
    model: String,
    max_tokens: Option<u32>,
    temperature_repr: String,
    top_p_repr: String,
}

impl BatchClass {
    fn from_request(request: &NormalizedChatRequest) -> Self {
        Self {
            model: request.model.clone(),
            max_tokens: request.generation.max_tokens,
            temperature_repr: format_float(request.generation.temperature),
            top_p_repr: format_float(request.generation.top_p),
        }
    }
}

impl Batcher {
    pub fn new(backend: Arc<dyn InferenceBackend>, config: BatchConfig) -> Self {
        let (tx, rx) = mpsc::channel(1_024);
        let worker_backend = backend.clone();
        tokio::spawn(run_batch_worker(worker_backend, rx, config));
        Self { backend, tx }
    }

    async fn submit(
        &self,
        request: NormalizedChatRequest,
    ) -> Result<crate::models::BackendChatResponse, BackendError> {
        let (response_tx, response_rx) = oneshot::channel();
        let class = BatchClass::from_request(&request);
        self.tx
            .send(BatchItem {
                class,
                request,
                response_tx,
            })
            .await
            .map_err(|_| BackendError::Unavailable("batcher queue closed".to_owned()))?;

        response_rx
            .await
            .map_err(|_| BackendError::Unavailable("batch response channel closed".to_owned()))?
    }
}

#[async_trait]
impl InferenceBackend for Batcher {
    fn name(&self) -> &str {
        "micro-batcher"
    }

    async fn execute_chat(
        &self,
        request: NormalizedChatRequest,
    ) -> Result<crate::models::BackendChatResponse, BackendError> {
        self.submit(request).await
    }

    async fn stream_chat(
        &self,
        request: NormalizedChatRequest,
    ) -> Result<BackendStream, BackendError> {
        self.backend.stream_chat(request).await
    }
}

async fn run_batch_worker(
    backend: Arc<dyn InferenceBackend>,
    mut rx: mpsc::Receiver<BatchItem>,
    config: BatchConfig,
) {
    let mut pending = VecDeque::new();
    loop {
        let first = if let Some(item) = pending.pop_front() {
            item
        } else {
            match rx.recv().await {
                Some(item) => item,
                None => break,
            }
        };

        if !config.enabled {
            let result = backend.execute_chat(first.request).await;
            let _ = first.response_tx.send(result);
            continue;
        }

        let class = first.class.clone();
        let deadline = Instant::now() + config.max_wait;
        let mut batch = vec![first];

        while batch.len() < config.max_batch_size {
            if let Some(position) = pending.iter().position(|item| item.class == class) {
                if let Some(item) = pending.remove(position) {
                    batch.push(item);
                    continue;
                }
            }

            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let remaining = deadline - now;
            let next = tokio::time::timeout(remaining, rx.recv()).await;
            match next {
                Ok(Some(item)) => {
                    if item.class == class {
                        batch.push(item);
                    } else {
                        pending.push_back(item);
                    }
                }
                Ok(None) => break,
                Err(_) => break,
            }
        }

        debug!(
            batch_size = batch.len(),
            model = %class.model,
            max_tokens = ?class.max_tokens,
            "flushing micro-batch"
        );

        // Adapter boundary supports per-request execution today; real providers can replace this
        // with a true batched call while preserving scheduler behavior.
        for item in batch {
            let result = backend.execute_chat(item.request).await;
            let _ = item.response_tx.send(result);
        }
    }
}

fn format_float(value: Option<f32>) -> String {
    value
        .map(|number| format!("{number:.4}"))
        .unwrap_or_else(|| "none".to_owned())
}
