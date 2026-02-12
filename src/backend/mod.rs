pub mod mock;

use async_trait::async_trait;
use futures_util::stream::BoxStream;
use thiserror::Error;

use crate::models::{BackendChatResponse, BackendChunk, NormalizedChatRequest};

pub type BackendStream = BoxStream<'static, Result<BackendChunk, BackendError>>;

#[async_trait]
pub trait InferenceBackend: Send + Sync {
    fn name(&self) -> &str;
    async fn execute_chat(
        &self,
        request: NormalizedChatRequest,
    ) -> Result<BackendChatResponse, BackendError>;
    async fn stream_chat(&self, request: NormalizedChatRequest) -> Result<BackendStream, BackendError>;
}

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("backend unavailable: {0}")]
    Unavailable(String),
    #[error("backend timeout: {0}")]
    Timeout(String),
    #[error("backend invalid response: {0}")]
    InvalidResponse(String),
}
