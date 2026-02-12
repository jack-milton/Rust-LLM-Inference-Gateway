use std::{
    collections::HashMap,
    env,
    time::{Duration, Instant},
};

use redis::AsyncCommands;
use tokio::sync::Mutex;
use tracing::warn;

use crate::models::BackendChatResponse;

#[derive(Debug, Clone, Copy)]
pub struct CacheConfig {
    pub ttl: Duration,
}

impl CacheConfig {
    pub fn from_env() -> Self {
        let ttl_secs = env::var("GATEWAY_CACHE_TTL_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(90);
        Self {
            ttl: Duration::from_secs(ttl_secs),
        }
    }
}

pub struct ResponseCache {
    backend: CacheBackend,
    config: CacheConfig,
}

enum CacheBackend {
    Memory(Mutex<HashMap<String, MemoryCacheItem>>),
    Redis {
        client: redis::Client,
        prefix: String,
    },
}

struct MemoryCacheItem {
    value: BackendChatResponse,
    expires_at: Instant,
}

impl ResponseCache {
    pub fn memory(config: CacheConfig) -> Self {
        Self {
            backend: CacheBackend::Memory(Mutex::new(HashMap::new())),
            config,
        }
    }

    pub fn from_env(config: CacheConfig) -> Self {
        let backend = match env::var("REDIS_URL") {
            Ok(url) if !url.trim().is_empty() => match redis::Client::open(url.clone()) {
                Ok(client) => {
                    let prefix =
                        env::var("GATEWAY_REDIS_PREFIX").unwrap_or_else(|_| "gateway".to_owned());
                    CacheBackend::Redis { client, prefix }
                }
                Err(error) => {
                    warn!(error = %error, "invalid REDIS_URL, falling back to in-memory cache");
                    CacheBackend::Memory(Mutex::new(HashMap::new()))
                }
            },
            _ => CacheBackend::Memory(Mutex::new(HashMap::new())),
        };

        Self { backend, config }
    }

    pub async fn get(&self, key: &str) -> Option<BackendChatResponse> {
        match &self.backend {
            CacheBackend::Memory(store) => {
                let mut guard = store.lock().await;
                let item = guard.get(key)?;
                if item.expires_at <= Instant::now() {
                    guard.remove(key);
                    return None;
                }
                Some(item.value.clone())
            }
            CacheBackend::Redis { client, prefix } => {
                let mut connection = match client.get_multiplexed_async_connection().await {
                    Ok(connection) => connection,
                    Err(error) => {
                        warn!(error = %error, "failed to get redis connection for cache get");
                        return None;
                    }
                };
                let redis_key = format!("{prefix}:cache:chat:{key}");
                let payload = match connection.get::<_, Option<String>>(&redis_key).await {
                    Ok(payload) => payload?,
                    Err(error) => {
                        warn!(error = %error, "redis get failed for cache");
                        return None;
                    }
                };
                match serde_json::from_str::<BackendChatResponse>(&payload) {
                    Ok(value) => Some(value),
                    Err(error) => {
                        warn!(error = %error, "failed to decode cached backend response");
                        None
                    }
                }
            }
        }
    }

    pub async fn set(&self, key: &str, value: &BackendChatResponse) {
        match &self.backend {
            CacheBackend::Memory(store) => {
                let mut guard = store.lock().await;
                guard.insert(
                    key.to_owned(),
                    MemoryCacheItem {
                        value: value.clone(),
                        expires_at: Instant::now() + self.config.ttl,
                    },
                );
            }
            CacheBackend::Redis { client, prefix } => {
                let mut connection = match client.get_multiplexed_async_connection().await {
                    Ok(connection) => connection,
                    Err(error) => {
                        warn!(error = %error, "failed to get redis connection for cache set");
                        return;
                    }
                };

                let payload = match serde_json::to_string(value) {
                    Ok(payload) => payload,
                    Err(error) => {
                        warn!(error = %error, "failed to serialize cached backend response");
                        return;
                    }
                };

                let redis_key = format!("{prefix}:cache:chat:{key}");
                if let Err(error) = connection
                    .set_ex::<_, _, ()>(&redis_key, payload, self.config.ttl.as_secs())
                    .await
                {
                    warn!(error = %error, "redis set failed for cache");
                }
            }
        }
    }
}
