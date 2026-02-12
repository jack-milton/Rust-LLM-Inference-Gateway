use std::sync::Arc;

use crate::{
    auth::ApiKeyRegistry, backend::InferenceBackend, coalescing::InflightCoalescer,
    limits::RateLimiter,
};

#[derive(Clone)]
pub struct AppState {
    pub backend: Arc<dyn InferenceBackend>,
    pub auth: Arc<ApiKeyRegistry>,
    pub rate_limiter: Arc<RateLimiter>,
    pub coalescer: Arc<InflightCoalescer>,
}

impl AppState {
    pub fn new<B>(backend: Arc<B>) -> Self
    where
        B: InferenceBackend + 'static,
    {
        let backend: Arc<dyn InferenceBackend> = backend;
        Self {
            backend,
            auth: Arc::new(ApiKeyRegistry::from_env()),
            rate_limiter: Arc::new(RateLimiter::default()),
            coalescer: Arc::new(InflightCoalescer::default()),
        }
    }
}
