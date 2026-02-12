use std::sync::Arc;

use crate::{
    auth::ApiKeyRegistry,
    backend::InferenceBackend,
    batcher::{BatchConfig, Batcher},
    cache::{CacheConfig, ResponseCache},
    coalescing::InflightCoalescer,
    limits::RateLimiter,
    metrics::AppMetrics,
};

#[derive(Clone)]
pub struct AppState {
    pub backend: Arc<dyn InferenceBackend>,
    pub batcher: Arc<Batcher>,
    pub auth: Arc<ApiKeyRegistry>,
    pub rate_limiter: Arc<RateLimiter>,
    pub response_cache: Arc<ResponseCache>,
    pub coalescer: Arc<InflightCoalescer>,
    pub metrics: Arc<AppMetrics>,
}

impl AppState {
    pub fn new<B>(backend: Arc<B>) -> Self
    where
        B: InferenceBackend + 'static,
    {
        let backend: Arc<dyn InferenceBackend> = backend;
        let batcher = Arc::new(Batcher::new(backend.clone(), BatchConfig::from_env()));
        Self {
            backend,
            batcher,
            auth: Arc::new(ApiKeyRegistry::from_env()),
            rate_limiter: Arc::new(RateLimiter::from_env()),
            response_cache: Arc::new(ResponseCache::from_env(CacheConfig::from_env())),
            coalescer: Arc::new(InflightCoalescer::default()),
            metrics: Arc::new(AppMetrics::new()),
        }
    }

    pub fn new_for_tests<B>(backend: Arc<B>) -> Self
    where
        B: InferenceBackend + 'static,
    {
        let backend: Arc<dyn InferenceBackend> = backend;
        let batcher = Arc::new(Batcher::new(backend.clone(), BatchConfig::from_env()));
        Self {
            backend,
            batcher,
            auth: Arc::new(ApiKeyRegistry::from_env()),
            rate_limiter: Arc::new(RateLimiter::in_memory()),
            response_cache: Arc::new(ResponseCache::memory(CacheConfig::from_env())),
            coalescer: Arc::new(InflightCoalescer::default()),
            metrics: Arc::new(AppMetrics::new()),
        }
    }
}
