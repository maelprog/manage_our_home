-- #991: when a recipe of the family was published, whoever of its members
-- published it. The screen that refuses a recipe another member published
-- already shows its card, dated as the library dates it (user's decision
-- of 2026-11-25). The publishers' policy (0066) shows each member their own
-- rows only, hence `SECURITY DEFINER`, with the guards of the functions of
-- 0067 and 0068: the family of `app.group_id` only, a member of it only,
-- `search_path` pinned (`pg_temp` last). It returns the two dates the
-- library already shows every reader, and nothing of the publisher: not
-- who, not the publication's id. No row: the recipe is not published, or
-- not the family's.
CREATE FUNCTION group_recipe_publication_dates(p_group_id UUID, p_recipe_id UUID)
RETURNS TABLE (published_at TIMESTAMPTZ, updated_at TIMESTAMPTZ)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    SELECT l.published_at, l.updated_at
    FROM library_recipe_publishers p
    JOIN recipes r ON r.id = p.source_recipe_id
    JOIN library_recipes l ON l.id = p.library_recipe_id
    WHERE r.id = p_recipe_id
      AND r.group_id = p_group_id
      AND p_group_id::text = current_setting('app.group_id', true)
      AND EXISTS (
          SELECT 1 FROM group_members gm
          WHERE gm.group_id = p_group_id
            AND gm.user_id::text = current_setting('app.user_id', true)
      )
$$;

COMMENT ON FUNCTION group_recipe_publication_dates(UUID, UUID) IS
    'Publication and update dates of the library publication taken from a recipe of the family of app.group_id, whoever published it, for its members only: issue #991.';
