use std::{collections::HashSet, env};

use axum::http::HeaderMap;

use crate::errors::AppError;

#[derive(Debug, Clone)]
pub struct RatePolicy {
    pub requests_per_minute: u32,
    pub tokens_per_minute: u64,
    pub tokens_per_day: u64,
}

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub api_key: String,
    pub user_id: String,
    pub policy: RatePolicy,
}

#[derive(Debug, Clone)]
pub struct ApiKeyRegistry {
    valid_keys: HashSet<String>,
    policy: RatePolicy,
}

impl ApiKeyRegistry {
    pub fn from_env() -> Self {
        let keys = env::var("GATEWAY_API_KEYS").unwrap_or_else(|_| "dev-key".to_owned());
        let mut valid_keys = keys
            .split(',')
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(ToOwned::to_owned)
            .collect::<HashSet<_>>();
        if valid_keys.is_empty() {
            valid_keys.insert("dev-key".to_owned());
        }

        let policy = RatePolicy {
            requests_per_minute: read_u32("GATEWAY_LIMIT_REQUESTS_PER_MINUTE", 120),
            tokens_per_minute: read_u64("GATEWAY_LIMIT_TOKENS_PER_MINUTE", 120_000),
            tokens_per_day: read_u64("GATEWAY_LIMIT_TOKENS_PER_DAY", 2_000_000),
        };

        Self { valid_keys, policy }
    }

    pub fn authenticate(&self, headers: &HeaderMap) -> Result<AuthContext, AppError> {
        let api_key = headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| AppError::Unauthorized("missing x-api-key header".to_owned()))?;

        if !self.valid_keys.contains(api_key) {
            return Err(AppError::Unauthorized("invalid api key".to_owned()));
        }

        Ok(AuthContext {
            api_key: api_key.to_owned(),
            user_id: format!("key_{}", redact_key(api_key)),
            policy: self.policy.clone(),
        })
    }
}

fn read_u32(name: &str, default: u32) -> u32 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(default)
}

fn read_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn redact_key(key: &str) -> String {
    key.chars().take(8).collect()
}
