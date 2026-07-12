"""Deterministic fake providers (ADR-021): `AGENT_PROVIDERS=fake`.

No network, no API key. Used by the e2e smoke test in CI (the paid-service ban
of ADR-012 applies to CI end to end) and for keyless local development. The
three hits exercise the whole date cascade (ADR-011): provider date (high),
LLM-extracted date (medium), unknown.
"""

from datetime import UTC, datetime

from aiagent.domain.models import (
    AgentAction,
    AgentStep,
    EventType,
    FinishAction,
    HitEnrichment,
    RawSearchHit,
    SearchAction,
)


class FakeSearchProvider:
    def search(self, keyword: str) -> list[RawSearchHit]:
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


class FakeHitEnricher:
    """Deterministic enrichment: a date only for the hit designed to exercise
    the LLM stage of the cascade, a stable event type and summary for all."""

    def enrich(self, hit: RawSearchHit) -> HitEnrichment:
        published_at = None
        if hit.title == "fake-llm-datable":
            published_at = datetime(2025, 8, 20, tzinfo=UTC)
        return HitEnrichment(
            published_at=published_at,
            event_type=EventType.ANNOUNCEMENT,
            summary=f"Fake summary for {hit.title}",
        )


class FakeAgentPolicy:
    """Deterministic policy (ADR-030) for keyless demos and e2e: search the
    goal, refine once (the fake provider returns the same hits, so the journal
    shows the deduplication at work), then stop with an explicit reason."""

    def decide(self, goal: str, steps: list[AgentStep], hits: list[RawSearchHit]) -> AgentAction:
        if len(steps) == 0:
            return SearchAction(query=goal, reason="Start with the user's goal as the query")
        if len(steps) == 1:
            return SearchAction(
                query=f"{goal} latest",
                reason="Refine for recency to check whether anything newer exists",
            )
        return FinishAction(reason="The refined query added nothing new; coverage looks sufficient")
