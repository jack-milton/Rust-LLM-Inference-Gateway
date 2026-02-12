# Changelog

All notable changes to this project will be documented in this file.

## [1.0.0] - 2026-02-12

### Added
- OpenAI-compatible `POST /v1/chat/completions` endpoint with streaming and non-streaming behavior.
- OpenAI-style SSE translation (`data: ...` + `[DONE]`).
- API-key authentication and per-key request/token quotas.
- In-flight request coalescing:
  - non-stream leader/follower dedupe
  - stream leader fanout with replay for followers
- Dynamic micro-batching scheduler for non-stream requests.
- Backend routing with health checks and circuit opening on repeated failures.
- OpenAI upstream adapter (`OPENAI_API_KEY` gated) plus mock fallback backends.
- Redis-backed (or in-memory fallback) quota tracking.
- Redis-backed (or in-memory fallback) one-shot response cache with `x-cache` headers.
- Prometheus metrics endpoint (`GET /metrics`).
- Integration test coverage for auth and cache behavior.
- CI workflow (`fmt`, `clippy -D warnings`, `test`).
- Containerization:
  - production Dockerfile
  - `docker-compose` stack with Redis, Prometheus, and Grafana

### Changed
- Refactored into library + binary structure for testability (`src/lib.rs` + `src/main.rs`).
- Repository now tracks `Cargo.lock` for reproducible builds.
