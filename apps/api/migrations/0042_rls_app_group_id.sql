-- #421: the RLS session variable is renamed `app.family_id` -> `app.group_id`.
--
-- The model has one entity, the group (`groups`); "family" is the word it
-- started with, and it survived in the name of the per-request setting
-- `scoped_tx` writes (`src/auth/session.rs`) — which has always held a
-- `group_id`. Every policy that read it is pointed at the new name here,
-- with no other change to its predicate, its command, its roles or its
-- permissiveness: `ALTER POLICY ... USING / WITH CHECK` replaces only the
-- expression it names and leaves the rest of the policy as it was.
--
-- The 25 policies below are those of `pg_policies` on a base migrated up
-- to 0041 whose `qual` or `with_check` read `app.family_id` — read from the
-- catalogue, not from the files, since several were replaced after the
-- migration that created them (`invitations_isolation`, 0001 then 0024).
-- The two policies whose name said "family" are renamed as well:
-- `groups_family_isolation` (0014) and `notifications_family_insert` (0036).
--
-- No transition period reading both names (issue #421): the binary from
-- before this migration sets `app.family_id`, which nothing reads any more,
-- so until it is replaced its scoped requests see no rows — RLS denies by
-- default, nothing leaks. Migrations 0001..0041 are left as they are: sqlx
-- checks their checksums, and their comments still say `app.family_id`.

-- Tables keyed directly on `group_id`.
ALTER POLICY budget_alerts_isolation ON budget_alerts
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY budget_categories_isolation ON budget_categories
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY budget_entries_isolation ON budget_entries
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY budget_entry_grocery_items_isolation ON budget_entry_grocery_items
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY budget_settings_isolation ON budget_settings
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY calendar_imports_isolation ON calendar_imports
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY events_isolation ON events
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY grocery_items_isolation ON grocery_items
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY group_members_isolation ON group_members
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY meal_history_isolation ON meal_history
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY message_read_state_isolation ON message_read_state
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY messages_isolation ON messages
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY recipe_ingredients_isolation ON recipe_ingredients
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY recipe_selections_isolation ON recipe_selections
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY recipes_isolation ON recipes
    USING (group_id::text = current_setting('app.group_id', true));
ALTER POLICY stock_items_isolation ON stock_items
    USING (group_id::text = current_setting('app.group_id', true));

-- The invitation-token branch of 0024 is kept as it is.
ALTER POLICY invitations_isolation ON invitations
    USING (
        group_id::text = current_setting('app.group_id', true)
        OR encode(token_hash, 'hex') = current_setting('app.invitation_token', true)
    );

-- Tables scoped through their parent row.
ALTER POLICY calendar_import_events_isolation ON calendar_import_events
    USING (EXISTS (
        SELECT 1 FROM calendar_imports ci
        WHERE ci.id = calendar_import_events.calendar_import_id
          AND ci.group_id::text = current_setting('app.group_id', true)
    ));
ALTER POLICY event_assignees_isolation ON event_assignees
    USING (EXISTS (
        SELECT 1 FROM events e
        WHERE e.id = event_assignees.event_id
          AND e.group_id::text = current_setting('app.group_id', true)
    ));
ALTER POLICY event_attachments_isolation ON event_attachments
    USING (EXISTS (
        SELECT 1 FROM events e
        WHERE e.id = event_attachments.event_id
          AND e.group_id::text = current_setting('app.group_id', true)
    ));
ALTER POLICY event_occurrence_completions_isolation ON event_occurrence_completions
    USING (EXISTS (
        SELECT 1 FROM events e
        WHERE e.id = event_occurrence_completions.event_id
          AND e.group_id::text = current_setting('app.group_id', true)
    ));
ALTER POLICY event_reminders_isolation ON event_reminders
    USING (EXISTS (
        SELECT 1 FROM events e
        WHERE e.id = event_reminders.event_id
          AND e.group_id::text = current_setting('app.group_id', true)
    ));
ALTER POLICY scheduled_notifications_isolation ON scheduled_notifications
    USING (EXISTS (
        SELECT 1 FROM events e
        WHERE e.id = scheduled_notifications.event_id
          AND e.group_id::text = current_setting('app.group_id', true)
    ));

-- The two policies named after the family.
ALTER POLICY groups_family_isolation ON groups
    USING (id::text = current_setting('app.group_id', true));
ALTER POLICY groups_family_isolation ON groups RENAME TO groups_group_isolation;

-- An INSERT policy: its expression is a WITH CHECK, and it has no USING.
ALTER POLICY notifications_family_insert ON notifications
    WITH CHECK (group_id::text = current_setting('app.group_id', true));
ALTER POLICY notifications_family_insert ON notifications RENAME TO notifications_group_insert;
