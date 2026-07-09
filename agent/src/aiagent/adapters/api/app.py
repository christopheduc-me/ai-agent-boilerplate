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

configure_logging()
logger = logging.getLogger(__name__)


@asynccontextmanager
async def lifespan(_app: FastAPI) -> AsyncIterator[None]:
    # Fail-fast (ADR-020): the API has development defaults for everything,
    # but a placeholder secret must never reach production.
    forbid_placeholders("agent-api", "INTERNAL_API_TOKEN")
    yield


app = FastAPI(title="aiagent task API", docs_url=None, redoc_url=None, lifespan=lifespan)


class TaskRequest(BaseModel):
    job_id: str
    keyword: str


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
    run_research_task.delay(body.job_id, body.keyword, request_id=request_id)
    response.headers["X-Request-Id"] = request_id
    logger.info(
        "task queued",
        extra={"request_id": request_id, "job_id": body.job_id},
    )
    return {"job_id": body.job_id, "state": "queued"}
