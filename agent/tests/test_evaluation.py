"""Model evaluation harness (ADR-045): the scoring and runner are pure and
tested here with fakes — no paid call. The CLI (`main`) touches real providers
and is exercised by hand, like the live tests."""

from datetime import UTC, date, datetime

import pytest

from aiagent.config import Settings
from aiagent.domain.models import (
    Critique,
    EventType,
    FinishAction,
    HitEnrichment,
    RawSearchHit,
    SearchAction,
)
from aiagent.evaluation import (
    CriticCase,
    EnrichmentCase,
    PolicyCase,
    Report,
    _settings_for_spec,
    evaluate,
    format_table,
    score_critic,
    score_enrichment,
    score_policy,
)


def a_hit(url: str = "https://x") -> RawSearchHit:
    return RawSearchHit(title="T", url=url, snippet="s")


# ---------------------------------------------------------------- scoring


def test_score_enrichment_perfect_is_one() -> None:
    case = EnrichmentCase("c", a_hit(), date(2026, 3, 12), frozenset({EventType.RELEASE}))
    got = HitEnrichment(
        published_at=datetime(2026, 3, 12, tzinfo=UTC),
        event_type=EventType.RELEASE,
        summary="A release.",
    )
    score, _ = score_enrichment(case, got)
    assert score == 1.0


def test_score_enrichment_penalizes_a_hallucinated_date() -> None:
    # Expected no date, but the model invented one -> only 2/3 checks pass.
    case = EnrichmentCase("c", a_hit(), None, frozenset({EventType.OPINION}))
    got = HitEnrichment(
        published_at=datetime(2020, 1, 1, tzinfo=UTC),
        event_type=EventType.OPINION,
        summary="S.",
    )
    score, detail = score_enrichment(case, got)
    assert score == pytest.approx(2 / 3)
    assert "date=X" in detail


def test_score_enrichment_penalizes_wrong_type_and_missing_summary() -> None:
    case = EnrichmentCase("c", a_hit(), None, frozenset({EventType.OPINION}))
    got = HitEnrichment(published_at=None, event_type=EventType.OTHER, summary="  ")
    score, _ = score_enrichment(case, got)
    assert score == pytest.approx(1 / 3)  # only the date check passes


def test_score_policy_is_all_or_nothing() -> None:
    case = PolicyCase("c", "goal", [], [], expected_kind="search")
    assert score_policy(case, SearchAction(query="q", reason="r"))[0] == 1.0
    assert score_policy(case, FinishAction(reason="r"))[0] == 0.0


def test_score_critic_rewards_assessment_and_recall() -> None:
    case = CriticCase("c", "goal", [], frozenset({"https://noise"}))
    good = Critique(assessment="Solid coverage.", irrelevant_urls=("https://noise",))
    assert score_critic(case, good)[0] == 1.0


def test_score_critic_penalizes_missed_noise_and_fallback_assessment() -> None:
    case = CriticCase("c", "goal", [], frozenset({"https://noise"}))
    # Missed the noise but gave an assessment -> 0.5.
    half = Critique(assessment="Looks fine.", irrelevant_urls=())
    assert score_critic(case, half)[0] == 0.5
    # Neutral fallback assessment does not count as a real one.
    fallback = Critique(
        assessment="self-critique unavailable (reply was not valid JSON)",
        irrelevant_urls=("https://noise",),
    )
    # Recall is perfect but the assessment is the neutral fallback -> 0.5.
    assert score_critic(case, fallback)[0] == 0.5


def test_score_critic_penalizes_false_drops() -> None:
    case = CriticCase("c", "goal", [], frozenset())  # nothing should be dropped
    over = Critique(assessment="Good.", irrelevant_urls=("https://on-topic",))
    assert score_critic(case, over)[0] == 0.5


# ---------------------------------------------------------------- report


def test_report_aggregates_by_capability_and_overall() -> None:
    report = Report()
    from aiagent.evaluation import CaseResult

    report.results = [
        CaseResult("enrichment", "a", 1.0, 0.1, ""),
        CaseResult("enrichment", "b", 0.0, 0.2, ""),
        CaseResult("policy", "c", 1.0, 0.3, ""),
    ]
    assert report.capability_score("enrichment") == 0.5
    assert report.capability_score("policy") == 1.0
    assert report.capability_score("critic") is None
    # Overall is the mean of the capabilities that ran (0.5, 1.0) = 0.75.
    assert report.overall() == 0.75
    assert report.total_latency() == pytest.approx(0.6)


