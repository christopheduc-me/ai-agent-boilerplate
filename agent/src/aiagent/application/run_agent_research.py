"""The agentic research loop (ADR-030): the policy decides, the loop executes.

Unlike the fixed `run_research` workflow, the LLM-backed policy drives the
control flow here — it picks its own queries, judges coverage from the growing
transcript, and decides when to stop. The loop only enforces the mechanics:
URL deduplication, the step budget (cost guard, in the spirit of ADR-017), the
live journal, and the final enrich/sort/deliver shared with the workflow mode.
"""

import logging

from aiagent.application.run_research import resolve_hits
from aiagent.domain.models import (
    AgentAction,
    AgentStep,
    AgentStepKind,
    AskAction,
    FinishAction,
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
    budget: SpendGuard | None,
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
    over_budget = budget is not None and budget.exceeded()
    if critique.gap_query and searches_done < max_steps and not over_budget:
        found = search.search(critique.gap_query)
        kept_keys = {normalize_url(h.url) for h in kept}
        new = [h for h in found if normalize_url(h.url) not in kept_keys]
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


def _ask_guard(
    action: AgentAction,
    clarifier: ClarificationRequester | None,
    clarification: str | None,
) -> AgentAction:
    """One clarification per job (ADR-032): once answered (or without a
    clarifier wired), a repeated ask degrades to a finish — no ping-pong."""
    if isinstance(action, AskAction) and (clarifier is None or clarification is not None):
        return FinishAction(
            reason="the policy asked for clarification again; finishing with what was found"
        )
    return action


def run_agent_research(
    job_id: str,
    goal: str,
    search: SearchProvider,
    enricher: HitEnricher,
    policy: AgentPolicy,
    sink: ResultSink,
    reporter: StepReporter,
    critic: ResultCritic | None = None,
    clarifier: ClarificationRequester | None = None,
    clarification: str | None = None,
    seen_urls: set[str] | None = None,
    page_dates: PageDateFetcher | None = None,
    max_steps: int = 5,
    budget: SpendGuard | None = None,
) -> list[ResearchResult] | None:
    """Runs the decision loop, then enriches, sorts and delivers like the
    workflow mode. Failure semantics are identical to `run_research`.

    With a critic (ADR-031), the agent reviews its own results once before
    delivery: off-topic hits are dropped, and if the critique names a gap and
    the search budget is not spent, one repair search runs — never more, so
    the cost stays bounded by `max_steps` searches plus one critique call.

    With a clarifier (ADR-032), the policy may ask the user one question:
    the job pauses (returns None — nothing delivered), and the user's answer
    re-dispatches it with `clarification` set."""
    try:
        sink.mark_started(job_id)
        hits: list[RawSearchHit] = []
        collected_urls: set[str] = set()
        steps: list[AgentStep] = []

        for seq in range(1, max_steps + 1):
            # Spend cap (ADR-048): money stops the run before the step budget
            # if the indicative cost crosses AGENT_MAX_COST_USD — a clean
            # forced finish, never a crash, like the step budget below.
            if budget is not None and budget.exceeded():
                step = AgentStep(
                    seq=seq,
                    kind=AgentStepKind.FINISH,
                    detail="",
                    reason=f"cost budget of ${budget.cap_usd:.2f} exhausted",
                )
                steps.append(step)
                _report(reporter, job_id, step)
                break
            action = _ask_guard(policy.decide(goal, steps, hits), clarifier, clarification)
            if isinstance(action, AskAction):
                # Pause (ADR-032): the backend flips the job to awaiting_input;
                # the answer restarts the loop from scratch (fresh journal).
                assert clarifier is not None  # enforced by _ask_guard
                clarifier.request_clarification(job_id, action.question)
                return None
            if isinstance(action, SearchAction):
                found = search.search(action.query)
                # Deduplication by canonical URL (ADR-034): retagged links do
                # not count as new hits across searches.
                new = [h for h in found if normalize_url(h.url) not in collected_urls]
                collected_urls.update(normalize_url(h.url) for h in new)
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

        # Skip the review pass (an extra LLM call) once over budget (ADR-048).
        if critic is not None and not (budget is not None and budget.exceeded()):
            hits = _self_critique(
                job_id, goal, hits, steps, search, critic, reporter, max_steps, budget
            )

        results = sort_by_publication_date(resolve_hits(hits, enricher, page_dates))
        if seen_urls is not None:
            # Recurring run (ADR-033): flag the delta against previous runs
            # and journal the verdict — the agent says whether the run was
            # worth it before delivering.
            results = flag_new(results, seen_urls)
            new_count = sum(1 for r in results if r.is_new)
            reason = (
                f"{new_count} new result(s) since the last run"
                if new_count
                else "Nothing new since the last run"
            )
            report = AgentStep(
                seq=steps[-1].seq + 1 if steps else 1,
                kind=AgentStepKind.REPORT,
                detail="",
                reason=reason,
                new_hits=new_count,
            )
            steps.append(report)
            _report(reporter, job_id, report)
        sink.deliver(job_id, results)
        return results
    except Exception as exc:
        try:
            sink.report_failure(job_id, str(exc))
        except Exception:  # noqa: BLE001 - keep the original error as the cause
            pass
        raise
