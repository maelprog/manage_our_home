-- #539: how long a recipe takes, in minutes — the « Temps » of the recipe
-- form, beside its name and number of people.
--
-- NULL means the recipe does not say. From one minute to a day, the bound
-- held on the API side by `recipes::crud::DURATION_MAX` and on the web
-- side by `validation::recipes::DURATION_MAX`.
--
-- RLS unchanged: the column lives on `recipes`, under its policies.
ALTER TABLE recipes
    ADD COLUMN duration_minutes INTEGER
        CHECK (duration_minutes BETWEEN 1 AND 1440);
