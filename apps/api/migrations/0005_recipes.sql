-- Epic 4: Recipes (rule-based suggestion algorithm, no ML).
--
-- v1 scope (architecture.md epic-scoping clarifications + idea.md): a
-- family-scoped recipe book with a structured ingredient list per recipe
-- (needed by the future Grocery-list epic), plus a `meal_history` log used
-- to drive variety (don't resuggest what was eaten in the last 2 weeks) and
-- a per-ingredient seasonality hint used to favor in-season ingredients.
-- The suggestion algorithm itself is pure Rust (src/recipes/suggestions.rs),
-- scoring recipes against current stock + meal_history + season — no ML,
-- room to swap in an Ollama-backed suggestion later per architecture.md.

CREATE TABLE recipes (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id        UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    created_by      UUID NOT NULL REFERENCES users(id),
    name            TEXT NOT NULL,
    instructions    TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX recipes_group_id_idx ON recipes (group_id);

ALTER TABLE recipes ENABLE ROW LEVEL SECURITY;
ALTER TABLE recipes FORCE ROW LEVEL SECURITY;
CREATE POLICY recipes_isolation ON recipes
    USING (group_id::text = current_setting('app.family_id', true));

-- `group_id` is denormalized here (rather than joined through `recipes`) so
-- RLS can enforce isolation directly on this table too, consistent with the
-- "no RLS gap" posture used across every tenant-scoped table.
CREATE TABLE recipe_ingredients (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    recipe_id       UUID NOT NULL REFERENCES recipes(id) ON DELETE CASCADE,
    group_id        UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    quantity        DOUBLE PRECISION,
    unit            TEXT,
    is_optional     BOOLEAN NOT NULL DEFAULT false,
    -- Months (1-12) this ingredient is in season. NULL means available
    -- year-round (e.g. pasta, salt) and never penalized/bonused for season.
    seasonal_months INTEGER[],
    CHECK (quantity IS NULL OR quantity >= 0)
);
CREATE INDEX recipe_ingredients_recipe_id_idx ON recipe_ingredients (recipe_id);
CREATE INDEX recipe_ingredients_group_id_idx ON recipe_ingredients (group_id);

ALTER TABLE recipe_ingredients ENABLE ROW LEVEL SECURITY;
ALTER TABLE recipe_ingredients FORCE ROW LEVEL SECURITY;
CREATE POLICY recipe_ingredients_isolation ON recipe_ingredients
    USING (group_id::text = current_setting('app.family_id', true));

-- Logs when a recipe was actually cooked/eaten by the family, feeding the
-- "variety" rule (idea.md: suggestions should vary based on what was eaten
-- in the last 2 weeks).
CREATE TABLE meal_history (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id        UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    recipe_id       UUID NOT NULL REFERENCES recipes(id) ON DELETE CASCADE,
    eaten_on        DATE NOT NULL DEFAULT CURRENT_DATE,
    created_by      UUID NOT NULL REFERENCES users(id),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX meal_history_group_id_idx ON meal_history (group_id);
CREATE INDEX meal_history_recipe_id_idx ON meal_history (recipe_id);

ALTER TABLE meal_history ENABLE ROW LEVEL SECURITY;
ALTER TABLE meal_history FORCE ROW LEVEL SECURITY;
CREATE POLICY meal_history_isolation ON meal_history
    USING (group_id::text = current_setting('app.family_id', true));
