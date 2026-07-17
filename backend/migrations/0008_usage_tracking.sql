-- ADR-038: per-run API spend tracking. Counters accumulate across task
-- attempts and HITL resumes (each attempt spends real money).
ALTER TABLE research_jobs
    ADD COLUMN llm_calls INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN llm_input_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN llm_output_tokens BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN search_calls INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN cost_usd DOUBLE PRECISION NOT NULL DEFAULT 0;
