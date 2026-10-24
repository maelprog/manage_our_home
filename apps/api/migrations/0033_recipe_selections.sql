-- #469: the recipes a family means to cook for its next shopping.
--
-- Until now the grocery list was generated from every recipe of the group
-- (`grocery_list::items::generate_grocery_items`). A selection names one
-- recipe the family has chosen; generating the list from the selections
-- alone is #470, and marking one `done` or `abandoned` is #473. This
-- migration and its API only create, list, resize and delete `planned`
-- selections.
--
-- `servings` is the number of people the recipe is cooked for this time,
-- set or changed by any member (controller's decision of 2026-10-24).
-- `recipes` has no portion count of its own, so NULL means "not said" and
-- scaling the quantities to it belongs to #470. The bound matches
-- `recipes::selections::SERVINGS_MAX`.
--
-- `group_id` is denormalized, as on `recipe_ingredients` and
-- `meal_history`, so the row-level policy reads it directly. The API checks
-- the recipe belongs to the same group before inserting.
--
-- `created_by` follows the regime of the other family content
-- (`jobs/account_purge.rs`): the row is kept, attributed to the anonymised
-- account, and exported to its author (`rgpd/mod.rs`).

CREATE TABLE recipe_selections (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id    UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    recipe_id   UUID NOT NULL REFERENCES recipes(id) ON DELETE CASCADE,
    status      TEXT NOT NULL DEFAULT 'planned'
                CHECK (status IN ('planned', 'done', 'abandoned')),
    servings    INTEGER CHECK (servings BETWEEN 1 AND 50),
    created_by  UUID NOT NULL REFERENCES users(id),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX recipe_selections_group_id_idx ON recipe_selections (group_id);

-- At most one `planned` selection per recipe: selecting it again finds this
-- row (`INSERT … ON CONFLICT`) instead of adding a second one. `done` and
-- `abandoned` rows are history and may pile up.
CREATE UNIQUE INDEX recipe_selections_one_planned_idx
    ON recipe_selections (recipe_id) WHERE status = 'planned';

ALTER TABLE recipe_selections ENABLE ROW LEVEL SECURITY;
ALTER TABLE recipe_selections FORCE ROW LEVEL SECURITY;
CREATE POLICY recipe_selections_isolation ON recipe_selections
    USING (group_id::text = current_setting('app.family_id', true));

-- `account_export_group_ids()` (0019) lists by hand every family-scoped
-- column that names a user, so a person's rows in a group they have left
-- still reach their export. `recipe_selections.created_by` is one more:
-- the function is replaced with one more branch, the rest unchanged.
-- `CREATE OR REPLACE` keeps the function's owner and its grants (0019
-- revoked PUBLIC and granted the runtime role).
CREATE OR REPLACE FUNCTION account_export_group_ids() RETURNS SETOF UUID
LANGUAGE sql
STABLE
SECURITY DEFINER
SET search_path = pg_catalog, public, pg_temp
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
    UNION SELECT rs.group_id FROM recipe_selections rs, me WHERE rs.created_by = me.id
    UNION SELECT gi.group_id FROM grocery_items gi, me WHERE gi.created_by = me.id
    UNION SELECT b.group_id FROM budget_entries b, me WHERE b.created_by = me.id
    UNION SELECT m.group_id FROM messages m, me WHERE m.created_by = me.id
    UNION SELECT r.group_id FROM message_read_state r, me WHERE r.user_id = me.id
    UNION SELECT c.group_id FROM calendar_imports c, me WHERE c.created_by = me.id
    UNION SELECT i.group_id FROM invitations i, me WHERE i.created_by = me.id
$$;
