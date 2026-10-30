-- #537: a family's accent colour, chosen by its owner or admins
-- (`PATCH /groups/:id`) and applied to its theme by apps/web (#538).
--
-- A closed list of eight identifiers, the same as
-- `manage_our_home_shared::validation::groups::Accent`: a free hexadecimal
-- colour was ruled out because the contrast of the theme would no longer be
-- guaranteed. Every existing family gets the default, `sarcelle`, the
-- accent the theme had before the choice existed.
--
-- RLS unchanged: the column lives on `groups`, under its policies.
ALTER TABLE groups ADD COLUMN accent TEXT NOT NULL DEFAULT 'sarcelle'
    CONSTRAINT groups_accent_check CHECK (accent IN (
        'sarcelle', 'ocean', 'prune', 'foret',
        'terracotta', 'framboise', 'ardoise', 'miel'
    ));
