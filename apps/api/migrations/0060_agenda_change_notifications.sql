-- #848: tell the members who see an agenda event that another member added,
-- changed, deleted or imported it. This migration only lays the schema; the
-- notifications are written, pushed and the setting exposed by the issues
-- that follow it.
--
-- The notification of such a change. `agenda_reminder` stays the reminder
-- of an event coming due. Same list as `center::Kind`.
ALTER TABLE notifications DROP CONSTRAINT notifications_kind_check;
ALTER TABLE notifications ADD CONSTRAINT notifications_kind_check
    CHECK (kind IN ('agenda_reminder', 'stock_expiry', 'message',
                    'invitation', 'grocery_item', 'budget_alert',
                    'agenda_change'));

-- Pushed as the other kinds are (0037): no payload, the service worker asks
-- which kind its device got. Same list as `push::PushKind`.
ALTER TABLE push_subscriptions DROP CONSTRAINT push_subscriptions_pending_push_kind_check;
ALTER TABLE push_subscriptions ADD CONSTRAINT push_subscriptions_pending_push_kind_check
    CHECK (pending_push_kind IN ('agenda_reminder', 'budget_alert', 'stock_expiry',
                                 'agenda_change', 'mixed'));

-- Whether the account is told of those changes, in the centre and on its
-- devices. On by default, existing accounts included. A setting held about
-- the person: exported in their profile, goes with the account.
ALTER TABLE users ADD COLUMN notify_agenda_changes BOOLEAN NOT NULL DEFAULT true;
