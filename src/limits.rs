use std::{
    collections::HashMap,
    env,
    time::{SystemTime, UNIX_EPOCH},
};

use redis::AsyncCommands;
use tokio::sync::Mutex;
use tracing::warn;

use crate::{auth::RatePolicy, models::NormalizedChatRequest};

#[derive(Debug, Clone)]
pub struct RateLimitSnapshot {
    pub limit_requests_per_minute: u32,
    pub remaining_requests_per_minute: u32,
    pub limit_tokens_per_minute: u64,
    pub remaining_tokens_per_minute: u64,
    pub limit_tokens_per_day: u64,
    pub remaining_tokens_per_day: u64,
    pub reset_requests_per_minute: u64,
    pub reset_tokens_per_day: u64,
}

impl RateLimitSnapshot {
    pub fn to_header_pairs(&self) -> Vec<(String, String)> {
        vec![
            (
                "x-ratelimit-limit-requests-minute".to_owned(),
                self.limit_requests_per_minute.to_string(),
            ),
            (
                "x-ratelimit-remaining-requests-minute".to_owned(),
                self.remaining_requests_per_minute.to_string(),
            ),
            (
                "x-ratelimit-limit-tokens-minute".to_owned(),
                self.limit_tokens_per_minute.to_string(),
            ),
            (
                "x-ratelimit-remaining-tokens-minute".to_owned(),
                self.remaining_tokens_per_minute.to_string(),
            ),
            (
                "x-ratelimit-limit-tokens-day".to_owned(),
                self.limit_tokens_per_day.to_string(),
            ),
            (
                "x-ratelimit-remaining-tokens-day".to_owned(),
                self.remaining_tokens_per_day.to_string(),
            ),
            (
                "x-ratelimit-reset-requests-minute".to_owned(),
                self.reset_requests_per_minute.to_string(),
            ),
            (
                "x-ratelimit-reset-tokens-day".to_owned(),
                self.reset_tokens_per_day.to_string(),
            ),
        ]
    }
}

#[derive(Debug)]
pub enum RateLimitError {
    RequestsPerMinute(RateLimitSnapshot),
    TokensPerMinute(RateLimitSnapshot),
    TokensPerDay(RateLimitSnapshot),
}

impl RateLimitError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::RequestsPerMinute(_) => "requests per minute quota exceeded",
            Self::TokensPerMinute(_) => "tokens per minute quota exceeded",
            Self::TokensPerDay(_) => "tokens per day quota exceeded",
        }
    }

    pub fn snapshot(&self) -> &RateLimitSnapshot {
        match self {
            Self::RequestsPerMinute(snapshot) => snapshot,
            Self::TokensPerMinute(snapshot) => snapshot,
            Self::TokensPerDay(snapshot) => snapshot,
        }
    }
}

pub struct RateLimiter {
    backend: RateLimiterBackend,
}

enum RateLimiterBackend {
    Memory(Mutex<HashMap<String, KeyUsage>>),
    Redis {
        client: redis::Client,
        prefix: String,
    },
}

#[derive(Debug, Clone)]
struct KeyUsage {
    minute_started_at: u64,
    day_started_at: u64,
    requests_in_minute: u32,
    tokens_in_minute: u64,
    tokens_in_day: u64,
}

impl KeyUsage {
    fn new(now: u64) -> Self {
        Self {
            minute_started_at: current_minute_start(now),
            day_started_at: current_day_start(now),
            requests_in_minute: 0,
            tokens_in_minute: 0,
            tokens_in_day: 0,
        }
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::from_env()
    }
}

impl RateLimiter {
    pub fn from_env() -> Self {
        match env::var("REDIS_URL") {
            Ok(url) if !url.trim().is_empty() => match redis::Client::open(url.clone()) {
                Ok(client) => {
                    let prefix =
                        env::var("GATEWAY_REDIS_PREFIX").unwrap_or_else(|_| "gateway".to_owned());
                    Self {
                        backend: RateLimiterBackend::Redis { client, prefix },
                    }
                }
                Err(error) => {
                    warn!(error = %error, "invalid REDIS_URL, falling back to in-memory limiter");
                    Self {
                        backend: RateLimiterBackend::Memory(Mutex::new(HashMap::new())),
                    }
                }
            },
            _ => Self {
                backend: RateLimiterBackend::Memory(Mutex::new(HashMap::new())),
            },
        }
    }

