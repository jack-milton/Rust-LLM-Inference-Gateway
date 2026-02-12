FROM rust:1.93-bookworm AS builder
WORKDIR /app

COPY Cargo.toml ./
COPY src ./src
COPY tests ./tests

RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/rust-llm-inference-gateway /usr/local/bin/gateway

EXPOSE 8080
CMD ["gateway"]
