-- #536: the budget's cap alerts. An expense that takes the month's spending
-- of the family to 80 % of its cap, past its cap, or past the cap of its
-- category writes a notification (`notifications`, 0036) to the owners and
-- the admins of the family, the members who reach the budget (#531).
--
-- The two switches of the settings page (#533), one row per family in
-- `budget_settings` (0035): the alerts on the family's cap (80 % and past
-- it) and those on the categories' caps. On for every family (user's
-- decision of 2026-10-30), the families without a row included: the API
-- reads a missing row as the defaults below.
ALTER TABLE budget_settings
    ADD COLUMN global_cap_alerts   BOOLEAN NOT NULL DEFAULT true,
    ADD COLUMN category_cap_alerts BOOLEAN NOT NULL DEFAULT true;

-- The alerts already sent: at most one per threshold, per category and per
-- month. Kept here rather than as a `dedup_key` of `notifications`, whose
-- rows go after 30 days (`NOTIFICATION_RETENTION_DAYS`) — before the end of
-- a 31-day month: an alert purged then would be sent again on a second
-- crossing that month. `category_id` NULL is the family's cap; `month` is the
-- first day of the month in Europe/Paris. No personal data: a family, a
-- month, a category and a threshold. Goes with the family, and with the
-- category (a category deleted and created again is a new id anyway).
CREATE TABLE budget_alerts (
    group_id    UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    month       DATE NOT NULL CHECK (extract(day FROM month) = 1),
    category_id UUID,
    -- 80: 80 % of the family's cap reached; 100: a cap exceeded. A
    -- category has the second only.
    threshold   SMALLINT NOT NULL CHECK (threshold IN (80, 100)),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (category_id IS NULL OR threshold = 100),
    FOREIGN KEY (category_id, group_id)
        REFERENCES budget_categories (id, group_id) ON DELETE CASCADE
);
-- NULLS NOT DISTINCT: the family's cap (NULL) is one scope, not one per row.
CREATE UNIQUE INDEX budget_alerts_once_uidx
    ON budget_alerts (group_id, month, category_id, threshold) NULLS NOT DISTINCT;
-- The category's cascade.
CREATE INDEX budget_alerts_category_id_idx ON budget_alerts (category_id);

ALTER TABLE budget_alerts ENABLE ROW LEVEL SECURITY;
ALTER TABLE budget_alerts FORCE ROW LEVEL SECURITY;
CREATE POLICY budget_alerts_isolation ON budget_alerts
    USING (group_id::text = current_setting('app.family_id', true));

-- The notification of an alert. Same list as `center::Kind`.
ALTER TABLE notifications DROP CONSTRAINT notifications_kind_check;
ALTER TABLE notifications ADD CONSTRAINT notifications_kind_check
    CHECK (kind IN ('agenda_reminder', 'stock_expiry', 'message',
                    'invitation', 'grocery_item', 'budget_alert'));
