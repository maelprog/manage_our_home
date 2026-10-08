-- #405: the page a recipe was imported from. The import
-- (`POST /groups/:id/recipes/import`) reads a recipe page's schema.org
-- JSON-LD and saves nothing: the member reviews the draft, and the recipe is
-- created by the existing `POST /groups/:id/recipes`, which now accepts this
-- URL. The recipe page links to it.
--
-- NULL for a recipe typed by hand. The API accepts only an `http`/`https`
-- URL of at most 2048 characters (`recipes::import::valid_source_url`); the
-- CHECK repeats the length bound so no other writer can store more. The
-- row-level `recipes_isolation` policy already covers the column.

ALTER TABLE recipes ADD COLUMN source_url TEXT
    CHECK (char_length(source_url) <= 2048);
