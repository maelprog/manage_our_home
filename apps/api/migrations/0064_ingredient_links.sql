-- #903: a family's manual links from an article's name to an ingredient
-- category, for the articles no category recognizes (typed by hand, or
-- scanned without a useful Open Food Facts category, under a name the
-- taxonomy does not know): « mon p'tit plaisir » → crème fraîche.
--
-- `name_key` is the article's name as `stocks::ingredient_links::link_key`
-- reads it (case, plurals, apostrophes and the like folded, accents kept):
-- one link holds for every stock line of that name, present and to come.
-- No key onto `stock_items`: deleting an article leaves its link. `name`
-- is the name as the last link set for it wrote it, for display. `tag` is
-- an Open Food Facts category id, checked by the API against the taxonomy
-- embedded in the binary (no table to point at); one the taxonomy later
-- drops gives no category.
--
-- `created_by`, who set the link last: any member may set or replace one,
-- its creator or an admin or the owner may delete it (the API checks). The
-- key is onto the membership, not onto `users` (as `budget_entries.paid_by`,
-- 0039): the link can only name a member of its own family, and when that
-- membership ends — the member leaves or is removed, or the account is
-- purged (`jobs::account_purge` deletes its memberships) — the member's
-- links go with it. Deleting the family deletes them too.
--
-- A family's matching setting, no personal data beyond who set it, deleted
-- with the membership: not exported (`rgpd_flow.rs` states why). At most
-- `LINKS_MAX` (200) per family, counted by the API under an advisory lock.
CREATE TABLE ingredient_links (
    group_id    UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    name_key    TEXT NOT NULL CHECK (name_key <> ''),
    name        TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    tag         TEXT NOT NULL CHECK (tag <> ''),
    created_by  UUID NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, name_key),
    CONSTRAINT ingredient_links_created_by_fkey
        FOREIGN KEY (group_id, created_by)
        REFERENCES group_members (group_id, user_id)
        ON DELETE CASCADE
);
-- Serves the membership's delete (the key's CASCADE).
CREATE INDEX ingredient_links_created_by_idx ON ingredient_links (group_id, created_by);

ALTER TABLE ingredient_links ENABLE ROW LEVEL SECURITY;
ALTER TABLE ingredient_links FORCE ROW LEVEL SECURITY;
CREATE POLICY ingredient_links_isolation ON ingredient_links
    USING (group_id::text = current_setting('app.group_id', true));
