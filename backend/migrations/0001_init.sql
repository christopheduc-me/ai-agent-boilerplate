-- Initial schema (ADR-007). Applied with `sqlx migrate run` once the PostgreSQL
-- adapter lands (next TDD step, see adapters::persistence).
CREATE TABLE users (
    id            UUID PRIMARY KEY,
    email         TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE research_jobs (
    id           UUID PRIMARY KEY,
    user_id      UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    keyword      TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('pending', 'running', 'completed', 'failed')),
    error        TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ
);
CREATE INDEX research_jobs_user_id_idx ON research_jobs (user_id, created_at DESC);

CREATE TABLE search_results (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id          UUID NOT NULL REFERENCES research_jobs (id) ON DELETE CASCADE,
    title           TEXT NOT NULL,
    url             TEXT NOT NULL,
    snippet         TEXT NOT NULL DEFAULT '',
    published_at    TIMESTAMPTZ,
    date_confidence TEXT NOT NULL CHECK (date_confidence IN ('high', 'medium', 'unknown')),
    raw             JSONB NOT NULL DEFAULT 'null'
);
CREATE INDEX search_results_job_id_idx ON search_results (job_id, published_at DESC NULLS LAST);
