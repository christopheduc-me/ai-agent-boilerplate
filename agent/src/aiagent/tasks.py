"""Celery tasks: thin glue wiring adapters into the use case (no business logic)."""

import logging
from collections.abc import Iterator
from contextlib import contextmanager
from typing import TYPE_CHECKING, Any

from aiagent import metrics
from aiagent.application import run_agent_research, run_research
from aiagent.celery_app import app
from aiagent.config import Settings
from aiagent.domain.models import ResearchResult
from aiagent.domain.ports import (
    AgentPolicy,
    HitEnricher,
    PageDateFetcher,
    ResultCritic,
    SearchProvider,
)
from aiagent.domain.usage import Pricing, SpendGuard, UsageMeter

if TYPE_CHECKING:
    from langgraph.checkpoint.base import BaseCheckpointSaver

logger = logging.getLogger(__name__)


def _pricing_for(settings: Settings) -> Pricing:
    """Indicative USD rates (ADR-038). Fakes are free (ADR-021): $0, so the
    spend cap (ADR-048) never trips in the keyless demo/e2e."""
    if settings.providers == "fake":
        return Pricing(llm_input_per_mtok=0.0, llm_output_per_mtok=0.0, search_per_call=0.0)
    return Pricing(
        llm_input_per_mtok=settings.llm_cost_input_per_mtok,
        llm_output_per_mtok=settings.llm_cost_output_per_mtok,
        search_per_call=settings.search_cost_per_call,
    )


@contextmanager
def _agent_checkpointer(settings: Settings) -> "Iterator[BaseCheckpointSaver[Any]]":
    """Durable checkpoint store for the LangGraph orchestrator (ADR-046),
    keyed by job_id. Redis is the worker's own infrastructure (the Celery
    broker), so this respects ADR-006 — the worker still never touches the
    database. A seam so tests can supply an in-memory saver instead."""
    from langgraph.checkpoint.redis import RedisSaver

    with RedisSaver.from_conn_string(settings.redis_url) as checkpointer:
        checkpointer.setup()  # idempotent: creates the Redis indices once
        yield checkpointer


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

    from aiagent.adapters.chat_model import make_chat_model, make_fallback_chat_models
    from aiagent.adapters.llm import LlmHitEnricher
    from aiagent.adapters.page import HttpPageDateFetcher

    return (
        build_search_provider(settings.search_providers, meter),
        LlmHitEnricher(
            make_chat_model(settings, max_tokens=256),
            meter=meter,
            model=settings.agent_model_id,
            system=settings.llm_backend,
            fallbacks=make_fallback_chat_models(settings, max_tokens=256),
        ),
        HttpPageDateFetcher(),
    )


def _search_provider(name: str, meter: UsageMeter | None) -> SearchProvider:
    """One live search adapter by name (ADR-051)."""
    if name == "tavily":
        from aiagent.adapters.tavily import TavilySearchProvider

        return TavilySearchProvider(meter=meter)
    if name == "duckduckgo":
        from aiagent.adapters.duckduckgo import DuckDuckGoSearchProvider

        return DuckDuckGoSearchProvider(meter=meter)
    raise ValueError(f"unknown search provider {name!r} (expected: tavily, duckduckgo)")


def build_search_provider(names: list[str], meter: UsageMeter | None = None) -> SearchProvider:
    """Builds the live `SearchProvider` from the configured engine names (ADR-051):
    a single adapter for one name, or an `AggregatingSearchProvider` fusing
    several (concurrent, deduplicated, rank-fused, partial-failure tolerant)."""
    providers = [_search_provider(name, meter) for name in names]
    if not providers:
        raise ValueError("no search provider configured (AGENT_SEARCH_PROVIDERS is empty)")
    if len(providers) == 1:
        return providers[0]
    from aiagent.adapters.aggregating_search import AggregatingSearchProvider

    return AggregatingSearchProvider(providers)


def build_policy(settings: Settings, meter: UsageMeter | None = None) -> AgentPolicy:
    """Selects the decision-maker of the agentic loop (ADR-030)."""
    if settings.providers == "fake":
        from aiagent.adapters.fake import FakeAgentPolicy

        return FakeAgentPolicy(meter)

    from aiagent.adapters.chat_model import make_chat_model, make_fallback_chat_models
    from aiagent.adapters.llm import LlmAgentPolicy

    return LlmAgentPolicy(
        make_chat_model(settings, max_tokens=256),
        meter=meter,
        model=settings.agent_model_id,
        system=settings.llm_backend,
        fallbacks=make_fallback_chat_models(settings, max_tokens=256),
    )


