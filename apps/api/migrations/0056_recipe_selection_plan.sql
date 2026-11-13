-- #811: when a selected recipe is meant to be cooked.
--
-- `recipe_selections` (0033) said which recipes the family has chosen, not
-- when. `planned_on` is the day, `meal_slot` the meal of that day. Both are
-- optional and changeable at any time by any member, like `servings`
-- (controller's decision of 2026-10-24): a selection without a date stays
-- valid, as every existing row is.
--
-- A meal without a day means nothing, so the second CHECK refuses it; the
-- API answers `invalid_meal_slot` before reaching it
-- (`recipes::selections::merge_plan`). The bound on the date (at most 366
-- days from the day of the request) moves with the day, so it is the API's
-- alone.
--
-- Still one `planned` selection per recipe
-- (`recipe_selections_one_planned_idx`, 0033): planning a recipe on two
-- days is out of this change.
--
-- RLS: `recipe_selections_isolation` (0033) filters whole rows by group,
-- so it covers these columns as is. Both columns are NULL on existing
-- rows: Postgres adds them without rewriting the table.
ALTER TABLE recipe_selections
    ADD COLUMN planned_on DATE,
    ADD COLUMN meal_slot TEXT CHECK (meal_slot IN ('lunch', 'dinner')),
    ADD CONSTRAINT recipe_selections_meal_needs_day
        CHECK (meal_slot IS NULL OR planned_on IS NOT NULL);
