# v1.0.0 Release Notes

## Summary
`v1.0.0` establishes a production-shaped Rust LLM inference gateway with an OpenAI-compatible API surface, streaming support, scheduling primitives, quotas, caching, routing, and observability.

## Highlights
- OpenAI-compatible chat completions endpoint (`/v1/chat/completions`) with stream and non-stream support.
- Streaming normalization to OpenAI-style SSE events.
- API-key auth and per-key rate limits/quotas.
- In-flight coalescing for identical requests (non-stream and stream fanout).
- Dynamic micro-batching scheduler for non-stream requests.
- Backend router with health checks and circuit-open behavior.
- OpenAI upstream adapter enabled via `OPENAI_API_KEY` and mock fallback backends.
- Redis-backed or in-memory fallback:
  - request/token quota state
  - one-shot response cache (`x-cache: hit|miss`)
- Prometheus metrics endpoint (`/metrics`).
- CI pipeline with strict linting and tests.
- Containerized local stack (`docker-compose`) including Redis, Prometheus, and Grafana.

## Validation
- `cargo fmt --check`
- `cargo clippy --all-targets --all-features -- -D warnings`
- `cargo test`
- Live smoke validation of cache and metrics with Redis enabled.

## Known next steps
- OTLP exporter wiring for distributed tracing backends (Jaeger/Tempo).
- Additional adapters (vLLM/TGI) and true provider-side batched inference calls.
- Load benchmark suite and dashboard snapshots for release artifacts.
