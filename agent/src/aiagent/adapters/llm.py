"""HitEnricher adapter backed by Claude via langchain-anthropic (ADR-010/011/027)."""

import json
from datetime import datetime
from typing import TYPE_CHECKING

from aiagent.domain.models import EventType, HitEnrichment, RawSearchHit, as_utc

if TYPE_CHECKING:
    from langchain_core.language_models import BaseChatModel

ENRICHMENT_PROMPT = """\
You analyze a web search result about a topic and reply with a single JSON
object, nothing else, with exactly these keys:

- "published_date": the publication date in ISO 8601 (YYYY-MM-DD or full
  timestamp), or null if it cannot be determined with reasonable confidence.
- "event_type": one of "announcement", "release", "funding", "legal",
  "incident", "research", "opinion", "other".
- "summary": one factual sentence (max 25 words) describing the event this
  page reports.

Title: {title}
URL: {url}
Excerpt: {snippet}
"""


def parse_extracted_date(text: str) -> datetime | None:
    """Parses an ISO date; anything that is not a clean ISO date means unknown."""
    cleaned = text.strip().strip("`\"' ")
    if not cleaned or cleaned.lower() in ("unknown", "null", "none"):
        return None
    try:
        return as_utc(datetime.fromisoformat(cleaned.replace("Z", "+00:00")))
    except ValueError:
        return None


def parse_enrichment(text: str) -> HitEnrichment:
    """Parses the model's JSON reply defensively: any malformed piece degrades
    to its neutral value instead of failing the whole research job."""
    cleaned = text.strip()
    if cleaned.startswith("```"):
        cleaned = cleaned.strip("`")
        cleaned = cleaned.removeprefix("json").strip()
    try:
        payload = json.loads(cleaned)
    except ValueError:
        return HitEnrichment()
    if not isinstance(payload, dict):
        return HitEnrichment()

    published_at = None
    if isinstance(payload.get("published_date"), str):
        published_at = parse_extracted_date(payload["published_date"])
    try:
        event_type = EventType(str(payload.get("event_type")))
    except ValueError:
        event_type = EventType.OTHER
    summary = payload.get("summary")
    if not isinstance(summary, str) or not summary.strip():
        summary = None

    return HitEnrichment(published_at=published_at, event_type=event_type, summary=summary)


class ClaudeHitEnricher:
    """Live adapter — requires ANTHROPIC_API_KEY; the model call itself is
    never exercised in CI (ADR-012). `llm` is injectable so the prompt/parse
    logic around it stays unit-testable with a fake chat model."""

    def __init__(self, model_id: str, llm: "BaseChatModel | None" = None) -> None:
        if llm is not None:
            self._llm = llm
            return
        from langchain_anthropic import ChatAnthropic

        # `model` / `max_tokens` are pydantic aliases mypy cannot see.
        self._llm = ChatAnthropic(model=model_id, max_tokens=256)  # type: ignore[call-arg]

    def enrich(self, hit: RawSearchHit) -> HitEnrichment:
        prompt = ENRICHMENT_PROMPT.format(title=hit.title, url=hit.url, snippet=hit.snippet)
        response = self._llm.invoke(prompt)
        content = response.content if isinstance(response.content, str) else str(response.content)
        return parse_enrichment(content)
