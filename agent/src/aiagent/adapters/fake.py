"""Deterministic fake providers (ADR-021): `AGENT_PROVIDERS=fake`.

No network, no API key. Used by the e2e smoke test in CI (the paid-service ban
of ADR-012 applies to CI end to end) and for keyless local development. The
three hits exercise the whole date cascade (ADR-011): provider date (high),
LLM-extracted date (medium), unknown.
"""

from datetime import UTC, datetime

from aiagent.domain.models import RawSearchHit


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


class FakeDateExtractor:
    """Finds a date only for the hit designed to exercise the LLM stage."""

    def extract_date(self, hit: RawSearchHit) -> datetime | None:
        if hit.title == "fake-llm-datable":
            return datetime(2025, 8, 20, tzinfo=UTC)
        return None
