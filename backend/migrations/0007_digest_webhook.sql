-- ADR-036: digest webhooks for recurring searches. When a scheduled run
-- delivers results the user has not seen before, the backend POSTs a digest
-- to this URL (best effort, never blocks ingestion).
ALTER TABLE recurring_searches
    ADD COLUMN webhook_url TEXT;
