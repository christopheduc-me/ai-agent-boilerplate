"""Model evaluation harness (ADR-045).

A directional way to answer "which model is good enough?" — the first question
anyone hits after wiring the local-LLM backend (ADR-041). It runs a small set
of golden cases against a real backend, scores each of the three LLM
capabilities (enrichment, policy, critique), and prints a comparison table
across the models you name.

This is a **directional signal, not a benchmark**: the case set is tiny and
the scoring is coarse on purpose — enough to tell a model that follows the
task from one that does not, cheaply. Forks extend the case lists below.

The scoring and runner are pure and unit-tested with fakes (ADR-012). Only the
CLI (`main`) touches real, paid providers, so — like the live tests — it is
never run in CI; you invoke it by hand.
"""

import argparse
import dataclasses
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import date

from aiagent.config import Settings
from aiagent.domain.models import (
    AgentAction,
    AgentStep,
    AgentStepKind,
    AskAction,
    Critique,
    EventType,
    HitEnrichment,
    RawSearchHit,
    SearchAction,
)
from aiagent.domain.ports import AgentPolicy, HitEnricher, ResultCritic
from aiagent.domain.usage import Pricing, UsageMeter

# ---------------------------------------------------------------- cases


@dataclass(frozen=True)
class EnrichmentCase:
    name: str
    hit: RawSearchHit
    expected_date: date | None
    acceptable_event_types: frozenset[EventType]


@dataclass(frozen=True)
class PolicyCase:
    name: str
    goal: str
    steps: list[AgentStep]
    hits: list[RawSearchHit]
    expected_kind: str  # "search" | "ask" | "finish"


@dataclass(frozen=True)
class CriticCase:
    name: str
    goal: str
    hits: list[RawSearchHit]
    # URLs a good critic should flag as off-topic (empty = it should drop none).
    expected_irrelevant: frozenset[str]


# ---------------------------------------------------------------- scoring


def score_enrichment(case: EnrichmentCase, enrichment: HitEnrichment) -> tuple[float, str]:
    """Three equally weighted checks: the date (exact, or correctly absent so
    the model does not hallucinate one), an acceptable event type, and a
    non-empty summary."""
    if case.expected_date is None:
        date_ok = enrichment.published_at is None
    else:
        date_ok = (
            enrichment.published_at is not None
            and enrichment.published_at.date() == case.expected_date
        )
    checks = {
        "date": date_ok,
        "type": enrichment.event_type in case.acceptable_event_types,
        "summary": bool(enrichment.summary and enrichment.summary.strip()),
    }
    score = sum(checks.values()) / len(checks)
    detail = " ".join(f"{k}={'ok' if v else 'X'}" for k, v in checks.items())
    return score, detail


def action_kind(action: AgentAction) -> str:
    if isinstance(action, SearchAction):
        return "search"
    if isinstance(action, AskAction):
        return "ask"
    return "finish"


def score_policy(case: PolicyCase, action: AgentAction) -> tuple[float, str]:
    """All-or-nothing on the decision kind — the loop's behavior hinges on it
    (a spurious finish ends the run, a spurious ask pauses it)."""
    got = action_kind(action)
    ok = got == case.expected_kind
    return (1.0 if ok else 0.0), f"want {case.expected_kind}, got {got}"


def score_critic(case: CriticCase, critique: Critique) -> tuple[float, str]:
    """Two equally weighted checks: a real assessment (not the neutral
    fallback), and correct handling of off-topic hits — full recall of the
    ones that should be flagged, or no false drops when none should be."""
    assessment_ok = bool(critique.assessment.strip()) and not critique.assessment.startswith(
        ("self-critique unavailable", "no assessment given")
    )
    flagged = set(critique.irrelevant_urls)
    if case.expected_irrelevant:
        noise_ok = case.expected_irrelevant <= flagged
        noise_detail = (
            f"recall={len(case.expected_irrelevant & flagged)}/{len(case.expected_irrelevant)}"
        )
    else:
        noise_ok = not flagged
        noise_detail = f"false-drops={len(flagged)}"
    score = (assessment_ok + noise_ok) / 2
    return score, f"assessment={'ok' if assessment_ok else 'X'} {noise_detail}"


# ---------------------------------------------------------------- runner


