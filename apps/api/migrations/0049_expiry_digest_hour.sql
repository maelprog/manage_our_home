-- #700: the daily expiry recap (#566) goes at the hour each member chose,
-- not at 9:00 for everyone.
--
-- `expiry_digest_hour` is that hour, Europe/Paris, 0 to 23; 9 by default,
-- the hour #566 sent at. Set on `/account/notifications`
-- (`PUT /account/notifications`).
--
-- `expiry_digest_sent_on` is the Paris day of the member's last recap pass,
-- set in the transaction that writes their notifications, whether there was
-- anything to announce or not: a member gets at most one recap a day, a
-- restarted process or a second replica included — the day used to be kept
-- in the process's memory. NULL: no pass yet.
ALTER TABLE users
    ADD COLUMN expiry_digest_hour SMALLINT NOT NULL DEFAULT 9
        CHECK (expiry_digest_hour BETWEEN 0 AND 23),
    ADD COLUMN expiry_digest_sent_on DATE;

-- What a recap announced, per member: two members of one family at two
-- hours each get the rows due at theirs. A row is due for a member again
-- only once its date differs from the one announced to them; the row
-- marked on `stock_items` (`expiry_notified_on`, 0048) could only tell
-- whether one recap, anyone's, had announced it. No personal data beyond
-- the link between an account and a family's item: goes with either, and
-- with the account's purge (`jobs::account_purge`).
CREATE TABLE stock_expiry_announcements (
    user_id       UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    stock_item_id UUID NOT NULL REFERENCES stock_items(id) ON DELETE CASCADE,
    -- The item's `expires_on` this member's recap announced.
    expires_on    DATE NOT NULL,
    PRIMARY KEY (user_id, stock_item_id)
);
-- The cascade from a deleted item.
CREATE INDEX stock_expiry_announcements_stock_item_id_idx
    ON stock_expiry_announcements (stock_item_id);

-- Written and read by the jobs only, on the admin pool, which bypasses RLS.
-- A member's request could read their own rows only.
ALTER TABLE stock_expiry_announcements ENABLE ROW LEVEL SECURITY;
ALTER TABLE stock_expiry_announcements FORCE ROW LEVEL SECURITY;
CREATE POLICY stock_expiry_announcements_own ON stock_expiry_announcements
    USING (user_id::text = current_setting('app.user_id', true));

-- What 0048 marked, carried over to every active member of the row's
-- family, whom #566's single recap went to; then the column goes.
INSERT INTO stock_expiry_announcements (user_id, stock_item_id, expires_on)
SELECT gm.user_id, s.id, s.expiry_notified_on
FROM stock_items s
JOIN group_members gm ON gm.group_id = s.group_id
JOIN users u ON u.id = gm.user_id
WHERE s.expiry_notified_on IS NOT NULL
  AND u.deactivated_at IS NULL AND u.deleted_at IS NULL;

ALTER TABLE stock_items DROP COLUMN expiry_notified_on;
