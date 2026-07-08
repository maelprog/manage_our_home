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
--
-- `amount_cents` is an integer number of cents rather than DOUBLE PRECISION:
-- unlike stocks' `quantity` (household counts, not currency — see
-- 0004_stocks.sql), this column IS currency, and summing DOUBLE PRECISION
-- values in `budget_summary` would accumulate binary floating-point rounding
-- error. The API still speaks euros (f64) at the request/response boundary;
-- conversion to/from cents happens in `src/budget/entries.rs`.

CREATE TABLE budget_entries (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id            UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    created_by          UUID NOT NULL REFERENCES users(id),
    grocery_item_id     UUID REFERENCES grocery_items(id) ON DELETE SET NULL,
    name                TEXT NOT NULL,
    amount_cents        BIGINT NOT NULL,
    spent_at            DATE NOT NULL DEFAULT current_date,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (amount_cents >= 0)
);
-- No separate `(group_id)` index: `(group_id, spent_at)` already covers
-- plain `WHERE group_id = $1` lookups via its leading column.
CREATE INDEX budget_entries_group_id_spent_at_idx ON budget_entries (group_id, spent_at);
-- Enforces "set price" idempotency: at most one budget entry per grocery
-- item, so retries/double-taps on set_grocery_item_price upsert instead of
-- creating duplicate spend.
CREATE UNIQUE INDEX budget_entries_grocery_item_id_uidx ON budget_entries (grocery_item_id)
    WHERE grocery_item_id IS NOT NULL;

ALTER TABLE budget_entries ENABLE ROW LEVEL SECURITY;
ALTER TABLE budget_entries FORCE ROW LEVEL SECURITY;
CREATE POLICY budget_entries_isolation ON budget_entries
    USING (group_id::text = current_setting('app.family_id', true));