@dataclass(frozen=True)
class CaseResult:
    capability: str
    name: str
    score: float
    latency_s: float
    detail: str
    error: str | None = None


@dataclass
class Report:
    results: list[CaseResult] = field(default_factory=list)

    def capability_score(self, capability: str) -> float | None:
        scores = [r.score for r in self.results if r.capability == capability]
        return sum(scores) / len(scores) if scores else None

    def overall(self) -> float:
        caps = [
            s
            for c in ("enrichment", "policy", "critic")
            if (s := self.capability_score(c)) is not None
        ]
        return sum(caps) / len(caps) if caps else 0.0

    def total_latency(self) -> float:
        return sum(r.latency_s for r in self.results)


def _timed[T](call: Callable[[], T]) -> tuple[T | None, float, str | None]:
    """Runs `call`, returning (result, latency, error). A raised exception
    becomes an error string — a broken call scores 0, never stops the sweep."""
    start = time.perf_counter()
    try:
        return call(), time.perf_counter() - start, None
    except Exception as exc:  # noqa: BLE001 - surfacing the failure is the whole point
        return None, time.perf_counter() - start, f"{type(exc).__name__}: {exc}"


def _run_enrichment(enricher: HitEnricher, case: EnrichmentCase) -> CaseResult:
    out, latency, error = _timed(lambda: enricher.enrich_many([case.hit])[0])
    if error is not None or out is None:
        return CaseResult("enrichment", case.name, 0.0, latency, error or "no result", error)
    score, detail = score_enrichment(case, out)
    return CaseResult("enrichment", case.name, score, latency, detail)


def _run_policy(policy: AgentPolicy, case: PolicyCase) -> CaseResult:
    out, latency, error = _timed(lambda: policy.decide(case.goal, case.steps, case.hits))
    if error is not None or out is None:
        return CaseResult("policy", case.name, 0.0, latency, error or "no result", error)
    score, detail = score_policy(case, out)
    return CaseResult("policy", case.name, score, latency, detail)


def _run_critic(critic: ResultCritic, case: CriticCase) -> CaseResult:
    out, latency, error = _timed(lambda: critic.critique(case.goal, case.hits))
    if error is not None or out is None:
        return CaseResult("critic", case.name, 0.0, latency, error or "no result", error)
    score, detail = score_critic(case, out)
    return CaseResult("critic", case.name, score, latency, detail)


def evaluate(
    enricher: HitEnricher,
    policy: AgentPolicy,
    critic: ResultCritic,
    *,
    enrichment_cases: list[EnrichmentCase] | None = None,
    policy_cases: list[PolicyCase] | None = None,
    critic_cases: list[CriticCase] | None = None,
) -> Report:
    """Runs every golden case against the three adapters, timing each and
    turning any raised error into a zero-scored result — a model that crashes
    on a case is exactly what the harness is meant to surface."""
    report = Report()
    for ec in enrichment_cases if enrichment_cases is not None else ENRICHMENT_CASES:
        report.results.append(_run_enrichment(enricher, ec))
    for pc in policy_cases if policy_cases is not None else POLICY_CASES:
        report.results.append(_run_policy(policy, pc))
    for cc in critic_cases if critic_cases is not None else CRITIC_CASES:
        report.results.append(_run_critic(critic, cc))
    return report


# ---------------------------------------------------------------- golden cases

ENRICHMENT_CASES: list[EnrichmentCase] = [
    EnrichmentCase(
        name="explicit-date-release",
        hit=RawSearchHit(
            title="Rust 1.99 released with faster incremental builds",
            url="https://blog.rust-lang.org/2026/03/12/Rust-1.99.0.html",
            snippet=(
                "The Rust team published this release announcement on 12 March 2026. "
                "Rust 1.99 ships faster incremental builds and stabilizes several APIs."
            ),
        ),
        expected_date=date(2026, 3, 12),
        acceptable_event_types=frozenset({EventType.RELEASE, EventType.ANNOUNCEMENT}),
    ),
    EnrichmentCase(
        name="explicit-date-funding",
        hit=RawSearchHit(
            title="Acme raises $40M Series B to scale its database",
            url="https://example.com/acme-series-b",
            snippet=(
                "On 4 February 2026, Acme announced a $40 million Series B round "
                "led by Foobar Ventures to grow its distributed database."
            ),
        ),
        expected_date=date(2026, 2, 4),
        acceptable_event_types=frozenset({EventType.FUNDING, EventType.ANNOUNCEMENT}),
    ),
    EnrichmentCase(
        name="no-derivable-date",
        hit=RawSearchHit(
            title="Why I still reach for boring technology",
            url="https://example.com/boring-technology",
            snippet=(
                "An opinion piece arguing that mature, unexciting tools beat novel "
                "ones for most teams. No date is stated anywhere in the text."
            ),
        ),
        expected_date=None,  # a good model must not hallucinate a date
        acceptable_event_types=frozenset({EventType.OPINION, EventType.OTHER}),
    ),
]

