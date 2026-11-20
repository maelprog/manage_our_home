-- #911: a publication of the shared library (0066) carries the address of
-- the page its recipe was imported from (`recipes.source_url`, #405), which
-- the snapshot of 0066 left out (user's decision of 2026-11-20). The
-- library's page shows it as « Source : <site> »: the recipe's author is
-- the site's, and the reader can go to it.
--
-- Taken with the rest of the snapshot when the recipe is published, and
-- replaced with it when the recipe is published again: NULL for a recipe
-- typed by hand. Same bound as `recipes.source_url`
-- (`SOURCE_URL_MAX_LEN`, 2048 characters); the API keeps only an `http` or
-- `https` address (`recipes::import::valid_source_url`).
--
-- Personal data: an address the family imported a recipe from, which says
-- nothing of who published it; readable by every signed-in user, as the
-- rest of the publication, under the policies of 0066.
ALTER TABLE library_recipes
    ADD COLUMN source_url TEXT CHECK (char_length(source_url) BETWEEN 1 AND 2048);
