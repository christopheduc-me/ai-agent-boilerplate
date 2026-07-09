"""SearchProvider adapter backed by Tavily (ADR-009)."""

from datetime import datetime
from typing import Any

from aiagent.domain.models import RawSearchHit, as_utc


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


class TavilySearchProvider:
    """Live adapter — requires TAVILY_API_KEY; never exercised in CI (ADR-012)."""

    def __init__(self, max_results: int = 10) -> None:
        from langchain_tavily import TavilySearch

        self._tool = TavilySearch(max_results=max_results)

    def search(self, keyword: str) -> list[RawSearchHit]:
        response = self._tool.invoke({"query": keyword})
        items = response.get("results", []) if isinstance(response, dict) else []
        return [hit_from_tavily(item) for item in items]
