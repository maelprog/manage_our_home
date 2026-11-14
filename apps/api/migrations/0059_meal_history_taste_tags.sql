-- #821: taste tags given when a recipe is marked done, kept with the meal.
-- A closed list (`recipes::taste::TasteTag`), stored under their wire
-- names; none is required, so a meal logged without them, and every meal
-- logged before this migration, has an empty list. « aime » and
-- « pas_aime » feed the suggestions' soft score (`recipes::suggestions`).
-- A household usage datum, no health datum: see the processing register,
-- treatment #4 (Recettes).
ALTER TABLE meal_history
    ADD COLUMN taste_tags TEXT[] NOT NULL DEFAULT '{}'
        CONSTRAINT meal_history_taste_tags_closed_list CHECK (
            taste_tags <@ ARRAY['ail', 'epice', 'sucre_sale', 'trop_long', 'aime', 'pas_aime']::TEXT[]
        );
