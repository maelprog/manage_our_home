-- #459: an email address names the same account whatever its capitals.
--
-- The API now stores and looks up every address in one form,
-- `normalize_email` (`apps/shared/src/validation/auth.rs`): surrounding
-- whitespace trimmed, every letter lowercased. This migration brings the
-- rows written before it to that form and makes the database refuse two
-- accounts whose addresses differ only by case, which `UNIQUE (email)`
-- (0001) let through: it compares bytes.
--
-- Before deploying, run on the production database:
--
--     SELECT lower(email) AS address, count(*) AS accounts
--     FROM users
--     GROUP BY lower(email)
--     HAVING count(*) > 1;
--
-- No row: the migration applies. Any row: two accounts or more answer to one
-- address, and the migration stops on them (below) rather than choose which
-- one keeps it. Merging or renaming them is a decision about people's data,
-- taken by hand before deploying again.

DO $$
DECLARE
    addresses BIGINT;
    accounts BIGINT;
BEGIN
    SELECT count(*), coalesce(sum(n), 0) INTO addresses, accounts
    FROM (
        SELECT count(*) AS n FROM users GROUP BY lower(email) HAVING count(*) > 1
    ) AS collisions;
    IF addresses > 0 THEN
        RAISE EXCEPTION
            'case-insensitive email: % address(es) shared by % accounts differing only by case',
            addresses, accounts
            USING HINT = 'List them with the query in the header of 0030_case_insensitive_email.sql and resolve them by hand before migrating.';
    END IF;
END
$$;

-- The guard, on `lower(email)` rather than `citext`: no extension to
-- install, and `email = $1` lookups keep the byte-wise `users_email_key`
-- index of 0001, which the stored form (already lowercase) makes
-- equivalent. This one only refuses what that one would let through.
-- Built before the rewrite below, not after: an `UPDATE users` queues the
-- deferred `users_has_auth_method_trigger` (0001) until commit, and
-- Postgres refuses `CREATE INDEX` on a table with pending trigger events.
CREATE UNIQUE INDEX users_email_lower_key ON users (lower(email));

UPDATE users SET email = lower(email) WHERE email <> lower(email);

UPDATE invitations
SET invited_email = lower(invited_email)
WHERE invited_email <> lower(invited_email);

-- Invitations received (0019), new version. The comparison was exact and its
-- comment said why: nothing else folded case. Now everything does. Both
-- sides are stored lowercase from this migration on; `lower()` on both
-- still keeps an invitation found by the account it names should a row ever
-- reach the table by another path. `CREATE OR REPLACE` keeps the owner and
-- the grants 0019 set (`REVOKE ... FROM PUBLIC`, `GRANT EXECUTE` to the
-- runtime role).
CREATE OR REPLACE FUNCTION account_export_received_invitations()
RETURNS TABLE (
    id UUID,
    group_id UUID,
    group_name TEXT,
    invited_by TEXT,
    invited_email TEXT,
    created_at TIMESTAMPTZ,
    expires_at TIMESTAMPTZ
)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    SELECT i.id, i.group_id, g.name, inviter.display_name, i.invited_email,
           i.created_at, i.expires_at
    FROM invitations i
    JOIN groups g ON g.id = i.group_id
    JOIN users inviter ON inviter.id = i.created_by
    JOIN users me
      ON me.id = NULLIF(current_setting('app.user_id', true), '')::uuid
    WHERE i.invited_email IS NOT NULL
      AND lower(i.invited_email) = lower(me.email)
    ORDER BY i.created_at
$$;

COMMENT ON FUNCTION account_export_received_invitations() IS
    'Invitations addressed to the email of the user of app.user_id, compared case-insensitively (#459), without their token (issue #140). Read by GET /account/export only.';
