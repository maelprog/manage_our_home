-- A purged account keeps no way to log in (#139).
--
-- The account purge (`src/jobs/account_purge.rs`) anonymises the `users`
-- row: it clears `password_hash`, deletes the account's
-- `oauth_identities` and stamps `deleted_at`. The two deferred triggers of
-- 0001 require every user to keep a password or an OAuth identity, so they
-- rejected that transaction at commit and no account could ever be purged.
-- A row with `deleted_at` set is exempt: it is no longer an account anyone
-- logs into (`AuthUser` refuses it), only the anonymous author of the
-- content its groups kept.

CREATE OR REPLACE FUNCTION check_user_has_auth_method() RETURNS trigger AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM users u
        WHERE u.id = NEW.id
          AND (u.deleted_at IS NOT NULL
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

CREATE OR REPLACE FUNCTION check_user_has_auth_method_from_identity() RETURNS trigger AS $$
DECLARE
    target_user_id UUID;
BEGIN
    target_user_id := COALESCE(NEW.user_id, OLD.user_id);
    IF NOT EXISTS (
        SELECT 1 FROM users u
        WHERE u.id = target_user_id
          AND (u.deleted_at IS NOT NULL
               OR u.password_hash IS NOT NULL
               OR EXISTS (
                SELECT 1 FROM oauth_identities oi WHERE oi.user_id = u.id
              ))
    ) THEN
        RAISE EXCEPTION 'user % has no auth method (password or oauth identity)', target_user_id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;