    #[cfg(test)]
    fn memory_for_tests() -> Self {
        Self {
            backend: RateLimiterBackend::Memory(Mutex::new(HashMap::new())),
        }
    }

    pub async fn check_and_consume(
        &self,
        api_key: &str,
        policy: &RatePolicy,
        estimated_tokens: u64,
    ) -> Result<RateLimitSnapshot, RateLimitError> {
        match &self.backend {
            RateLimiterBackend::Memory(usage_map) => {
                check_and_consume_memory(usage_map, api_key, policy, estimated_tokens).await
            }
            RateLimiterBackend::Redis { client, prefix } => {
                check_and_consume_redis(client, prefix, api_key, policy, estimated_tokens).await
            }
        }
    }

    pub async fn reconcile_tokens(&self, api_key: &str, estimated: u64, actual: u64) {
        if estimated == actual {
            return;
        }

        match &self.backend {
            RateLimiterBackend::Memory(usage_map) => {
                reconcile_tokens_memory(usage_map, api_key, estimated, actual).await;
            }
            RateLimiterBackend::Redis { client, prefix } => {
                reconcile_tokens_redis(client, prefix, api_key, estimated, actual).await;
            }
        }
    }
}

pub fn estimate_request_tokens(request: &NormalizedChatRequest) -> u64 {
    let prompt_tokens = request
        .messages
        .iter()
        .map(|message| rough_token_estimate(&message.content))
        .sum::<u64>();

    let completion_estimate = request.generation.max_tokens.unwrap_or(256) as u64;
    prompt_tokens.saturating_add(completion_estimate)
}

async fn check_and_consume_memory(
    usage_map: &Mutex<HashMap<String, KeyUsage>>,
    api_key: &str,
    policy: &RatePolicy,
    estimated_tokens: u64,
) -> Result<RateLimitSnapshot, RateLimitError> {
    let now = unix_timestamp();
    let mut usage_map = usage_map.lock().await;
    let usage = usage_map
        .entry(api_key.to_owned())
        .or_insert_with(|| KeyUsage::new(now));

    refresh_windows(now, usage);

    if usage.requests_in_minute.saturating_add(1) > policy.requests_per_minute {
        return Err(RateLimitError::RequestsPerMinute(snapshot(
            policy, usage, now,
        )));
    }

    if usage.tokens_in_minute.saturating_add(estimated_tokens) > policy.tokens_per_minute {
        return Err(RateLimitError::TokensPerMinute(snapshot(
            policy, usage, now,
        )));
    }

    if usage.tokens_in_day.saturating_add(estimated_tokens) > policy.tokens_per_day {
        return Err(RateLimitError::TokensPerDay(snapshot(policy, usage, now)));
    }

    usage.requests_in_minute = usage.requests_in_minute.saturating_add(1);
    usage.tokens_in_minute = usage.tokens_in_minute.saturating_add(estimated_tokens);
    usage.tokens_in_day = usage.tokens_in_day.saturating_add(estimated_tokens);

    Ok(snapshot(policy, usage, now))
}

async fn reconcile_tokens_memory(
    usage_map: &Mutex<HashMap<String, KeyUsage>>,
    api_key: &str,
    estimated: u64,
    actual: u64,
) {
    let now = unix_timestamp();
    let mut usage_map = usage_map.lock().await;
    let Some(usage) = usage_map.get_mut(api_key) else {
        return;
    };

    refresh_windows(now, usage);

    if actual > estimated {
        let diff = actual - estimated;
        usage.tokens_in_minute = usage.tokens_in_minute.saturating_add(diff);
        usage.tokens_in_day = usage.tokens_in_day.saturating_add(diff);
    } else {
        let diff = estimated - actual;
        usage.tokens_in_minute = usage.tokens_in_minute.saturating_sub(diff);
        usage.tokens_in_day = usage.tokens_in_day.saturating_sub(diff);
    }
}

