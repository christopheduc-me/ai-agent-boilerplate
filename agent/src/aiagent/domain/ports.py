"""Ports (hexagonal architecture): the use case depends only on these Protocols."""

from datetime import datetime
from typing import Protocol

from aiagent.domain.models import RawSearchHit, ResearchResult


class SearchProvider(Protocol):
    def search(self, keyword: str) -> list[RawSearchHit]: ...


class DateExtractor(Protocol):
    """LLM-backed fallback used when the provider gives no publication date (ADR-011)."""

    def extract_date(self, hit: RawSearchHit) -> datetime | None: ...


class ResultSink(Protocol):
    """Job lifecycle callbacks — in production, the Rust API (ADR-006/016)."""

    def mark_started(self, job_id: str) -> None: ...

    def deliver(self, job_id: str, results: list[ResearchResult]) -> None: ...

    def report_failure(self, job_id: str, error: str) -> None: ...
