"""Celery tasks: thin glue wiring adapters into the use case (no business logic)."""

import logging

from aiagent.application import run_agent_research, run_research
from aiagent.celery_app import app
from aiagent.config import Settings
from aiagent.domain.ports import (
    AgentPolicy,
    HitEnricher,
    PageDateFetcher,
    ResultCritic,
    SearchProvider,
)
from aiagent.domain.usage import Pricing, UsageMeter

logger = logging.getLogger(__name__)


def build_providers(
    settings: Settings, meter: UsageMeter | None = None
) -> tuple[SearchProvider, HitEnricher, PageDateFetcher]:
    """Selects the provider adapters (ADR-021): live (Tavily + Claude + page
    metadata) by default, deterministic fakes with `AGENT_PROVIDERS=fake`.
    The meter records spend (ADR-038)."""
    if settings.providers == "fake":
        from aiagent.adapters.fake import (
            FakeHitEnricher,
            FakePageDateFetcher,
            FakeSearchProvider,
        )

        return FakeSearchProvider(meter), FakeHitEnricher(meter), FakePageDateFetcher()

    from aiagent.adapters.llm import ClaudeHitEnricher
    from aiagent.adapters.page import HttpPageDateFetcher
    from aiagent.adapters.tavily import TavilySearchProvider

    return (
        TavilySearchProvider(meter=meter),
        ClaudeHitEnricher(settings.agent_model_id, meter=meter),
        HttpPageDateFetcher(),
    )


def build_policy(settings: Settings, meter: UsageMeter | None = None) -> AgentPolicy:
    """Selects the decision-maker of the agentic loop (ADR-030)."""
    if settings.providers == "fake":
        from aiagent.adapters.fake import FakeAgentPolicy

        return FakeAgentPolicy(meter)

    from aiagent.adapters.llm import ClaudeAgentPolicy

    return ClaudeAgentPolicy(settings.agent_model_id, meter=meter)


def build_critic(settings: Settings, meter: UsageMeter | None = None) -> ResultCritic:
    """Selects the self-critique reviewer (ADR-031)."""
    if settings.providers == "fake":
        from aiagent.adapters.fake import FakeResultCritic

        return FakeResultCritic(meter)

    from aiagent.adapters.llm import ClaudeResultCritic

    return ClaudeResultCritic(settings.agent_model_id, meter=meter)


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
def run_research_task(
    job_id: str,
    keyword: str,
    request_id: str | None = None,
    mode: str = "workflow",
    clarification: str | None = None,
    recurring: bool = False,
    seen_urls: list[str] | None = None,
) -> int:
    settings = Settings.from_env()
    request_id = request_id or job_id
    # One-shot searches carry no memory; a recurring run flags its results
    # against the (possibly empty, on the first run) memory (ADR-033).
    memory = set(seen_urls or []) if recurring else None
    log_ctx = {"request_id": request_id, "job_id": job_id, "mode": mode}
    logger.info("research task started", extra=log_ctx)

    from aiagent.adapters.sink import HttpResultSink

    sink = HttpResultSink(
        settings.backend_internal_url,
        settings.internal_api_token,
        request_id=request_id,
    )
    meter = UsageMeter()
    try:
        search, enricher, page_dates = build_providers(settings, meter)
        policy = build_policy(settings, meter) if mode == "agent" else None
        critic = build_critic(settings, meter) if mode == "agent" else None
    except Exception as exc:
        # Misconfiguration (missing API key...) must surface to the user as a failed job.
        logger.error("agent misconfigured", extra=log_ctx, exc_info=True)
        sink.report_failure(job_id, f"agent misconfigured: {exc}")
        raise

    def report_usage() -> None:
        """Spend tracking (ADR-038): sent at every task end (success, pause,
        failure) so retries and resumed runs accumulate their real cost.
        Best-effort — losing the metric never fails the job."""
        usage = meter.snapshot()
        if usage.llm_calls == 0 and usage.search_calls == 0:
            return
        pricing = (
            # Fake providers are free (ADR-021): calls are counted, cost is $0.
            Pricing(llm_input_per_mtok=0.0, llm_output_per_mtok=0.0, search_per_call=0.0)
            if settings.providers == "fake"
            else Pricing(
                llm_input_per_mtok=settings.llm_cost_input_per_mtok,
                llm_output_per_mtok=settings.llm_cost_output_per_mtok,
                search_per_call=settings.search_cost_per_call,
            )
        )
        try:
            sink.report_usage(job_id, usage, pricing)
        except Exception:  # noqa: BLE001 - best effort by contract
            logger.warning("failed to report usage", extra=log_ctx, exc_info=True)

    try:
        if policy is not None:
            # Agent mode (ADR-030/031/032): the policy drives the loop, the
            # critic reviews the results before delivery, and the sink also
            # implements StepReporter + ClarificationRequester. The user's
            # answer (if any) is folded into the goal for the policy.
            goal = keyword
            if clarification:
                goal = f'{keyword} (user clarification: "{clarification}")'
            outcome = run_agent_research(
                job_id,
                goal,
                search,
                enricher,
                policy,
                sink,
                sink,
                critic=critic,
                clarifier=sink,
                clarification=clarification,
                seen_urls=memory,
                page_dates=page_dates,
                max_steps=settings.agent_max_steps,
            )
            if outcome is None:
                # Paused (ADR-032): the job awaits the user's answer; a fresh
                # task will be dispatched when it arrives.
                logger.info("research task paused awaiting user input", extra=log_ctx)
                return 0
            results = outcome
        else:
            results = run_research(
                job_id, keyword, search, enricher, sink, seen_urls=memory, page_dates=page_dates
            )
    except Exception:
        logger.error("research task failed", extra=log_ctx, exc_info=True)
        raise
    finally:
        report_usage()
    logger.info("research task completed", extra={**log_ctx, "results": len(results)})
    return len(results)
