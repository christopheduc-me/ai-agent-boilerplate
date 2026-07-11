"""Celery tasks: thin glue wiring adapters into the use case (no business logic)."""

import logging

from aiagent.application import run_research
from aiagent.celery_app import app
from aiagent.config import Settings
from aiagent.domain.ports import HitEnricher, SearchProvider

logger = logging.getLogger(__name__)


def build_providers(settings: Settings) -> tuple[SearchProvider, HitEnricher]:
    """Selects the provider adapters (ADR-021): live (Tavily + Claude) by
    default, deterministic fakes with `AGENT_PROVIDERS=fake`."""
    if settings.providers == "fake":
        from aiagent.adapters.fake import FakeHitEnricher, FakeSearchProvider

        return FakeSearchProvider(), FakeHitEnricher()

    from aiagent.adapters.llm import ClaudeHitEnricher
    from aiagent.adapters.tavily import TavilySearchProvider

    return TavilySearchProvider(), ClaudeHitEnricher(settings.agent_model_id)


@app.task(
    name="aiagent.run_research",
    bind=False,
    # Transient failures (network, provider hiccup) are retried with exponential
    # backoff; idempotence makes re-runs safe (ADR-016). After the last retry the
    # job stays failed via report_failure / the backend reaper.
    autoretry_for=(Exception,),
    max_retries=3,
    retry_backoff=True,
    retry_backoff_max=600,
    retry_jitter=True,
)
def run_research_task(job_id: str, keyword: str, request_id: str | None = None) -> int:
    settings = Settings.from_env()
    request_id = request_id or job_id
    log_ctx = {"request_id": request_id, "job_id": job_id}
    logger.info("research task started", extra=log_ctx)

    from aiagent.adapters.sink import HttpResultSink

    sink = HttpResultSink(
        settings.backend_internal_url,
        settings.internal_api_token,
        request_id=request_id,
    )
    try:
        search, date_extractor = build_providers(settings)
    except Exception as exc:
        # Misconfiguration (missing API key...) must surface to the user as a failed job.
        logger.error("agent misconfigured", extra=log_ctx, exc_info=True)
        sink.report_failure(job_id, f"agent misconfigured: {exc}")
        raise

    try:
        results = run_research(job_id, keyword, search, date_extractor, sink)
    except Exception:
        logger.error("research task failed", extra=log_ctx, exc_info=True)
        raise
    logger.info("research task completed", extra={**log_ctx, "results": len(results)})
    return len(results)
