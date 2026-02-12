use std::{
    convert::Infallible,
    sync::Arc,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::State,
    http::{header::CONTENT_TYPE, HeaderMap},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    Json,
};
use futures_util::StreamExt;
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    backend::InferenceBackend,
    coalescing::CoalesceOutcome,
    errors::AppError,
    limits::{estimate_request_tokens, RateLimitSnapshot},
    models::{
        ChatCompletionsChunk, ChatCompletionsRequest, ChatCompletionsResponse,
        NormalizedChatRequest,
    },
    scheduler,
    state::AppState,
};

pub async fn healthz() -> &'static str {
    "ok"
}

pub async fn metrics(State(state): State<AppState>) -> Response {
    match state.metrics.render() {
        Ok(body) => (
            [(CONTENT_TYPE, "text/plain; version=0.0.4; charset=utf-8")],
            body,
        )
            .into_response(),
        Err(error) => AppError::Internal(format!("metrics render failed: {error}")).into_response(),
    }
}

pub async fn chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ChatCompletionsRequest>,
) -> Response {
    let started = Instant::now();
    let stream = request.stream;
    let _inflight = state.metrics.inflight_guard();

    let response = match process_chat_completions(state.clone(), headers, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    };

    state.metrics.observe_request(
        "/v1/chat/completions",
        "POST",
        stream,
        response.status().as_u16(),
        started.elapsed(),
    );

    response
}

async fn process_chat_completions(
    state: AppState,
    headers: HeaderMap,
    request: ChatCompletionsRequest,
) -> Result<Response, AppError> {
    let client_user = request.user.clone();
    let auth_context = state.auth.authenticate(&headers)?;
    let user_id = auth_context.user_id.clone();
    let normalized = request
        .into_normalized(user_id)
        .map_err(AppError::BadRequest)?;
    let estimated_tokens = estimate_request_tokens(&normalized);
    let rate_snapshot = state
        .rate_limiter
        .check_and_consume(
            &auth_context.api_key,
            &auth_context.policy,
            estimated_tokens,
        )
        .await
        .map_err(|error| AppError::RateLimited {
            message: error.message().to_owned(),
            headers: error.snapshot().to_header_pairs(),
        })?;

    let fingerprint = scheduler::fingerprint_for(&normalized);
    info!(
        request_id = %normalized.request_id,
        user_id = %normalized.user_id,
        model = %normalized.model,
        stream = normalized.stream,
        estimated_tokens,
        client_user = %client_user.unwrap_or_default(),
        fingerprint = %fingerprint.as_str(),
        "chat request accepted"
    );

    if normalized.stream {
        stream_completion(
            state,
            normalized,
            auth_context.api_key,
            fingerprint.as_str().to_owned(),
            estimated_tokens,
            rate_snapshot,
        )
        .await
    } else {
        one_shot_completion(
            state,
            normalized,
            auth_context.api_key,
            fingerprint.as_str().to_owned(),
            estimated_tokens,
            rate_snapshot,
        )
        .await
    }
}

async fn one_shot_completion(
    state: AppState,
    request: NormalizedChatRequest,
    api_key: String,
    fingerprint: String,
    estimated_tokens: u64,
    rate_snapshot: RateLimitSnapshot,
) -> Result<Response, AppError> {
    let created = unix_timestamp();
    let response_id = format!("chatcmpl-{}", Uuid::new_v4());
    let cache_key = fingerprint.clone();

    if let Some(cached) = state.response_cache.get(&cache_key).await {
        state
            .rate_limiter
            .reconcile_tokens(&api_key, estimated_tokens, cached.usage.total_tokens as u64)
            .await;
        state.metrics.observe_usage(&cached.usage);

        let payload =
            ChatCompletionsResponse::from_backend(response_id, created, request.model, cached);
        let mut response = Json(payload).into_response();
        apply_rate_limit_headers(response.headers_mut(), &rate_snapshot);
        crate::errors::apply_header(response.headers_mut(), "x-cache", "hit");
        return Ok(response);
    }

    let execution_backend: Arc<dyn InferenceBackend> = state.batcher.clone();

    let (backend_response, coalesced) = state
        .coalescer
        .execute_or_join(fingerprint, execution_backend, request.clone())
        .await
        .map_err(|error| {
            state.metrics.observe_backend_error("one_shot");
            AppError::Backend(error.to_string())
        })?;
    state
        .rate_limiter
        .reconcile_tokens(
            &api_key,
            estimated_tokens,
            backend_response.usage.total_tokens as u64,
        )
        .await;
    state.metrics.observe_usage(&backend_response.usage);
    state
        .response_cache
        .set(&cache_key, &backend_response)
        .await;

    let payload = ChatCompletionsResponse::from_backend(
        response_id,
        created,
        request.model,
        backend_response,
    );
    let mut response = Json(payload).into_response();
    apply_rate_limit_headers(response.headers_mut(), &rate_snapshot);
    crate::errors::apply_header(response.headers_mut(), "x-cache", "miss");

    if coalesced == CoalesceOutcome::Joined {
        info!("one-shot response served from inflight coalescing");
    }

    Ok(response)
}

