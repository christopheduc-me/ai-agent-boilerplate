"""LangGraph orchestration (ADR-046): parity with the hand-rolled loop, plus
the checkpointed interrupt/resume HITL. Driven with scripted ports and an
in-memory checkpointer — deterministic, no I/O, no paid call."""

from datetime import UTC, datetime

import pytest
from langgraph.checkpoint.memory import InMemorySaver

from aiagent.adapters.orchestration.langgraph_agent import run_agent_graph
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
from aiagent.domain.usage import Pricing, SpendGuard, UsageMeter


class ScriptedPolicy:
    def __init__(self, actions: list[AgentAction]) -> None:
        self._actions = list(actions)
        self.seen: list[tuple[int, int]] = []

    def decide(self, goal: str, steps: list[AgentStep], hits: list[RawSearchHit]) -> AgentAction:
        self.seen.append((len(steps), len(hits)))
        self.last_goal = goal
        return self._actions.pop(0)


class MappedSearch:
    def __init__(self, by_query: dict[str, list[RawSearchHit]]) -> None:
        self._by_query = by_query
        self.queries: list[str] = []

    def search(self, keyword: str) -> list[RawSearchHit]:
        self.queries.append(keyword)
        return self._by_query.get(keyword, [])


class NeutralEnricher:
    def enrich_many(self, hits: list[RawSearchHit]) -> list[HitEnrichment]:
        return [HitEnrichment() for _ in hits]


class RecordingSink:
    def __init__(self) -> None:
        self.started: list[str] = []
        self.delivered: list[tuple[str, int]] = []
        self.failures: list[tuple[str, str]] = []
        self.results: list = []

    def mark_started(self, job_id: str) -> None:
        self.started.append(job_id)

    def deliver(self, job_id: str, results) -> None:  # type: ignore[no-untyped-def]
        self.delivered.append((job_id, len(results)))
        self.results = results

    def report_failure(self, job_id: str, error: str) -> None:
        self.failures.append((job_id, error))


class RecordingReporter:
    def __init__(self) -> None:
        self.steps: list[AgentStep] = []

    def report_step(self, job_id: str, step: AgentStep) -> None:
        self.steps.append(step)


class RecordingClarifier:
    def __init__(self) -> None:
        self.questions: list[tuple[str, str]] = []

    def request_clarification(self, job_id: str, question: str) -> None:
        self.questions.append((job_id, question))


def hit(url: str, title: str = "t") -> RawSearchHit:
    return RawSearchHit(
        title=title, url=url, snippet="s", published_at=datetime(2026, 1, 1, tzinfo=UTC)
    )


def run(job_id, goal, search, policy, sink, reporter, checkpointer=None, **kw):  # type: ignore[no-untyped-def]
    return run_agent_graph(
        job_id,
        goal,
        search,
        NeutralEnricher(),
        policy,
        sink,
        reporter,
        checkpointer or InMemorySaver(),
        **kw,
    )


# ---------------------------------------------------------------- parity


def test_searches_deduplicates_and_finishes() -> None:
    search = MappedSearch(
        {
            "rust": [hit("https://a"), hit("https://b")],
            "rust 2026": [hit("https://b"), hit("https://c")],
        }
    )
    policy = ScriptedPolicy(
        [
            SearchAction(query="rust", reason="start"),
            SearchAction(query="rust 2026", reason="refine"),
            FinishAction(reason="coverage sufficient"),
        ]
    )
    sink, reporter = RecordingSink(), RecordingReporter()

    results = run("job-1", "rust", search, policy, sink, reporter, max_steps=5)

    assert search.queries == ["rust", "rust 2026"]
    assert [r.url for r in results] == ["https://a", "https://b", "https://c"]
    assert sink.started == ["job-1"] and sink.delivered == [("job-1", 3)]
    assert [(s.seq, s.kind, s.detail, s.new_hits) for s in reporter.steps] == [
        (1, AgentStepKind.SEARCH, "rust", 2),
        (2, AgentStepKind.SEARCH, "rust 2026", 1),
        (3, AgentStepKind.FINISH, "", 0),
    ]


