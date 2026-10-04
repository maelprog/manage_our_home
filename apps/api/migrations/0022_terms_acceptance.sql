-- Issue #319. The CGU (`docs/terms-of-service.md`) are accepted by ticking a
-- box — at registration, or, for an account opened through Google or before
-- this migration, on the page its session is held at until it does — and the
-- acceptance is kept: which version, and when.
--
-- The version is the CGU's `Version en vigueur` line, mirrored by
-- `validation::auth::TERMS_VERSION`: a date, `YYYY-MM-DD`, that moves only
-- with a substantial modification. A member who accepted an earlier version
-- is told on the home page until they acknowledge the new one, which then
-- replaces the pair below.
--
-- NULL (both together): no acceptance on file — accounts created before this
-- migration, and accounts created through Google sign-in, which never sees
-- the registration form.
ALTER TABLE users
    ADD COLUMN terms_accepted_version TEXT,
    ADD COLUMN terms_accepted_at TIMESTAMPTZ,
    ADD CONSTRAINT users_terms_acceptance_complete
        CHECK ((terms_accepted_version IS NULL) = (terms_accepted_at IS NULL));

COMMENT ON COLUMN users.terms_accepted_version IS
    'Version of the CGU the account holder last accepted (issue #319): the date of the CGU''s "Version en vigueur" line. NULL: no acceptance on file.';
COMMENT ON COLUMN users.terms_accepted_at IS
    'When the account holder accepted terms_accepted_version (issue #319). NULL: no acceptance on file.';