async fn stream_completion(
    state: AppState,
    request: NormalizedChatRequest,
    api_key: String,
    fingerprint: String,
    estimated_tokens: u64,
    rate_snapshot: RateLimitSnapshot,
) -> Result<Response, AppError> {
    let created = unix_timestamp();
    let response_id = format!("chatcmpl-{}", Uuid::new_v4());
    let model = request.model.clone();
    let stream_join = state
        .coalescer
        .join_or_create_stream(fingerprint.clone())
        .await;
    if stream_join.is_leader {
        let backend = state.backend.clone();
        let coalescer = state.coalescer.clone();
        let request_for_leader = request;
        let key = fingerprint.clone();
        let metrics = state.metrics.clone();
        tokio::spawn(async move {
            let backend_stream = match backend.stream_chat(request_for_leader).await {
                Ok(stream) => stream,
                Err(error) => {
                    metrics.observe_backend_error("stream_leader_start");
                    coalescer
                        .publish_stream_item(&key, Err(error.to_string()))
                        .await;
                    return;
                }
            };

            tokio::pin!(backend_stream);
            while let Some(next) = backend_stream.next().await {
                match next {
                    Ok(chunk) => {
                        let done = chunk.done;
                        coalescer.publish_stream_item(&key, Ok(chunk)).await;
                        if done {
                            break;
                        }
                    }
                    Err(error) => {
                        metrics.observe_backend_error("stream_leader_read");
                        coalescer
                            .publish_stream_item(&key, Err(error.to_string()))
                            .await;
                        break;
                    }
                }
            }
        });
    }

    let outbound = async_stream::stream! {
        let mut stream_rx = stream_join.receiver;
        let mut emitted_role = false;
        while let Some(next) = stream_rx.recv().await {
            match next {
                Ok(chunk) => {
                    if !emitted_role {
                        emitted_role = true;
                        let role_chunk = ChatCompletionsChunk::role(&response_id, created, &model);
                        yield Ok::<Event, Infallible>(json_event(role_chunk));
                    }

                    if let Some(delta) = chunk.delta {
                        let delta_chunk = ChatCompletionsChunk::delta(&response_id, created, &model, delta);
                        yield Ok::<Event, Infallible>(json_event(delta_chunk));
                    }

                    if chunk.done {
                        if let Some(usage) = chunk.usage {
                            state
                                .rate_limiter
                                .reconcile_tokens(
                                    &api_key,
                                    estimated_tokens,
                                    usage.total_tokens as u64,
                                )
                                .await;
                            state.metrics.observe_usage(&usage);
                            info!(
                                prompt_tokens = usage.prompt_tokens,
                                completion_tokens = usage.completion_tokens,
                                total_tokens = usage.total_tokens,
                                "stream usage summary"
                            );
                        }
                        let finish_reason = chunk.finish_reason.unwrap_or_else(|| "stop".to_owned());
                        let done_chunk = ChatCompletionsChunk::finish(&response_id, created, &model, finish_reason);
                        yield Ok::<Event, Infallible>(json_event(done_chunk));
                    }
                }
                Err(error) => {
                    state.metrics.observe_backend_error("stream_fanout");
                    warn!(error = %error, "backend stream error");
                    let error_json = serde_json::json!({
                        "error": {
                            "message": error,
                            "type": "backend_error"
                        }
                    });
                    yield Ok::<Event, Infallible>(Event::default().data(error_json.to_string()));
                    break;
                }
            }
        }

        yield Ok::<Event, Infallible>(Event::default().data("[DONE]"));
    };

    let mut response = Sse::new(outbound)
        .keep_alive(KeepAlive::new().interval(std::time::Duration::from_secs(10)))
        .into_response();
    apply_rate_limit_headers(response.headers_mut(), &rate_snapshot);
    Ok(response)
}

fn apply_rate_limit_headers(headers: &mut axum::http::HeaderMap, snapshot: &RateLimitSnapshot) {
    for (name, value) in snapshot.to_header_pairs() {
        crate::errors::apply_header(headers, &name, &value);
    }
}

fn json_event<T: serde::Serialize>(payload: T) -> Event {
    match serde_json::to_string(&payload) {
        Ok(serialized) => Event::default().data(serialized),
        Err(error) => {
            let fallback = serde_json::json!({
                "error": {
                    "message": format!("serialization error: {error}"),
                    "type": "server_error"
                }
            });
            Event::default().data(fallback.to_string())
        }
    }
}

fn unix_timestamp() -> i64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    duration.as_secs() as i64
}