def test_budget_exhaustion_forces_a_finish_step() -> None:
    search = MappedSearch({"q": [hit("https://a")]})
    policy = ScriptedPolicy(
        [SearchAction(query="q", reason="1"), SearchAction(query="q", reason="2")]
    )
    sink, reporter = RecordingSink(), RecordingReporter()

    run("job-2", "goal", search, policy, sink, reporter, max_steps=2)

    assert [s.kind for s in reporter.steps] == [
        AgentStepKind.SEARCH,
        AgentStepKind.SEARCH,
        AgentStepKind.FINISH,
    ]
    assert "budget" in reporter.steps[-1].reason
    assert sink.delivered == [("job-2", 1)]


def test_critique_drops_off_topic_hits() -> None:
    search = MappedSearch({"q": [hit("https://a"), hit("https://off-topic")]})
    policy = ScriptedPolicy([SearchAction(query="q", reason="r"), FinishAction(reason="done")])
    critic = _ScriptedCritic(
        Critique(assessment="One is unrelated.", irrelevant_urls=("https://off-topic",))
    )
    sink, reporter = RecordingSink(), RecordingReporter()

    results = run("job-5", "goal", search, policy, sink, reporter, critic=critic, max_steps=5)

    assert [r.url for r in results] == ["https://a"]
    last = reporter.steps[-1]
    assert last.kind is AgentStepKind.CRITIQUE and "dropped 1 off-topic" in last.reason


def test_critique_gap_triggers_one_repair_search() -> None:
    search = MappedSearch({"q": [hit("https://a")], "q recent": [hit("https://fresh")]})
    policy = ScriptedPolicy([SearchAction(query="q", reason="r"), FinishAction(reason="done")])
    critic = _ScriptedCritic(Critique(assessment="No recent source.", gap_query="q recent"))
    sink, reporter = RecordingSink(), RecordingReporter()

    results = run("job-6", "goal", search, policy, sink, reporter, critic=critic, max_steps=5)

    assert search.queries == ["q", "q recent"]
    assert {r.url for r in results} == {"https://a", "https://fresh"}
    assert [s.kind for s in reporter.steps] == [
        AgentStepKind.SEARCH,
        AgentStepKind.FINISH,
        AgentStepKind.CRITIQUE,
        AgentStepKind.SEARCH,
    ]


# ---------------------------------------------------------------- spend cap (ADR-048)


class _BurningSearch:
    """Burns a fixed LLM cost into the meter per call, so a cost cap can be
    exercised with no paid provider — parity with the loop's BurningSearch."""

    def __init__(self, hits: list[RawSearchHit], meter: UsageMeter, tokens: int) -> None:
        self._hits = hits
        self._meter = meter
        self._tokens = tokens
        self.queries: list[str] = []

    def search(self, keyword: str) -> list[RawSearchHit]:
        self.queries.append(keyword)
        self._meter.record_llm(self._tokens, 0)
        return list(self._hits)


_BURN_PRICING = Pricing(llm_input_per_mtok=25.0, llm_output_per_mtok=0.0, search_per_call=0.0)


def test_cost_cap_forces_a_finish_step() -> None:
    meter = UsageMeter()
    guard = SpendGuard(meter, _BURN_PRICING, cap_usd=0.03)  # trips after 2 searches
    search = _BurningSearch([hit("https://a")], meter, tokens=1_000)  # $0.025 per search
    policy = ScriptedPolicy([SearchAction(query="q", reason=str(i)) for i in range(5)])
    sink, reporter = RecordingSink(), RecordingReporter()

    run("job-c1", "goal", search, policy, sink, reporter, budget=guard, max_steps=5)

    assert search.queries == ["q", "q"]  # step budget 5 untouched; money stops it
    assert [s.kind for s in reporter.steps] == [
        AgentStepKind.SEARCH,
        AgentStepKind.SEARCH,
        AgentStepKind.FINISH,
    ]
    assert "cost" in reporter.steps[-1].reason
    assert sink.delivered == [("job-c1", 1)]


def test_cost_cap_skips_the_critique_when_over_budget() -> None:
    meter = UsageMeter()
    guard = SpendGuard(meter, _BURN_PRICING, cap_usd=0.02)  # trips after 1 search
    search = _BurningSearch([hit("https://a"), hit("https://off-topic")], meter, tokens=1_000)
    policy = ScriptedPolicy(
        [SearchAction(query="q", reason="1"), SearchAction(query="q", reason="2")]
    )
    critic = _ScriptedCritic(Critique(assessment="off", irrelevant_urls=("https://off-topic",)))
    sink, reporter = RecordingSink(), RecordingReporter()

    results = run(
        "job-c2", "goal", search, policy, sink, reporter, critic=critic, budget=guard, max_steps=5
    )

    # The critique node early-returns over budget: no CRITIQUE step, nothing dropped.
    assert all(s.kind is not AgentStepKind.CRITIQUE for s in reporter.steps)
    assert {r.url for r in results} == {"https://a", "https://off-topic"}


