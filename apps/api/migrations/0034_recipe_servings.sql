-- #470: the number of people a recipe is written for.
--
-- Generating the grocery list from the planned recipes scales each
-- ingredient's quantity to the number of people of the selection
-- (`recipe_selections.servings`, 0033) against this base: quantity ×
-- selection / base. NULL means the recipe does not say, and its quantities
-- are then taken as written, as they are for a selection without a number
-- of people (decision of 2026-10-24). Same bound as the selection's, held
-- by `recipes::selections::SERVINGS_MAX` on the API side.

ALTER TABLE recipes
    ADD COLUMN servings INTEGER CHECK (servings BETWEEN 1 AND 50);
