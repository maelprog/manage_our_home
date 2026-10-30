-- #638: who paid an expense, a member of its family or nobody (NULL). The
-- form of `/budget/new` offers the members, the signed-in one ticked, and
-- « Personne » (user's decisions of 2026-10-30).
--
-- The key is composite, onto the membership rather than onto `users`: an
-- expense can only name a member of its own family, whatever the API
-- checks, and when that membership ends — the member leaves or is removed,
-- or the account is purged (`jobs::account_purge` deletes its memberships;
-- the `users` row is anonymised, never deleted, so during the 30-day grace
-- period after `POST /account/delete` the payer stays named) — only
-- `paid_by` is cleared (PostgreSQL 15+ column list, as `category_id` in
-- 0035): the expense stays, without a payer (user's decision of
-- 2026-10-30). Entries written before this migration have none.
--
-- A current member only, then: the export reads the expenses a member paid
-- (`rgpd`), and the family is already among theirs through `group_members`,
-- so `account_export_group_ids()` (0019) needs no new branch.
--
-- RLS unchanged: the column lives on `budget_entries`, under its policy.
ALTER TABLE budget_entries ADD COLUMN paid_by UUID;
ALTER TABLE budget_entries
    ADD CONSTRAINT budget_entries_paid_by_fkey
    FOREIGN KEY (group_id, paid_by)
    REFERENCES group_members (group_id, user_id)
    ON DELETE SET NULL (paid_by);
-- Serves the membership's delete (the key's SET NULL).
CREATE INDEX budget_entries_paid_by_idx ON budget_entries (paid_by)
    WHERE paid_by IS NOT NULL;
