# Multi-stage build (ADR-014): cargo-chef caches the dependency layer.
FROM rust:1.89-slim AS chef
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
RUN useradd --system --uid 10001 app
COPY --from=builder /build/target/release/backend /app/backend
COPY --from=builder /build/target/release/healthcheck /app/healthcheck
USER app
EXPOSE 8000
ENTRYPOINT ["/app/backend"]
