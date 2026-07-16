"""FastAPI micro-API (ADR-005): the bridge between the Rust backend and Celery.

No business logic here — it authenticates the internal call and enqueues the task
through the official Celery client. The `X-Request-Id` sent by the backend (the
job id, ADR-018) rides along into the Celery task and is echoed on the response.
"""

import logging
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from typing import Annotated

from fastapi import FastAPI, Header, HTTPException, Response
from pydantic import BaseModel

from aiagent.config import Settings, forbid_placeholders
from aiagent.logging_setup import configure_logging
from aiagent.tasks import run_research_task
from aiagent.telemetry import configure_telemetry

configure_logging()
logger = logging.getLogger(__name__)
# Traces (ADR-029, opt-in): joins the backend's trace and lets the Celery
# instrumentation carry the context to the worker through the broker.
TELEMETRY_ENABLED = configure_telemetry("agent-api")


@asynccontextmanager
async def lifespan(_app: FastAPI) -> AsyncIterator[None]:
    # Fail-fast (ADR-020): the API has development defaults for everything,
    # but a placeholder secret must never reach production.
    forbid_placeholders("agent-api", "INTERNAL_API_TOKEN")
    yield


app = FastAPI(title="aiagent task API", docs_url=None, redoc_url=None, lifespan=lifespan)
if TELEMETRY_ENABLED:
    from opentelemetry.instrumentation.fastapi import FastAPIInstrumentor

    FastAPIInstrumentor.instrument_app(app)


class TaskRequest(BaseModel):
    job_id: str
    keyword: str
    # "workflow" (fixed pipeline) or "agent" (decision loop, ADR-030); the
    # default keeps pre-ADR-030 backends compatible.
    mode: str = "workflow"
    # The user's answer to the agent's clarification question (ADR-032);
    # only set when a paused job is re-dispatched.
    clarification: str | None = None
    # Recurring-search run (ADR-033): when true the agent flags the delta
    # against seen_urls (empty on the first run) and journals a report.
    recurring: bool = False
    seen_urls: list[str] = []


@app.get("/healthz")
def healthz() -> dict[str, str]:
    return {"status": "ok"}


@app.post("/tasks", status_code=202)
def enqueue_task(
    body: TaskRequest,
    response: Response,
    x_internal_token: Annotated[str | None, Header()] = None,
    x_request_id: Annotated[str | None, Header()] = None,
) -> dict[str, str]:
    settings = Settings.from_env()
    if x_internal_token != settings.internal_api_token:
        raise HTTPException(status_code=401, detail="invalid or missing internal token")

    request_id = x_request_id or body.job_id
    run_research_task.delay(
        body.job_id,
        body.keyword,
        request_id=request_id,
        mode=body.mode,
        clarification=body.clarification,
        recurring=body.recurring,
        seen_urls=body.seen_urls,
    )
    response.headers["X-Request-Id"] = request_id
    logger.info(
        "task queued",
        extra={"request_id": request_id, "job_id": body.job_id},
    )
    return {"job_id": body.job_id, "state": "queued"}
