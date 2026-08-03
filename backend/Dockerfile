# Multi-stage build (ADR-014): cargo-chef caches the dependency layer.
# 1.94 is the floor imposed by sqlx 0.9 (ADR-064); GitLab CI uses `rust:1-slim`
# and GitHub Actions `rust-toolchain@stable`, both already above it.
#
# The `-bookworm` suffix is load-bearing and must stay in step with the runtime
# stage below: plain `rust:1.97-slim` is Debian 13 (trixie, glibc 2.41), and a
# binary it produces dies on `debian:bookworm-slim` (glibc 2.36) with
# `version GLIBC_2.38 not found`. The image builds fine either way — only
# actually starting the container catches it.
FROM rust:1.97-slim-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /build

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release --bins

FROM debian:bookworm-slim AS runtime
# reqwest 0.13 verifies TLS through rustls-platform-verifier, which reads the
# system trust store instead of roots baked into the binary (ADR-064). Without
# this package every outbound HTTPS call (agent dispatch, webhooks, Slack,
# Telegram) fails to verify.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 app
COPY --from=builder /build/target/release/backend /app/backend
COPY --from=builder /build/target/release/healthcheck /app/healthcheck
USER app
EXPOSE 8000
ENTRYPOINT ["/app/backend"]