def build_critic(settings: Settings, meter: UsageMeter | None = None) -> ResultCritic:
    """Selects the self-critique reviewer (ADR-031)."""
    if settings.providers == "fake":
        from aiagent.adapters.fake import FakeResultCritic

        return FakeResultCritic(meter)

    from aiagent.adapters.chat_model import make_chat_model, make_fallback_chat_models
    from aiagent.adapters.llm import LlmResultCritic

    return LlmResultCritic(
        make_chat_model(settings, max_tokens=512),
        meter=meter,
        model=settings.agent_model_id,
        system=settings.llm_backend,
        fallbacks=make_fallback_chat_models(settings, max_tokens=512),
    )


def _run_agent(
    settings: Settings,
    job_id: str,
    keyword: str,
    clarification: str | None,
    search: SearchProvider,
    enricher: HitEnricher,
    policy: AgentPolicy,
    critic: ResultCritic | None,
    # HttpResultSink structurally satisfies ResultSink + StepReporter +
    # ClarificationRequester; typed Any to pass it in all three roles.
    sink: Any,
    memory: set[str] | None,
    page_dates: PageDateFetcher,
    budget: SpendGuard,
) -> list[ResearchResult] | None:
    """Dispatches the agent mode to the configured orchestrator (ADR-046).
    Both drive the same ports; `sink` also acts as StepReporter and
    ClarificationRequester. Returns the results, or None when paused."""
    if settings.agent_orchestrator == "loop":
        goal = keyword
        if clarification:
            goal = f'{keyword} (user clarification: "{clarification}")'
        return run_agent_research(
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
            budget=budget,
        )
    from aiagent.adapters.orchestration.langgraph_agent import run_agent_graph

    with _agent_checkpointer(settings) as checkpointer:
        return run_agent_graph(
            job_id,
            keyword,
            search,
            enricher,
            policy,
            sink,
            sink,
            checkpointer,
            critic=critic,
            clarifier=sink,
            seen_urls=memory,
            page_dates=page_dates,
            max_steps=settings.agent_max_steps,
            budget=budget,
            # The re-dispatch after an answer carries it as `clarification`;
            # for the graph that means: resume from the checkpoint (ADR-046).
            resume_answer=clarification,
        )


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
    # Spend cap (ADR-048): checked live against this same meter; 0 disables it,
    # and the fakes price at $0 so it never trips in the keyless demo.
    budget = SpendGuard(meter, _pricing_for(settings), settings.agent_max_cost_usd)
    try:
        search, enricher, page_dates = build_providers(settings, meter)
        policy = build_policy(settings, meter) if mode == "agent" else None
        critic = build_critic(settings, meter) if mode == "agent" else None
    except Exception as exc:
        # Misconfiguration (missing API key...) must surface to the user as a failed job.
        logger.error("agent misconfigured", extra=log_ctx, exc_info=True)
        sink.report_failure(job_id, f"agent misconfigured: {exc}")
        raise

    # Outcome for the job metric (ADR-050); the exit paths below refine it.
    job_outcome = "failed"

    def report_usage() -> None:
        """Spend tracking (ADR-038): sent at every task end (success, pause,
        failure) so retries and resumed runs accumulate their real cost.
        Also feeds the job/cost metrics (ADR-050). Best-effort — losing a
        metric never fails the job."""
        usage = meter.snapshot()
        pricing = _pricing_for(settings)
        metrics.record_job(job_outcome, usage.cost_usd(pricing))
        if usage.llm_calls == 0 and usage.search_calls == 0:
            return
        try:
            sink.report_usage(job_id, usage, pricing)
        except Exception:  # noqa: BLE001 - best effort by contract
            logger.warning("failed to report usage", extra=log_ctx, exc_info=True)

    try:
        if policy is not None:
            # Agent mode (ADR-030/031/032): the policy drives the decisions, the
            # critic reviews the results before delivery, and the sink also
            # implements StepReporter + ClarificationRequester. Two orchestrators
            # (ADR-046) share these ports: the LangGraph StateGraph (default,
            # durable checkpointing + native interrupt HITL) or the hand-rolled
            # loop (AGENT_ORCHESTRATOR=loop).
            outcome = _run_agent(
                settings,
                job_id,
                keyword,
                clarification,
                search,
                enricher,
                policy,
                critic,
                sink,
                memory,
                page_dates,
                budget,
            )
            if outcome is None:
                # Paused (ADR-032): the job awaits the user's answer; a fresh
                # task will be dispatched when it arrives.
                job_outcome = "paused"
                logger.info("research task paused awaiting user input", extra=log_ctx)
                return 0
            results = outcome
        else:
            results = run_research(
                job_id, keyword, search, enricher, sink, seen_urls=memory, page_dates=page_dates
            )
        job_outcome = "completed"
    except Exception:
        logger.error("research task failed", extra=log_ctx, exc_info=True)
        raise
    finally:
        report_usage()
    logger.info("research task completed", extra={**log_ctx, "results": len(results)})
    return len(results)
