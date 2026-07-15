"""Domain model: pure data + pure logic, no I/O (hexagonal core, ADR-004)."""

from dataclasses import dataclass, field
from datetime import UTC, datetime
from enum import StrEnum
from typing import Any


class DateConfidence(StrEnum):
    HIGH = "high"
    MEDIUM = "medium"
    UNKNOWN = "unknown"


class EventType(StrEnum):
    """Coarse classification of what a result reports, shown as a badge on the
    frontend timeline (ADR-027)."""

    ANNOUNCEMENT = "announcement"
    RELEASE = "release"
    FUNDING = "funding"
    LEGAL = "legal"
    INCIDENT = "incident"
    RESEARCH = "research"
    OPINION = "opinion"
    OTHER = "other"


@dataclass(frozen=True)
class RawSearchHit:
    """What a search provider returns before date resolution."""

    title: str
    url: str
    snippet: str
    published_at: datetime | None = None
    raw: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class HitEnrichment:
    """What the LLM adds to a raw hit (ADR-027): a publication date when the
    provider gave none (ADR-011 cascade), an event type, a one-line summary."""

    published_at: datetime | None = None
    event_type: EventType = EventType.OTHER
    summary: str | None = None


@dataclass(frozen=True)
class ResearchResult:
    """A hit with its resolved publication date (ADR-011) and its timeline
    enrichment (ADR-027)."""

    title: str
    url: str
    snippet: str
    published_at: datetime | None
    date_confidence: DateConfidence
    event_type: EventType = EventType.OTHER
    summary: str | None = None
    raw: dict[str, Any] = field(default_factory=dict)


class AgentStepKind(StrEnum):
    """What the agent decided at one step of the loop (ADR-030/031)."""

    SEARCH = "search"
    FINISH = "finish"
    CRITIQUE = "critique"


@dataclass(frozen=True)
class SearchAction:
    """The policy wants to run (another) search with its own query."""

    query: str
    reason: str


@dataclass(frozen=True)
class FinishAction:
    """The policy judges the goal reached (or not reachable) and stops."""

    reason: str


AgentAction = SearchAction | FinishAction


@dataclass(frozen=True)
class Critique:
    """The agent's self-assessment of its own results (ADR-031), produced
    before delivery: a verdict for the journal, the URLs judged off-topic
    (dropped from the delivery), and at most one gap worth one repair search."""

    assessment: str
    irrelevant_urls: tuple[str, ...] = ()
    gap_query: str | None = None


@dataclass(frozen=True)
class AgentStep:
    """One executed decision, recorded for the live journal (ADR-030)."""

    seq: int
    kind: AgentStepKind
    detail: str  # the query for SEARCH, empty for FINISH
    reason: str  # the policy's own explanation, shown verbatim in the UI
    new_hits: int = 0  # hits added by this step after URL deduplication


def _sort_key(result: ResearchResult) -> tuple[int, float]:
    if result.published_at is None:
        # Unknown dates go last (displayed in a separate section, ADR-011).
        return (1, 0.0)
    return (0, -result.published_at.timestamp())


def sort_by_publication_date(results: list[ResearchResult]) -> list[ResearchResult]:
    """Newest first; results without a date last."""
    return sorted(results, key=_sort_key)


def as_utc(value: datetime) -> datetime:
    """Normalizes naive datetimes to UTC so sorting never mixes aware/naive."""
    if value.tzinfo is None:
        return value.replace(tzinfo=UTC)
    return value.astimezone(UTC)
