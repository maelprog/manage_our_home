-- #853: the settings of `/account/notifications` become switches, each
-- saved as soon as it changes.
--
-- The agenda changes (#848) are three switches: the additions, the changes
-- and the deletions, each on by default. An account keeps for all three
-- what it had chosen for the one setting it replaces. An import (#852),
-- which adds or changes events, is told to an account that keeps either
-- of the first two on (`agenda::change_notices`). Held about the person as
-- the one before: exported in their profile, goes with the account.
--
-- No UPDATE: a row written in this transaction would leave trigger events
-- pending on `users`, and Postgres refuses any further ALTER of it. The
-- old column is renamed into the first switch, and the other two take its
-- values by a rewrite (`USING`).
ALTER TABLE users RENAME COLUMN notify_agenda_changes TO notify_agenda_added;
ALTER TABLE users
    ADD COLUMN notify_agenda_changed BOOLEAN NOT NULL DEFAULT true,
    ADD COLUMN notify_agenda_deleted BOOLEAN NOT NULL DEFAULT true;
ALTER TABLE users
    ALTER COLUMN notify_agenda_changed TYPE BOOLEAN USING notify_agenda_added,
    ALTER COLUMN notify_agenda_deleted TYPE BOOLEAN USING notify_agenda_added;

-- The reminders are two switches, by notification and by email: both off
-- is `none`, the reminders reach the notification centre alone. The
-- constraint is the one 0020 declared with the column.
ALTER TABLE users DROP CONSTRAINT users_reminder_channel_check;
ALTER TABLE users ADD CONSTRAINT users_reminder_channel_check
    CHECK (reminder_channel IN ('push', 'email', 'both', 'none'));

-- The daily expiry recap (#700) is a switch: no hour, no recap. Its CHECK
-- (0049) holds for the hours that remain; NULL passes it.
ALTER TABLE users ALTER COLUMN expiry_digest_hour DROP NOT NULL;
