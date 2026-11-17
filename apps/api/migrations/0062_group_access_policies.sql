-- #863: a family's access policy — from which role its members reach each
-- restricted feature, chosen by its owner. Schema and RLS only: the API
-- that reads and writes it, and the modules that apply it, come after.
--
-- One row per (family, feature) the owner has set. No row: the default,
-- `admin`, which keeps today's behaviour — the hard-coded checks
-- (`can_modify` in each module, `google_calendar::can_configure`, the
-- budget reserved to admins and owners since #531) admit admins and owners.
--
-- `feature`, a closed list:
--   budget               the family budget
--   google_calendar      connecting or disconnecting a Google Calendar
--   stocks_edit          editing in full (beyond the quantity) or deleting
--                        another member's stock item
--   recipes_edit         editing or deleting another member's recipe
--   grocery_edit         editing or deleting another member's grocery item
--   agenda_edit          editing or deleting another member's event
--   messagerie_moderate  editing or deleting another member's message
-- `min_role`: the lowest role admitted, a `group_role` (0001): `standard`,
-- `admin` or `owner`; the enum refuses anything else.
--
-- No personal data: a family's settings, like `budget_settings`. Goes with
-- the family.
CREATE TABLE group_access_policies (
    group_id    UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    feature     TEXT NOT NULL
        CONSTRAINT group_access_policies_feature_known CHECK (feature IN (
            'budget', 'google_calendar', 'stocks_edit', 'recipes_edit',
            'grocery_edit', 'agenda_edit', 'messagerie_moderate'
        )),
    min_role    group_role NOT NULL,
    PRIMARY KEY (group_id, feature)
);

-- Read by the family's members under their family scope. Written only by
-- the API, after checking the caller owns the family; the policy, like
-- those of the other family tables, keeps every statement, writes
-- included, inside the scoped family.
ALTER TABLE group_access_policies ENABLE ROW LEVEL SECURITY;
ALTER TABLE group_access_policies FORCE ROW LEVEL SECURITY;
CREATE POLICY group_access_policies_isolation ON group_access_policies
    USING (group_id::text = current_setting('app.group_id', true));
