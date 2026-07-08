-- Epic 5: Grocery list.
--
-- v1 scope (architecture.md epic-scoping clarifications + docs/v1-scope.md):
-- one shared list per family, fed by three sources: `missing_ingredients`
-- emitted by the Recipes suggestion algorithm, `low_stock` items from
-- Stocks, and manual entries added directly by members. `source` records
-- which of the three created the row (purely informational — all three
-- behave identically once created), and `source_recipe_id` is kept for
-- provenance when a row was generated from a recipe's missing ingredients
-- (SET NULL on recipe deletion so the grocery item survives even if the
-- recipe is later removed).

CREATE TABLE grocery_items (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id            UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    created_by          UUID NOT NULL REFERENCES users(id),
    name                TEXT NOT NULL,
    quantity            DOUBLE PRECISION,
    unit                TEXT,
    checked             BOOLEAN NOT NULL DEFAULT false,
    source              TEXT NOT NULL DEFAULT 'manual',
    source_recipe_id    UUID REFERENCES recipes(id) ON DELETE SET NULL,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (quantity IS NULL OR quantity >= 0),
    CHECK (source IN ('manual', 'recipe', 'low_stock'))
);
CREATE INDEX grocery_items_group_id_idx ON grocery_items (group_id);

ALTER TABLE grocery_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE grocery_items FORCE ROW LEVEL SECURITY;
CREATE POLICY grocery_items_isolation ON grocery_items
    USING (group_id::text = current_setting('app.family_id', true));
