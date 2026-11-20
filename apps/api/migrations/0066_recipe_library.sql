-- #905: the shared recipe library. A member may publish one of their
-- family's recipes (opt-in, recipe by recipe); every signed-in user reads
-- the publications, whatever family they belong to, and none of them can
-- tell which family or which person a publication comes from (user's
-- decisions of 2026-11-17: opt-in publication, anonymous author,
-- independent copy, moderation by the superadmin).
--
-- A publication is a snapshot taken when it is published: name, method,
-- servings, duration, and the ingredients with their seasons. No photo, no
-- attachment, no source URL. It does not follow its source recipe, and a
-- family's copy of it (`recipes.library_recipe_id`) does not follow it
-- either.
--
-- Three tables:
--
-- - `library_recipes` and `library_ingredients`, the publication itself,
--   readable by anyone signed in. Neither has a `group_id`, a
--   `created_by`, or any column holding a key onto `users`, `groups`,
--   `group_members` or `recipes`: a `SELECT *` on them names nobody, so no
--   read query can leak the author by selecting one column too many;
-- - `library_recipe_publishers`, who published what, and from which of
--   their recipes: the only link from a publication to a person. Its
--   policy shows a row to its publisher alone, so another user's query
--   finds nothing there to join on. The API never returns it to anyone
--   but the publisher (their export).
--
-- Every publication has exactly one publisher row and every publisher row
-- one publication: each table holds a key onto the other, the one from
-- `library_recipes` checked at commit, so the transaction that publishes
-- writes the publication, then its publisher row, and an orphan cannot be
-- committed. Another user's publication cannot be claimed either: its
-- publisher row already holds the primary key. An
-- orphan would be a publication nobody could edit or delete, and that the
-- account purge, which finds publications through this table, would miss.
-- Deleting either row deletes the other, the ingredients with them.
--
-- Writes, under the runtime role:
-- - publishing: any signed-in user (`app.user_id` set), writing the
--   publisher row with their own id;
-- - editing and deleting a publication and its ingredients: its publisher
--   alone, through the rows of `library_recipe_publishers` their policy
--   shows them;
-- - withdrawal by the superadmin: through the admin pool, which bypasses
--   RLS, as the other `/admin/*` writes (no policy names the superadmin).
--
-- Personal data: who published what, deleted with the account
-- (`jobs::account_purge` deletes the publications of a purged account; the
-- copies families made keep their recipe, `library_recipe_id` dropping to
-- NULL), and exported to the publisher (`GET /account/export`,
-- `library_recipes`).
CREATE TABLE library_recipes (
    id               UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name             TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    instructions     TEXT CHECK (char_length(instructions) <= 20000),
    servings         INTEGER CHECK (servings BETWEEN 1 AND 50),
    duration_minutes INTEGER CHECK (duration_minutes BETWEEN 1 AND 1440),
    published_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- `position` orders the ingredients as the recipe listed them, and bounds
-- their number at 100 per publication.
CREATE TABLE library_ingredients (
    library_recipe_id UUID NOT NULL REFERENCES library_recipes(id) ON DELETE CASCADE,
    position          SMALLINT NOT NULL CHECK (position BETWEEN 0 AND 99),
    name              TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    quantity          DOUBLE PRECISION CHECK (quantity >= 0),
    unit              TEXT CHECK (char_length(unit) <= 50),
    is_optional       BOOLEAN NOT NULL DEFAULT false,
    -- As `recipe_ingredients.seasonal_months` (0005): NULL, all year.
    seasonal_months   INTEGER[]
                      CHECK (seasonal_months <@ ARRAY[1,2,3,4,5,6,7,8,9,10,11,12]),
    PRIMARY KEY (library_recipe_id, position)
);

-- `source_recipe_id`, the family recipe the publication was taken from:
-- one publication per recipe at most, and the publication outlives the
-- recipe (NULL once it is deleted).
CREATE TABLE library_recipe_publishers (
    library_recipe_id UUID PRIMARY KEY
                      REFERENCES library_recipes(id) ON DELETE CASCADE,
    user_id           UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    source_recipe_id  UUID UNIQUE REFERENCES recipes(id) ON DELETE SET NULL
);
-- Serves the purge and the export, which look publications up by person.
CREATE INDEX library_recipe_publishers_user_id_idx
    ON library_recipe_publishers (user_id);

ALTER TABLE library_recipes
    ADD CONSTRAINT library_recipes_publisher_fkey
        FOREIGN KEY (id) REFERENCES library_recipe_publishers (library_recipe_id)
        ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED;

-- Search by name and by ingredient: case-folded, prefix-capable. Both
-- columns are bounded above (200 characters), so an entry always fits a
-- B-tree page.
CREATE INDEX library_recipes_name_idx
    ON library_recipes (lower(name) text_pattern_ops);
CREATE INDEX library_ingredients_name_idx
    ON library_ingredients (lower(name) text_pattern_ops);

-- A family's copy of a publication: shown as « Issue de la bibliothèque ».
-- No synchronisation; the copy stays when the publication goes.
ALTER TABLE recipes
    ADD COLUMN library_recipe_id UUID
        REFERENCES library_recipes(id) ON DELETE SET NULL;
-- Serves the `SET NULL` when a publication is deleted.
CREATE INDEX recipes_library_recipe_id_idx
    ON recipes (library_recipe_id) WHERE library_recipe_id IS NOT NULL;

ALTER TABLE library_recipe_publishers ENABLE ROW LEVEL SECURITY;
ALTER TABLE library_recipe_publishers FORCE ROW LEVEL SECURITY;
CREATE POLICY library_recipe_publishers_self ON library_recipe_publishers
    USING (user_id::text = current_setting('app.user_id', true));

-- Signed in: `app.user_id` set and not empty (a setting once set in a
-- session reads back as '' after its transaction).
ALTER TABLE library_recipes ENABLE ROW LEVEL SECURITY;
ALTER TABLE library_recipes FORCE ROW LEVEL SECURITY;
CREATE POLICY library_recipes_read ON library_recipes
    FOR SELECT
    USING (current_setting('app.user_id', true) <> '');
CREATE POLICY library_recipes_publish ON library_recipes
    FOR INSERT
    WITH CHECK (current_setting('app.user_id', true) <> '');
CREATE POLICY library_recipes_publisher_update ON library_recipes
    FOR UPDATE
    USING (EXISTS (SELECT 1 FROM library_recipe_publishers p
                   WHERE p.library_recipe_id = library_recipes.id))
    WITH CHECK (EXISTS (SELECT 1 FROM library_recipe_publishers p
                        WHERE p.library_recipe_id = library_recipes.id));
CREATE POLICY library_recipes_publisher_delete ON library_recipes
    FOR DELETE
    USING (EXISTS (SELECT 1 FROM library_recipe_publishers p
                   WHERE p.library_recipe_id = library_recipes.id));

-- The `EXISTS` above and below read `library_recipe_publishers` under its
-- own policy: they find the caller's rows only.
ALTER TABLE library_ingredients ENABLE ROW LEVEL SECURITY;
ALTER TABLE library_ingredients FORCE ROW LEVEL SECURITY;
CREATE POLICY library_ingredients_read ON library_ingredients
    FOR SELECT
    USING (current_setting('app.user_id', true) <> '');
CREATE POLICY library_ingredients_publisher_write ON library_ingredients
    FOR ALL
    USING (EXISTS (SELECT 1 FROM library_recipe_publishers p
                   WHERE p.library_recipe_id = library_ingredients.library_recipe_id))
    WITH CHECK (EXISTS (SELECT 1 FROM library_recipe_publishers p
                        WHERE p.library_recipe_id = library_ingredients.library_recipe_id));
