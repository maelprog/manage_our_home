-- #620: an event may be private, visible to its participants alone.
--
-- Participants are the event's creator and the members of its "pour qui"
-- (`event_assignees`, 0011) — no other list. Everyone else in the group,
-- owner and admins included, does not see the event at all: not in the
-- lists, not by id, and none of what hangs off it (reminders and their
-- scheduled notifications, attachments, per-occurrence completions,
-- assignees). The filter is a row-level policy, not a `WHERE` in the
-- handlers, so a query that forgets it still cannot leak the row.
--
-- The children need no change of their own: each of their policies is an
-- `EXISTS (SELECT 1 FROM events e ...)`, and Postgres applies the policy of
-- `events` to that subquery as well. A child row of a private event is
-- therefore visible exactly when the event is.

ALTER TABLE events ADD COLUMN is_private BOOLEAN NOT NULL DEFAULT false;

-- Is the user of `app.user_id` among the event's assignees?
--
-- The `events` policy cannot read `event_assignees` directly: the policy of
-- `event_assignees` joins back to `events` (0011, 0042), and two policies
-- that each read the other's table make Postgres stop every query on either
-- with "infinite recursion detected in policy". Of the ways out:
--   * rewriting the policy of `event_assignees` so it no longer reads
--     `events` would need its own `group_id` column — and then it would no
--     longer hide the assignees of a private event, which is required;
--   * this function reads `event_assignees` as its owner, the migration
--     role, which carries `BYPASSRLS` (apps/api/README.md), so the read does
--     not go back through any policy and the loop is cut.
-- It answers one question, for one event, about the user the request is
-- scoped to — read from the setting rather than taken as an argument, like
-- `account_export_group_ids` (0019) — and returns nothing else. That is why
-- it stays executable by PUBLIC, unlike the 0019 functions: the policy of
-- `events` runs it as whichever role reads `events`, and a role that could
-- not would have every query on `events` refused, while what it gives away
-- is whether the caller's own user is assigned to an event whose id the
-- caller already has.
--
-- `search_path` is pinned with `pg_temp` last, for the reason 0019 gives:
-- otherwise a caller's temporary table could shadow `event_assignees` and
-- be read with the owner's rights.
CREATE FUNCTION event_is_assigned_to_current_user(p_event_id UUID) RETURNS BOOLEAN
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    SELECT EXISTS (
        SELECT 1 FROM event_assignees ea
        WHERE ea.event_id = p_event_id
          AND ea.user_id::text = current_setting('app.user_id', true)
    )
$$;

COMMENT ON FUNCTION event_is_assigned_to_current_user(UUID) IS
    'Whether the user of app.user_id is an assignee of the event (issue #620). Read by the events_isolation policy, which cannot read event_assignees itself without recursing.';

-- The group branch is the one 0042 left; the participant branch is new.
-- The policy is `FOR ALL` with no `WITH CHECK`, so the same expression
-- checks the rows written: a write may not leave behind a private event its
-- author would not see — the API refuses that case before writing
-- (`agenda::may_set_privacy`).
ALTER POLICY events_isolation ON events
    USING (
        group_id::text = current_setting('app.group_id', true)
        AND (
            NOT is_private
            OR created_by::text = current_setting('app.user_id', true)
            OR event_is_assigned_to_current_user(id)
        )
    );

-- Deleting a group deletes every event in it, private ones included, by
-- cascade from `groups`; their attachment rows go with them. The objects
-- those rows name do not: `DELETE /groups/:id` collects their keys first and
-- removes them from storage (`groups::delete_group`). Read under the policy
-- above, that collection would miss the attachments of every private event
-- the owner is not a participant of, and leave their files in the bucket
-- with nothing pointing at them.
--
-- This function returns those keys and nothing else, for the group it is
-- given, only when that is the group of `app.group_id` and the user of
-- `app.user_id` owns it — the only role allowed to delete a group. For
-- anyone else it returns no row.
CREATE FUNCTION group_attachment_storage_keys(p_group_id UUID) RETURNS SETOF TEXT
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    SELECT a.storage_key
    FROM event_attachments a
    JOIN events e ON e.id = a.event_id
    WHERE e.group_id = p_group_id
      AND p_group_id::text = current_setting('app.group_id', true)
      AND EXISTS (
          SELECT 1 FROM group_members gm
          WHERE gm.group_id = e.group_id
            AND gm.user_id::text = current_setting('app.user_id', true)
            AND gm.role = 'owner'
      )
$$;

COMMENT ON FUNCTION group_attachment_storage_keys(UUID) IS
    'Storage keys of every attachment of the group, when it is the group of app.group_id, private events included, for its owner only (issue #620). Read by DELETE /groups/:id before the cascade.';
