"""RAG knowledge base (ADR-063): chunking, keyless embeddings, the HTTP client
to the backend, retrieval grounding, and the embed task."""

import math

import httpx
import respx

from aiagent.adapters.fake import FakeEmbeddingProvider, FakeKnowledgeRetriever
from aiagent.adapters.knowledge import (
    EmbeddingKnowledgeRetriever,
    HttpKnowledgeClient,
    OllamaEmbeddingProvider,
    OpenAIEmbeddingProvider,
)
from aiagent.domain.knowledge import chunk_text
from aiagent.tasks import _augment_with_grounding, _knowledge_grounding, embed_document_task

BASE = "http://backend:8000"


def _cosine(a: list[float], b: list[float]) -> float:
    dot = sum(x * y for x, y in zip(a, b, strict=True))
    na = math.sqrt(sum(x * x for x in a))
    nb = math.sqrt(sum(y * y for y in b))
    return 0.0 if na == 0 or nb == 0 else dot / (na * nb)


# ---------------------------------------------------------------- chunking


def test_chunk_text_splits_with_overlap() -> None:
    text = " ".join(f"word{i}" for i in range(200))
    chunks = chunk_text(text, size=200, overlap=50)
    assert len(chunks) > 1
    assert all(len(c) <= 200 for c in chunks)
    # Reassembling the (overlapping) chunks covers the whole text.
    assert "word0" in chunks[0] and "word199" in chunks[-1]


def test_chunk_text_handles_empty_and_short() -> None:
    assert chunk_text("   ") == []
    assert chunk_text("short text") == ["short text"]


# ---------------------------------------------------------------- fake embeddings


def test_fake_embeddings_are_deterministic_and_lexically_similar() -> None:
    embedder = FakeEmbeddingProvider()
    [v] = embedder.embed(["the sky is blue"])
    assert len(v) == FakeEmbeddingProvider.DIM
    # Deterministic across calls (no per-process hash salt).
    assert embedder.embed(["the sky is blue"]) == [v]

    query = embedder.embed(["sky"])[0]
    related = embedder.embed(["the sky is blue today"])[0]
    unrelated = embedder.embed(["quarterly financial report"])[0]
    # Sharing the word "sky" yields a higher cosine than unrelated text.
    assert _cosine(query, related) > _cosine(query, unrelated)


# ---------------------------------------------------------------- HTTP client


@respx.mock
def test_client_stores_chunks_reports_failure_and_retrieves() -> None:
    store = respx.post(f"{BASE}/internal/documents/doc-1/chunks").mock(
        return_value=httpx.Response(204)
    )
    fail = respx.post(f"{BASE}/internal/documents/doc-1/failure").mock(
        return_value=httpx.Response(204)
    )
    retrieve = respx.post(f"{BASE}/internal/retrieve").mock(
        return_value=httpx.Response(
            200, json={"chunks": [{"content": "the sky is blue", "document_name": "notes.md"}]}
        )
    )
    client = HttpKnowledgeClient(BASE, "secret-token")

    client.store_chunks("doc-1", [{"seq": 0, "content": "c", "embedding": [0.1, 0.2]}])
    assert store.calls.last.request.headers["x-internal-token"] == "secret-token"

    client.report_document_failure("doc-1", "boom")
    assert fail.called

    hits = client.retrieve_chunks("job-1", [0.1, 0.2], 5)
    assert hits == ["the sky is blue"]
    import json

    assert json.loads(retrieve.calls.last.request.content)["job_id"] == "job-1"


# ---------------------------------------------------------------- retriever grounding


@respx.mock
def test_retriever_embeds_then_retrieves_and_degrades_gracefully() -> None:
    route = respx.post(f"{BASE}/internal/retrieve").mock(
        return_value=httpx.Response(
            200, json={"chunks": [{"content": "grounding", "document_name": "d"}]}
        )
    )
    retriever = EmbeddingKnowledgeRetriever(
        FakeEmbeddingProvider(), HttpKnowledgeClient(BASE, "tok")
    )
    assert retriever.retrieve("job-1", "sky", 3) == ["grounding"]
    assert route.called

    # A backend error degrades to no grounding (never fails the job).
    respx.post(f"{BASE}/internal/retrieve").mock(return_value=httpx.Response(500))
    assert retriever.retrieve("job-1", "sky", 3) == []


