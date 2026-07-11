-- Timeline enrichment (ADR-027): event classification + one-line LLM summary.
ALTER TABLE search_results
    ADD COLUMN event_type TEXT NOT NULL DEFAULT 'other',
    ADD COLUMN summary TEXT;
