-- ADR-033: recurring searches with memory. The backend scheduler re-runs
-- saved keywords on an interval; each run is a normal research_job linked
-- back to its recurring search, and results carry an is_new flag computed
-- against the URLs of previous runs.
CREATE TABLE recurring_searches (
    id               UUID PRIMARY KEY,
    user_id          UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    keyword          TEXT NOT NULL,
    mode             TEXT NOT NULL DEFAULT 'workflow',
    interval_minutes INTEGER NOT NULL CHECK (interval_minutes BETWEEN 1 AND 10080),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_run_at      TIMESTAMPTZ
);
CREATE INDEX recurring_searches_user_idx ON recurring_searches (user_id, created_at DESC);

ALTER TABLE research_jobs
    -- Keep run history when the recurring search is deleted.
    ADD COLUMN recurring_search_id UUID REFERENCES recurring_searches (id) ON DELETE SET NULL;
CREATE INDEX research_jobs_recurring_idx ON research_jobs (recurring_search_id, created_at DESC);

ALTER TABLE search_results
    ADD COLUMN is_new BOOLEAN NOT NULL DEFAULT TRUE;
