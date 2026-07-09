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
RUN useradd --system --uid 10001 app
WORKDIR /app
COPY --from=builder /app/.venv /app/.venv
COPY --from=builder /app/src /app/src
ENV PATH="/app/.venv/bin:$PATH"
USER app
EXPOSE 8001
# Default command: the API. The worker overrides it (docker-compose / prod).
CMD ["uvicorn", "aiagent.adapters.api.app:app", "--host", "0.0.0.0", "--port", "8001"]
