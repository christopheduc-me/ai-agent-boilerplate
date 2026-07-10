"""DateExtractor adapter backed by Claude via langchain-anthropic (ADR-010/011)."""

from datetime import datetime
from typing import TYPE_CHECKING

from aiagent.domain.models import RawSearchHit, as_utc

if TYPE_CHECKING:
    from langchain_core.language_models import BaseChatModel

EXTRACTION_PROMPT = """\
You extract the publication date of a web page from its title and excerpt.

Title: {title}
URL: {url}
Excerpt: {snippet}

Reply with the publication date in ISO 8601 format (YYYY-MM-DD or full timestamp)
and nothing else. If the publication date cannot be determined with reasonable
confidence, reply with exactly: unknown
"""


def parse_extracted_date(text: str) -> datetime | None:
    """Parses the model's reply; anything that is not a clean ISO date means unknown."""
    cleaned = text.strip().strip("`\"' ")
    if not cleaned or cleaned.lower() == "unknown":
        return None
    try:
        return as_utc(datetime.fromisoformat(cleaned.replace("Z", "+00:00")))
    except ValueError:
        return None


class ClaudeDateExtractor:
    """Live adapter — requires ANTHROPIC_API_KEY; the model call itself is
    never exercised in CI (ADR-012). `llm` is injectable so the prompt/parse
    logic around it stays unit-testable with a fake chat model."""

    def __init__(self, model_id: str, llm: "BaseChatModel | None" = None) -> None:
        if llm is not None:
            self._llm = llm
            return
        from langchain_anthropic import ChatAnthropic

        # `model` / `max_tokens` are pydantic aliases mypy cannot see.
        self._llm = ChatAnthropic(model=model_id, max_tokens=64)  # type: ignore[call-arg]

    def extract_date(self, hit: RawSearchHit) -> datetime | None:
        prompt = EXTRACTION_PROMPT.format(title=hit.title, url=hit.url, snippet=hit.snippet)
        response = self._llm.invoke(prompt)
        content = response.content if isinstance(response.content, str) else str(response.content)
        return parse_extracted_date(content)
