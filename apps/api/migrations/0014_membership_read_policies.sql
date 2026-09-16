-- Issue #113. A member can read their own membership rows, and the groups
-- they belong to, without `app.family_id` — and still write nothing without
-- it.
--
-- `groups_membership_isolation` (0001) was meant to let a user see the
-- groups they belong to with only `app.user_id` set: that is how "my groups"
-- is listed before any family is chosen (`user_scoped_tx`). Its membership
-- branch is an `EXISTS` over `group_members` — and a subquery inside a policy
-- is itself subject to the policies of the table it reads.
-- `group_members_isolation` only ever matched `app.family_id`, which
-- `user_scoped_tx` does not set, so under a role that does not bypass RLS
-- (the `NOSUPERUSER NOBYPASSRLS` role apps/api/README.md prescribes for
-- `DATABASE_URL`) the branch never matched: `GET /groups` answered `[]` to
-- the owner of a group.
--
-- Nothing noticed, because the API itself only ever ran as a superuser —
-- in the `e2e` job, in the handler-level integration tests, and in
-- infra/docker-compose.yml — and RLS does not apply to a superuser at all.
--
-- 1. `group_members_self_read`, `FOR SELECT` only. Policies of the same
--    command are OR-ed, so reads become "rows of the scoped family, or rows
--    that name the caller". `INSERT`, `UPDATE` and `DELETE` still see only
--    `group_members_isolation`, so no membership is writable without
--    `app.family_id` — in particular nobody joins another family by
--    inserting a row that names themselves.
--
-- 2. `groups_membership_isolation` was `FOR ALL`. With its membership branch
--    now able to match, it would have let a member rename or delete any
--    group they belong to with only `app.user_id` set. It is split: the
--    family branch keeps every command, the membership branch is read-only.
--    Writes on `groups` stay exactly as scoped as they were in practice.
--
-- What becomes readable is what the caller already gets from `GET /groups`:
-- which groups they belong to, and their own role in each. No other member's
-- row, and no group they are not in. `apps/api/tests/rls.rs` holds both the
-- read and the write half.
CREATE POLICY group_members_self_read ON group_members
    FOR SELECT
    USING (user_id::text = current_setting('app.user_id', true));

DROP POLICY groups_membership_isolation ON groups;

CREATE POLICY groups_family_isolation ON groups
    USING (id::text = current_setting('app.family_id', true));

CREATE POLICY groups_membership_read ON groups
    FOR SELECT
    USING (
        EXISTS (
            SELECT 1 FROM group_members gm
            WHERE gm.group_id = groups.id
              AND gm.user_id::text = current_setting('app.user_id', true)
        )
    );

-- "My groups" is now looked up by `user_id` alone — in `list_groups`, in
-- `create_group`'s cap, and in `group_members_self_read` itself. The primary
-- key leads with `group_id` and cannot serve that lookup.
CREATE INDEX group_members_user_id_idx ON group_members (user_id);
