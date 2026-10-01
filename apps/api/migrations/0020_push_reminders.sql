-- Issue #306. Reminders go out by notification by default, by email as an
-- option, or both.
--
-- The channel is the recipient's, not the reminder's: one choice per
-- account, read by the reminder worker when a notification falls due
-- (`jobs::scheduled_notifications`). A member who switches to email gets
-- the reminders already set by email too, which is what the warning shown
-- to a member without notifications tells them to do.
--
-- Existing accounts keep email, the only channel they knew; a new account
-- starts on notifications. Hence the default written twice: the first fills
-- the rows already there, the second is what an INSERT gets from now on.
ALTER TABLE users
    ADD COLUMN reminder_channel TEXT NOT NULL DEFAULT 'email'
        CHECK (reminder_channel IN ('push', 'email', 'both'));
ALTER TABLE users ALTER COLUMN reminder_channel SET DEFAULT 'push';

-- `event_reminders.channel` (0002) only ever held 'email'. With the channel
-- decided per account, a per-reminder value would say 'email' for a
-- reminder that goes out as a notification — in the art. 15 export among
-- other places. It goes, rather than be widened into a second, contradicting
-- source.
ALTER TABLE event_reminders DROP COLUMN channel;

-- One row per device that accepted notifications for an account: the push
-- service's endpoint for it (Web Push, RFC 8030). Account-level, like
-- `sessions`: not family-scoped, no RLS; every read filters on `user_id`.
--
-- Nothing else of the subscription is kept. Pushes carry no payload
-- (`notifications::push`), so the browser's encryption keys (`p256dh`,
-- `auth`) would never be used.
--
-- `platform` leaves room for the native pushes of the future mobile
-- application (APNs, FCM), whose device tokens are not URLs; only 'web'
-- exists today.
--
-- An endpoint is unique: the same device subscribed again, under another
-- account that signed in on it, moves to that account
-- (`notifications::preferences::subscribe`). An account keeps 50 devices at
-- most (controller's decision of 2026-10-01): the 51st replaces the one
-- least recently registered or delivered to — `last_seen_at` (the page
-- registers the device again on each visit) or `last_success_at`, the later.
-- A row goes when its push service answers 404 or 410 (expired, or
-- withdrawn by the browser), when it has failed at least
-- `notifications::push::MAX_CONSECUTIVE_FAILURES` times in a row over at
-- least `MIN_FAILING_DAYS` (`consecutive_failures`, `failing_since`; a
-- delivery resets both), when the member removes their devices, and with
-- the account (`jobs::account_purge`).
CREATE TABLE push_subscriptions (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id              UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    platform             TEXT NOT NULL DEFAULT 'web' CHECK (platform = 'web'),
    endpoint             TEXT NOT NULL UNIQUE,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_success_at      TIMESTAMPTZ,
    consecutive_failures INTEGER NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),
    failing_since        TIMESTAMPTZ
);
CREATE INDEX push_subscriptions_user_id_idx ON push_subscriptions (user_id);
