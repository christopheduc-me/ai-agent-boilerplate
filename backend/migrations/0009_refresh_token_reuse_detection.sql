-- Refresh-token reuse detection (ADR-056): rotation becomes a lineage.
--   * family_id groups every token a login rotates into, so the whole lineage
--     can be revoked at once when a stolen (already-consumed) token is replayed.
--   * consumed_at keeps a rotated-away token instead of deleting it, so the
--     replay is caught as reuse rather than looking like an unknown token.
-- Existing rows each get their own family (gen_random_uuid is built into
-- PostgreSQL 13+), so a pre-migration token stays usable and isolated.
ALTER TABLE refresh_tokens
    ADD COLUMN family_id UUID NOT NULL DEFAULT gen_random_uuid();
ALTER TABLE refresh_tokens
    ADD COLUMN consumed_at TIMESTAMPTZ;

-- The default only backfills existing rows; the application always supplies it.
ALTER TABLE refresh_tokens ALTER COLUMN family_id DROP DEFAULT;

CREATE INDEX idx_refresh_tokens_family ON refresh_tokens (family_id);
