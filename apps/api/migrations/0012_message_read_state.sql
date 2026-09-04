-- Issue #73 (dashboard) needs a real "unread messages" signal — none
-- existed (`messages` carries no per-user read marker at all). One row per
-- (family, user) with a single `last_read_at` watermark is enough to answer
-- "which messages are unread" (any `messages.created_at > last_read_at`,
-- see apps/api/src/messagerie/messages.rs::unread_messages) and is far
-- cheaper than a row per (message, user) read receipt, which this app has
-- no other use for (no per-message "seen by" UI). The trade-off has two
-- halves, both accepted v1 limitations rather than oversights:
--
--   * a message can't be individually marked unread again once the
--     watermark has passed it;
--   * the watermark has **no floor**. A single instant per (family, user)
--     cannot say "these messages were read and those were not", so
--     advancing it marks read *everything older than it* — rendered or
--     not. Opening `/messagerie` renders one page and still offers
--     "Charger les messages plus anciens", yet everything behind that link
--     becomes read. #100 settled this deliberately (reading (a): opening
--     the messagerie means "I have seen it, I start over from zero");
--     giving the marker a floor would mean paginating from it, or a
--     per-message granularity — a different data model. See
--     `read_watermark` in apps/web/src/routes/messagerie/thread.rs.
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
