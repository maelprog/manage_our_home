-- Epic 3: Stocks (family pantry/fridge inventory).
--
-- v1 scope (architecture.md epic-scoping clarifications): manual entry
-- only, no scan/OCR (that's a future epic on top of this one). The reorder
-- threshold is defined per article and shared at the family level, not
-- per-user, so it lives directly on the row rather than in a per-member
-- preferences table.

CREATE TABLE stock_items (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id            UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    created_by          UUID NOT NULL REFERENCES users(id),
    name                TEXT NOT NULL,
    category            TEXT,
    -- DOUBLE PRECISION rather than NUMERIC: the `sqlx` build here isn't
    -- compiled with the `bigdecimal`/`rust_decimal` feature (see other
    -- migrations, which stick to INTEGER/BIGINT), and quantity precision
    -- needs are low (household pantry counts, not currency).
    quantity            DOUBLE PRECISION NOT NULL DEFAULT 0,
    unit                TEXT NOT NULL DEFAULT 'unit',
    -- Shared per-article threshold (architecture.md): when quantity drops to
    -- or below this, the item is considered low stock. NULL means no
    -- threshold is tracked for this article.
    reorder_threshold   DOUBLE PRECISION,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (quantity >= 0),
    CHECK (reorder_threshold IS NULL OR reorder_threshold >= 0)
);
CREATE INDEX stock_items_group_id_idx ON stock_items (group_id);

ALTER TABLE stock_items ENABLE ROW LEVEL SECURITY;
ALTER TABLE stock_items FORCE ROW LEVEL SECURITY;
CREATE POLICY stock_items_isolation ON stock_items
    USING (group_id::text = current_setting('app.family_id', true));
