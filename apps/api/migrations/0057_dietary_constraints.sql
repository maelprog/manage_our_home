-- #817: a member's food allergies and dislikes, and a household's kitchen
-- equipment.
--
-- An allergy is health data (RGPD art. 9). It belongs to the person, not to
-- a family: `user_allergies` has no `group_id`, and only its holder reads or
-- writes it. For each family they belong to, the holder chooses who learns
-- of each allergy (`allergy_sharing.share`, controller's decision of
-- 2026-11-12):
--   * `public`    — the family sees the member's name and the allergy;
--   * `anonymous` — the family sees that someone has an allergy, and the
--                   server may use the tag in its own computations, never
--                   sent to anyone;
--   * `private`   — nothing, ever. The default: an allergy with no
--                   `allergy_sharing` row for a family is private there.
-- Nothing about it is copied elsewhere: whatever another member is shown is
-- read through the functions below, at each request, so a change of level
-- takes effect on the next read.
--
-- Dislikes are the member's alone and are never shared: they only act on
-- what is proposed to that member (#818). The equipment is the household's
-- and not sensitive: every member reads and writes it, like the recipes.
--
-- Every value here is written by the API after it validated it
-- (`recipes::constraints`, `shared::validation::dietary`): the API logs a
-- database error with its detail, which would carry a tag, so the writes
-- are shaped to raise none (`ON CONFLICT DO NOTHING`, `WHERE EXISTS`).

CREATE TABLE user_allergies (
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    tag         TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, tag)
);

-- One row per (allergy, family) whose level the holder set. Its key onto
-- `group_members` both limits the rows to the families the holder belongs
-- to and deletes them when the membership ends — leaving, being removed,
-- the family deleted, the account purged. Its key onto `user_allergies`
-- deletes them with the allergy.
CREATE TABLE allergy_sharing (
    user_id     UUID NOT NULL,
    tag         TEXT NOT NULL,
    group_id    UUID NOT NULL,
    share       TEXT NOT NULL DEFAULT 'private'
                CHECK (share IN ('public', 'anonymous', 'private')),
    PRIMARY KEY (user_id, tag, group_id),
    FOREIGN KEY (user_id, tag)
        REFERENCES user_allergies (user_id, tag) ON DELETE CASCADE,
    FOREIGN KEY (group_id, user_id)
        REFERENCES group_members (group_id, user_id) ON DELETE CASCADE
);
CREATE INDEX allergy_sharing_group_idx ON allergy_sharing (group_id, user_id);

CREATE TABLE user_dislikes (
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    tag         TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, tag)
);

-- The holder alone, reading and writing, whatever family the request is
-- scoped to: the owner and the admins of a family do not see them either.
ALTER TABLE user_allergies ENABLE ROW LEVEL SECURITY;
ALTER TABLE user_allergies FORCE ROW LEVEL SECURITY;
CREATE POLICY user_allergies_holder ON user_allergies
    USING (user_id::text = current_setting('app.user_id', true));

ALTER TABLE allergy_sharing ENABLE ROW LEVEL SECURITY;
ALTER TABLE allergy_sharing FORCE ROW LEVEL SECURITY;
CREATE POLICY allergy_sharing_holder ON allergy_sharing
    USING (user_id::text = current_setting('app.user_id', true));

ALTER TABLE user_dislikes ENABLE ROW LEVEL SECURITY;
ALTER TABLE user_dislikes FORCE ROW LEVEL SECURITY;
CREATE POLICY user_dislikes_holder ON user_dislikes
    USING (user_id::text = current_setting('app.user_id', true));

-- The household's equipment: codes of `shared::validation::dietary::EQUIPMENT`.
CREATE TABLE group_equipment (
    group_id    UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    code        TEXT NOT NULL,
    PRIMARY KEY (group_id, code)
);

ALTER TABLE group_equipment ENABLE ROW LEVEL SECURITY;
ALTER TABLE group_equipment FORCE ROW LEVEL SECURITY;
CREATE POLICY group_equipment_isolation ON group_equipment
    USING (group_id::text = current_setting('app.group_id', true));

-- What a recipe needs, from the same list. Empty on existing recipes.
-- `recipes_isolation` filters whole rows, so it covers the column as is.
ALTER TABLE recipes ADD COLUMN required_equipment TEXT[] NOT NULL DEFAULT '{}';

-- What the other members of a family may read of a member's allergies, and
-- nothing else: the only path across members. Each answers for the family
-- of `app.group_id` only, and only to a member of it (the user of
-- `app.user_id`); for anyone else it returns nothing. `SECURITY DEFINER`
-- runs them as their owner, the migration role, which bypasses RLS
-- (apps/api/README.md); `search_path` is pinned with `pg_temp` last, for the
-- reason 0019 gives. They stay executable by PUBLIC, like the functions of
-- 0044: each answers only inside the caller's own family, so there is
-- nothing across families to withhold. A `private` row is never read.

-- `public`: who, and which allergy.
CREATE FUNCTION group_public_allergies(p_group_id UUID)
RETURNS TABLE (user_id UUID, display_name TEXT, tag TEXT)
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    SELECT s.user_id, u.display_name, s.tag
    FROM allergy_sharing s
    JOIN users u ON u.id = s.user_id
    WHERE s.group_id = p_group_id
      AND s.share = 'public'
      AND p_group_id::text = current_setting('app.group_id', true)
      AND EXISTS (
          SELECT 1 FROM group_members gm
          WHERE gm.group_id = p_group_id
            AND gm.user_id::text = current_setting('app.user_id', true)
      )
    ORDER BY u.display_name, s.user_id, s.tag
$$;

-- `anonymous`: whether at least one member has such an allergy — no name,
-- no tag, no count.
CREATE FUNCTION group_has_anonymous_allergy(p_group_id UUID) RETURNS BOOLEAN
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    SELECT EXISTS (
        SELECT 1 FROM allergy_sharing s
        WHERE s.group_id = p_group_id
          AND s.share = 'anonymous'
          AND p_group_id::text = current_setting('app.group_id', true)
          AND EXISTS (
              SELECT 1 FROM group_members gm
              WHERE gm.group_id = p_group_id
                AND gm.user_id::text = current_setting('app.user_id', true)
          )
    )
$$;

-- `anonymous`: the set of tags, without who holds them, for the server's
-- own computations (#818). Never sent to a client.
CREATE FUNCTION group_anonymous_allergy_tags(p_group_id UUID) RETURNS SETOF TEXT
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    SELECT DISTINCT s.tag
    FROM allergy_sharing s
    WHERE s.group_id = p_group_id
      AND s.share = 'anonymous'
      AND p_group_id::text = current_setting('app.group_id', true)
      AND EXISTS (
          SELECT 1 FROM group_members gm
          WHERE gm.group_id = p_group_id
            AND gm.user_id::text = current_setting('app.user_id', true)
      )
    ORDER BY 1
$$;

COMMENT ON FUNCTION group_public_allergies(UUID) IS
    'Allergies shared publicly in the family of app.group_id, with their holder, for a member of it (issue #817).';
COMMENT ON FUNCTION group_has_anonymous_allergy(UUID) IS
    'Whether a member of the family of app.group_id shares an allergy anonymously there, for a member of it (issue #817).';
COMMENT ON FUNCTION group_anonymous_allergy_tags(UUID) IS
    'Tags shared anonymously in the family of app.group_id, without their holder, for the server''s own computations only (issue #817).';
