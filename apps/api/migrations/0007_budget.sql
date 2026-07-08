-- Epic 6: Budget.
--
-- v1 scope (architecture.md epic-scoping clarifications + docs/v1-scope.md):
-- tied to the grocery list, not a general expense tracker (rent, bills,
-- etc.). Prices are entered manually (no price lookup/scraping in v1) and
-- can optionally be associated with a grocery item once it's checked/bought.
-- `name` is denormalized from the grocery item at entry time so a budget
-- entry survives the grocery item being deleted later (SET NULL on
-- `grocery_item_id`). Cumulation per period (e.g. per month) is computed on
-- read via `date_trunc('month', spent_at)`, not stored.

CREATE TABLE budget_entries (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id            UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    created_by          UUID NOT NULL REFERENCES users(id),
    grocery_item_id     UUID REFERENCES grocery_items(id) ON DELETE SET NULL,
    name                TEXT NOT NULL,
    amount              DOUBLE PRECISION NOT NULL,
    spent_at            DATE NOT NULL DEFAULT current_date,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (amount >= 0)
);
CREATE INDEX budget_entries_group_id_idx ON budget_entries (group_id);
CREATE INDEX budget_entries_group_id_spent_at_idx ON budget_entries (group_id, spent_at);

ALTER TABLE budget_entries ENABLE ROW LEVEL SECURITY;
ALTER TABLE budget_entries FORCE ROW LEVEL SECURITY;
CREATE POLICY budget_entries_isolation ON budget_entries
    USING (group_id::text = current_setting('app.family_id', true));
