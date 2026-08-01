-- Security audit log (ADR-057): append-only record of abuse-relevant events
-- (failed/throttled logins, refresh-token reuse, quota hits). user_id is
-- nullable — an unknown-email login has no account — and drops the row if the
-- account is deleted. Retention is enforced by the background loop (delete_before).
CREATE TABLE security_events (
    id UUID PRIMARY KEY,
    kind TEXT NOT NULL,
    user_id UUID REFERENCES users (id) ON DELETE SET NULL,
    client_ip TEXT,
    detail TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Newest-first operator queries and the retention purge both scan by time.
CREATE INDEX idx_security_events_created ON security_events (created_at);
CREATE INDEX idx_security_events_user ON security_events (user_id);
