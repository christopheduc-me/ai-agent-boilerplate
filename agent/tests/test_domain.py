from datetime import UTC, datetime

from aiagent.domain.models import (
    DateConfidence,
    ResearchResult,
    as_utc,
    sort_by_publication_date,
)


def result(title: str, published_at: datetime | None) -> ResearchResult:
    return ResearchResult(
        title=title,
        url=f"https://example.com/{title}",
        snippet="",
        published_at=published_at,
        date_confidence=DateConfidence.HIGH if published_at else DateConfidence.UNKNOWN,
    )


def test_sorts_newest_first_with_unknown_dates_last() -> None:
    results = [
        result("old", datetime(2023, 1, 1, tzinfo=UTC)),
        result("no-date", None),
        result("new", datetime(2026, 6, 1, tzinfo=UTC)),
        result("mid", datetime(2025, 3, 15, tzinfo=UTC)),
    ]

    ordered = sort_by_publication_date(results)

    assert [r.title for r in ordered] == ["new", "mid", "old", "no-date"]


def test_as_utc_makes_naive_datetimes_aware() -> None:
    naive = datetime(2025, 1, 1, 12, 0, 0)
    aware = as_utc(naive)
    assert aware.tzinfo is UTC
    assert aware.hour == 12
