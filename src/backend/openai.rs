use std::{env, time::Duration};

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::json;
use tracing::debug;

use crate::{
    backend::{BackendError, BackendStream, InferenceBackend},
    models::{BackendChatResponse, BackendChunk, MessageRole, NormalizedChatRequest, Usage},
};

#[derive(Clone)]
pub struct OpenAiAdapter {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl OpenAiAdapter {
    pub fn from_env() -> Result<Option<Self>, String> {
        let Some(api_key) = env::var("OPENAI_API_KEY")
            .ok()
            .filter(|value| !value.is_empty())
        else {
            return Ok(None);
        };
        let base_url = env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_owned())
            .trim_end_matches('/')
            .to_owned();
        let timeout_secs = env::var("OPENAI_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(60);

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .map_err(|error| format!("failed to build OpenAI HTTP client: {error}"))?;

        Ok(Some(Self {
            client,
            api_key,
            base_url,
        }))
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base_url, path.trim_start_matches('/'))
    }
}

#[async_trait]
impl InferenceBackend for OpenAiAdapter {
    fn name(&self) -> &str {
        "openai-adapter"
    }

    async fn execute_chat(
        &self,
        request: NormalizedChatRequest,
    ) -> Result<BackendChatResponse, BackendError> {
        let payload = json!({
            "model": request.model,
            "messages": request
                .messages
                .iter()
                .map(|message| json!({"role": role_name(&message.role), "content": message.content}))
                .collect::<Vec<_>>(),
            "max_tokens": request.generation.max_tokens,
            "temperature": request.generation.temperature,
            "top_p": request.generation.top_p,
            "stream": false
        });

        let response = self
            .client
            .post(self.url("/chat/completions"))
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|error| BackendError::Unavailable(error.to_string()))?;

        if !response.status().is_success() {
            return Err(map_http_error(
                response.status(),
                response
                    .text()
                    .await
                    .unwrap_or_else(|_| "unknown backend error".to_owned()),
            ));
        }

        let parsed: OpenAiChatResponse = response
            .json()
            .await
            .map_err(|error| BackendError::InvalidResponse(error.to_string()))?;

        let choice = parsed.choices.first().ok_or_else(|| {
            BackendError::InvalidResponse("missing choices in response".to_owned())
        })?;
        let content = choice.message.content.clone().unwrap_or_default();
        let usage = parsed.usage.map(Usage::from).unwrap_or_else(|| {
            let prompt_tokens = request
                .messages
                .iter()
                .map(|message| rough_token_estimate(&message.content))
                .sum::<u32>();
            let completion_tokens = rough_token_estimate(&content);
            Usage::new(prompt_tokens, completion_tokens)
        });

        Ok(BackendChatResponse {
            content,
            finish_reason: choice
                .finish_reason
                .clone()
                .unwrap_or_else(|| "stop".to_owned()),
            usage,
        })
    }

    async fn stream_chat(
        &self,
        request: NormalizedChatRequest,
    ) -> Result<BackendStream, BackendError> {
        let payload = json!({
            "model": request.model,
            "messages": request
                .messages
                .iter()
                .map(|message| json!({"role": role_name(&message.role), "content": message.content}))
                .collect::<Vec<_>>(),
            "max_tokens": request.generation.max_tokens,
            "temperature": request.generation.temperature,
            "top_p": request.generation.top_p,
            "stream": true,
            "stream_options": {
                "include_usage": true
            }
        });

        let response = self
            .client
            .post(self.url("/chat/completions"))
            .bearer_auth(&self.api_key)
            .json(&payload)
            .send()
            .await
            .map_err(|error| BackendError::Unavailable(error.to_string()))?;

        if !response.status().is_success() {
            return Err(map_http_error(
                response.status(),
                response
                    .text()
                    .await
                    .unwrap_or_else(|_| "unknown backend error".to_owned()),
            ));
        }

        let mut upstream = response.bytes_stream();
        let mut buffer = String::new();

        let stream = async_stream::stream! {
            let mut final_usage: Option<Usage> = None;
            let mut done_emitted = false;

            while let Some(next) = upstream.next().await {
                let bytes = match next {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        yield Err(BackendError::Unavailable(error.to_string()));
                        break;
                    }
                };

                let text = match std::str::from_utf8(&bytes) {
                    Ok(text) => text,
                    Err(error) => {
                        yield Err(BackendError::InvalidResponse(error.to_string()));
                        break;
                    }
                };

                buffer.push_str(text);

                while let Some(index) = buffer.find('\n') {
                    let line = buffer[..index].trim().to_owned();
                    buffer.drain(..=index);
                    if line.is_empty() {
                        continue;
                    }

                    let Some(payload) = line.strip_prefix("data:") else {
                        continue;
                    };
                    let payload = payload.trim();

                    if payload == "[DONE]" {
                        if !done_emitted {
                            yield Ok(BackendChunk {
                                delta: None,
                                finish_reason: Some("stop".to_owned()),
                                usage: final_usage.clone(),
                                done: true,
                            });
                            done_emitted = true;
                        }
                        continue;
                    }

                    let parsed: OpenAiStreamResponse = match serde_json::from_str(payload) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            yield Err(BackendError::InvalidResponse(error.to_string()));
                            continue;
                        }
                    };

                    if let Some(usage) = parsed.usage.map(Usage::from) {
                        final_usage = Some(usage);
                    }

                    if let Some(choice) = parsed.choices.first() {
                        if let Some(content) = choice.delta.content.clone().filter(|value| !value.is_empty()) {
                            yield Ok(BackendChunk {
                                delta: Some(content),
                                finish_reason: None,
                                usage: None,
                                done: false,
                            });
                        }

                        if let Some(reason) = choice.finish_reason.clone() {
                            if !done_emitted {
                                yield Ok(BackendChunk {
                                    delta: None,
                                    finish_reason: Some(reason),
                                    usage: final_usage.clone(),
                                    done: true,
                                });
                                done_emitted = true;
                            }
                        }
                    }
                }
            }

            if !done_emitted {
                yield Ok(BackendChunk {
                    delta: None,
                    finish_reason: Some("stop".to_owned()),
                    usage: final_usage,
                    done: true,
                });
            }
        };

        debug!(backend = self.name(), "stream prepared");
        Ok(stream.boxed())
    }
}

fn map_http_error(status: StatusCode, body: String) -> BackendError {
    let trimmed = body.chars().take(400).collect::<String>();
    match status {
        StatusCode::TOO_MANY_REQUESTS => {
            BackendError::Unavailable(format!("rate limited: {trimmed}"))
        }
        StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT => {
            BackendError::Timeout(format!("upstream timeout: {trimmed}"))
        }
        _ => BackendError::InvalidResponse(format!("status {}: {trimmed}", status.as_u16())),
    }
}

fn role_name(role: &MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

fn rough_token_estimate(text: &str) -> u32 {
    if text.trim().is_empty() {
        return 0;
    }
    text.split_whitespace().count() as u32
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamResponse {
    #[serde(default)]
    choices: Vec<OpenAiStreamChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAiStreamChoice {
    #[serde(default)]
    delta: OpenAiDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct OpenAiDelta {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

impl From<OpenAiUsage> for Usage {
    fn from(value: OpenAiUsage) -> Self {
        Usage {
            prompt_tokens: value.prompt_tokens,
            completion_tokens: value.completion_tokens,
            total_tokens: value.total_tokens,
        }
    }
}
