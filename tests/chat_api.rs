use std::env;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use rust_llm_inference_gateway::{backend::mock::MockBackend, build_app, state::AppState};
use tower::util::ServiceExt;

fn api_key_for_tests() -> String {
    env::var("GATEWAY_API_KEYS")
        .ok()
        .and_then(|value| {
            value
                .split(',')
                .map(str::trim)
                .find(|key| !key.is_empty())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "dev-key".to_owned())
}

#[tokio::test]
async fn returns_unauthorized_when_api_key_missing() {
    let state = AppState::new_for_tests(std::sync::Arc::new(MockBackend::default()));
    let app = build_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"model":"mock-1","messages":[{"role":"user","content":"hello"}]}"#,
                ))
                .expect("request build"),
        )
        .await
        .expect("request execution");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn returns_cache_hit_on_repeated_identical_non_stream_request() {
    let state = AppState::new_for_tests(std::sync::Arc::new(MockBackend::default()));
    let app = build_app(state);
    let api_key = api_key_for_tests();
    let body =
        r#"{"model":"mock-1","messages":[{"role":"user","content":"repeat me"}],"stream":false}"#;

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-api-key", &api_key)
                .body(Body::from(body))
                .expect("request build"),
        )
        .await
        .expect("first request execution");
    assert_eq!(first.status(), StatusCode::OK);
    assert_eq!(
        first
            .headers()
            .get("x-cache")
            .and_then(|value| value.to_str().ok()),
        Some("miss")
    );

    let second = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("content-type", "application/json")
                .header("x-api-key", &api_key)
                .body(Body::from(body))
                .expect("request build"),
        )
        .await
        .expect("second request execution");
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        second
            .headers()
            .get("x-cache")
            .and_then(|value| value.to_str().ok()),
        Some("hit")
    );

    let bytes = to_bytes(second.into_body(), 1024 * 1024)
        .await
        .expect("body should be readable");
    let body = String::from_utf8(bytes.to_vec()).expect("response body should be UTF-8");
    assert!(body.contains("\"chat.completion\""));
}
