# Multi-stage build (ADR-014). One image for the FastAPI API and the Celery
# worker — only the container `command` differs (see docker-compose.yml).
FROM python:3.12-slim AS builder
COPY --from=ghcr.io/astral-sh/uv:latest /uv /usr/local/bin/uv
WORKDIR /app
ENV UV_PROJECT_ENVIRONMENT=/app/.venv
COPY pyproject.toml uv.lock ./
RUN uv sync --frozen --no-install-project --no-dev
COPY src ./src
RUN uv sync --frozen --no-dev

FROM python:3.12-slim AS runtime
# Apply Debian security updates at build time (ADR-074). The base image is
# rebuilt on its own cadence, which lags the archive: the weekly Trivy scan
# (ADR-015) flagged 9 HIGH findings — all CVE-2026-53615 in util-linux — while
# `python:3.12-slim` still shipped 2.41-5 and Debian had already published
# 2.41.5-0+deb13u1. Pulling a fresh base does not help; upgrading does.
# Only the runtime stage matters — builder packages never ship.
RUN apt-get update \
    && apt-get upgrade -y --no-install-recommends \
    && rm -rf /var/lib/apt/lists/*
RUN useradd --system --uid 10001 app
WORKDIR /app
COPY --from=builder /app/.venv /app/.venv
COPY --from=builder /app/src /app/src
ENV PATH="/app/.venv/bin:$PATH"
USER app
EXPOSE 8001
# Default command: the API. The worker overrides it (docker-compose / prod).
CMD ["uvicorn", "aiagent.adapters.api.app:app", "--host", "0.0.0.0", "--port", "8001"]
