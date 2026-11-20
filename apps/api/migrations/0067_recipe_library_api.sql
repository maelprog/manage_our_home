-- #906: the library's routes (`recipes::library`) over the schema of 0066.
--
-- Two rate limits per user, counted in `rate_limit_log` like the others
-- (0029, 0043, 0050): reading the library (`library_read`) and publishing
-- to it (`library_publish`). Only the list of kinds the table accepts
-- changes; 0050 is left as it was.
ALTER TABLE rate_limit_log DROP CONSTRAINT rate_limit_log_kind_check;
ALTER TABLE rate_limit_log ADD CONSTRAINT rate_limit_log_kind_check
    CHECK (kind IN ('recipe_import', 'openfoodfacts', 'account_exists_notice', 'google_sign_in',
                    'library_read', 'library_publish'));

-- A family holds at most 50 publications (user's decision of 2026-11-20).
-- They are the publications whose source recipe is one of the family's,
-- whoever of its members published them; the publishers' policy (0066)
-- shows each member their own rows only, so the count is taken here. Same
-- guards as the functions of 0057 and 0058: the family of `app.group_id`
-- only, a member of it only, `SECURITY DEFINER` with `search_path` pinned
-- (`pg_temp` last). It returns a number and nothing else: no publication,
-- no publisher. A publication whose source recipe was deleted
-- (`source_recipe_id` NULL) no longer counts.
CREATE FUNCTION group_library_publication_count(p_group_id UUID) RETURNS BIGINT
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    SELECT count(*)
    FROM library_recipe_publishers p
    JOIN recipes r ON r.id = p.source_recipe_id
    WHERE r.group_id = p_group_id
      AND p_group_id::text = current_setting('app.group_id', true)
      AND EXISTS (
          SELECT 1 FROM group_members gm
          WHERE gm.group_id = p_group_id
            AND gm.user_id::text = current_setting('app.user_id', true)
      )
$$;

COMMENT ON FUNCTION group_library_publication_count(UUID) IS
    'Publications of the shared library taken from recipes of the family of app.group_id, for its members only: the quota of issue #906.';
