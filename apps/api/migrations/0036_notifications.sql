-- #534: the notification centre. Until now a reminder was pushed to a
-- device or emailed and nothing of it was kept: a member without a device
-- subscribed had nothing to read in the application.
--
-- One row per recipient. The events that write one (controller's decision
-- of 2026-10-21) are the `kind` values below: an agenda reminder coming due
-- (`jobs::scheduled_notifications`), a stock item nearing its expiry date
-- (`jobs::stock_expiry`), a message posted, an invitation addressed to
-- an existing account, an item added to the grocery list. Budget alerts
-- (#536) will add their own kind with a new migration.
--
-- `object_id` names the event, stock item, message, invitation or grocery
-- item the notification is about, per `kind`: no foreign key, since it
-- points into five tables, and the object may be deleted before the
-- notification goes. `body` is the short text shown in the list, built by
-- `notifications::center::body` (its user-supplied parts bounded and
-- flattened to one line); it never carries a message's content, which is
-- encrypted at rest (`messages.content`).
--
-- `actor_id` is the member whose action caused it (NULL for the jobs). The
-- body may carry their display name, so the account purge deletes the
-- notifications an account caused along with those it received
-- (`jobs::account_purge`).
--
-- `dedup_key` makes a notification at most once per recipient for the jobs,
-- which may see the same event again on a later pass (a retried reminder, an
-- item still expiring an hour later): `INSERT … ON CONFLICT DO NOTHING`
-- against the unique index below. NULL for the notifications written by a
-- request, which happen once by construction.
--
-- Deleted 30 days after `created_at`, read or not
-- (`jobs::retention_purge::NOTIFICATION_RETENTION_DAYS`, controller's
-- decision of 2026-10-21).

CREATE TABLE notifications (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    group_id    UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL
                CHECK (kind IN ('agenda_reminder', 'stock_expiry', 'message',
                                'invitation', 'grocery_item')),
    object_id   UUID,
    actor_id    UUID REFERENCES users(id) ON DELETE CASCADE,
    body        TEXT NOT NULL CHECK (char_length(body) BETWEEN 1 AND 300),
    dedup_key   TEXT,
    read_at     TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The list, newest first, and its cursor `(created_at, id)`.
CREATE INDEX notifications_user_id_created_at_idx
    ON notifications (user_id, created_at DESC, id DESC);
-- The unread count, read on every page of the application.
CREATE INDEX notifications_unread_idx
    ON notifications (user_id) WHERE read_at IS NULL;
-- The retention purge.
CREATE INDEX notifications_created_at_idx ON notifications (created_at);
-- The account purge, on the notifications an account caused.
CREATE INDEX notifications_actor_id_idx ON notifications (actor_id);
-- NULLs are distinct: the request-made notifications never conflict.
CREATE UNIQUE INDEX notifications_user_id_dedup_key_uidx
    ON notifications (user_id, dedup_key);

-- Per recipient, not per family: a member reads, marks read and (nobody
-- does yet) deletes their own notifications only, whatever family they
-- came from — an invitation's comes from a family they are not a member
-- of. Writing one is the other way round: a member's request writes the
-- notifications of the other members of the family it acts in, so the
-- INSERT is checked on the family, as every family-scoped table is. The
-- jobs write on the admin pool, which bypasses both.
ALTER TABLE notifications ENABLE ROW LEVEL SECURITY;
ALTER TABLE notifications FORCE ROW LEVEL SECURITY;
CREATE POLICY notifications_recipient_select ON notifications FOR SELECT
    USING (user_id::text = current_setting('app.user_id', true));
CREATE POLICY notifications_recipient_update ON notifications FOR UPDATE
    USING (user_id::text = current_setting('app.user_id', true));
CREATE POLICY notifications_recipient_delete ON notifications FOR DELETE
    USING (user_id::text = current_setting('app.user_id', true));
CREATE POLICY notifications_family_insert ON notifications FOR INSERT
    WITH CHECK (group_id::text = current_setting('app.family_id', true));
