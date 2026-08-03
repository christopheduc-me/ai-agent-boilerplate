"""Knowledge-base adapters (ADR-063): the live embedder and the HTTP client that
talks to the Rust backend's internal API (chunk callback + retrieval). The agent
owns embeddings; the backend owns the vector store (ADR-006)."""

import logging
from typing import Any

import httpx

logger = logging.getLogger(__name__)


class OllamaEmbeddingProvider:
    """Embeddings from a local Ollama model (ADR-063), e.g. ``nomic-embed-text``
    (768 dims, matching the ``document_chunks`` column). Keyless, on the host's
    Ollama (``AGENT_LLM_BASE_URL``). Uses the batch ``/api/embed`` endpoint."""

    def __init__(self, base_url: str, model: str, client: httpx.Client | None = None) -> None:
        self._url = f"{base_url.rstrip('/')}/api/embed"
        self._model = model
        self._client = client or httpx.Client(timeout=60)

    def embed(self, texts: list[str]) -> list[list[float]]:
        response = self._client.post(self._url, json={"model": self._model, "input": texts})
        response.raise_for_status()
        embeddings: list[list[float]] = response.json()["embeddings"]
        return embeddings


class OpenAIEmbeddingProvider:
    """Cloud embeddings from OpenAI (ADR-063): the ``text-embedding-3-*`` models
    over HTTPS. Requests ``dimensions=768`` (the v3 models support Matryoshka
    truncation) so the vectors match the backend's ``vector(768)`` column with no
    migration — enabling a 100%-cloud pairing with a hosted LLM (Claude)."""

    DIM = 768

    def __init__(
        self,
        api_key: str,
        model: str,
        base_url: str = "https://api.openai.com/v1",
        client: httpx.Client | None = None,
    ) -> None:
        self._url = f"{base_url.rstrip('/')}/embeddings"
        self._model = model
        self._headers = {"Authorization": f"Bearer {api_key}"}
        self._client = client or httpx.Client(timeout=60)

    def embed(self, texts: list[str]) -> list[list[float]]:
        response = self._client.post(
            self._url,
            json={"model": self._model, "input": texts, "dimensions": self.DIM},
            headers=self._headers,
        )
        response.raise_for_status()
        # The API echoes an `index` per item; order by it to be safe.
        data = sorted(response.json()["data"], key=lambda item: item["index"])
        embeddings: list[list[float]] = [item["embedding"] for item in data]
        return embeddings


class HttpKnowledgeClient:
    """Internal callbacks to the Rust backend (ADR-063): stores a document's
    embedded chunks, reports an embedding failure, and retrieves the nearest
    chunks for a job. Same shared token as the result sink (ADR-005/006)."""

    def __init__(
        self,
        base_url: str,
        internal_token: str,
        client: httpx.Client | None = None,
    ) -> None:
        self._base_url = base_url.rstrip("/")
        self._client = client or httpx.Client(timeout=30)
        self._headers = {"X-Internal-Token": internal_token}

    def store_chunks(self, document_id: str, chunks: list[dict[str, Any]]) -> None:
        response = self._client.post(
            f"{self._base_url}/internal/documents/{document_id}/chunks",
            json={"chunks": chunks},
            headers=self._headers,
        )
        response.raise_for_status()

    def report_document_failure(self, document_id: str, error: str) -> None:
        response = self._client.post(
            f"{self._base_url}/internal/documents/{document_id}/failure",
            json={"error": error},
            headers=self._headers,
        )
        response.raise_for_status()

    def retrieve_chunks(self, job_id: str, embedding: list[float], k: int) -> list[str]:
        response = self._client.post(
            f"{self._base_url}/internal/retrieve",
            json={"job_id": job_id, "embedding": embedding, "k": k},
            headers=self._headers,
        )
        response.raise_for_status()
        return [chunk["content"] for chunk in response.json()["chunks"]]


class EmbeddingKnowledgeRetriever:
    """`KnowledgeRetriever` (ADR-063): embeds the query and asks the backend for
    the nearest chunks of the job's owner. Best-effort — any failure degrades to
    no grounding, never failing the job."""

    def __init__(self, embedder: Any, client: HttpKnowledgeClient) -> None:
        self._embedder = embedder
        self._client = client

    def retrieve(self, job_id: str, query: str, k: int = 5) -> list[str]:
        try:
            embedding = self._embedder.embed([query])[0]
            return self._client.retrieve_chunks(job_id, embedding, k)
        except Exception:  # noqa: BLE001 - grounding is best-effort by contract
            logger.warning("knowledge retrieval failed", extra={"job_id": job_id}, exc_info=True)
            return []
