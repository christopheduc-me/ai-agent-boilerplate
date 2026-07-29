"""LangGraph orchestration of the agent mode (ADR-046).

The default orchestrator for `mode=agent`: the same decision loop as the
hand-rolled `run_agent_research` (ADR-030/031/032/033), expressed as a
LangGraph `StateGraph`. It is an **adapter** — it depends only on the domain
ports (`AgentPolicy`, `SearchProvider`, `ResultCritic`, `StepReporter`,
`ClarificationRequester`, `ResultSink`), so the domain stays framework-free
and the loop remains available behind `AGENT_ORCHESTRATOR=loop`.

Two things the graph buys over the plain loop:
- **Durable checkpointing** (Redis, keyed by `job_id`): the graph state is
  persisted at every super-step, so a resumed run continues instead of redoing
  the work.
- **Native HITL** via `interrupt()`: the clarification pause is a first-class
  graph primitive; the user's answer resumes the graph mid-flight (ADR-032)
  rather than re-dispatching a fresh run.

The graph state holds only JSON-friendly primitives (hits/steps as dicts) so
checkpoint serialization never depends on pickling domain dataclasses — the
nodes convert to/from domain types at the port boundary.
"""

import logging
from datetime import datetime
from typing import TYPE_CHECKING, Any, TypedDict

from langgraph.graph import END, START, StateGraph
from langgraph.types import Command, interrupt

from aiagent.application.run_research import resolve_hits
from aiagent.domain.models import (
    AgentAction,
    AgentStep,
    AgentStepKind,
    AskAction,
    RawSearchHit,
    ResearchResult,
    SearchAction,
    flag_new,
    sort_by_publication_date,
)
from aiagent.domain.ports import (
    AgentPolicy,
    ClarificationRequester,
    HitEnricher,
    PageDateFetcher,
    ResultCritic,
    ResultSink,
    SearchProvider,
    StepReporter,
)
from aiagent.domain.urls import normalize_url
from aiagent.domain.usage import SpendGuard

if TYPE_CHECKING:
    from langgraph.checkpoint.base import BaseCheckpointSaver

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------- state + serialization


class GraphState(TypedDict):
    """Checkpointed state — JSON-friendly only (hits/steps as dicts)."""

    goal: str
    clarification: str | None
    asked: bool
    hits: list[dict[str, Any]]
    collected: list[str]  # canonical URLs already seen
    steps: list[dict[str, Any]]
    next: dict[str, Any]  # the decided action: {"kind", "query"/"question", "reason"}


def _hit_to_dict(hit: RawSearchHit) -> dict[str, Any]:
    return {
        "title": hit.title,
        "url": hit.url,
        "snippet": hit.snippet,
        "published_at": hit.published_at.isoformat() if hit.published_at else None,
        "raw": hit.raw,
    }


def _hit_from_dict(data: dict[str, Any]) -> RawSearchHit:
    published = data.get("published_at")
    return RawSearchHit(
        title=data["title"],
        url=data["url"],
        snippet=data["snippet"],
        published_at=datetime.fromisoformat(published) if published else None,
        raw=data.get("raw") or {},
    )


def _step_to_dict(step: AgentStep) -> dict[str, Any]:
    return {
        "seq": step.seq,
        "kind": step.kind.value,
        "detail": step.detail,
        "reason": step.reason,
        "new_hits": step.new_hits,
    }


def _step_from_dict(data: dict[str, Any]) -> AgentStep:
    return AgentStep(
        seq=data["seq"],
        kind=AgentStepKind(data["kind"]),
        detail=data["detail"],
        reason=data["reason"],
        new_hits=data["new_hits"],
    )


def _searches_done(steps: list[dict[str, Any]]) -> int:
    return sum(1 for s in steps if s["kind"] == AgentStepKind.SEARCH.value)


# ---------------------------------------------------------------- graph construction


