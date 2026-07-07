-- Fixes the bug where a recurring task's single `events.completed_at`
-- column marked *every* occurrence of the series as done. Completion is now
-- tracked per (event, occurrence) pair for recurring tasks; `events.completed_at`
-- remains authoritative only for one-off tasks (rrule IS NULL).
CREATE TABLE event_occurrence_completions (
    event_id            UUID NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    occurrence_at       TIMESTAMPTZ NOT NULL,
    completed_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_by        UUID NOT NULL REFERENCES users(id),
    PRIMARY KEY (event_id, occurrence_at)
);

-- Same join-back-to-events RLS pattern as event_reminders/event_attachments.
ALTER TABLE event_occurrence_completions ENABLE ROW LEVEL SECURITY;
ALTER TABLE event_occurrence_completions FORCE ROW LEVEL SECURITY;
CREATE POLICY event_occurrence_completions_isolation ON event_occurrence_completions
    USING (
        EXISTS (
            SELECT 1 FROM events e
            WHERE e.id = event_occurrence_completions.event_id
              AND e.group_id::text = current_setting('app.family_id', true)
        )
    );
