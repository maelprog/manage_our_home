-- #620 (review): the account export keeps what a person put on an event they
-- no longer see.
--
-- A private event is visible to its participants alone (0044). A person who
-- uploaded an attachment, or ticked an occurrence, on such an event and was
-- since taken off its "pour qui" no longer sees the event — nor, through the
-- policies that join back to `events`, their own attachment or completion.
-- Both rows still name them (`uploaded_by`, `completed_by`), so the export
-- (art. 15) has to reach them.
--
-- These two functions return those rows, for the group of `app.group_id`
-- and for the user of `app.user_id` only, and nothing of the event itself:
-- not its title, not its other rows. Same shape and same reasons as
-- `account_export_group_ids` (0019): `SECURITY DEFINER` to read past the
-- policy, `search_path` pinned with `pg_temp` last, the caller read from the
-- setting rather than taken as an argument. Unlike the 0019 functions they
-- stay executable by PUBLIC, as the 0044 ones do: each answers only for the
-- caller's own rows in the caller's own group, so there is nothing across
-- families to withhold.

CREATE FUNCTION account_export_event_attachments(p_group_id UUID)
RETURNS TABLE (
    id UUID,
    event_id UUID,
    storage_key TEXT,
    filename TEXT,
    mime_type TEXT,
    size_bytes BIGINT,
    created_at TIMESTAMPTZ
)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    SELECT a.id, a.event_id, a.storage_key, a.filename, a.mime_type, a.size_bytes, a.created_at
    FROM event_attachments a JOIN events e ON e.id = a.event_id
    WHERE e.group_id = p_group_id
      AND p_group_id::text = current_setting('app.group_id', true)
      AND a.uploaded_by::text = current_setting('app.user_id', true)
    ORDER BY a.created_at
$$;

CREATE FUNCTION account_export_event_completions(p_group_id UUID)
RETURNS TABLE (
    event_id UUID,
    occurrence_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ
)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    SELECT c.event_id, c.occurrence_at, c.completed_at
    FROM event_occurrence_completions c JOIN events e ON e.id = c.event_id
    WHERE e.group_id = p_group_id
      AND p_group_id::text = current_setting('app.group_id', true)
      AND c.completed_by::text = current_setting('app.user_id', true)
    ORDER BY c.completed_at
$$;

COMMENT ON FUNCTION account_export_event_attachments(UUID) IS
    'Attachments the user of app.user_id uploaded in the group of app.group_id, private events they cannot see included (issue #620). Read by GET /account/export only.';
COMMENT ON FUNCTION account_export_event_completions(UUID) IS
    'Occurrence completions the user of app.user_id made in the group of app.group_id, private events they cannot see included (issue #620). Read by GET /account/export only.';
