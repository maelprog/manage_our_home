-- #774: an attachment's thumbnail (`thumbnail_key`, 0053) is an object of
-- its own in the bucket, and deleting a group must remove it with the file.
-- `DELETE /groups/:id` reads the keys to remove through this function
-- (0044, #620) before the cascade takes the rows; it now returns the
-- thumbnails' keys as well as the attachments'.
--
-- Same signature, same guards: the group of `app.group_id`, for its owner
-- only, private events included. CREATE OR REPLACE keeps the function's
-- owner, its privileges and SECURITY DEFINER's search_path as written.
CREATE OR REPLACE FUNCTION group_attachment_storage_keys(p_group_id UUID) RETURNS SETOF TEXT
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    SELECT k.key
    FROM event_attachments a
    JOIN events e ON e.id = a.event_id
    CROSS JOIN LATERAL (VALUES (a.storage_key), (a.thumbnail_key)) AS k(key)
    WHERE k.key IS NOT NULL
      AND e.group_id = p_group_id
      AND p_group_id::text = current_setting('app.group_id', true)
      AND EXISTS (
          SELECT 1 FROM group_members gm
          WHERE gm.group_id = e.group_id
            AND gm.user_id::text = current_setting('app.user_id', true)
            AND gm.role = 'owner'
      )
$$;

COMMENT ON FUNCTION group_attachment_storage_keys(UUID) IS
    'Storage keys of every attachment of the group and of their thumbnails, when it is the group of app.group_id, private events included, for its owner only (issues #620, #774). Read by DELETE /groups/:id before the cascade.';
