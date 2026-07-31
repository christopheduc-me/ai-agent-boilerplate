"""SearchProvider adapter backed by DuckDuckGo (ADR-051) — keyless.

A second search engine that needs no API key, so a fork can run live search
with zero credentials, and the aggregator (`aggregating_search`) has a real
second source. DuckDuckGo scraping is rate-limited, so it is best used behind
the aggregator's partial-failure tolerance rather than alone.
"""

from typing import Any

from aiagent.domain.models import RawSearchHit
from aiagent.domain.usage import UsageMeter


def hit_from_ddg(item: dict[str, Any]) -> RawSearchHit:
    """Pure mapping from a DuckDuckGo result item — unit-testable, no network.
    DuckDuckGo returns no publication date, so the date cascade (ADR-011) falls
    back to the page fetch / LLM as usual."""
    return RawSearchHit(
        title=str(item.get("title", "")),
        url=str(item.get("href", "")),
        snippet=str(item.get("body", "")),
        published_at=None,
        raw=item,
    )


class DuckDuckGoSearchProvider:
    """Keyless live adapter (ADR-051); never exercised in CI (ADR-012)."""

    def __init__(self, max_results: int = 10, meter: UsageMeter | None = None) -> None:
        self._max_results = max_results
        self._meter = meter

    def search(self, keyword: str) -> list[RawSearchHit]:
        from ddgs import DDGS

        if self._meter is not None:
            self._meter.record_search()  # counted like any search (ADR-038), though free
        items = DDGS().text(keyword, max_results=self._max_results)
        return [hit_from_ddg(item) for item in items]