def test_augment_with_grounding_folds_chunks_into_the_goal() -> None:
    assert _augment_with_grounding("rust", []) == "rust"
    grounded = _augment_with_grounding("rust", ["the sky is blue"])
    assert grounded.startswith("rust")
    assert "knowledge base" in grounded
    assert "the sky is blue" in grounded


def test_grounding_is_skipped_in_fake_mode(monkeypatch) -> None:
    # The keyless e2e must stay deterministic: no grounding with the fakes.
    from aiagent.config import Settings

    monkeypatch.setenv("AGENT_PROVIDERS", "fake")
    settings = Settings.from_env()
    assert _knowledge_grounding(settings, "job-1", "rust") == []


def test_fake_retriever_returns_nothing() -> None:
    assert FakeKnowledgeRetriever().retrieve("job-1", "sky") == []


def test_embed_backend_defaults_per_backend(monkeypatch) -> None:
    from aiagent.config import Settings

    monkeypatch.delenv("AGENT_EMBED_MODEL", raising=False)
    monkeypatch.setenv("AGENT_EMBED_BACKEND", "openai")
    assert Settings.from_env().embed_model == "text-embedding-3-small"

    monkeypatch.setenv("AGENT_EMBED_BACKEND", "ollama")
    assert Settings.from_env().embed_model == "nomic-embed-text"

    # An explicit model overrides the per-backend default.
    monkeypatch.setenv("AGENT_EMBED_MODEL", "text-embedding-3-large")
    assert Settings.from_env().embed_model == "text-embedding-3-large"


# ---------------------------------------------------------------- Ollama embedder


@respx.mock
def test_ollama_embedder_calls_the_batch_endpoint() -> None:
    route = respx.post("http://ollama:11434/api/embed").mock(
        return_value=httpx.Response(200, json={"embeddings": [[0.1, 0.2, 0.3]]})
    )
    out = OllamaEmbeddingProvider("http://ollama:11434", "nomic-embed-text").embed(["hi"])
    assert out == [[0.1, 0.2, 0.3]]
    import json

    body = json.loads(route.calls.last.request.content)
    assert body == {"model": "nomic-embed-text", "input": ["hi"]}


@respx.mock
def test_openai_embedder_requests_768_dims_with_auth() -> None:
    route = respx.post("https://api.openai.com/v1/embeddings").mock(
        return_value=httpx.Response(
            200,
            json={"data": [{"index": 1, "embedding": [0.4]}, {"index": 0, "embedding": [0.1]}]},
        )
    )
    out = OpenAIEmbeddingProvider("sk-key", "text-embedding-3-small").embed(["a", "b"])
    # Ordered by index (not response order).
    assert out == [[0.1], [0.4]]
    request = route.calls.last.request
    assert request.headers["authorization"] == "Bearer sk-key"
    import json

    body = json.loads(request.content)
    assert body["model"] == "text-embedding-3-small"
    assert body["dimensions"] == OpenAIEmbeddingProvider.DIM  # 768, matches the migration


# ---------------------------------------------------------------- embed task


@respx.mock
def test_embed_document_task_chunks_embeds_and_posts(monkeypatch) -> None:
    monkeypatch.setenv("AGENT_PROVIDERS", "fake")
    monkeypatch.setenv("BACKEND_INTERNAL_URL", BASE)
    monkeypatch.setenv("INTERNAL_API_TOKEN", "tok")
    route = respx.post(f"{BASE}/internal/documents/doc-9/chunks").mock(
        return_value=httpx.Response(204)
    )

    n = embed_document_task("doc-9", "notes.md", "the sky is blue and clear")

    assert n >= 1
    import json

    payload = json.loads(route.calls.last.request.content)
    assert payload["chunks"][0]["seq"] == 0
    assert len(payload["chunks"][0]["embedding"]) == FakeEmbeddingProvider.DIM


@respx.mock
def test_embed_document_task_reports_empty_content(monkeypatch) -> None:
    monkeypatch.setenv("AGENT_PROVIDERS", "fake")
    monkeypatch.setenv("BACKEND_INTERNAL_URL", BASE)
    monkeypatch.setenv("INTERNAL_API_TOKEN", "tok")
    fail = respx.post(f"{BASE}/internal/documents/doc-empty/failure").mock(
        return_value=httpx.Response(204)
    )

    assert embed_document_task("doc-empty", "empty.md", "   ") == 0
    assert fail.called
