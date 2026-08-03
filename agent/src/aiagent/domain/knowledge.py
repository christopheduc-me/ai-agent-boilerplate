"""Knowledge-base text chunking (ADR-063): split an uploaded document into
overlapping windows small enough to embed and retrieve. Pure domain — the
embedding itself is a port (``EmbeddingProvider``)."""

DEFAULT_CHUNK_SIZE = 1000
DEFAULT_OVERLAP = 150


def chunk_text(
    content: str,
    size: int = DEFAULT_CHUNK_SIZE,
    overlap: int = DEFAULT_OVERLAP,
) -> list[str]:
    """Splits ``content`` into chunks of at most ``size`` characters with
    ``overlap`` shared characters between consecutive chunks (so a passage on a
    boundary is not lost). Whitespace-normalized; empty input yields no chunks.
    Prefers to break on a whitespace boundary near the window end."""
    text = " ".join(content.split())
    if not text:
        return []
    if size <= 0:
        return [text]
    overlap = max(0, min(overlap, size - 1))

    chunks: list[str] = []
    start = 0
    n = len(text)
    while start < n:
        end = min(start + size, n)
        if end < n:
            # Back up to the last space so we do not cut a word in half.
            space = text.rfind(" ", start, end)
            if space > start:
                end = space
        chunks.append(text[start:end].strip())
        if end >= n:
            break
        start = max(end - overlap, start + 1)
    return [c for c in chunks if c]
