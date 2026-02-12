use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use tokio::{sync::mpsc, time::sleep};
use tokio_stream::wrappers::ReceiverStream;
use tracing::debug;

use crate::backend::{BackendError, BackendStream, InferenceBackend};
use crate::models::{BackendChatResponse, BackendChunk, MessageRole, NormalizedChatRequest, Usage};

#[derive(Debug, Clone)]
pub struct MockBackend {
    name: String,
    token_delay: Duration,
}

impl Default for MockBackend {
    fn default() -> Self {
        Self {
            name: "mock-backend".to_owned(),
            token_delay: Duration::from_millis(35),
        }
    }
}

impl MockBackend {
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
}

#[async_trait]
impl InferenceBackend for MockBackend {
    fn name(&self) -> &str {
        &self.name
    }

    async fn execute_chat(
        &self,
        request: NormalizedChatRequest,
    ) -> Result<BackendChatResponse, BackendError> {
        let content = render_response(&request);
        let usage = estimate_usage(&request, &content);

        Ok(BackendChatResponse {
            content,
            finish_reason: "stop".to_owned(),
            usage,
        })
    }

    async fn stream_chat(
        &self,
        request: NormalizedChatRequest,
    ) -> Result<BackendStream, BackendError> {
        let content = render_response(&request);
        let usage = estimate_usage(&request, &content);
        let delay = self.token_delay;
        let (tx, rx) = mpsc::channel(32);

        tokio::spawn(async move {
            let tokens = split_for_stream(&content);
            for token in tokens {
                if tx
                    .send(Ok(BackendChunk {
                        delta: Some(token),
                        finish_reason: None,
                        usage: None,
                        done: false,
                    }))
                    .await
                    .is_err()
                {
                    return;
                }

                sleep(delay).await;
            }

            let _ = tx
                .send(Ok(BackendChunk {
                    delta: None,
                    finish_reason: Some("stop".to_owned()),
                    usage: Some(usage),
                    done: true,
                }))
                .await;
        });

        debug!(backend = %self.name, "stream prepared");
        Ok(ReceiverStream::new(rx).boxed())
    }
}

fn render_response(request: &NormalizedChatRequest) -> String {
    let prompt = request
        .messages
        .iter()
        .rev()
        .find(|message| message.role == MessageRole::User)
        .map(|message| message.content.as_str())
        .unwrap_or("hello");

    format!("Mock response for model {}: {}", request.model, prompt)
}

fn estimate_usage(request: &NormalizedChatRequest, completion: &str) -> Usage {
    let prompt_tokens = request
        .messages
        .iter()
        .map(|message| rough_token_estimate(&message.content))
        .sum::<u32>();
    let completion_tokens = rough_token_estimate(completion);
    Usage::new(prompt_tokens, completion_tokens)
}

fn rough_token_estimate(text: &str) -> u32 {
    if text.trim().is_empty() {
        return 0;
    }
    text.split_whitespace().count() as u32
}

fn split_for_stream(text: &str) -> Vec<String> {
    let tokens: Vec<String> = text.split_whitespace().map(ToString::to_string).collect();
    let len = tokens.len();

    tokens
        .into_iter()
        .enumerate()
        .map(|(index, token)| {
            if index + 1 == len {
                token
            } else {
                format!("{token} ")
            }
        })
        .collect()
}
