-- Per-user notification channels (ADR-061): where a user receives digests,
-- chosen in their profile (in addition to the optional per-recurring-search
-- webhook, ADR-036). `secret` holds a per-channel credential (Telegram bot
-- token); it is never returned by the API. Cascades on account deletion (ADR-058).
CREATE TABLE notification_channels (
    id         UUID PRIMARY KEY,
    user_id    UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('slack', 'telegram')),
    target     TEXT NOT NULL,
    secret     TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_notification_channels_user ON notification_channels (user_id);