async fn check_and_consume_redis(
    client: &redis::Client,
    prefix: &str,
    api_key: &str,
    policy: &RatePolicy,
    estimated_tokens: u64,
) -> Result<RateLimitSnapshot, RateLimitError> {
    let now = unix_timestamp();
    let minute_start = current_minute_start(now);
    let day_start = current_day_start(now);
    let minute_reset = minute_start.saturating_add(60);
    let day_reset = day_start.saturating_add(86_400);

    let req_key = format!("{prefix}:rl:{api_key}:m:{minute_start}:req");
    let tok_min_key = format!("{prefix}:rl:{api_key}:m:{minute_start}:tok");
    let tok_day_key = format!("{prefix}:rl:{api_key}:d:{day_start}:tok");

    let req_ttl = minute_reset.saturating_sub(now).max(1);
    let day_ttl = day_reset.saturating_sub(now).max(1);

    let mut connection = match client.get_multiplexed_async_connection().await {
        Ok(connection) => connection,
        Err(error) => {
            warn!(error = %error, "redis unavailable for rate limit check");
            return Ok(empty_snapshot(policy, now));
        }
    };

    let script = redis::Script::new(
        r#"
local req_key = KEYS[1]
local tok_min_key = KEYS[2]
local tok_day_key = KEYS[3]
local req_inc = tonumber(ARGV[1])
local tok_inc = tonumber(ARGV[2])
local req_limit = tonumber(ARGV[3])
local tok_min_limit = tonumber(ARGV[4])
local tok_day_limit = tonumber(ARGV[5])
local req_ttl = tonumber(ARGV[6])
local tok_min_ttl = tonumber(ARGV[7])
local tok_day_ttl = tonumber(ARGV[8])

local req = redis.call('INCRBY', req_key, req_inc)
if req == req_inc then redis.call('EXPIRE', req_key, req_ttl) end
local tok_min = redis.call('INCRBY', tok_min_key, tok_inc)
if tok_min == tok_inc then redis.call('EXPIRE', tok_min_key, tok_min_ttl) end
local tok_day = redis.call('INCRBY', tok_day_key, tok_inc)
if tok_day == tok_inc then redis.call('EXPIRE', tok_day_key, tok_day_ttl) end

if req > req_limit or tok_min > tok_min_limit or tok_day > tok_day_limit then
  redis.call('DECRBY', req_key, req_inc)
  redis.call('DECRBY', tok_min_key, tok_inc)
  redis.call('DECRBY', tok_day_key, tok_inc)
  return {0, req, tok_min, tok_day}
end

return {1, req, tok_min, tok_day}
"#,
    );

    let values = script
        .key(req_key)
        .key(tok_min_key)
        .key(tok_day_key)
        .arg(1i64)
        .arg(estimated_tokens as i64)
        .arg(policy.requests_per_minute as i64)
        .arg(policy.tokens_per_minute as i64)
        .arg(policy.tokens_per_day as i64)
        .arg(req_ttl as i64)
        .arg(req_ttl as i64)
        .arg(day_ttl as i64)
        .invoke_async::<Vec<i64>>(&mut connection)
        .await;

    let values = match values {
        Ok(values) if values.len() == 4 => values,
        Ok(values) => {
            warn!(
                count = values.len(),
                "unexpected redis limiter script result length"
            );
            return Ok(empty_snapshot(policy, now));
        }
        Err(error) => {
            warn!(error = %error, "redis limiter script execution failed");
            return Ok(empty_snapshot(policy, now));
        }
    };

    let allowed = values[0] == 1;
    let req_count = values[1].max(0) as u64;
    let tok_min_count = values[2].max(0) as u64;
    let tok_day_count = values[3].max(0) as u64;
    let snapshot = snapshot_from_counts(policy, req_count, tok_min_count, tok_day_count, now);

    if allowed {
        Ok(snapshot)
    } else if req_count > policy.requests_per_minute as u64 {
        Err(RateLimitError::RequestsPerMinute(snapshot))
    } else if tok_min_count > policy.tokens_per_minute {
        Err(RateLimitError::TokensPerMinute(snapshot))
    } else {
        Err(RateLimitError::TokensPerDay(snapshot))
    }
}

