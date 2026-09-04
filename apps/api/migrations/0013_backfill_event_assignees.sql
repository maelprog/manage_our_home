-- Issue #99. `0011_event_assignees.sql` created the junction table empty:
-- every event already in the database got no assignment row at all, so
-- `assignees_for_events` returned `[]` for it and the dashboard's
-- "Prochains événements" card (apps/web/src/routes/home.rs, `agenda_row`)
-- painted a bare "?" ring followed by an em dash and nothing — on *every*
-- pre-#73 event, until someone re-edited it.
--
-- The assignment those rows should have had is the one #73 chose as the
-- default for a new event: its creator (apps/api/src/agenda/events.rs,
-- `resolve_assignees`, which falls back to `[creator]` on the same terms).
-- Backfilling it is a data fix, not a schema change, so it lands as its own
-- migration rather than an edit to 0011.
--
-- The `NOT EXISTS` guard is load-bearing, and `ON CONFLICT DO NOTHING` is
-- not a substitute for it: the conflict target is (event_id, user_id), so a
-- plain `SELECT id, created_by FROM events` would happily *add* the creator
-- to an event somebody had deliberately assigned to someone else — turning
-- "assigné à Robin" into "assigné à Camille, Robin", complete with a mixed
-- avatar colour. Only events carrying no assignment at all are touched.
-- `ON CONFLICT DO NOTHING` stays as a cheap net under a future edit of that
-- clause; with it in place the statement is a no-op on every re-run.
--
-- No RLS scoping clause, and none is possible: `app.family_id` is per-request
-- and unset during a migration, so the `event_assignees` policy would match
-- no row. It doesn't have to — migrations run as the schema-owning role,
-- which is a superuser and so bypasses row security even under FORCE (see
-- apps/api/README.md on why the *runtime* role must not be: that is the
-- connection RLS exists to constrain, and it is a different role).
INSERT INTO event_assignees (event_id, user_id)
SELECT e.id, e.created_by
FROM events e
WHERE NOT EXISTS (
    SELECT 1 FROM event_assignees ea WHERE ea.event_id = e.id
)
ON CONFLICT DO NOTHING;
