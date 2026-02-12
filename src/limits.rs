use std::{
    collections::HashMap,
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::sync::Mutex;

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

#[derive(Debug, Default)]
pub struct RateLimiter {
    usage: Mutex<HashMap<String, KeyUsage>>,
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

impl RateLimiter {
    pub async fn check_and_consume(
        &self,
        api_key: &str,
        policy: &RatePolicy,
        estimated_tokens: u64,
    ) -> Result<RateLimitSnapshot, RateLimitError> {
        let now = unix_timestamp();
        let mut usage_map = self.usage.lock().await;
        let usage = usage_map
            .entry(api_key.to_owned())
            .or_insert_with(|| KeyUsage::new(now));

        refresh_windows(now, usage);

        if usage.requests_in_minute.saturating_add(1) > policy.requests_per_minute {
            return Err(RateLimitError::RequestsPerMinute(snapshot(policy, usage, now)));
        }

        if usage.tokens_in_minute.saturating_add(estimated_tokens) > policy.tokens_per_minute {
            return Err(RateLimitError::TokensPerMinute(snapshot(policy, usage, now)));
        }

        if usage.tokens_in_day.saturating_add(estimated_tokens) > policy.tokens_per_day {
            return Err(RateLimitError::TokensPerDay(snapshot(policy, usage, now)));
        }

        usage.requests_in_minute = usage.requests_in_minute.saturating_add(1);
        usage.tokens_in_minute = usage.tokens_in_minute.saturating_add(estimated_tokens);
        usage.tokens_in_day = usage.tokens_in_day.saturating_add(estimated_tokens);

        Ok(snapshot(policy, usage, now))
    }

    pub async fn reconcile_tokens(&self, api_key: &str, estimated: u64, actual: u64) {
        if estimated == actual {
            return;
        }

        let now = unix_timestamp();
        let mut usage_map = self.usage.lock().await;
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
    RateLimitSnapshot {
        limit_requests_per_minute: policy.requests_per_minute,
        remaining_requests_per_minute: policy
            .requests_per_minute
            .saturating_sub(usage.requests_in_minute),
        limit_tokens_per_minute: policy.tokens_per_minute,
        remaining_tokens_per_minute: policy
            .tokens_per_minute
            .saturating_sub(usage.tokens_in_minute),
        limit_tokens_per_day: policy.tokens_per_day,
        remaining_tokens_per_day: policy.tokens_per_day.saturating_sub(usage.tokens_in_day),
        reset_requests_per_minute: current_minute_start(now).saturating_add(60),
        reset_tokens_per_day: current_day_start(now).saturating_add(86_400),
    }
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
        let limiter = RateLimiter::default();
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