async fn reconcile_tokens_redis(
    client: &redis::Client,
    prefix: &str,
    api_key: &str,
    estimated: u64,
    actual: u64,
) {
    let now = unix_timestamp();
    let minute_start = current_minute_start(now);
    let day_start = current_day_start(now);
    let minute_reset = minute_start.saturating_add(60);
    let day_reset = day_start.saturating_add(86_400);
    let req_ttl = minute_reset.saturating_sub(now).max(1);
    let day_ttl = day_reset.saturating_sub(now).max(1);

    let tok_min_key = format!("{prefix}:rl:{api_key}:m:{minute_start}:tok");
    let tok_day_key = format!("{prefix}:rl:{api_key}:d:{day_start}:tok");
    let diff = actual as i64 - estimated as i64;
    if diff == 0 {
        return;
    }

    let mut connection = match client.get_multiplexed_async_connection().await {
        Ok(connection) => connection,
        Err(error) => {
            warn!(error = %error, "redis unavailable for token reconciliation");
            return;
        }
    };

    if diff > 0 {
        let _: redis::RedisResult<()> = connection.incr(&tok_min_key, diff).await;
        let _: redis::RedisResult<()> = connection.incr(&tok_day_key, diff).await;
    } else {
        let adjust = diff.abs();
        let _: redis::RedisResult<()> = connection.decr(&tok_min_key, adjust).await;
        let _: redis::RedisResult<()> = connection.decr(&tok_day_key, adjust).await;
    }
    let _: redis::RedisResult<bool> = connection.expire(&tok_min_key, req_ttl as i64).await;
    let _: redis::RedisResult<bool> = connection.expire(&tok_day_key, day_ttl as i64).await;
}

fn rough_token_estimate(text: &str) -> u64 {
    if text.trim().is_empty() {
        return 0;
    }
    text.split_whitespace().count() as u64
}

fn refresh_windows(now: u64, usage: &mut KeyUsage) {
    let minute_start = current_minute_start(now);
    if usage.minute_started_at != minute_start {
        usage.minute_started_at = minute_start;
        usage.requests_in_minute = 0;
        usage.tokens_in_minute = 0;
    }

    let day_start = current_day_start(now);
    if usage.day_started_at != day_start {
        usage.day_started_at = day_start;
        usage.tokens_in_day = 0;
    }
}

fn snapshot(policy: &RatePolicy, usage: &KeyUsage, now: u64) -> RateLimitSnapshot {
    snapshot_from_counts(
        policy,
        usage.requests_in_minute as u64,
        usage.tokens_in_minute,
        usage.tokens_in_day,
        now,
    )
}

fn snapshot_from_counts(
    policy: &RatePolicy,
    request_count: u64,
    tokens_minute_count: u64,
    tokens_day_count: u64,
    now: u64,
) -> RateLimitSnapshot {
    RateLimitSnapshot {
        limit_requests_per_minute: policy.requests_per_minute,
        remaining_requests_per_minute: policy
            .requests_per_minute
            .saturating_sub(request_count as u32),
        limit_tokens_per_minute: policy.tokens_per_minute,
        remaining_tokens_per_minute: policy.tokens_per_minute.saturating_sub(tokens_minute_count),
        limit_tokens_per_day: policy.tokens_per_day,
        remaining_tokens_per_day: policy.tokens_per_day.saturating_sub(tokens_day_count),
        reset_requests_per_minute: current_minute_start(now).saturating_add(60),
        reset_tokens_per_day: current_day_start(now).saturating_add(86_400),
    }
}

fn empty_snapshot(policy: &RatePolicy, now: u64) -> RateLimitSnapshot {
    snapshot_from_counts(policy, 0, 0, 0, now)
}

fn current_minute_start(now: u64) -> u64 {
    (now / 60) * 60
}

fn current_day_start(now: u64) -> u64 {
    (now / 86_400) * 86_400
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{GenerationParams, MessageRole, NormalizedChatRequest, NormalizedMessage};

    #[tokio::test]
    async fn limits_consume_and_reconcile() {
        let limiter = RateLimiter::memory_for_tests();
        let policy = RatePolicy {
            requests_per_minute: 10,
            tokens_per_minute: 1_000,
            tokens_per_day: 10_000,
        };

        limiter
            .check_and_consume("key-1", &policy, 100)
            .await
            .expect("initial consume should pass");

        limiter.reconcile_tokens("key-1", 100, 70).await;
        let snapshot = limiter
            .check_and_consume("key-1", &policy, 70)
            .await
            .expect("second consume should pass");

        assert!(snapshot.remaining_tokens_per_minute <= 860);
    }

    #[test]
    fn estimate_tokens_uses_prompt_and_max_tokens() {
        let request = NormalizedChatRequest {
            request_id: "req-1".to_owned(),
            user_id: "user-1".to_owned(),
            model: "mock".to_owned(),
            messages: vec![NormalizedMessage {
                role: MessageRole::User,
                content: "hello world".to_owned(),
            }],
            generation: GenerationParams {
                max_tokens: Some(20),
                temperature: None,
                top_p: None,
            },
            stream: false,
        };

        assert_eq!(estimate_request_tokens(&request), 22);
    }
}
