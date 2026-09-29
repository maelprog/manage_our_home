-- Issue #140. The account export must reach what a person wrote in a group
-- they have since left.
--
-- `leave_group` and `remove_member` delete the `group_members` row and
-- nothing else: events, messages, stock items… written in that group stay
-- in it. `GET /account/export` found its groups through that row, so all of
-- it silently dropped out of the export, although it is still held and
-- still names its author — against art. 15 and against the privacy
-- policy's promise of the whole of the data.
--
-- Every family-scoped table is under a forced RLS policy keyed on
-- `app.family_id`: the runtime role cannot even learn which groups hold
-- such rows without being told the group first. This function answers
-- that one question, and nothing more: the ids of the groups where a row
-- names the caller (`app.user_id`, the same setting the RLS policies of
-- 0014 read). It returns no content. The export then reads each group
-- through `scoped_tx`, under the ordinary policies, filtered on the
-- caller's own rows.
--
-- `SECURITY DEFINER` runs it as its owner, the migration role, which
-- carries `BYPASSRLS` (apps/api/README.md) — the only way to look across
-- families for a role that does not. `search_path` is pinned, `pg_temp`
-- last: Postgres searches the session's temporary schema *first* unless the
-- path names it, and any role may create a TEMP table or view, so without
-- that last entry a caller could shadow `users` or `groups` with an object
-- of their own and have it read with the owner's rights. The caller is read from
-- the setting rather than taken as an argument, so it answers only for the
-- user the request is scoped to, like every policy it stands in for.
--
-- The list follows every column that references `users(id)` in a
-- family-scoped table. Reminders and their scheduled notifications hang
-- off `events.created_by` and need no line of their own.

CREATE FUNCTION account_export_group_ids() RETURNS SETOF UUID
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    WITH me AS (
        SELECT NULLIF(current_setting('app.user_id', true), '')::uuid AS id
    )
    SELECT g.id FROM groups g, me WHERE g.created_by = me.id
    UNION SELECT gm.group_id FROM group_members gm, me WHERE gm.user_id = me.id
    UNION SELECT e.group_id FROM events e, me WHERE e.created_by = me.id
    UNION SELECT e.group_id FROM event_assignees ea JOIN events e ON e.id = ea.event_id, me
          WHERE ea.user_id = me.id
    UNION SELECT e.group_id FROM event_attachments a JOIN events e ON e.id = a.event_id, me
          WHERE a.uploaded_by = me.id
    UNION SELECT e.group_id FROM event_occurrence_completions c JOIN events e ON e.id = c.event_id, me
          WHERE c.completed_by = me.id
    UNION SELECT s.group_id FROM stock_items s, me WHERE s.created_by = me.id
    UNION SELECT r.group_id FROM recipes r, me WHERE r.created_by = me.id
    UNION SELECT m.group_id FROM meal_history m, me WHERE m.created_by = me.id
    UNION SELECT gi.group_id FROM grocery_items gi, me WHERE gi.created_by = me.id
    UNION SELECT b.group_id FROM budget_entries b, me WHERE b.created_by = me.id
    UNION SELECT m.group_id FROM messages m, me WHERE m.created_by = me.id
    UNION SELECT r.group_id FROM message_read_state r, me WHERE r.user_id = me.id
    UNION SELECT c.group_id FROM calendar_imports c, me WHERE c.created_by = me.id
    UNION SELECT i.group_id FROM invitations i, me WHERE i.created_by = me.id
$$;

COMMENT ON FUNCTION account_export_group_ids() IS
    'Ids of the groups holding a row that names the user of app.user_id, current member or not (issue #140). Read by GET /account/export only; returns no content.';

-- Invitations received. An invitation addressed to the person's email by a
-- member of a group they do not belong to is data about them (art. 15), yet
-- it names them by address, not by id, and sits under the `invitations`
-- policy, which needs the group or the token. This second function returns
-- those invitations themselves and nothing else of the group: which group
-- (id and name) and who sent it — both already in the invitation email
-- (art. 14 information) — and when. Not the token: it is the link, and
-- still a key to the group while pending. Accepting an invitation deletes
-- it, so what remains is pending or expired and not yet purged.
--
-- Matched exactly, as everywhere else an address is compared: registration
-- refuses a duplicate on `email = $1` and login looks the account up the
-- same way (`auth::register`, `auth::login`), with no case folding. An
-- invitation typed with other capitals names, for this service, another
-- address.

CREATE FUNCTION account_export_received_invitations()
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
      AND i.invited_email = me.email
    ORDER BY i.created_at
$$;

COMMENT ON FUNCTION account_export_received_invitations() IS
    'Invitations addressed to the email of the user of app.user_id, without their token (issue #140). Read by GET /account/export only.';

-- Who may call them. A function is executable by PUBLIC by default; these
-- two look across families, so they are not. They go to the role that
-- serves requests under RLS, whatever its name (`app_role` in
-- apps/api/README.md, another in a given deployment), recognised by what
-- the README grants it: it reads *and writes* the family tables (`SELECT`
-- and `INSERT` on `events`, granted to it by name) without bypassing RLS.
-- Excluded:
--   * superusers and `BYPASSRLS` roles — they need no grant;
--   * members of `pg_read_all_data` / `pg_write_all_data` — a read-only or
--     reporting role holds `SELECT` through them without being the API;
--   * roles whose privilege only comes through PUBLIC or another role —
--     `has_table_privilege` would count those; the ACL entry below does not.
-- A runtime role created after this migration gets them from the
-- `GRANT EXECUTE` line the README prescribes with its table grants.
REVOKE EXECUTE ON FUNCTION account_export_group_ids() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION account_export_received_invitations() FROM PUBLIC;

DO $$
DECLARE
    runtime_role NAME;
BEGIN
    FOR runtime_role IN
        SELECT r.rolname FROM pg_roles r
        WHERE NOT r.rolsuper
          AND NOT r.rolbypassrls
          AND r.rolname !~ '^pg_'
          AND NOT pg_has_role(r.oid, 'pg_read_all_data', 'MEMBER')
          AND NOT pg_has_role(r.oid, 'pg_write_all_data', 'MEMBER')
          AND EXISTS (
              SELECT 1
              FROM pg_class c, aclexplode(c.relacl) acl
              WHERE c.oid = 'public.events'::regclass
                AND acl.grantee = r.oid
                AND acl.privilege_type = 'SELECT'
          )
          AND EXISTS (
              SELECT 1
              FROM pg_class c, aclexplode(c.relacl) acl
              WHERE c.oid = 'public.events'::regclass
                AND acl.grantee = r.oid
                AND acl.privilege_type = 'INSERT'
          )
    LOOP
        EXECUTE format(
            'GRANT EXECUTE ON FUNCTION account_export_group_ids(), account_export_received_invitations() TO %I',
            runtime_role
        );
    END LOOP;
END
$$;
