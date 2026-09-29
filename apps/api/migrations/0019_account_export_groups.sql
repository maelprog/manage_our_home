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
-- families for a role that does not. `search_path` is pinned so the
-- caller cannot substitute tables of their own. The caller is read from
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
SET search_path = pg_catalog, public
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
-- Matched case-insensitively: an inviter typing `Alice@…` addresses the same
-- mailbox as the account's `alice@…`.

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
SET search_path = pg_catalog, public
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
    'Invitations addressed to the email of the user of app.user_id, without their token (issue #140). Read by GET /account/export only.';

-- Who may call them. A function is executable by PUBLIC by default; these
-- two look across families, so they are not. They go to the role that
-- serves requests under RLS: whatever its name (`app_role` in
-- apps/api/README.md, another in a given deployment), it is the one that
-- can read the family tables without bypassing RLS. A superuser or
-- `BYPASSRLS` role needs no grant, and gets none. A runtime role created
-- after this migration gets them from the `GRANT EXECUTE` line the README
-- prescribes with its table grants.
REVOKE EXECUTE ON FUNCTION account_export_group_ids() FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION account_export_received_invitations() FROM PUBLIC;

DO $$
DECLARE
    runtime_role NAME;
BEGIN
    FOR runtime_role IN
        SELECT rolname FROM pg_roles
        WHERE NOT rolsuper
          AND NOT rolbypassrls
          AND rolname !~ '^pg_'
          AND has_table_privilege(oid, 'public.events', 'SELECT')
    LOOP
        EXECUTE format(
            'GRANT EXECUTE ON FUNCTION account_export_group_ids(), account_export_received_invitations() TO %I',
            runtime_role
        );
    END LOOP;
END
$$;
