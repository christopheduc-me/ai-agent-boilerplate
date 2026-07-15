"""The agentic loop (ADR-030), tested with a scripted policy and port fakes."""

from datetime import UTC, datetime

import pytest

from aiagent.application.run_agent_research import run_agent_research
from aiagent.domain.models import (
    AgentAction,
    AgentStep,
    AgentStepKind,
    AskAction,
    Critique,
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


# ---------------------------------------------------------------- self-critique (ADR-031)


class ScriptedCritic:
    def __init__(self, critique: Critique) -> None:
        self._critique = critique
        self.seen: list[tuple[str, int]] = []

    def critique(self, goal: str, hits: list[RawSearchHit]) -> Critique:
        self.seen.append((goal, len(hits)))
        return self._critique


def test_critique_runs_after_finish_and_drops_off_topic_hits() -> None:
    search = MappedSearch({"q": [hit("https://a"), hit("https://off-topic")]})
    policy = ScriptedPolicy([SearchAction(query="q", reason="r"), FinishAction(reason="done")])
    critic = ScriptedCritic(
        Critique(
            assessment="One result is unrelated to the goal.",
            irrelevant_urls=("https://off-topic", "https://never-collected"),
        )
    )
    sink, reporter = RecordingSink(), RecordingReporter()

    results = run_agent_research(
        "job-5",
        "goal",
        search,
        NeutralEnricher(),
        policy,
        sink,
        reporter,
        critic=critic,
        max_steps=5,
    )

    # The off-topic hit is dropped from the delivery, and the journal says so.
    assert [r.url for r in results] == ["https://a"]
    assert critic.seen == [("goal", 2)]
    last = reporter.steps[-1]
    assert last.kind is AgentStepKind.CRITIQUE
    assert last.seq == 3  # search, finish, critique
    assert "unrelated" in last.reason and "dropped 1 off-topic" in last.reason


def test_critique_gap_triggers_one_repair_search_within_budget() -> None:
    search = MappedSearch({"q": [hit("https://a")], "q recent": [hit("https://fresh")]})
    policy = ScriptedPolicy([SearchAction(query="q", reason="r"), FinishAction(reason="done")])
    critic = ScriptedCritic(Critique(assessment="No recent source.", gap_query="q recent"))
    sink, reporter = RecordingSink(), RecordingReporter()

    results = run_agent_research(
        "job-6",
        "goal",
        search,
        NeutralEnricher(),
        policy,
        sink,
        reporter,
        critic=critic,
        max_steps=5,
    )

    assert search.queries == ["q", "q recent"]
    assert {r.url for r in results} == {"https://a", "https://fresh"}
    kinds = [s.kind for s in reporter.steps]
    assert kinds == [
        AgentStepKind.SEARCH,
        AgentStepKind.FINISH,
        AgentStepKind.CRITIQUE,
        AgentStepKind.SEARCH,
    ]
    repair = reporter.steps[-1]
    assert repair.detail == "q recent" and repair.new_hits == 1
    assert "self-critique" in repair.reason


def test_critique_gap_is_ignored_when_the_search_budget_is_spent() -> None:
    search = MappedSearch({"q": [hit("https://a")]})
    policy = ScriptedPolicy(
        [SearchAction(query="q", reason="1"), SearchAction(query="q", reason="2")]
    )
    critic = ScriptedCritic(Critique(assessment="Gap remains.", gap_query="q more"))
    sink, reporter = RecordingSink(), RecordingReporter()

    run_agent_research(
        "job-7",
        "goal",
        search,
        NeutralEnricher(),
        policy,
        sink,
        reporter,
        critic=critic,
        max_steps=2,
    )

    # Two searches allowed: the gap named by the critique is not searched.
    assert search.queries == ["q", "q"]
    assert [s.kind for s in reporter.steps] == [
        AgentStepKind.SEARCH,
        AgentStepKind.SEARCH,
        AgentStepKind.FINISH,
        AgentStepKind.CRITIQUE,
    ]


def test_without_a_critic_the_loop_behaves_as_before() -> None:
    search = MappedSearch({"q": [hit("https://a")]})
    policy = ScriptedPolicy([SearchAction(query="q", reason="r"), FinishAction(reason="done")])
    sink, reporter = RecordingSink(), RecordingReporter()

    results = run_agent_research(
        "job-8", "goal", search, NeutralEnricher(), policy, sink, reporter, max_steps=5
    )

    assert len(results) == 1
    assert [s.kind for s in reporter.steps] == [AgentStepKind.SEARCH, AgentStepKind.FINISH]


# ---------------------------------------------------------------- clarification (ADR-032)


class RecordingClarifier:
    def __init__(self) -> None:
        self.questions: list[tuple[str, str]] = []

    def request_clarification(self, job_id: str, question: str) -> None:
        self.questions.append((job_id, question))


def test_ask_pauses_the_job_without_delivering() -> None:
    search = MappedSearch({})
    policy = ScriptedPolicy([AskAction(question="Animal or car?", reason="ambiguous")])
    sink, reporter, clarifier = RecordingSink(), RecordingReporter(), RecordingClarifier()

    outcome = run_agent_research(
        "job-9",
        "jaguar",
        search,
        NeutralEnricher(),
        policy,
        sink,
        reporter,
        clarifier=clarifier,
        max_steps=5,
    )

    assert outcome is None  # paused: nothing delivered, no failure
    assert clarifier.questions == [("job-9", "Animal or car?")]
    assert sink.delivered == [] and sink.failures == []
    assert search.queries == []


def test_ask_after_an_answer_degrades_to_finish() -> None:
    # The guard against question ping-pong: one clarification per job.
    search = MappedSearch({"q": [hit("https://a")]})
    policy = ScriptedPolicy(
        [SearchAction(query="q", reason="r"), AskAction(question="again?", reason="r")]
    )
    sink, reporter, clarifier = RecordingSink(), RecordingReporter(), RecordingClarifier()

    outcome = run_agent_research(
        "job-10",
        "goal",
        search,
        NeutralEnricher(),
        policy,
        sink,
        reporter,
        clarifier=clarifier,
        clarification="the car",
        max_steps=5,
    )

    assert outcome is not None and len(outcome) == 1  # delivered normally
    assert clarifier.questions == []
    assert reporter.steps[-1].kind is AgentStepKind.FINISH


def test_ask_without_a_clarifier_degrades_to_finish() -> None:
    search = MappedSearch({"q": [hit("https://a")]})
    policy = ScriptedPolicy(
        [SearchAction(query="q", reason="r"), AskAction(question="hm?", reason="r")]
    )
    sink, reporter = RecordingSink(), RecordingReporter()

    outcome = run_agent_research(
        "job-11", "goal", search, NeutralEnricher(), policy, sink, reporter, max_steps=5
    )

    assert outcome is not None and sink.delivered == [("job-11", 1)]
