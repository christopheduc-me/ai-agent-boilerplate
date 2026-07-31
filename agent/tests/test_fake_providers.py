"""Fake providers and provider selection (ADR-021)."""

import pytest
from pydantic import ValidationError

from aiagent.adapters.fake import (
    FakeAgentPolicy,
    FakeHitEnricher,
    FakePageDateFetcher,
    FakeResultCritic,
    FakeSearchProvider,
)
from aiagent.application import run_research
from aiagent.config import Settings
from aiagent.domain.models import (
    AgentStep,
    AgentStepKind,
    AskAction,
    DateConfidence,
    EventType,
    FinishAction,
    SearchAction,
)
from aiagent.tasks import build_critic, build_policy, build_providers


def settings_with(providers: str) -> Settings:
    return Settings(
        redis_url="redis://localhost:6379/0",
        backend_internal_url="http://localhost:8000",
        internal_api_token="t",
        agent_model_id="claude-opus-4-8",
        providers=providers,
        search_providers=["tavily"],
        agent_max_steps=5,
        agent_max_cost_usd=2.0,
        agent_orchestrator="langgraph",
        llm_cost_input_per_mtok=5.0,
        llm_cost_output_per_mtok=25.0,
        search_cost_per_call=0.008,
        llm_backend="anthropic",
        llm_base_url="http://localhost:11434",
        llm_timeout_seconds=60.0,
        llm_max_retries=2,
        model_fallbacks=[],
    )


class NullSink:
    def mark_started(self, job_id: str) -> None: ...
    def deliver(self, job_id: str, results: list) -> None: ...
    def report_failure(self, job_id: str, error: str) -> None: ...


def test_build_providers_selects_fakes() -> None:
    search, enricher, page_dates = build_providers(settings_with("fake"))
    assert isinstance(search, FakeSearchProvider)
    assert isinstance(enricher, FakeHitEnricher)
    assert isinstance(page_dates, FakePageDateFetcher)


def test_build_search_provider_single_and_aggregated() -> None:
    from aiagent.adapters.aggregating_search import AggregatingSearchProvider
    from aiagent.adapters.duckduckgo import DuckDuckGoSearchProvider
    from aiagent.tasks import build_search_provider

    # One engine -> the bare adapter; several -> the aggregator (ADR-051).
    assert isinstance(build_search_provider(["duckduckgo"]), DuckDuckGoSearchProvider)
    assert isinstance(
        build_search_provider(["duckduckgo", "duckduckgo"]), AggregatingSearchProvider
    )


def test_build_search_provider_rejects_an_unknown_engine() -> None:
    from aiagent.tasks import build_search_provider

    with pytest.raises(ValueError, match="unknown search provider"):
        build_search_provider(["bing"])


def test_build_providers_live_requires_credentials(monkeypatch) -> None:
    """The live path still fails fast without keys (covered by ADR-020 at
    worker startup; this guards the factory itself)."""
    monkeypatch.delenv("TAVILY_API_KEY", raising=False)
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    with pytest.raises(ValidationError):
        build_providers(settings_with("live"))


def test_fake_run_exercises_the_full_date_cascade() -> None:
    """One deterministic run covers every cascade stage (ADR-011/035) + sorting."""
    results = run_research(
        "job-1",
        "anything",
        FakeSearchProvider(),
        FakeHitEnricher(),
        NullSink(),
        page_dates=FakePageDateFetcher(),
    )

    titles = [r.title for r in results]
    assert titles == [
        "fake-dated-recent",  # 2026-05 — provider date, newest first
        "fake-page-datable",  # 2025-12 — date declared by the page (ADR-035)
        "fake-llm-datable",  # 2025-08 — date found by the fake LLM
        "fake-dated-old",  # 2023
        "fake-undatable",  # no date — always last
    ]
    by_title = {r.title: r for r in results}
    assert by_title["fake-dated-recent"].date_confidence == DateConfidence.HIGH
    assert by_title["fake-page-datable"].date_confidence == DateConfidence.HIGH
    assert by_title["fake-llm-datable"].date_confidence == DateConfidence.MEDIUM
    assert by_title["fake-undatable"].date_confidence == DateConfidence.UNKNOWN
    # Enrichment (ADR-027): deterministic event type and summary on every result.
    assert all(r.event_type == EventType.ANNOUNCEMENT for r in results)
    assert by_title["fake-undatable"].summary == "Fake summary for fake-undatable"


def test_fake_search_is_deterministic() -> None:
    provider = FakeSearchProvider()
    assert provider.search("kw") == provider.search("kw")


def test_fake_policy_searches_refines_then_finishes() -> None:
    policy = FakeAgentPolicy()
    first = policy.decide("rust", [], [])
    assert first == SearchAction(query="rust", reason="Start with the user's goal as the query")

    one_step = [AgentStep(seq=1, kind=AgentStepKind.SEARCH, detail="rust", reason="r", new_hits=4)]
    second = policy.decide("rust", one_step, [])
    assert isinstance(second, SearchAction) and second.query == "rust latest"

    two_steps = one_step + [
        AgentStep(seq=2, kind=AgentStepKind.SEARCH, detail="rust latest", reason="r", new_hits=0)
    ]
    assert isinstance(policy.decide("rust", two_steps, []), FinishAction)


def test_build_policy_selects_the_fake(monkeypatch) -> None:
    monkeypatch.setenv("AGENT_PROVIDERS", "fake")
    assert isinstance(build_policy(Settings.from_env()), FakeAgentPolicy)


def test_fake_critic_returns_a_stable_non_destructive_review() -> None:
    critique = FakeResultCritic().critique("rust", FakeSearchProvider().search("rust"))
    assert "All 5 results" in critique.assessment
    assert critique.irrelevant_urls == () and critique.gap_query is None


def test_build_critic_selects_the_fake(monkeypatch) -> None:
    monkeypatch.setenv("AGENT_PROVIDERS", "fake")
    assert isinstance(build_critic(Settings.from_env()), FakeResultCritic)


def test_fake_policy_asks_once_on_an_ambiguous_goal() -> None:
    policy = FakeAgentPolicy()
    first = policy.decide("ambiguous topic", [], [])
    assert isinstance(first, AskAction)

    # The task folds the answer into the goal on resume: no second question.
    resumed = policy.decide('ambiguous topic (user clarification: "cars")', [], [])
    assert isinstance(resumed, SearchAction)
