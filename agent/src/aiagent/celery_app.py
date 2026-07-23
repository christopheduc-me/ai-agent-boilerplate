"""Celery application (broker + result backend on Redis, ADR-004)."""

import os
from typing import Any

from celery import Celery
from celery.signals import setup_logging, worker_init, worker_process_init

from aiagent.config import forbid_placeholders, require_env
from aiagent.logging_setup import configure_logging
from aiagent.telemetry import configure_telemetry

_redis_url = os.environ.get("REDIS_URL", "redis://localhost:6379/0")


@setup_logging.connect
def _configure_worker_logging(**_kwargs: Any) -> None:
    """Keep our structured logging (ADR-018) instead of Celery's hijack."""
    configure_logging()


@worker_process_init.connect
def _configure_worker_telemetry(**_kwargs: Any) -> None:
    """Traces (ADR-029, opt-in): resume the trace context injected by the
    producer into the task message, and propagate it again on the httpx
    callbacks to the backend. Per child process, as the OTel Celery docs
    require."""
    configure_telemetry("agent-worker")


@worker_init.connect
def _check_required_env(**_kwargs: Any) -> None:
    """Fail-fast (ADR-020): without the provider keys, every task would fail at
    runtime — refuse to start instead. Fires only in the worker process, so the
    FastAPI container (which needs no provider key) is unaffected. With fake
    providers (ADR-021) no key is needed at all; with a local LLM backend
    (ADR-041) only the search key remains required."""
    if os.environ.get("AGENT_PROVIDERS", "live") != "fake":
        keys = ["TAVILY_API_KEY"]
        if os.environ.get("AGENT_LLM_BACKEND", "anthropic") == "anthropic":
            keys.append("ANTHROPIC_API_KEY")
        require_env("agent-worker", *keys)
    forbid_placeholders("agent-worker", "INTERNAL_API_TOKEN")


app = Celery("aiagent", broker=_redis_url, backend=_redis_url, include=["aiagent.tasks"])
# Reliability (ADR-016): a task is acked only after it finishes, and is requeued
# if the worker process dies mid-task. Safe because the whole flow is idempotent
# (started is a no-op when not pending, result delivery replaces).
app.conf.task_acks_late = True
app.conf.task_reject_on_worker_lost = True
app.conf.worker_prefetch_multiplier = 1
