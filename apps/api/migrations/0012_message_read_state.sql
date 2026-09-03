-- Issue #73 (dashboard) needs a real "unread messages" signal — none
-- existed (`messages` carries no per-user read marker at all). One row per
-- (family, user) with a single `last_read_at` watermark is enough to answer
-- "which messages are unread" (any `messages.created_at > last_read_at`,
-- see apps/api/src/messagerie/messages.rs::unread_messages) and is far
-- cheaper than a row per (message, user) read receipt, which this app has
-- no other use for (no per-message "seen by" UI). The trade-off: a message
-- can't be individually marked unread again once the watermark has passed
-- it. That's an accepted v1 limitation, not an oversight.
--
-- `last_read_at` advances when the member opens `/messagerie`
-- (apps/web/src/routes/messagerie/thread.rs), mirroring how `messages`
-- itself is scoped: no `id` column, `(group_id, user_id)` is the natural
-- key.

CREATE TABLE message_read_state (
    group_id            UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    user_id             UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    last_read_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, user_id)
);

ALTER TABLE message_read_state ENABLE ROW LEVEL SECURITY;
ALTER TABLE message_read_state FORCE ROW LEVEL SECURITY;
CREATE POLICY message_read_state_isolation ON message_read_state
    USING (group_id::text = current_setting('app.family_id', true));
