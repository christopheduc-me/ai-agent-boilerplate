-- Email notification channel (ADR-062): widen the kind CHECK to allow 'email'
-- alongside slack/telegram (migration 0011). Delivery is opt-in via the SMTP_*
-- config; an email channel carries no per-channel secret (SMTP credentials are
-- server-level).
ALTER TABLE notification_channels
    DROP CONSTRAINT notification_channels_kind_check;
ALTER TABLE notification_channels
    ADD CONSTRAINT notification_channels_kind_check
    CHECK (kind IN ('slack', 'telegram', 'email'));