def _build_graph(
    job_id: str,
    search: SearchProvider,
    policy: AgentPolicy,
    reporter: StepReporter,
    critic: ResultCritic | None,
    has_clarifier: bool,
    max_steps: int,
    budget: SpendGuard | None,
) -> "StateGraph[GraphState]":
    """Builds the StateGraph; nodes close over the injected ports. The graph is
    rebuilt per task (fresh ports), while the checkpointer restores the state —
    exactly the split we want across a HITL pause spanning two Celery tasks."""

    def _report(step: AgentStep) -> None:
        """The journal is cosmetic (ADR-030): a failed report never fails the job."""
        try:
            reporter.report_step(job_id, step)
        except Exception:  # noqa: BLE001 - best effort by contract
            logger.warning("failed to report agent step", extra={"job_id": job_id}, exc_info=True)

    def _goal_with_clarification(state: GraphState) -> str:
        if state["clarification"]:
            return f'{state["goal"]} (user clarification: "{state["clarification"]}")'
        return state["goal"]

    def decide(state: GraphState) -> dict[str, Any]:
        # Step budget (ADR-030): once the search budget is spent, finish
        # without asking the policy again.
        if _searches_done(state["steps"]) >= max_steps:
            return {"next": {"kind": "finish", "reason": f"step budget of {max_steps} exhausted"}}
        # Spend cap (ADR-048): money can stop the run before the step budget.
        if budget is not None and budget.exceeded():
            return {
                "next": {
                    "kind": "finish",
                    "reason": f"cost budget of ${budget.cap_usd:.2f} exhausted",
                }
            }
        action = policy.decide(
            _goal_with_clarification(state),
            [_step_from_dict(s) for s in state["steps"]],
            [_hit_from_dict(h) for h in state["hits"]],
        )
        action = _apply_ask_guard(action, state, has_clarifier)
        return {"next": _action_to_dict(action)}

    def route(state: GraphState) -> str:
        return str(state["next"]["kind"])

    def do_search(state: GraphState) -> dict[str, Any]:
        query, reason = state["next"]["query"], state["next"]["reason"]
        found = search.search(query)
        seen = set(state["collected"])
        new = [h for h in found if normalize_url(h.url) not in seen]
        step = AgentStep(
            seq=len(state["steps"]) + 1,
            kind=AgentStepKind.SEARCH,
            detail=query,
            reason=reason,
            new_hits=len(new),
        )
        _report(step)
        return {
            "hits": state["hits"] + [_hit_to_dict(h) for h in new],
            "collected": state["collected"] + [normalize_url(h.url) for h in new],
            "steps": state["steps"] + [_step_to_dict(step)],
        }

    def do_ask(state: GraphState) -> dict[str, Any]:
        # HITL (ADR-032): pause the graph until the user answers. On resume the
        # answer flows back here; the backend callback is issued by the caller
        # when it sees the interrupt (so it fires once, not again on resume).
        answer = interrupt(state["next"]["question"])
        return {"clarification": answer, "asked": True}

    def finalize(state: GraphState) -> dict[str, Any]:
        step = AgentStep(
            seq=len(state["steps"]) + 1,
            kind=AgentStepKind.FINISH,
            detail="",
            reason=state["next"]["reason"],
        )
        _report(step)
        return {"steps": state["steps"] + [_step_to_dict(step)]}

    def critique(state: GraphState) -> dict[str, Any]:
        assert critic is not None  # only wired when a critic exists
        # Skip the review pass (an extra LLM call) once over budget (ADR-048).
        if budget is not None and budget.exceeded():
            return {}
        hits = [_hit_from_dict(h) for h in state["hits"]]
        verdict = critic.critique(_goal_with_clarification(state), hits)
        kept = [h for h in hits if h.url not in verdict.irrelevant_urls]
        dropped = len(hits) - len(kept)
        reason = verdict.assessment
        if dropped:
            reason = f"{reason} (dropped {dropped} off-topic result{'s' if dropped > 1 else ''})"
        step = AgentStep(
            seq=len(state["steps"]) + 1,
            kind=AgentStepKind.CRITIQUE,
            detail=verdict.gap_query or "",
            reason=reason,
            new_hits=0,
        )
        _report(step)
        steps = state["steps"] + [_step_to_dict(step)]

        # One budget-bounded repair search for the named gap (ADR-031) — bounded
        # by both the step budget and the spend cap (ADR-048).
        over_budget = budget is not None and budget.exceeded()
        if verdict.gap_query and _searches_done(steps) < max_steps and not over_budget:
            found = search.search(verdict.gap_query)
            kept_keys = {normalize_url(h.url) for h in kept}
            new = [h for h in found if normalize_url(h.url) not in kept_keys]
            kept.extend(new)
            repair = AgentStep(
                seq=len(steps) + 1,
                kind=AgentStepKind.SEARCH,
                detail=verdict.gap_query,
                reason="Repair pass: filling the gap named by the self-critique",
                new_hits=len(new),
            )
            _report(repair)
            steps = steps + [_step_to_dict(repair)]

        return {"hits": [_hit_to_dict(h) for h in kept], "steps": steps}

    graph: StateGraph[GraphState] = StateGraph(GraphState)
    graph.add_node("decide", decide)
    graph.add_node("search", do_search)
    graph.add_node("ask", do_ask)
    graph.add_node("finalize", finalize)
    graph.add_edge(START, "decide")
    graph.add_conditional_edges(
        "decide", route, {"search": "search", "ask": "ask", "finish": "finalize"}
    )
    graph.add_edge("search", "decide")
    graph.add_edge("ask", "decide")
    if critic is not None:
        graph.add_node("critique", critique)
        graph.add_edge("finalize", "critique")
        graph.add_edge("critique", END)
    else:
        graph.add_edge("finalize", END)
    return graph


def _action_to_dict(action: AgentAction) -> dict[str, Any]:
    if isinstance(action, SearchAction):
        return {"kind": "search", "query": action.query, "reason": action.reason}
    if isinstance(action, AskAction):
        return {"kind": "ask", "question": action.question, "reason": action.reason}
    return {"kind": "finish", "reason": action.reason}


