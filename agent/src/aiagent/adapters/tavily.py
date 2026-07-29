"""SearchProvider adapter backed by Tavily (ADR-009)."""

from datetime import datetime
from typing import Any

from aiagent.domain.models import RawSearchHit, as_utc
from aiagent.domain.usage import UsageMeter


def parse_provider_date(value: object) -> datetime | None:
    """Parses the provider's `published_date` field; tolerant of absence/garbage."""
    if not value:
        return None
    try:
        return as_utc(datetime.fromisoformat(str(value).replace("Z", "+00:00")))
    except ValueError:
        return None


def hit_from_tavily(item: dict[str, Any]) -> RawSearchHit:
    """Pure mapping from a Tavily result item — unit-testable without any network."""
    return RawSearchHit(
        title=str(item.get("title", "")),
        url=str(item.get("url", "")),
        snippet=str(item.get("content", "")),
        published_at=parse_provider_date(item.get("published_date")),
        raw=item,
    )


def hits_from_tavily_response(response: object) -> list[RawSearchHit]:
    """Maps a Tavily response to hits, **raising on a provider error** rather
    than treating it as zero results. A quota/key error (`{"error": ...}`) would
    otherwise be swallowed as an empty search, and the agent would keep
    searching against a dead provider — burning its step budget and LLM spend on
    a run that silently returns nothing. Raising instead fails the job fast with
    the provider's message (the use case turns it into a reported failure)."""
    if isinstance(response, dict) and response.get("error"):
        raise RuntimeError(f"Tavily search failed: {response['error']}")
    items = response.get("results", []) if isinstance(response, dict) else []
    return [hit_from_tavily(item) for item in items]


class TavilySearchProvider:
    """Live adapter — requires TAVILY_API_KEY; never exercised in CI (ADR-012)."""

    def __init__(self, max_results: int = 10, meter: UsageMeter | None = None) -> None:
        from langchain_tavily import TavilySearch

        self._tool = TavilySearch(max_results=max_results)
        self._meter = meter

    def search(self, keyword: str) -> list[RawSearchHit]:
        if self._meter is not None:
            self._meter.record_search()  # each call spends a Tavily credit (ADR-038)
        return hits_from_tavily_response(self._tool.invoke({"query": keyword}))
