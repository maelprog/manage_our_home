-- #822: the opinion given when a recipe is marked done becomes one choice
-- and a free note, in place of #821's closed list of tags (0059).
--
-- `taste`: « Aimé », « Neutre » or « Pas aimé » (`recipes::taste::Taste`),
-- under their wire names; « neutre » by default, so a meal logged without
-- one, or by « log a meal », changes no score. `taste_note`: the member's
-- own words, trimmed by the API, absent rather than empty, at most 200
-- characters (`TASTE_NOTE_MAX_CHARS`). A household usage datum written by
-- a member, no health datum: see the processing register, treatment #4
-- (Recettes).
ALTER TABLE meal_history
    ADD COLUMN taste TEXT NOT NULL DEFAULT 'neutre'
        CONSTRAINT meal_history_taste_known CHECK (taste IN ('aime', 'neutre', 'pas_aime')),
    ADD COLUMN taste_note TEXT
        CONSTRAINT meal_history_taste_note_bounded CHECK (char_length(taste_note) BETWEEN 1 AND 200);

-- A verdict of 0059 carries over; both at once, as 0059 let through, said
-- nothing and becomes « neutre ». The other tags (ail, épicé, sucré-salé,
-- trop long) scored nothing and have no place left.
UPDATE meal_history SET taste = CASE
    WHEN 'aime' = ANY(taste_tags) AND NOT 'pas_aime' = ANY(taste_tags) THEN 'aime'
    WHEN 'pas_aime' = ANY(taste_tags) AND NOT 'aime' = ANY(taste_tags) THEN 'pas_aime'
    ELSE 'neutre'
END
WHERE taste_tags <> '{}';

ALTER TABLE meal_history DROP COLUMN taste_tags;
