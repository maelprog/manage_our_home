-- #818: the tags another member of a family shares anonymously there,
-- without their holder, for the allergy warning a member is given on a
-- recipe (`recipes::dietary_filter`).
--
-- `group_anonymous_allergy_tags` (0057) answers for every member, the
-- caller included, and without holders: a tag the caller also shares
-- anonymously comes out once, whether another member shares it or not. The
-- warning's `anonymous` must say "another member", never the caller alone,
-- so this one leaves the caller's own rows out. Same guards as the
-- functions of 0057: the family of `app.group_id` only, a member of it
-- only, a `private` row never read, `SECURITY DEFINER` with `search_path`
-- pinned (`pg_temp` last). Never sent to a client: the API uses the tags
-- in its own computations and says at most that one matches.

CREATE FUNCTION group_other_anonymous_allergy_tags(p_group_id UUID) RETURNS SETOF TEXT
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    SELECT DISTINCT s.tag
    FROM allergy_sharing s
    WHERE s.group_id = p_group_id
      AND s.share = 'anonymous'
      AND s.user_id::text IS DISTINCT FROM current_setting('app.user_id', true)
      AND p_group_id::text = current_setting('app.group_id', true)
      AND EXISTS (
          SELECT 1 FROM group_members gm
          WHERE gm.group_id = p_group_id
            AND gm.user_id::text = current_setting('app.user_id', true)
      )
    ORDER BY 1
$$;

COMMENT ON FUNCTION group_other_anonymous_allergy_tags(UUID) IS
    'Tags shared anonymously in the family of app.group_id by members other than app.user_id, without their holder, for the server''s own computations only (issue #818).';
