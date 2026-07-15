"""The agentic research loop (ADR-030): the policy decides, the loop executes.

Unlike the fixed `run_research` workflow, the LLM-backed policy drives the
control flow here — it picks its own queries, judges coverage from the growing
transcript, and decides when to stop. The loop only enforces the mechanics:
URL deduplication, the step budget (cost guard, in the spirit of ADR-017), the
live journal, and the final enrich/sort/deliver shared with the workflow mode.
"""

import logging

from aiagent.application.run_research import resolve_hit
from aiagent.domain.models import (
    AgentStep,
    AgentStepKind,
    RawSearchHit,
    ResearchResult,
    SearchAction,
    sort_by_publication_date,
)
from aiagent.domain.ports import (
    AgentPolicy,
    HitEnricher,
    ResultCritic,
    ResultSink,
    SearchProvider,
    StepReporter,
)

logger = logging.getLogger(__name__)


def _report(reporter: StepReporter, job_id: str, step: AgentStep) -> None:
    """The journal is cosmetic: losing a step must never fail the job."""
    try:
        reporter.report_step(job_id, step)
    except Exception:  # noqa: BLE001 - best effort by contract
        logger.warning("failed to report agent step", extra={"job_id": job_id}, exc_info=True)


def _self_critique(
    job_id: str,
    goal: str,
    hits: list[RawSearchHit],
    steps: list[AgentStep],
    search: SearchProvider,
    critic: ResultCritic,
    reporter: StepReporter,
    max_steps: int,
) -> list[RawSearchHit]:
    """The review pass (ADR-031): drop what the critic judged off-topic,
    journal the verdict, and fill at most one named gap if budget remains."""
    critique = critic.critique(goal, hits)
    kept = [h for h in hits if h.url not in critique.irrelevant_urls]
    dropped = len(hits) - len(kept)
    reason = critique.assessment
    if dropped:
        plural = "s" if dropped > 1 else ""
        reason = f"{reason} (dropped {dropped} off-topic result{plural})"
    step = AgentStep(
        seq=steps[-1].seq + 1 if steps else 1,
        kind=AgentStepKind.CRITIQUE,
        detail=critique.gap_query or "",
        reason=reason,
        new_hits=0,
    )
    steps.append(step)
    _report(reporter, job_id, step)

    searches_done = sum(1 for s in steps if s.kind is AgentStepKind.SEARCH)
    if critique.gap_query and searches_done < max_steps:
        found = search.search(critique.gap_query)
        seen_urls = {h.url for h in kept}
        new = [h for h in found if h.url not in seen_urls]
        kept.extend(new)
        repair = AgentStep(
            seq=step.seq + 1,
            kind=AgentStepKind.SEARCH,
            detail=critique.gap_query,
            reason="Repair pass: filling the gap named by the self-critique",
            new_hits=len(new),
        )
        steps.append(repair)
        _report(reporter, job_id, repair)
    return kept


def run_agent_research(
    job_id: str,
    goal: str,
    search: SearchProvider,
    enricher: HitEnricher,
    policy: AgentPolicy,
    sink: ResultSink,
    reporter: StepReporter,
    critic: ResultCritic | None = None,
    max_steps: int = 5,
) -> list[ResearchResult]:
    """Runs the decision loop, then enriches, sorts and delivers like the
    workflow mode. Failure semantics are identical to `run_research`.

    With a critic (ADR-031), the agent reviews its own results once before
    delivery: off-topic hits are dropped, and if the critique names a gap and
    the search budget is not spent, one repair search runs — never more, so
    the cost stays bounded by `max_steps` searches plus one critique call."""
    try:
        sink.mark_started(job_id)
        hits: list[RawSearchHit] = []
        seen_urls: set[str] = set()
        steps: list[AgentStep] = []

        for seq in range(1, max_steps + 1):
            action = policy.decide(goal, steps, hits)
            if isinstance(action, SearchAction):
                found = search.search(action.query)
                new = [h for h in found if h.url not in seen_urls]
                seen_urls.update(h.url for h in new)
                hits.extend(new)
                step = AgentStep(
                    seq=seq,
                    kind=AgentStepKind.SEARCH,
                    detail=action.query,
                    reason=action.reason,
                    new_hits=len(new),
                )
            else:
                step = AgentStep(
                    seq=seq, kind=AgentStepKind.FINISH, detail="", reason=action.reason
                )
            steps.append(step)
            _report(reporter, job_id, step)
            if step.kind is AgentStepKind.FINISH:
                break
        else:
            # The policy never said stop: the budget does (cost guard).
            step = AgentStep(
                seq=max_steps + 1,
                kind=AgentStepKind.FINISH,
                detail="",
                reason=f"step budget of {max_steps} exhausted",
            )
            steps.append(step)
            _report(reporter, job_id, step)

        if critic is not None:
            hits = _self_critique(job_id, goal, hits, steps, search, critic, reporter, max_steps)

        results = sort_by_publication_date([resolve_hit(hit, enricher) for hit in hits])
        sink.deliver(job_id, results)
        return results
    except Exception as exc:
        try:
            sink.report_failure(job_id, str(exc))
        except Exception:  # noqa: BLE001 - keep the original error as the cause
            pass
        raise
