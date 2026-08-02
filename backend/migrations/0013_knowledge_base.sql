-- RAG knowledge base (ADR-063): per-user documents, chunked and embedded for
-- retrieval-augmented grounding. Requires the pgvector extension (the compose
-- postgres uses the pgvector/pgvector image). Embedding dimension 768 matches
-- the default embedder (nomic-embed-text / the deterministic fake); a different
-- model dimension needs a new migration.
CREATE EXTENSION IF NOT EXISTS vector;

CREATE TABLE documents (
    id         UUID PRIMARY KEY,
    user_id    UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    -- pending (embedding dispatched) -> ready (chunks stored) | failed.
    status     TEXT NOT NULL CHECK (status IN ('pending', 'ready', 'failed')),
    error      TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX documents_user_idx ON documents (user_id, created_at DESC);

CREATE TABLE document_chunks (
    id          UUID PRIMARY KEY,
    document_id UUID NOT NULL REFERENCES documents (id) ON DELETE CASCADE,
    user_id     UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,
    content     TEXT NOT NULL,
    embedding   vector(768) NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
-- Approximate nearest-neighbour search on cosine distance (HNSW, pgvector).
CREATE INDEX document_chunks_embedding_idx
    ON document_chunks USING hnsw (embedding vector_cosine_ops);
CREATE INDEX document_chunks_user_idx ON document_chunks (user_id);
