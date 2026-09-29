-- Issue #289. The holder of a deactivated account (#256) can log in to a
-- single page and ask for its reactivation.
--
-- A restricted session is what a correct login on a deactivated account
-- opens: `AuthUser` refuses it on every route, and only the "compte
-- désactivé" routes accept it (`auth::session::DeactivatedSession`). It stops
-- working once the account is reactivated or purged.
ALTER TABLE sessions
    ADD COLUMN restricted BOOLEAN NOT NULL DEFAULT false;

COMMENT ON COLUMN sessions.restricted IS
    'Opened by a login on a deactivated account (issue #289): gives access to the deactivated-account page only, and only while the account stays deactivated.';

-- One pending request per account (the primary key). The row lives only as
-- long as the request is pending: the superadmin's reactivation or refusal
-- deletes it, the audit log keeps the trace, and the account purge deletes
-- it with the rest. While it exists, the purge 2 years after the
-- deactivation is suspended (`jobs::account_purge::purge_due`), unless a
-- request was already refused since the deactivation (below).
CREATE TABLE account_reactivation_requests (
    user_id      UUID PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
    requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    message      TEXT CHECK (char_length(message) <= 1000)
);

COMMENT ON TABLE account_reactivation_requests IS
    'Pending reactivation requests of deactivated accounts (issue #289). Deleted when the superadmin reactivates or refuses, and by the account purge.';

-- Controller's decision of 2026-09-29: only the first request since the
-- deactivation suspends the purge. After a refusal the holder may ask
-- again, but the deadline runs. Cleared by a deactivation, a reactivation
-- and the purge.
ALTER TABLE users
    ADD COLUMN reactivation_refused_at TIMESTAMPTZ;

COMMENT ON COLUMN users.reactivation_refused_at IS
    'When the superadmin last refused a reactivation request since the account was deactivated (issue #289). Set: a new request no longer suspends the purge.';
