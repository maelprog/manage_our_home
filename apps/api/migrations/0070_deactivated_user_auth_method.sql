-- A deactivated account may be left with no way to log in (#987).
--
-- A Google sign-in on a deactivated account whose address was never
-- verified takes it over as on an active one (`auth::oauth_google`): the
-- password — possibly set by whoever registered the address first — is
-- erased. #194 still holds, so no Google identity is bound to it, and the
-- account is left with neither. The deferred trigger of 0001 (as 0016
-- left it) rejected that transaction at commit.
--
-- A row with `deactivated_at` set is now exempt, like a purged one: it
-- gets no ordinary session anyway (#289). Every other account still needs
-- a password or an OAuth identity, and since the trigger fires on every
-- UPDATE of `users`, reactivating an account with neither (clearing
-- `deactivated_at`) is rejected at commit too.
--
-- The Google identity that took the account over is kept, unbound, in
-- `users.pending_google_sub` — the one write #194 lets through on a
-- deactivated account besides the take-over itself. The reactivation
-- (`POST /admin/users/:id/reactivate`) binds it when the account has no
-- other way to log in, then clears it; the account purge clears it too.
--
-- `check_user_has_auth_method_from_identity` (on `oauth_identities`) is
-- left as 0016 wrote it: only the account purge deletes an identity, and
-- it stamps `deleted_at` in the same transaction.

CREATE OR REPLACE FUNCTION check_user_has_auth_method() RETURNS trigger AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM users u
        WHERE u.id = NEW.id
          AND (u.deleted_at IS NOT NULL
               OR u.deactivated_at IS NOT NULL
               OR u.password_hash IS NOT NULL
               OR EXISTS (
                SELECT 1 FROM oauth_identities oi WHERE oi.user_id = u.id
              ))
    ) THEN
        RAISE EXCEPTION 'user % has no auth method (password or oauth identity)', NEW.id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

ALTER TABLE users ADD COLUMN pending_google_sub TEXT;
