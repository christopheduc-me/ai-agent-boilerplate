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


def run_agent_research(
    job_id: str,
    goal: str,
    search: SearchProvider,
    enricher: HitEnricher,
    policy: AgentPolicy,
    sink: ResultSink,
    reporter: StepReporter,
    max_steps: int = 5,
) -> list[ResearchResult]:
    """Runs the decision loop, then enriches, sorts and delivers like the
    workflow mode. Failure semantics are identical to `run_research`."""
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

        results = sort_by_publication_date([resolve_hit(hit, enricher) for hit in hits])
        sink.deliver(job_id, results)
        return results
    except Exception as exc:
        try:
            sink.report_failure(job_id, str(exc))
        except Exception:  # noqa: BLE001 - keep the original error as the cause
            pass
        raise
