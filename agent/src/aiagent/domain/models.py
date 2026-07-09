"""Domain model: pure data + pure logic, no I/O (hexagonal core, ADR-004)."""

from dataclasses import dataclass, field
from datetime import UTC, datetime
from enum import StrEnum
from typing import Any


class DateConfidence(StrEnum):
    HIGH = "high"
    MEDIUM = "medium"
    UNKNOWN = "unknown"


@dataclass(frozen=True)
class RawSearchHit:
    """What a search provider returns before date resolution."""

    title: str
    url: str
    snippet: str
    published_at: datetime | None = None
    raw: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class ResearchResult:
    """A hit with its resolved publication date (ADR-011)."""

    title: str
    url: str
    snippet: str
    published_at: datetime | None
    date_confidence: DateConfidence
    raw: dict[str, Any] = field(default_factory=dict)


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
