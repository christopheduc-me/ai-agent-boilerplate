"""ResultSink adapter: HTTP callback to the Rust backend (ADR-006)."""

from typing import Any

import httpx

from aiagent.domain.models import ResearchResult


def serialize_result(result: ResearchResult) -> dict[str, Any]:
    return {
        "title": result.title,
        "url": result.url,
        "snippet": result.snippet,
        "published_at": result.published_at.isoformat() if result.published_at else None,
        "date_confidence": result.date_confidence.value,
        "raw": result.raw,
    }


class HttpResultSink:
    def __init__(
        self,
        base_url: str,
        internal_token: str,
        client: httpx.Client | None = None,
        request_id: str | None = None,
    ) -> None:
        self._base_url = base_url.rstrip("/")
        self._client = client or httpx.Client(timeout=30)
        self._headers = {"X-Internal-Token": internal_token}
        if request_id:
            # Correlation (ADR-018): callbacks carry the id end to end.
            self._headers["X-Request-Id"] = request_id

    def mark_started(self, job_id: str) -> None:
        response = self._client.post(
            f"{self._base_url}/internal/jobs/{job_id}/started",
            headers=self._headers,
        )
        response.raise_for_status()

    def deliver(self, job_id: str, results: list[ResearchResult]) -> None:
        response = self._client.post(
            f"{self._base_url}/internal/jobs/{job_id}/results",
            json={"results": [serialize_result(r) for r in results]},
            headers=self._headers,
        )
        response.raise_for_status()

    def report_failure(self, job_id: str, error: str) -> None:
        response = self._client.post(
            f"{self._base_url}/internal/jobs/{job_id}/failure",
            json={"error": error},
            headers=self._headers,
        )
        response.raise_for_status()
