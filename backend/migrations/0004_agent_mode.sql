-- ADR-030: workflow vs agent mode, and the agent's decision journal.
ALTER TABLE research_jobs
    ADD COLUMN mode TEXT NOT NULL DEFAULT 'workflow';

CREATE TABLE agent_steps (
    job_id UUID NOT NULL REFERENCES research_jobs (id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    detail TEXT NOT NULL,
    reason TEXT NOT NULL,
    new_hits INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Idempotence (ADR-016): a Celery retry re-sends the same step.
    PRIMARY KEY (job_id, seq)
);
