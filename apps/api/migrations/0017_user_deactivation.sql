-- Issue #256. A support deactivation and a purge are two states, not one.
--
-- `POST /admin/users/:id/deactivate` used to stamp `deleted_at`, the column
-- the account purge (`src/jobs/account_purge.rs`) stamps once it has
-- anonymised a row. The purge only looks at rows where `deleted_at` is NULL,
-- so a deactivated account — email, password hash, name, Google identity
-- all intact — was never purged at all.
--
-- Controller's decision of 2026-09-28: deactivation anonymises nothing and
-- gets a column of its own; the superadmin can reactivate; an account left
-- deactivated is purged after 2 years (the CNIL's recommendation for an
-- inactive account), its holder warned by email 30 days before. `deleted_at`
-- keeps a single meaning: "purged".
ALTER TABLE users
    ADD COLUMN deactivated_at TIMESTAMPTZ,
    ADD COLUMN deactivation_notice_sent_at TIMESTAMPTZ;

COMMENT ON COLUMN users.deactivated_at IS
    'When the superadmin deactivated the account (issue #256). NULL: not deactivated, or reactivated since. The account purge anonymises the row 2 years after.';
COMMENT ON COLUMN users.deactivation_notice_sent_at IS
    'When the email warning of the coming purge of a deactivated account was sent (issue #256). Cleared on reactivation.';

-- Accounts deactivated before this migration carry `deleted_at` without
-- having been anonymised: they move to the new column. A purged row is
-- recognised by the address the purge gives it.
UPDATE users
SET deactivated_at = deleted_at,
    deleted_at = NULL
WHERE deleted_at IS NOT NULL
  AND email <> 'deleted-' || id || '@deleted.invalid';