POLICY_CASES: list[PolicyCase] = [
    PolicyCase(
        name="fresh-clear-goal-searches",
        goal="rust 1.99 release notes and community reactions",
        steps=[],
        hits=[],
        expected_kind="search",  # nothing collected yet, clear goal -> search
    ),
    PolicyCase(
        name="good-coverage-finishes",
        goal="rust 1.99 release date",
        steps=[
            AgentStep(
                seq=1,
                kind=AgentStepKind.SEARCH,
                detail="rust 1.99 release",
                reason="start",
                new_hits=6,
            ),
            AgentStep(
                seq=2,
                kind=AgentStepKind.SEARCH,
                detail="rust 1.99 release date",
                reason="refine",
                new_hits=0,
            ),
        ],
        hits=[
            RawSearchHit(
                title="Announcing Rust 1.99",
                url="https://blog.rust-lang.org/x",
                snippet="Released 12 March 2026.",
            ),
            RawSearchHit(
                title="Rust 1.99 in detail",
                url="https://example.com/y",
                snippet="A walkthrough of the release.",
            ),
        ],
        expected_kind="finish",  # coverage looks sufficient, last search added nothing
    ),
]

CRITIC_CASES: list[CriticCase] = [
    CriticCase(
        name="drops-obvious-noise",
        goal="rust 1.99 release",
        hits=[
            RawSearchHit(
                title="Announcing Rust 1.99",
                url="https://blog.rust-lang.org/rust-1-99",
                snippet="The Rust 1.99 release and its highlights.",
            ),
            RawSearchHit(
                title="The 20 best pasta recipes for winter",
                url="https://example.com/pasta-recipes",
                snippet="Comfort-food pasta dishes to cook this season.",
            ),
        ],
        expected_irrelevant=frozenset({"https://example.com/pasta-recipes"}),
    ),
    CriticCase(
        name="keeps-on-topic-results",
        goal="rust 1.99 release",
        hits=[
            RawSearchHit(
                title="Announcing Rust 1.99",
                url="https://blog.rust-lang.org/rust-1-99",
                snippet="The Rust 1.99 release and its highlights.",
            ),
            RawSearchHit(
                title="Rust 1.99 review: what the release means in practice",
                url="https://example.com/rust-1-99-review",
                snippet="A hands-on look at the Rust 1.99 release.",
            ),
        ],
        expected_irrelevant=frozenset(),  # nothing should be dropped
    ),
]


# ---------------------------------------------------------------- CLI


def _pricing_for(settings: Settings) -> Pricing:
    """Local models are free; the hosted backend uses the env cost rates."""
    if settings.llm_backend == "ollama":
        return Pricing(0.0, 0.0, 0.0)
    return Pricing(
        llm_input_per_mtok=settings.llm_cost_input_per_mtok,
        llm_output_per_mtok=settings.llm_cost_output_per_mtok,
        search_per_call=settings.search_cost_per_call,
    )


def _settings_for_spec(spec: str, base: Settings) -> Settings:
    """A model spec is `backend:model_id` — split on the first colon only, so
    Ollama tags like `gemma4:latest` survive."""
    backend, _, model_id = spec.partition(":")
    if not model_id:
        raise SystemExit(f"bad model spec {spec!r} — expected 'backend:model_id'")
    return dataclasses.replace(base, llm_backend=backend, agent_model_id=model_id)


