"""Deterministic fake providers (ADR-021): `AGENT_PROVIDERS=fake`.

No network, no API key. Used by the e2e smoke test in CI (the paid-service ban
of ADR-012 applies to CI end to end) and for keyless local development. The
hits exercise the whole date cascade (ADR-011/035): provider date (high),
page-declared date (high, ADR-035), LLM-extracted date (medium), unknown.
"""

from datetime import UTC, datetime

from aiagent.domain.models import (
    AgentAction,
    AgentStep,
    AskAction,
    Critique,
    EventType,
    FinishAction,
    HitEnrichment,
    RawSearchHit,
    SearchAction,
)
from aiagent.domain.usage import UsageMeter


class FakeSearchProvider:
    def __init__(self, meter: UsageMeter | None = None) -> None:
        self._meter = meter

    def search(self, keyword: str) -> list[RawSearchHit]:
        # ADR-038: fakes count their calls with zero cost — the keyless demo
        # shows honest call counts and a $0 total.
        if self._meter is not None:
            self._meter.record_search()
        raw = {"provider": "fake", "keyword": keyword}
        return [
            RawSearchHit(
                title="fake-dated-old",
                url="https://example.com/old",
                snippet=f"Old article about {keyword}",
                published_at=datetime(2023, 1, 15, tzinfo=UTC),
                raw=raw,
            ),
            RawSearchHit(
                title="fake-dated-recent",
                url="https://example.com/recent",
                snippet=f"Recent article about {keyword}",
                published_at=datetime(2026, 5, 1, tzinfo=UTC),
                raw=raw,
            ),
            RawSearchHit(
                title="fake-page-datable",
                url="https://example.com/page",
                snippet=f"Article about {keyword} whose page declares its date (ADR-035)",
                published_at=None,
                raw=raw,
            ),
            RawSearchHit(
                title="fake-llm-datable",
                url="https://example.com/llm",
                snippet=f"Article about {keyword} whose date only the LLM can find",
                published_at=None,
                raw=raw,
            ),
            RawSearchHit(
                title="fake-undatable",
                url="https://example.com/undatable",
                snippet=f"Undatable page about {keyword}",
                published_at=None,
                raw=raw,
            ),
        ]


class _FakeLlm:
    """Shared meter plumbing for the fake LLM-backed adapters (ADR-038)."""

    def __init__(self, meter: UsageMeter | None = None) -> None:
        self._meter = meter

    def _count(self) -> None:
        if self._meter is not None:
            self._meter.record_llm(0, 0)


class FakePageDateFetcher:
    """Deterministic stage 2 (ADR-035): only the hit designed for it has a
    page-declared date — ranked high, above the LLM's medium."""

    def fetch_published_date(self, url: str) -> datetime | None:
        if url == "https://example.com/page":
            return datetime(2025, 12, 1, tzinfo=UTC)
        return None


class FakeHitEnricher(_FakeLlm):
    """Deterministic enrichment: a date only for the hit designed to exercise
    the LLM stage of the cascade, a stable event type and summary for all."""

    def enrich_many(self, hits: list[RawSearchHit]) -> list[HitEnrichment]:
        return [self.enrich(hit) for hit in hits]

    def enrich(self, hit: RawSearchHit) -> HitEnrichment:
        self._count()
        published_at = None
        if hit.title == "fake-llm-datable":
            published_at = datetime(2025, 8, 20, tzinfo=UTC)
        return HitEnrichment(
            published_at=published_at,
            event_type=EventType.ANNOUNCEMENT,
            summary=f"Fake summary for {hit.title}",
        )


class FakeAgentPolicy(_FakeLlm):
    """Deterministic policy (ADR-030) for keyless demos and e2e: search the
    goal, refine once (the fake provider returns the same hits, so the journal
    shows the deduplication at work), then stop with an explicit reason."""

    def decide(self, goal: str, steps: list[AgentStep], hits: list[RawSearchHit]) -> AgentAction:
        self._count()
        # Deterministic HITL trigger (ADR-032): a goal containing "ambiguous"
        # asks for clarification once; the task appends the user's answer to
        # the goal on resume, which disarms the trigger.
        if len(steps) == 0 and "ambiguous" in goal and "(user clarification:" not in goal:
            return AskAction(
                question="Your goal looks ambiguous — which meaning do you want?",
                reason="The goal can be read several ways; asking before spending searches",
            )
        if len(steps) == 0:
            return SearchAction(query=goal, reason="Start with the user's goal as the query")
        if len(steps) == 1:
            return SearchAction(
                query=f"{goal} latest",
                reason="Refine for recency to check whether anything newer exists",
            )
        return FinishAction(reason="The refined query added nothing new; coverage looks sufficient")


class FakeResultCritic(_FakeLlm):
    """Deterministic self-critique (ADR-031): a stable verdict, nothing
    dropped, no gap — the e2e journal shows the review step without changing
    the delivered fake results."""

    def critique(self, goal: str, hits: list[RawSearchHit]) -> Critique:
        self._count()
        return Critique(
            assessment=(
                f"All {len(hits)} results relate to the goal; "
                "one could not be dated and is listed separately."
            )
        )
