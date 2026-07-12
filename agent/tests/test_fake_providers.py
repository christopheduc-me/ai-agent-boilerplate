"""Fake providers and provider selection (ADR-021)."""

import pytest
from pydantic import ValidationError

from aiagent.adapters.fake import FakeAgentPolicy, FakeHitEnricher, FakeSearchProvider
from aiagent.application import run_research
from aiagent.config import Settings
from aiagent.domain.models import (
    AgentStep,
    AgentStepKind,
    DateConfidence,
    EventType,
    FinishAction,
    SearchAction,
)
from aiagent.tasks import build_policy, build_providers


def settings_with(providers: str) -> Settings:
    return Settings(
        redis_url="redis://localhost:6379/0",
        backend_internal_url="http://localhost:8000",
        internal_api_token="t",
        agent_model_id="claude-opus-4-8",
        providers=providers,
        agent_max_steps=5,
    )


class NullSink:
    def mark_started(self, job_id: str) -> None: ...
    def deliver(self, job_id: str, results: list) -> None: ...
    def report_failure(self, job_id: str, error: str) -> None: ...


def test_build_providers_selects_fakes() -> None:
    search, enricher = build_providers(settings_with("fake"))
    assert isinstance(search, FakeSearchProvider)
    assert isinstance(enricher, FakeHitEnricher)


def test_build_providers_live_requires_credentials(monkeypatch) -> None:
    """The live path still fails fast without keys (covered by ADR-020 at
    worker startup; this guards the factory itself)."""
    monkeypatch.delenv("TAVILY_API_KEY", raising=False)
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    with pytest.raises(ValidationError):
        build_providers(settings_with("live"))


def test_fake_run_exercises_the_full_date_cascade() -> None:
    """One deterministic run covers high/medium/unknown confidence and sorting."""
    results = run_research("job-1", "anything", FakeSearchProvider(), FakeHitEnricher(), NullSink())

    titles = [r.title for r in results]
    assert titles == [
        "fake-dated-recent",  # 2026 — newest first
        "fake-llm-datable",  # 2025 — date found by the fake LLM
        "fake-dated-old",  # 2023
        "fake-undatable",  # no date — always last
    ]
    by_title = {r.title: r for r in results}
    assert by_title["fake-dated-recent"].date_confidence == DateConfidence.HIGH
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
