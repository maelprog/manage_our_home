-- #566: the daily recap of the stock items entering their expiry window.
-- Once a day, at 9:00 Europe/Paris, `jobs::stock_expiry_recap` gathers per
-- family the items of quantity > 0 whose « à consommer jusqu'au » date is
-- at most `EXPIRY_SOON_DAYS` days ahead, expired ones included, and that
-- were not in a recap yet for that date; it writes one notification to
-- each member (`notifications`, kind `stock_expiry`, 0036) and pushes the
-- devices of those whose channel includes notifications.
--
-- `expiry_notified_on` is the `expires_on` the last recap announced, set in
-- the transaction that writes the notifications. A row is due again only
-- once its date changes: a second pass the same day finds nothing, and a
-- date moved to another day close by is announced again. NULL: never in a
-- recap. Kept on the row rather than as a `dedup_key` of `notifications`,
-- whose rows go after 30 days (`NOTIFICATION_RETENTION_DAYS`), while an
-- expired item may stay in the stock much longer. No personal data: a date
-- the server wrote, covered by the row's `stock_items_isolation` policy;
-- goes with the row.
ALTER TABLE stock_items ADD COLUMN expiry_notified_on DATE;

-- The recap is pushed as the reminders and the budget alerts are (0037):
-- no payload, so the service worker asks which kind of push its device got
-- and shows that kind's fixed text. Same list as `push::PushKind`.
ALTER TABLE push_subscriptions DROP CONSTRAINT push_subscriptions_pending_push_kind_check;
ALTER TABLE push_subscriptions ADD CONSTRAINT push_subscriptions_pending_push_kind_check
    CHECK (pending_push_kind IN ('agenda_reminder', 'budget_alert', 'stock_expiry', 'mixed'));
