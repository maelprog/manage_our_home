-- #532: spending categories, a monthly cap for the whole family and an
-- optional monthly cap per category. Set by owners and admins, the only
-- members who reach the budget (#531); the API enforces that, as for the
-- entries.
--
-- Amounts are cents, as `budget_entries.amount_cents` (0007): the API speaks
-- euros at its boundary and converts (`validation::budget::cap_to_cents`).
-- A cap is strictly positive: removing it is NULL, never 0, which would make
-- every expense an overrun. Usage (total, percentage, overrun) is computed
-- on read by `budget_summary`, not stored.

CREATE TABLE budget_categories (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id            UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    -- Trimmed by the API, 1 to 40 characters
    -- (`validation::budget::CATEGORY_NAME_MAX`).
    name                TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 40),
    -- A named key from the closed list of the budget mock-up, never a hex
    -- value: `apps/web` maps each key onto a DESIGN.md token (#533). Same
    -- list as `validation::budget::CATEGORY_COLORS`.
    color               TEXT NOT NULL
                        CHECK (color IN ('blue', 'violet', 'green', 'orange', 'ochre')),
    monthly_cap_cents   BIGINT CHECK (monthly_cap_cents > 0),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Target of the entries' composite key below.
    UNIQUE (id, group_id)
);
-- A name once per family, whatever its case: « Loisirs » and « loisirs »
-- would be two lines nobody can tell apart in the summary. The index also
-- serves `WHERE group_id = $1` through its leading column.
CREATE UNIQUE INDEX budget_categories_group_id_name_uidx
    ON budget_categories (group_id, lower(name));

ALTER TABLE budget_categories ENABLE ROW LEVEL SECURITY;
ALTER TABLE budget_categories FORCE ROW LEVEL SECURITY;
CREATE POLICY budget_categories_isolation ON budget_categories
    USING (group_id::text = current_setting('app.family_id', true));

-- An entry's category. NULL is « Sans catégorie »: entries written before
-- this migration, entries filed under none, and the entries of a deleted
-- category. The key is composite so an entry can only name a category of
-- its own family, whatever the API checks; on delete only `category_id` is
-- cleared (PostgreSQL 15+ column list), `group_id` stays.
ALTER TABLE budget_entries ADD COLUMN category_id UUID;
ALTER TABLE budget_entries
    ADD CONSTRAINT budget_entries_category_fkey
    FOREIGN KEY (category_id, group_id)
    REFERENCES budget_categories (id, group_id)
    ON DELETE SET NULL (category_id);
CREATE INDEX budget_entries_category_id_idx ON budget_entries (category_id);

-- The family's budget settings, one row per group, written on first use.
-- `monthly_cap_cents` is the cap for the whole family (NULL: none). Creating
-- a category also locks this row (`SELECT … FOR UPDATE`), so two concurrent
-- creations cannot both pass the 30-category bound.
CREATE TABLE budget_settings (
    group_id            UUID PRIMARY KEY REFERENCES groups(id) ON DELETE CASCADE,
    monthly_cap_cents   BIGINT CHECK (monthly_cap_cents > 0),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE budget_settings ENABLE ROW LEVEL SECURITY;
ALTER TABLE budget_settings FORCE ROW LEVEL SECURITY;
CREATE POLICY budget_settings_isolation ON budget_settings
    USING (group_id::text = current_setting('app.family_id', true));
