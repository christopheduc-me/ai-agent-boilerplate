"""The agentic loop (ADR-030), tested with a scripted policy and port fakes."""

from datetime import UTC, datetime

import pytest

from aiagent.application.run_agent_research import run_agent_research
from aiagent.domain.models import (
    AgentAction,
    AgentStep,
    AgentStepKind,
    FinishAction,
    HitEnrichment,
    RawSearchHit,
    SearchAction,
)


class ScriptedPolicy:
    """Plays a fixed list of decisions, recording what it was shown."""

    def __init__(self, actions: list[AgentAction]) -> None:
        self._actions = list(actions)
        self.seen: list[tuple[list[AgentStep], int]] = []

    def decide(self, goal: str, steps: list[AgentStep], hits: list[RawSearchHit]) -> AgentAction:
        self.seen.append((list(steps), len(hits)))
        return self._actions.pop(0)


class MappedSearch:
    """Returns canned hits per query; unknown queries return nothing."""

    def __init__(self, by_query: dict[str, list[RawSearchHit]]) -> None:
        self._by_query = by_query
        self.queries: list[str] = []

    def search(self, keyword: str) -> list[RawSearchHit]:
        self.queries.append(keyword)
        return self._by_query.get(keyword, [])


class NeutralEnricher:
    def enrich(self, hit: RawSearchHit) -> HitEnrichment:
        return HitEnrichment()


class RecordingSink:
    def __init__(self) -> None:
        self.started: list[str] = []
        self.delivered: list[tuple[str, int]] = []
        self.failures: list[tuple[str, str]] = []

    def mark_started(self, job_id: str) -> None:
        self.started.append(job_id)

    def deliver(self, job_id: str, results) -> None:  # type: ignore[no-untyped-def]
        self.delivered.append((job_id, len(results)))
        self.results = results

    def report_failure(self, job_id: str, error: str) -> None:
        self.failures.append((job_id, error))


class RecordingReporter:
    def __init__(self, fail: bool = False) -> None:
        self.steps: list[AgentStep] = []
        self._fail = fail

    def report_step(self, job_id: str, step: AgentStep) -> None:
        if self._fail:
            raise RuntimeError("journal endpoint down")
        self.steps.append(step)


def hit(url: str, title: str = "t") -> RawSearchHit:
    return RawSearchHit(
        title=title, url=url, snippet="s", published_at=datetime(2026, 1, 1, tzinfo=UTC)
    )


def test_loop_searches_deduplicates_and_finishes() -> None:
    search = MappedSearch(
        {
            "rust": [hit("https://a"), hit("https://b")],
            "rust 2026": [hit("https://b"), hit("https://c")],  # b is a duplicate
        }
    )
    policy = ScriptedPolicy(
        [
            SearchAction(query="rust", reason="start with the goal"),
            SearchAction(query="rust 2026", reason="refine for recency"),
            FinishAction(reason="coverage sufficient"),
        ]
    )
    sink, reporter = RecordingSink(), RecordingReporter()

    results = run_agent_research(
        "job-1", "rust", search, NeutralEnricher(), policy, sink, reporter, max_steps=5
    )

    assert search.queries == ["rust", "rust 2026"]
    assert [r.url for r in results] == ["https://a", "https://b", "https://c"]
    assert sink.started == ["job-1"] and sink.delivered == [("job-1", 3)]
    # The journal recorded every decision with the dedup-aware hit counts.
    assert [(s.seq, s.kind, s.detail, s.new_hits) for s in reporter.steps] == [
        (1, AgentStepKind.SEARCH, "rust", 2),
        (2, AgentStepKind.SEARCH, "rust 2026", 1),
        (3, AgentStepKind.FINISH, "", 0),
    ]
    # The policy saw the growing transcript (steps, hits) before each decision.
    assert [(len(steps), hits) for steps, hits in policy.seen] == [(0, 0), (1, 2), (2, 3)]


def test_budget_exhaustion_forces_a_finish_step() -> None:
    search = MappedSearch({"q": [hit("https://a")]})
    policy = ScriptedPolicy(
        [SearchAction(query="q", reason="1"), SearchAction(query="q", reason="2")]
    )
    sink, reporter = RecordingSink(), RecordingReporter()

    run_agent_research(
        "job-2", "goal", search, NeutralEnricher(), policy, sink, reporter, max_steps=2
    )

    # Two searches allowed, then the loop finishes on its own and says so.
    kinds = [s.kind for s in reporter.steps]
    assert kinds == [AgentStepKind.SEARCH, AgentStepKind.SEARCH, AgentStepKind.FINISH]
    assert "budget" in reporter.steps[-1].reason
    assert sink.delivered == [("job-2", 1)]


def test_a_failing_journal_never_fails_the_job() -> None:
    search = MappedSearch({"q": [hit("https://a")]})
    policy = ScriptedPolicy([SearchAction(query="q", reason="r"), FinishAction(reason="done")])
    sink = RecordingSink()

    results = run_agent_research(
        "job-3",
        "goal",
        search,
        NeutralEnricher(),
        policy,
        sink,
        RecordingReporter(fail=True),
        max_steps=5,
    )

    assert len(results) == 1 and sink.delivered == [("job-3", 1)]
    assert sink.failures == []


def test_search_failure_reports_and_propagates() -> None:
    class ExplodingSearch:
        def search(self, keyword: str) -> list[RawSearchHit]:
            raise RuntimeError("provider down")

    policy = ScriptedPolicy([SearchAction(query="q", reason="r")])
    sink = RecordingSink()

    with pytest.raises(RuntimeError, match="provider down"):
        run_agent_research(
            "job-4",
            "goal",
            ExplodingSearch(),
            NeutralEnricher(),
            policy,
            sink,
            RecordingReporter(),
            max_steps=5,
        )
    assert sink.failures == [("job-4", "provider down")]
