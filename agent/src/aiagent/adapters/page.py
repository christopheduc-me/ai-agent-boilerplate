"""PageDateFetcher adapter (ADR-035): reads the publication date a page
declares about itself — JSON-LD `datePublished`, then OpenGraph
`article:published_time`. Structured metadata written by the publisher is
authoritative (`high` confidence), cheaper than an LLM call, and stdlib to
parse. Everything here is defensive: a dead page, a parser error or garbage
metadata just mean "no date", never a failed job.
"""

import json
import logging
from datetime import datetime
from html.parser import HTMLParser
from typing import Any

import httpx

from aiagent.adapters.llm import parse_extracted_date

logger = logging.getLogger(__name__)

#: Pages are fetched to read <head> metadata: 512 KiB is plenty.
MAX_BYTES = 512 * 1024


class _MetadataCollector(HTMLParser):
    """Collects JSON-LD script bodies and OpenGraph date meta tags."""

    def __init__(self) -> None:
        super().__init__(convert_charrefs=True)
        self.jsonld_chunks: list[str] = []
        self.og_dates: list[str] = []
        self._in_jsonld = False

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        attributes = {k.lower(): (v or "") for k, v in attrs}
        if tag == "script" and attributes.get("type", "").lower() == "application/ld+json":
            self._in_jsonld = True
            self.jsonld_chunks.append("")
        if tag == "meta" and attributes.get("property", "").lower() == "article:published_time":
            if content := attributes.get("content", "").strip():
                self.og_dates.append(content)

    def handle_endtag(self, tag: str) -> None:
        if tag == "script":
            self._in_jsonld = False

    def handle_data(self, data: str) -> None:
        if self._in_jsonld and self.jsonld_chunks:
            self.jsonld_chunks[-1] += data


def _jsonld_dates(payload: Any) -> list[str]:
    """Walks a JSON-LD document (object, list, or @graph) for datePublished."""
    if isinstance(payload, list):
        return [date for item in payload for date in _jsonld_dates(item)]
    if not isinstance(payload, dict):
        return []
    dates: list[str] = []
    if isinstance(payload.get("datePublished"), str):
        dates.append(payload["datePublished"])
    dates.extend(_jsonld_dates(payload.get("@graph", [])))
    return dates


def extract_published_date(html: str) -> datetime | None:
    """The date the page declares, or None. JSON-LD wins over OpenGraph."""
    collector = _MetadataCollector()
    try:
        collector.feed(html)
    except Exception:  # noqa: BLE001 - malformed HTML must mean "no date"
        return None

    candidates: list[str] = []
    for chunk in collector.jsonld_chunks:
        try:
            candidates.extend(_jsonld_dates(json.loads(chunk)))
        except ValueError:
            continue
    candidates.extend(collector.og_dates)

    for candidate in candidates:
        if parsed := parse_extracted_date(candidate):
            return parsed
    return None


class HttpPageDateFetcher:
    """Fetches the page and reads its declared date. Bounded (timeout + size
    cap) and silent on failure — stage 2 is an opportunistic improvement, the
    cascade continues to the LLM without it."""

    def __init__(self, client: httpx.Client | None = None) -> None:
        self._client = client or httpx.Client(
            timeout=10,
            follow_redirects=True,
            headers={"User-Agent": "aiagent-boilerplate/1.0 (+date metadata)"},
        )

    def fetch_published_date(self, url: str) -> datetime | None:
        try:
            with self._client.stream("GET", url) as response:
                response.raise_for_status()
                size = 0
                chunks: list[bytes] = []
                for chunk in response.iter_bytes():
                    chunks.append(chunk)
                    size += len(chunk)
                    if size >= MAX_BYTES:
                        break  # metadata lives in <head>; stop downloading
                html = b"".join(chunks).decode(response.encoding or "utf-8", errors="replace")
            return extract_published_date(html)
        except Exception:  # noqa: BLE001 - unreachable page = no date, never a failure
            logger.info("page date fetch failed", extra={"url": url}, exc_info=True)
            return None
