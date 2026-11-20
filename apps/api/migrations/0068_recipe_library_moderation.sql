-- #908: reporting a publication of the shared library (0066) and its
-- moderation by the superadmin, plus the two ways a family's publications
-- end with a member's departure (user's decisions of 2026-11-20).
--
-- `library_reports`: one report per user and per publication, a reason
-- from a closed list of six and a comment of 500 characters at most. Its
-- reporter is readable by themselves alone (the policy below) and by the
-- superadmin's pool, which bypasses RLS; the API never shows it to anyone,
-- the superadmin included, and never to the publisher. Its
-- `library_recipe_id` holds no key onto `library_recipes`: a report
-- outlives the publication it is about, so the superadmin's decision and
-- what led to it stay readable until the purge.
--
-- A report is open until the superadmin closes it — dismissed, or the
-- publication withdrawn — or until its publication is gone for another
-- reason (its publisher withdrew it, left the family, or their account was
-- purged): the hourly retention purge closes it then as `lapsed`. A closed
-- report is deleted six months after its closure, an open one never
-- (user's decision of 2026-11-20; `jobs::retention_purge`). The reporter's
-- account purge deletes their reports (`jobs::account_purge`).
CREATE TABLE library_reports (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    library_recipe_id UUID NOT NULL,
    reporter_id       UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    reason            TEXT NOT NULL CHECK (reason IN ('illegal', 'copyright', 'hateful',
                                                      'spam', 'health_hazard', 'other')),
    comment           TEXT CHECK (char_length(comment) BETWEEN 1 AND 500),
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    resolution        TEXT CHECK (resolution IN ('dismissed', 'withdrawn', 'lapsed')),
    closed_at         TIMESTAMPTZ,
    CHECK ((resolution IS NULL) = (closed_at IS NULL)),
    UNIQUE (library_recipe_id, reporter_id)
);
-- Serves the export and the account purge, which look reports up by person.
CREATE INDEX library_reports_reporter_id_idx ON library_reports (reporter_id);
-- Serves the purge of closed reports.
CREATE INDEX library_reports_closed_at_idx
    ON library_reports (closed_at) WHERE closed_at IS NOT NULL;

-- The runtime role reads and writes its caller's reports, nothing else; it
-- never closes one (no UPDATE nor DELETE policy): the superadmin's pool
-- does.
ALTER TABLE library_reports ENABLE ROW LEVEL SECURITY;
ALTER TABLE library_reports FORCE ROW LEVEL SECURITY;
CREATE POLICY library_reports_reporter_read ON library_reports
    FOR SELECT
    USING (reporter_id::text = current_setting('app.user_id', true));
CREATE POLICY library_reports_reporter_write ON library_reports
    FOR INSERT
    WITH CHECK (reporter_id::text = current_setting('app.user_id', true));

-- A family recipe whose publication the superadmin withdrew: publishing it
-- again is refused, 409 `withdrawn_by_moderation`, until the superadmin
-- lifts the block (deletes the row). Keyed by the source recipe, so the
-- block holds whichever member publishes it, and goes with the recipe.
-- `library_recipe_id` names the withdrawn publication, gone since: the
-- superadmin lifts the block by it. No account in it.
CREATE TABLE library_withdrawn_sources (
    source_recipe_id  UUID PRIMARY KEY REFERENCES recipes(id) ON DELETE CASCADE,
    library_recipe_id UUID NOT NULL UNIQUE,
    withdrawn_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Readable by the family holding the recipe (`recipes`'s own policy, on
-- `app.group_id`), written by the superadmin's pool alone.
ALTER TABLE library_withdrawn_sources ENABLE ROW LEVEL SECURITY;
ALTER TABLE library_withdrawn_sources FORCE ROW LEVEL SECURITY;
CREATE POLICY library_withdrawn_sources_group_read ON library_withdrawn_sources
    FOR SELECT
    USING (EXISTS (SELECT 1 FROM recipes r
                   WHERE r.id = library_withdrawn_sources.source_recipe_id));

-- Reports per user, counted in `rate_limit_log` like the library's other
-- limits (0067).
ALTER TABLE rate_limit_log DROP CONSTRAINT rate_limit_log_kind_check;
ALTER TABLE rate_limit_log ADD CONSTRAINT rate_limit_log_kind_check
    CHECK (kind IN ('recipe_import', 'openfoodfacts', 'account_exists_notice', 'google_sign_in',
                    'library_read', 'library_publish', 'library_report'));

-- A member's publications go when they leave the family (user's decision
-- of 2026-11-20): the copies stay, the family's quota is freed. Deletes
-- the publications taken from a recipe of `p_group_id` whose publisher is
-- no longer one of its members; called by the departure, once the
-- membership is deleted (`groups::leave_group`, `groups::remove_member`).
-- The publishers' policy (0066) shows each user their own rows only, and
-- the one departing by removal is not the caller, hence `SECURITY
-- DEFINER`, guarded as the functions of 0067: the family of `app.group_id`
-- only. It removes nothing a member published: only what no current member
-- of the family stands behind. Returns how many publications went.
CREATE FUNCTION group_library_unpublish_departed(p_group_id UUID) RETURNS BIGINT
LANGUAGE sql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    WITH gone AS (
        DELETE FROM library_recipes l
        USING library_recipe_publishers p, recipes r
        WHERE p.library_recipe_id = l.id
          AND r.id = p.source_recipe_id
          AND r.group_id = p_group_id
          AND p_group_id::text = current_setting('app.group_id', true)
          AND NOT EXISTS (
              SELECT 1 FROM group_members gm
              WHERE gm.group_id = p_group_id AND gm.user_id = p.user_id
          )
        RETURNING l.id
    )
    SELECT count(*) FROM gone
$$;

COMMENT ON FUNCTION group_library_unpublish_departed(UUID) IS
    'Deletes the library publications taken from recipes of the family of app.group_id whose publisher is no longer a member of it: the departure of issue #908.';

-- The owner of a family withdraws any publication taken from one of its
-- recipes, whoever published it, a former member included (user's
-- decision of 2026-11-20). Same guards, plus the caller (`app.user_id`)
-- being the owner of `p_group_id`. Returns how many publications went: 0
-- or 1, a recipe having one publication at most.
CREATE FUNCTION group_library_unpublish_as_owner(p_group_id UUID, p_recipe_id UUID)
RETURNS BIGINT
LANGUAGE sql
VOLATILE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
AS $$
    WITH gone AS (
        DELETE FROM library_recipes l
        USING library_recipe_publishers p, recipes r
        WHERE p.library_recipe_id = l.id
          AND r.id = p.source_recipe_id
          AND r.id = p_recipe_id
          AND r.group_id = p_group_id
          AND p_group_id::text = current_setting('app.group_id', true)
          AND EXISTS (
              SELECT 1 FROM group_members gm
              WHERE gm.group_id = p_group_id
                AND gm.user_id::text = current_setting('app.user_id', true)
                AND gm.role = 'owner'
          )
        RETURNING l.id
    )
    SELECT count(*) FROM gone
$$;

COMMENT ON FUNCTION group_library_unpublish_as_owner(UUID, UUID) IS
    'Deletes the library publication taken from a recipe of the family of app.group_id, for its owner only: issue #908.';