def test_recurring_run_flags_the_delta_and_journals_a_report() -> None:
    search = MappedSearch({"q": [hit("https://old"), hit("https://fresh")]})
    policy = ScriptedPolicy([SearchAction(query="q", reason="r"), FinishAction(reason="done")])
    sink, reporter = RecordingSink(), RecordingReporter()

    results = run(
        "job-12", "goal", search, policy, sink, reporter, seen_urls={"https://old"}, max_steps=5
    )

    assert {r.url: r.is_new for r in results} == {"https://old": False, "https://fresh": True}
    report = reporter.steps[-1]
    assert report.kind is AgentStepKind.REPORT and report.new_hits == 1


def test_search_failure_reports_and_propagates() -> None:
    class ExplodingSearch:
        def search(self, keyword: str) -> list[RawSearchHit]:
            raise RuntimeError("provider down")

    policy = ScriptedPolicy([SearchAction(query="q", reason="r")])
    sink = RecordingSink()

    with pytest.raises(Exception, match="provider down"):
        run("job-4", "goal", ExplodingSearch(), policy, sink, RecordingReporter(), max_steps=5)
    assert sink.failures == [("job-4", "provider down")] or sink.failures[-1][0] == "job-4"


# ---------------------------------------------------------------- HITL (ADR-032/046)


def test_ask_pauses_the_job_without_delivering() -> None:
    search = MappedSearch({})
    policy = ScriptedPolicy([AskAction(question="Animal or car?", reason="ambiguous")])
    sink, reporter, clarifier = RecordingSink(), RecordingReporter(), RecordingClarifier()

    outcome = run(
        "job-9", "jaguar", search, policy, sink, reporter, clarifier=clarifier, max_steps=5
    )

    assert outcome is None
    assert clarifier.questions == [("job-9", "Animal or car?")]
    assert sink.delivered == [] and sink.failures == []


def test_answer_resumes_the_graph_from_its_checkpoint() -> None:
    # The heart of ADR-046: the run pauses on a question, and the user's answer
    # resumes the SAME graph — the search done before the pause is preserved,
    # not redone.
    cp = InMemorySaver()
    search = MappedSearch({"jaguar top speed": [hit("https://spec")]})
    policy = ScriptedPolicy(
        [
            AskAction(question="Animal or car?", reason="ambiguous"),
            SearchAction(query="jaguar top speed", reason="the car, per the user"),
            FinishAction(reason="done"),
        ]
    )
    sink, reporter, clarifier = RecordingSink(), RecordingReporter(), RecordingClarifier()

    paused = run(
        "job-r", "jaguar", search, policy, sink, reporter, checkpointer=cp, clarifier=clarifier
    )
    assert paused is None and clarifier.questions == [("job-r", "Animal or car?")]

    # Same job_id + same checkpointer = resume; the answer flows into the graph.
    results = run(
        "job-r",
        "jaguar",
        search,
        policy,
        sink,
        reporter,
        checkpointer=cp,
        clarifier=clarifier,
        resume_answer="the car",
    )

    assert results is not None and [r.url for r in results] == ["https://spec"]
    assert sink.delivered == [("job-r", 1)]
    # The policy was consulted post-answer with the clarification folded in.
    assert "the car" in policy.last_goal


def test_ask_after_an_answer_degrades_to_finish() -> None:
    search = MappedSearch({"q": [hit("https://a")]})
    policy = ScriptedPolicy(
        [SearchAction(query="q", reason="r"), AskAction(question="again?", reason="r")]
    )
    sink, reporter, clarifier = RecordingSink(), RecordingReporter(), RecordingClarifier()

    outcome = run(
        "job-10",
        "goal",
        search,
        policy,
        sink,
        reporter,
        clarifier=clarifier,
        clarification="the car",
        max_steps=5,
    )

    assert outcome is not None and len(outcome) == 1
    assert clarifier.questions == []
    assert reporter.steps[-1].kind is AgentStepKind.FINISH


class _ScriptedCritic:
    def __init__(self, critique: Critique) -> None:
        self._critique = critique

    def critique(self, goal: str, hits: list[RawSearchHit]) -> Critique:
        return self._critique
