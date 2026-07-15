"""LLM adapters backed by Claude via langchain-anthropic: the HitEnricher
(ADR-010/011/027) and the AgentPolicy driving the agentic loop (ADR-030)."""

import json
from datetime import datetime
from typing import TYPE_CHECKING

from aiagent.domain.models import (
    AgentAction,
    AgentStep,
    Critique,
    EventType,
    FinishAction,
    HitEnrichment,
    RawSearchHit,
    SearchAction,
    as_utc,
)

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


POLICY_PROMPT = """\
You are a research agent gathering fresh, relevant web results about a goal.
Decide your next action and reply with a single JSON object, nothing else:

- to search (again): {{"action": "search", "query": "...", "reason": "..."}}
- to stop:           {{"action": "finish", "reason": "..."}}

Rules: refine or vary the query instead of repeating one that brought nothing
new; stop as soon as coverage looks sufficient or further searches stop adding
results. "reason" is one short sentence, shown to the user as your journal.

Goal: {goal}

Searches so far (query -> new results added):
{transcript}

Results collected so far ({count}):
{titles}
"""


def parse_action(text: str) -> AgentAction:
    """Parses the policy's JSON reply defensively: anything malformed means
    FINISH — a confused model must never burn the step budget."""
    cleaned = text.strip()
    if cleaned.startswith("```"):
        cleaned = cleaned.strip("`")
        cleaned = cleaned.removeprefix("json").strip()
    try:
        payload = json.loads(cleaned)
    except ValueError:
        return FinishAction(reason="policy reply was not valid JSON")
    if not isinstance(payload, dict):
        return FinishAction(reason="policy reply was not a JSON object")

    reason = payload.get("reason")
    reason = reason.strip() if isinstance(reason, str) and reason.strip() else "no reason given"
    query = payload.get("query")
    if payload.get("action") == "search" and isinstance(query, str) and query.strip():
        return SearchAction(query=query.strip(), reason=reason)
    return FinishAction(reason=reason)


class ClaudeAgentPolicy:
    """Live AgentPolicy (ADR-030) — the LLM sees the goal, the transcript of
    its own past decisions and the collected titles (token-frugal), and picks
    the next action. Same injectable-`llm` pattern as ClaudeHitEnricher."""

    def __init__(self, model_id: str, llm: "BaseChatModel | None" = None) -> None:
        if llm is not None:
            self._llm = llm
            return
        from langchain_anthropic import ChatAnthropic

        # `model` / `max_tokens` are pydantic aliases mypy cannot see.
        self._llm = ChatAnthropic(model=model_id, max_tokens=256)  # type: ignore[call-arg]

    def decide(self, goal: str, steps: list[AgentStep], hits: list[RawSearchHit]) -> AgentAction:
        transcript = "\n".join(f'- "{s.detail}" -> {s.new_hits} new' for s in steps) or "- none yet"
        titles = "\n".join(f"- {h.title}" for h in hits[:30]) or "- none yet"
        prompt = POLICY_PROMPT.format(
            goal=goal, transcript=transcript, count=len(hits), titles=titles
        )
        response = self._llm.invoke(prompt)
        content = response.content if isinstance(response.content, str) else str(response.content)
        return parse_action(content)


CRITIQUE_PROMPT = """\
You are reviewing the results a research agent collected for a goal, before
they are delivered. Reply with a single JSON object, nothing else:

{{"assessment": "...", "irrelevant_urls": [...], "gap_query": "..." or null}}

- "assessment": one or two sentences judging how well the results cover the
  goal (shown to the user verbatim).
- "irrelevant_urls": the URLs of results clearly unrelated to the goal (empty
  list if none). Be conservative: only drop obvious noise.
- "gap_query": if one important angle is missing, a single search query that
  would fill it; otherwise null.

Goal: {goal}

Results ({count}):
{listing}
"""


def parse_critique(text: str) -> Critique:
    """Parses the critic's JSON reply defensively: anything malformed becomes
    a neutral critique (no drops, no gap) — the review must never fail a job."""
    cleaned = text.strip()
    if cleaned.startswith("```"):
        cleaned = cleaned.strip("`")
        cleaned = cleaned.removeprefix("json").strip()
    try:
        payload = json.loads(cleaned)
    except ValueError:
        return Critique(assessment="self-critique unavailable (reply was not valid JSON)")
    if not isinstance(payload, dict):
        return Critique(assessment="self-critique unavailable (reply was not a JSON object)")

    assessment = payload.get("assessment")
    if not isinstance(assessment, str) or not assessment.strip():
        assessment = "no assessment given"
    urls = payload.get("irrelevant_urls")
    irrelevant = tuple(u for u in urls if isinstance(u, str)) if isinstance(urls, list) else ()
    gap = payload.get("gap_query")
    gap_query = gap.strip() if isinstance(gap, str) and gap.strip() else None
    return Critique(assessment=assessment.strip(), irrelevant_urls=irrelevant, gap_query=gap_query)


class ClaudeResultCritic:
    """Live ResultCritic (ADR-031) — one call reviewing the whole result set;
    same injectable-`llm` pattern as the other Claude adapters."""

    def __init__(self, model_id: str, llm: "BaseChatModel | None" = None) -> None:
        if llm is not None:
            self._llm = llm
            return
        from langchain_anthropic import ChatAnthropic

        # `model` / `max_tokens` are pydantic aliases mypy cannot see.
        self._llm = ChatAnthropic(model=model_id, max_tokens=512)  # type: ignore[call-arg]

    def critique(self, goal: str, hits: list[RawSearchHit]) -> Critique:
        listing = "\n".join(f"- {h.title} — {h.url}\n  {h.snippet}" for h in hits[:30]) or "- none"
        prompt = CRITIQUE_PROMPT.format(goal=goal, count=len(hits), listing=listing)
        response = self._llm.invoke(prompt)
        content = response.content if isinstance(response.content, str) else str(response.content)
        return parse_critique(content)
