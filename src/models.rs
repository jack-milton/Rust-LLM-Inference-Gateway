use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionsRequest {
    pub model: String,
    pub messages: Vec<OpenAiMessage>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub top_p: Option<f32>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OpenAiMessage {
    pub role: MessageRole,
    pub content: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone)]
pub struct NormalizedChatRequest {
    pub request_id: String,
    pub user_id: String,
    pub model: String,
    pub messages: Vec<NormalizedMessage>,
    pub generation: GenerationParams,
    pub stream: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct NormalizedMessage {
    pub role: MessageRole,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct GenerationParams {
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
}

impl ChatCompletionsRequest {
    pub fn into_normalized(self, user_id: String) -> Result<NormalizedChatRequest, String> {
        if self.model.trim().is_empty() {
            return Err("model is required".to_owned());
        }
        if self.messages.is_empty() {
            return Err("messages must not be empty".to_owned());
        }

        let messages = self
            .messages
            .into_iter()
            .map(|message| NormalizedMessage {
                role: message.role,
                content: message.content,
            })
            .collect();

        Ok(NormalizedChatRequest {
            request_id: format!("req_{}", Uuid::new_v4()),
            user_id,
            model: self.model,
            messages,
            generation: GenerationParams {
                max_tokens: self.max_tokens,
                temperature: self.temperature,
                top_p: self.top_p,
            },
            stream: self.stream,
        })
    }
}

#[derive(Debug, Clone)]
pub struct BackendChatResponse {
    pub content: String,
    pub finish_reason: String,
    pub usage: Usage,
}

#[derive(Debug, Clone)]
pub struct BackendChunk {
    pub delta: Option<String>,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
    pub done: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

impl Usage {
    pub fn new(prompt_tokens: u32, completion_tokens: u32) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionsResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Usage,
}

#[derive(Debug, Serialize)]
pub struct ChatChoice {
    pub index: usize,
    pub message: AssistantMessage,
    pub finish_reason: String,
}

#[derive(Debug, Serialize)]
pub struct AssistantMessage {
    pub role: &'static str,
    pub content: String,
}

impl ChatCompletionsResponse {
    pub fn from_backend(
        id: String,
        created: i64,
        model: String,
        backend: BackendChatResponse,
    ) -> Self {
        Self {
            id,
            object: "chat.completion".to_owned(),
            created,
            model,
            choices: vec![ChatChoice {
                index: 0,
                message: AssistantMessage {
                    role: "assistant",
                    content: backend.content,
                },
                finish_reason: backend.finish_reason,
            }],
            usage: backend.usage,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ChatCompletionsChunk {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChunkChoice>,
}

#[derive(Debug, Serialize)]
pub struct ChunkChoice {
    pub index: usize,
    pub delta: DeltaMessage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DeltaMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

impl ChatCompletionsChunk {
    pub fn role(id: &str, created: i64, model: &str) -> Self {
        Self {
            id: id.to_owned(),
            object: "chat.completion.chunk".to_owned(),
            created,
            model: model.to_owned(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: DeltaMessage {
                    role: Some("assistant"),
                    content: None,
                },
                finish_reason: None,
            }],
        }
    }

    pub fn delta(id: &str, created: i64, model: &str, content: String) -> Self {
        Self {
            id: id.to_owned(),
            object: "chat.completion.chunk".to_owned(),
            created,
            model: model.to_owned(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: DeltaMessage {
                    role: None,
                    content: Some(content),
                },
                finish_reason: None,
            }],
        }
    }

    pub fn finish(id: &str, created: i64, model: &str, finish_reason: String) -> Self {
        Self {
            id: id.to_owned(),
            object: "chat.completion.chunk".to_owned(),
            created,
            model: model.to_owned(),
            choices: vec![ChunkChoice {
                index: 0,
                delta: DeltaMessage {
                    role: None,
                    content: None,
                },
                finish_reason: Some(finish_reason),
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_rejects_empty_message_list() {
        let request = ChatCompletionsRequest {
            model: "gpt-test".to_owned(),
            messages: vec![],
            max_tokens: None,
            temperature: None,
            top_p: None,
            stream: false,
            user: None,
        };

        let error = request
            .into_normalized("user_123".to_owned())
            .expect_err("empty message list should fail");

        assert_eq!(error, "messages must not be empty");
    }

    #[test]
    fn usage_total_is_computed() {
        let usage = Usage::new(11, 7);
        assert_eq!(usage.total_tokens, 18);
    }
}
