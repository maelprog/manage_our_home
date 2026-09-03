-- Issue #73 (dashboard) surfaced a real gap in the Agenda epic: an event
-- carries no notion of "who it's for" beyond its creator. This table lets an
-- event be assigned to one or more family members, defaulting to the
-- creator when none is chosen explicitly (apps/api/src/agenda/events.rs).
--
-- A junction table rather than an array column on `events`, same call as
-- `group_members` for group/user pairs: it gets `ON DELETE CASCADE` on both
-- sides for free, and a plain B-tree instead of a GIN one.
--
-- No separate index on `event_id`: the primary key's own index is
-- `(event_id, user_id)` and `event_id` is its leading column, so every
-- lookup this table serves (`WHERE event_id = ANY(...)`, the RLS join)
-- already uses it. A second index on the same prefix would only cost
-- another write per row.

CREATE TABLE event_assignees (
    event_id            UUID NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    user_id             UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (event_id, user_id)
);

-- event_assignees has no direct group_id column, same pattern as
-- event_reminders/event_attachments (0002_agenda.sql): the RLS policy joins
-- back to events.
ALTER TABLE event_assignees ENABLE ROW LEVEL SECURITY;
ALTER TABLE event_assignees FORCE ROW LEVEL SECURITY;
CREATE POLICY event_assignees_isolation ON event_assignees
    USING (
        EXISTS (
            SELECT 1 FROM events e
            WHERE e.id = event_assignees.event_id
              AND e.group_id::text = current_setting('app.family_id', true)
        )
    );
