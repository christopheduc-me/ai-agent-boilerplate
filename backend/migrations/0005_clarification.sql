-- ADR-032: human-in-the-loop clarification. The agent can pause a job with a
-- question; the user's answer re-dispatches it.
ALTER TABLE research_jobs
    ADD COLUMN question TEXT,
    ADD COLUMN answer TEXT;

ALTER TABLE research_jobs
    DROP CONSTRAINT research_jobs_status_check;
ALTER TABLE research_jobs
    ADD CONSTRAINT research_jobs_status_check
    CHECK (status IN ('pending', 'running', 'awaiting_input', 'completed', 'failed'));
