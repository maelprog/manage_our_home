-- Epic 2: Agenda (events, tasks-as-events, recurrence, reminders, files).

CREATE TABLE events (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id            UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    created_by          UUID NOT NULL REFERENCES users(id),
    title               TEXT NOT NULL,
    description         TEXT,
    location            TEXT,
    starts_at           TIMESTAMPTZ NOT NULL,
    ends_at             TIMESTAMPTZ NOT NULL,
    all_day             BOOLEAN NOT NULL DEFAULT FALSE,
    -- A task is an agenda event, not a separate entity (architecture.md's
    -- epic-scoping clarification): `is_task` + `completed_at` are the only
    -- task-specific fields, everything else (recurrence, reminders) is
    -- shared with regular events.
    is_task             BOOLEAN NOT NULL DEFAULT FALSE,
    completed_at        TIMESTAMPTZ,
    -- RFC 5545 RRULE string (e.g. "FREQ=WEEKLY;BYDAY=MO,WE;COUNT=10"),
    -- parsed/expanded on read via the `rrule` crate (src/agenda/recurrence.rs).
    -- NULL means a one-off event. Chosen over a custom recurrence schema so
    -- it maps directly onto the future Google Calendar import (epic #9).
    rrule               TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (ends_at >= starts_at),
    CHECK (completed_at IS NULL OR is_task)
);
CREATE INDEX events_group_range_idx ON events (group_id, starts_at, ends_at);

ALTER TABLE events ENABLE ROW LEVEL SECURITY;
ALTER TABLE events FORCE ROW LEVEL SECURITY;
CREATE POLICY events_isolation ON events
    USING (group_id::text = current_setting('app.family_id', true));

CREATE TABLE event_reminders (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_id            UUID NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    -- Minutes before each occurrence's `starts_at` that the notification
    -- should fire.
    offset_minutes      INTEGER NOT NULL CHECK (offset_minutes >= 0),
    channel             TEXT NOT NULL DEFAULT 'email' CHECK (channel = 'email'),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX event_reminders_event_id_idx ON event_reminders (event_id);

-- event_reminders has no direct group_id column, so its RLS policy joins
-- back to events (same pattern as invitations -> groups membership check).
ALTER TABLE event_reminders ENABLE ROW LEVEL SECURITY;
ALTER TABLE event_reminders FORCE ROW LEVEL SECURITY;
CREATE POLICY event_reminders_isolation ON event_reminders
    USING (
        EXISTS (
            SELECT 1 FROM events e
            WHERE e.id = event_reminders.event_id
              AND e.group_id::text = current_setting('app.family_id', true)
        )
    );

CREATE TABLE event_attachments (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_id            UUID NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    uploaded_by         UUID NOT NULL REFERENCES users(id),
    -- Object key in the MinIO bucket; never a public URL (src/storage.rs
    -- issues short-lived presigned GET URLs on demand).
    storage_key         TEXT NOT NULL UNIQUE,
    filename            TEXT NOT NULL,
    mime_type           TEXT NOT NULL,
    size_bytes          BIGINT NOT NULL CHECK (size_bytes > 0),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX event_attachments_event_id_idx ON event_attachments (event_id);

ALTER TABLE event_attachments ENABLE ROW LEVEL SECURITY;
ALTER TABLE event_attachments FORCE ROW LEVEL SECURITY;
CREATE POLICY event_attachments_isolation ON event_attachments
    USING (
        EXISTS (
            SELECT 1 FROM events e
            WHERE e.id = event_attachments.event_id
              AND e.group_id::text = current_setting('app.family_id', true)
        )
    );

-- Persisted job-queue table (architecture.md correction #4): reminders must
-- survive restarts/deploys, so each due notification is a row here, polled
-- by the worker in src/jobs/scheduled_notifications.rs rather than an
-- in-process scheduler. `occurrence_at` disambiguates recurring events: one
-- row per (reminder, occurrence) pair, refilled on a rolling window by the
-- worker rather than materialized indefinitely for open-ended RRULEs.
CREATE TABLE scheduled_notifications (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_reminder_id   UUID NOT NULL REFERENCES event_reminders(id) ON DELETE CASCADE,
    event_id            UUID NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    occurrence_at        TIMESTAMPTZ NOT NULL,
    fire_at             TIMESTAMPTZ NOT NULL,
    status              TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'sent', 'failed')),
    attempts            INTEGER NOT NULL DEFAULT 0,
    last_error          TEXT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (event_reminder_id, occurrence_at)
);
CREATE INDEX scheduled_notifications_pending_idx ON scheduled_notifications (fire_at) WHERE status = 'pending';

ALTER TABLE scheduled_notifications ENABLE ROW LEVEL SECURITY;
ALTER TABLE scheduled_notifications FORCE ROW LEVEL SECURITY;
CREATE POLICY scheduled_notifications_isolation ON scheduled_notifications
    USING (
        EXISTS (
            SELECT 1 FROM events e
            WHERE e.id = scheduled_notifications.event_id
              AND e.group_id::text = current_setting('app.family_id', true)
        )
    );
-- The worker runs as the migration-owning role (bypasses RLS by design,
-- same trust boundary as jobs/account_purge.rs) since it must see pending
-- notifications across every family, not just one request's scope.