def _apply_ask_guard(action: AgentAction, state: GraphState, has_clarifier: bool) -> AgentAction:
    """One clarification per job (ADR-032): a repeat ask — or an ask with no
    clarifier wired — degrades to a finish, so the loop never ping-pongs."""
    if isinstance(action, AskAction) and (
        not has_clarifier or state["asked"] or state["clarification"] is not None
    ):
        from aiagent.domain.models import FinishAction

        return FinishAction(
            reason="the policy asked for clarification again; finishing with what was found"
        )
    return action


# ---------------------------------------------------------------- entry point


def run_agent_graph(
    job_id: str,
    goal: str,
    search: SearchProvider,
    enricher: HitEnricher,
    policy: AgentPolicy,
    sink: ResultSink,
    reporter: StepReporter,
    checkpointer: "BaseCheckpointSaver[Any]",
    critic: ResultCritic | None = None,
    clarifier: ClarificationRequester | None = None,
    clarification: str | None = None,
    seen_urls: set[str] | None = None,
    page_dates: PageDateFetcher | None = None,
    max_steps: int = 5,
    resume_answer: str | None = None,
    budget: SpendGuard | None = None,
) -> list[ResearchResult] | None:
    """Runs the agent mode on a LangGraph StateGraph (ADR-046), then enriches,
    sorts and delivers like the workflow mode — same contract as
    `run_agent_research`. Returns the results, or None when the graph paused on
    a clarification (ADR-032).

    `resume_answer` set means the user answered a pending clarification: the
    graph resumes from its Redis checkpoint instead of starting fresh.
    """
    config: dict[str, Any] = {"configurable": {"thread_id": job_id}}
    # The compiled Pregel graph's invoke/get_state have intricate overloads;
    # typed Any here since this adapter drives it as plain glue.
    compiled: Any = _build_graph(
        job_id, search, policy, reporter, critic, clarifier is not None, max_steps, budget
    ).compile(checkpointer=checkpointer)

    try:
        sink.mark_started(job_id)

        resuming = resume_answer is not None and _has_checkpoint(compiled, config)
        if resuming:
            outcome = compiled.invoke(Command(resume=resume_answer), config)
        else:
            # Fresh run. An already-known clarification (the loop's model, or a
            # resume with no checkpoint to restore) starts the run informed:
            # it is folded into the goal and marks the question as spent.
            informed = clarification if clarification is not None else resume_answer
            outcome = compiled.invoke(
                {
                    "goal": goal,
                    "clarification": informed,
                    "asked": informed is not None,
                    "hits": [],
                    "collected": [],
                    "steps": [],
                    "next": {},
                },
                config,
            )

        # An interrupt (ADR-032 pause) surfaces in the invoke return itself —
        # the reliable in-process signal. Reading it back from get_state()
        # instead raced against the Redis checkpoint write and could miss the
        # pause, delivering the (empty) partial state as a completed job.
        question = _interrupt_question(outcome)
        if question is not None:
            assert clarifier is not None
            clarifier.request_clarification(job_id, question)
            return None

        return _deliver(job_id, outcome, enricher, sink, seen_urls, page_dates, reporter)
    except Exception as exc:
        try:
            sink.report_failure(job_id, str(exc))
        except Exception:  # noqa: BLE001 - keep the original error as the cause
            pass
        raise


def _has_checkpoint(compiled: Any, config: dict[str, Any]) -> bool:
    # Resume path only: read a checkpoint written by an earlier, fully finished
    # task (the paused run) — safely flushed, unlike a same-invoke read-back.
    return compiled.get_state(config).created_at is not None


def _interrupt_question(outcome: Any) -> str | None:
    """The pending clarification question if the graph paused, from the invoke
    return value (`__interrupt__`), else None."""
    if isinstance(outcome, dict):
        interrupts = outcome.get("__interrupt__")
        if interrupts:
            return str(interrupts[0].value)
    return None


def _deliver(
    job_id: str,
    state: dict[str, Any],
    enricher: HitEnricher,
    sink: ResultSink,
    seen_urls: set[str] | None,
    page_dates: PageDateFetcher | None,
    reporter: StepReporter,
) -> list[ResearchResult]:
    """Shared tail with the loop: enrich, sort, flag the recurring delta, and
    deliver (ADR-011/027/033)."""
    hits = [_hit_from_dict(h) for h in state["hits"]]
    results = sort_by_publication_date(resolve_hits(hits, enricher, page_dates))
    if seen_urls is not None:
        results = flag_new(results, seen_urls)
        new_count = sum(1 for r in results if r.is_new)
        reason = (
            f"{new_count} new result(s) since the last run"
            if new_count
            else "Nothing new since the last run"
        )
        report = AgentStep(
            seq=len(state["steps"]) + 1,
            kind=AgentStepKind.REPORT,
            detail="",
            reason=reason,
            new_hits=new_count,
        )
        try:
            reporter.report_step(job_id, report)
        except Exception:  # noqa: BLE001 - best effort by contract
            logger.warning("failed to report delta step", extra={"job_id": job_id}, exc_info=True)
    sink.deliver(job_id, results)
    return results