def evaluate_spec(spec: str, base: Settings) -> tuple[Report, float]:
    """Builds the three real adapters for one model spec (sharing a meter) and
    runs the sweep. Returns the report and the run's indicative USD cost."""
    from aiagent.adapters.chat_model import make_chat_model
    from aiagent.adapters.llm import LlmAgentPolicy, LlmHitEnricher, LlmResultCritic

    settings = _settings_for_spec(spec, base)
    meter = UsageMeter()
    model, system = settings.agent_model_id, settings.llm_backend
    enricher = LlmHitEnricher(
        make_chat_model(settings, max_tokens=256), meter=meter, model=model, system=system
    )
    policy = LlmAgentPolicy(
        make_chat_model(settings, max_tokens=256), meter=meter, model=model, system=system
    )
    critic = LlmResultCritic(
        make_chat_model(settings, max_tokens=512), meter=meter, model=model, system=system
    )
    report = evaluate(enricher, policy, critic)
    cost = meter.snapshot().cost_usd(_pricing_for(settings))
    return report, cost


def _fmt_pct(value: float | None) -> str:
    return "  -  " if value is None else f"{value * 100:4.0f}%"


def format_table(rows: list[tuple[str, Report, float]]) -> str:
    """A plain aligned comparison table — no dependency, copy-pasteable."""
    header = (
        f"{'MODEL':<28} {'enrich':>7} {'policy':>7} {'critic':>7} "
        f"{'overall':>8} {'lat/s':>7} {'cost$':>8}"
    )
    lines = [header, "-" * len(header)]
    for label, report, cost in rows:
        lines.append(
            f"{label:<28} "
            f"{_fmt_pct(report.capability_score('enrichment')):>7} "
            f"{_fmt_pct(report.capability_score('policy')):>7} "
            f"{_fmt_pct(report.capability_score('critic')):>7} "
            f"{_fmt_pct(report.overall()):>8} "
            f"{report.total_latency():>7.1f} "
            f"{cost:>8.4f}"
        )
    return "\n".join(lines)


def failures_below(rows: list[tuple[str, Report, float]], threshold: float) -> list[str]:
    """Model specs whose overall score is under `threshold` (0..1), each with a
    human-readable reason (the per-capability breakdown, so a single collapsed
    capability is visible). An empty list means every model cleared the bar —
    this is what the `--fail-under` pre-release gate keys its exit code on."""
    messages: list[str] = []
    for label, report, _cost in rows:
        overall = report.overall()
        if overall < threshold:
            caps = " ".join(
                f"{c}={_fmt_pct(report.capability_score(c)).strip()}"
                for c in ("enrichment", "policy", "critic")
            )
            messages.append(
                f"{label}: overall {overall * 100:.0f}% < {threshold * 100:.0f}% ({caps})"
            )
    return messages


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(
        prog="python -m aiagent.evaluation",
        description="Score LLM models on the agent's three capabilities (ADR-045).",
    )
    parser.add_argument(
        "models",
        nargs="*",
        help="model specs 'backend:model_id' (e.g. ollama:gemma4:latest "
        "anthropic:claude-opus-4-8); defaults to the env-configured backend",
    )
    parser.add_argument(
        "--verbose", "-v", action="store_true", help="print every case's score and detail"
    )
    parser.add_argument(
        "--fail-under",
        type=float,
        default=None,
        metavar="PCT",
        help="exit non-zero if any model's overall score is below PCT (0..1) — the "
        "pre-release quality gate; run it by hand with a real backend before shipping "
        "a prompt or model change (deliberately not in CI: no API keys there, ADR-045)",
    )
    args = parser.parse_args(argv)
    if args.fail_under is not None and not 0.0 <= args.fail_under <= 1.0:
        raise SystemExit(f"--fail-under expects a fraction in 0..1, got {args.fail_under}")

    base = Settings.from_env()
    specs = args.models or [f"{base.llm_backend}:{base.agent_model_id}"]

    rows: list[tuple[str, Report, float]] = []
    for spec in specs:
        print(f"evaluating {spec} ...", flush=True)
        report, cost = evaluate_spec(spec, base)
        rows.append((spec, report, cost))
        if args.verbose:
            for r in report.results:
                print(f"  [{r.capability:<10}] {r.name:<28} {r.score * 100:3.0f}%  {r.detail}")

    print()
    print(format_table(rows))

    if args.fail_under is not None:
        failures = failures_below(rows, args.fail_under)
        if failures:
            print()
            for message in failures:
                print(f"FAIL {message}")
            raise SystemExit(1)


if __name__ == "__main__":
    main()
