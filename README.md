# Rust LLM Inference Gateway

OpenAI-compatible reverse proxy scaffold for LLM inference with a scheduler-friendly internal model.

Current foundation includes:
- `POST /v1/chat/completions` (streaming + non-streaming)
- OpenAI-style SSE event formatting (`data: ...` + terminal `data: [DONE]`)
- Request normalization into internal structs
- Adapter abstraction with `OpenAiAdapter` and `MockBackend`
- Backend router (round-robin selection + health probing + simple circuit breaker)
- API key authentication (`x-api-key`)
- Redis-backed (or in-memory fallback) per-key request/token rate limiting with `x-ratelimit-*` headers
- Redis-backed (or in-memory fallback) one-shot response cache with `x-cache: hit|miss`
- Prometheus metrics endpoint at `GET /metrics`
- In-flight request coalescing:
  - one-shot dedupe for identical non-stream requests
  - streaming fanout for identical stream requests (leader + followers)
- Dynamic micro-batching for non-stream requests:
  - batch class by model + decoding params
  - flush on max batch size or max wait window

## Internal data models

Ingress payload (`OpenAI` shape):
- `ChatCompletionsRequest { model, messages, max_tokens, temperature, top_p, stream, user }`

Normalized internal request (scheduler-facing):
- `NormalizedChatRequest`
- `request_id`
- `user_id`
- `model`
- `messages: Vec<NormalizedMessage>`
- `generation: GenerationParams { max_tokens, temperature, top_p }`
- `stream`

Backend adapter contract:
- `execute_chat(req) -> BackendChatResponse`
- `stream_chat(req) -> Stream<Item = BackendChunk>`

This keeps provider-specific logic in adapters while the scheduler/router layer works on one canonical request type.

## Request flow

1. Client calls `POST /v1/chat/completions`.
2. API key is authenticated against configured keys.
3. Request is validated + normalized into `NormalizedChatRequest`.
4. Estimated token budget is charged against per-key quotas (Redis if configured).
5. Request fingerprint is computed (SHA-256 over model/messages/decoding params).
6. For `stream=false`: response cache is checked first, then in-flight coalescer dedupes concurrent misses.
7. Backend router selects a healthy backend endpoint for execution.
8. For `stream=true`: coalescer elects a leader stream and followers receive replay + live fanout chunks; output is mapped to OpenAI SSE with `[DONE]`.

## Running

```bash
cargo run
```

Server listens on `0.0.0.0:8080`.

### Non-stream request

```bash
curl -s http://localhost:8080/v1/chat/completions \
  -H 'content-type: application/json' \
  -H 'x-api-key: dev-key' \
  -d '{
    "model":"mock-1",
    "messages":[{"role":"user","content":"Write one sentence about Rust."}],
    "stream":false
  }'
```

### Streaming request

```bash
curl -N http://localhost:8080/v1/chat/completions \
  -H 'content-type: application/json' \
  -H 'x-api-key: dev-key' \
  -d '{
    "model":"mock-1",
    "messages":[{"role":"user","content":"Say hello in five words."}],
    "stream":true
  }'
```

## Project layout

- `src/main.rs`: app bootstrap + routes
- `src/handlers.rs`: HTTP handlers + SSE mapping
- `src/models.rs`: OpenAI and internal canonical models
- `src/auth.rs`: API key auth and default policy config
- `src/cache.rs`: response cache with Redis and in-memory backends
- `src/limits.rs`: per-key request/token quota accounting and headers
- `src/metrics.rs`: Prometheus metrics registry and exporters
- `src/batcher.rs`: dynamic micro-batching scheduler for one-shot requests
- `src/coalescing.rs`: one-shot dedupe and streaming fanout coalescing
- `src/router.rs`: backend routing, health checks, and circuit breaker logic
- `src/backend/mod.rs`: adapter trait and errors
- `src/backend/openai.rs`: OpenAI backend adapter (stream + non-stream)
- `src/backend/mock.rs`: mock backend implementation
- `src/scheduler.rs`: request fingerprinting primitive (coalescing key base)
- `src/errors.rs`: OpenAI-style error envelope

## Configuration

- `GATEWAY_API_KEYS`: comma-separated keys (default: `dev-key`)
- `GATEWAY_LIMIT_REQUESTS_PER_MINUTE`: per-key request budget (default: `120`)
- `GATEWAY_LIMIT_TOKENS_PER_MINUTE`: per-key token budget (default: `120000`)
- `GATEWAY_LIMIT_TOKENS_PER_DAY`: per-key daily token budget (default: `2000000`)
- `GATEWAY_CACHE_TTL_SECS`: one-shot response cache TTL (default: `90`)
- `GATEWAY_BATCH_ENABLED`: enable/disable micro-batching (default: `true`)
- `GATEWAY_BATCH_MAX_SIZE`: flush size for one-shot micro-batches (default: `8`)
- `GATEWAY_BATCH_MAX_WAIT_MS`: max wait before flush (default: `10`)
- `REDIS_URL`: enable Redis-backed quotas/cache (optional)
- `GATEWAY_REDIS_PREFIX`: Redis key namespace prefix (default: `gateway`)
- `OPENAI_API_KEY`: enable OpenAI adapter (optional)
- `OPENAI_BASE_URL`: OpenAI-compatible base URL (default: `https://api.openai.com/v1`)
- `OPENAI_TIMEOUT_SECS`: OpenAI request timeout seconds (default: `60`)

## Next implementation slices

1. Add OpenTelemetry tracing spans across auth, quota, coalescing, batch, and router stages.
2. Add stricter stream-failure token reconciliation with Redis-side atomic adjustments.
3. Add vLLM/TGI adapters and true provider-side batched inference calls.
4. Add load test harness + benchmark dashboards for p50/p95/p99 and throughput curves.