# ---------------------------------------------------------------- runner


class FakeEnricher:
    def __init__(self, enrichment: HitEnrichment) -> None:
        self._e = enrichment

    def enrich_many(self, hits: list[RawSearchHit]) -> list[HitEnrichment]:
        return [self._e for _ in hits]


class RaisingEnricher:
    def enrich_many(self, hits: list[RawSearchHit]) -> list[HitEnrichment]:
        raise RuntimeError("model exploded")


class FakePolicy:
    def decide(self, goal, steps, hits):  # noqa: ANN001, ANN201
        return SearchAction(query="q", reason="r")


class FakeCritic:
    def critique(self, goal, hits):  # noqa: ANN001, ANN201
        return Critique(assessment="Fine.", irrelevant_urls=())


def test_evaluate_runs_all_capabilities() -> None:
    enricher = FakeEnricher(HitEnrichment(event_type=EventType.RELEASE, summary="S."))
    report = evaluate(
        enricher,  # type: ignore[arg-type]
        FakePolicy(),  # type: ignore[arg-type]
        FakeCritic(),  # type: ignore[arg-type]
    )
    caps = {r.capability for r in report.results}
    assert caps == {"enrichment", "policy", "critic"}
    assert all(r.error is None for r in report.results)


def test_evaluate_turns_a_raised_error_into_a_zero_scored_result() -> None:
    report = evaluate(
        RaisingEnricher(),  # type: ignore[arg-type]
        FakePolicy(),  # type: ignore[arg-type]
        FakeCritic(),  # type: ignore[arg-type]
        enrichment_cases=[
            EnrichmentCase("boom", a_hit(), None, frozenset({EventType.OTHER})),
        ],
    )
    enrichment_results = [r for r in report.results if r.capability == "enrichment"]
    assert len(enrichment_results) == 1
    assert enrichment_results[0].score == 0.0
    assert "model exploded" in (enrichment_results[0].error or "")


# ---------------------------------------------------------------- CLI helpers


def _base_settings() -> Settings:
    return Settings(
        redis_url="r",
        backend_internal_url="b",
        internal_api_token="t",
        agent_model_id="claude-opus-4-8",
        providers="live",
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


def test_settings_for_spec_splits_on_the_first_colon() -> None:
    # Ollama tags contain a colon — the model id must keep it.
    s = _settings_for_spec("ollama:gemma4:latest", _base_settings())
    assert s.llm_backend == "ollama"
    assert s.agent_model_id == "gemma4:latest"


def test_settings_for_spec_rejects_a_spec_without_a_model() -> None:
    with pytest.raises(SystemExit):
        _settings_for_spec("ollama", _base_settings())


def test_format_table_lists_every_model_and_the_headers() -> None:
    from aiagent.evaluation import CaseResult

    report = Report(results=[CaseResult("enrichment", "a", 1.0, 0.5, "")])
    table = format_table([("ollama:gemma4:latest", report, 0.0)])
    assert "MODEL" in table and "overall" in table
    assert "ollama:gemma4:latest" in table


# ---------------------------------------------------------------- gate


def _report_scoring(overall_pairs: list[tuple[str, float]]) -> Report:
    from aiagent.evaluation import CaseResult

    return Report(results=[CaseResult(cap, cap, score, 0.0, "") for cap, score in overall_pairs])


def test_failures_below_returns_models_under_the_bar() -> None:
    from aiagent.evaluation import failures_below

    passing = ("anthropic:good", _report_scoring([("enrichment", 1.0), ("policy", 1.0)]), 0.0)
    failing = ("ollama:weak", _report_scoring([("enrichment", 0.4), ("policy", 0.6)]), 0.0)
    msgs = failures_below([passing, failing], 0.8)
    assert len(msgs) == 1
    assert "ollama:weak" in msgs[0]
    assert "50%" in msgs[0]  # overall (0.4 + 0.6) / 2


def test_failures_below_is_empty_when_every_model_clears_the_bar() -> None:
    from aiagent.evaluation import failures_below

    rows = [("anthropic:good", _report_scoring([("enrichment", 0.9), ("policy", 1.0)]), 0.0)]
    assert failures_below(rows, 0.8) == []
