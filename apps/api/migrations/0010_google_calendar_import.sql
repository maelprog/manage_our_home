-- Epic 9: Google Calendar import (one-way).
--
-- v1 design decision (docs/v1-scope.md row 9): a private ICS feed URL
-- ("Secret address in iCal format", found under Google Calendar Settings ->
-- a given calendar -> "Integrate calendar") rather than full OAuth2 +
-- Calendar REST API. Google's secret ICS URL already encodes a per-calendar
-- read-only capability token, refreshed instantly if revoked, and needs no
-- consent screen, token refresh flow, or Google Cloud project. Fetching and
-- parsing it on demand (or on a manual "import now" trigger) is enough for
-- one-way, pull-based import — the OAuth + push-notification machinery
-- Google's API offers only pays for itself once bidirectional sync is in
-- scope, which is explicitly deferred past v1. The tradeoff: ICS feeds lag
-- Google's live state by up to a few hours (Google-side caching) and don't
-- support webhooks, so v1 import is inherently "pull, on demand", never
-- real-time — acceptable for a family calendar mirror.
--
-- The feed URL is treated as a bearer credential (anyone with it can read
-- the calendar), so it's stored encrypted via pgcrypto (pgp_sym_encrypt),
-- same technique as Messagerie content / OAuth refresh tokens, with its own
-- dedicated key isolated from both.

CREATE TABLE calendar_imports (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id            UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    created_by          UUID NOT NULL REFERENCES users(id),
    label               TEXT NOT NULL,
    feed_url            BYTEA NOT NULL,            -- pgp_sym_encrypt(url, key)
    last_imported_at    TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX calendar_imports_group_id_idx ON calendar_imports (group_id);

ALTER TABLE calendar_imports ENABLE ROW LEVEL SECURITY;
ALTER TABLE calendar_imports FORCE ROW LEVEL SECURITY;
CREATE POLICY calendar_imports_isolation ON calendar_imports
    USING (group_id::text = current_setting('app.family_id', true));

-- One row per (import connection, external VEVENT UID), mapping to the
-- `events` row it produced. Lets re-running an import be idempotent
-- (update-in-place keyed by UID + Google's SEQUENCE/LAST-MODIFIED, rather
-- than duplicating events every time) without adding an
-- "external id" column to the shared `events` table that every other epic
-- reading `events` would otherwise have to know to ignore.
CREATE TABLE calendar_import_events (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    calendar_import_id  UUID NOT NULL REFERENCES calendar_imports(id) ON DELETE CASCADE,
    event_id            UUID NOT NULL REFERENCES events(id) ON DELETE CASCADE,
    external_uid        TEXT NOT NULL,
    -- ICS DTSTAMP/LAST-MODIFIED of the version we last imported, used to
    -- skip re-writing an occurrence that hasn't changed upstream.
    external_updated_at TIMESTAMPTZ,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (calendar_import_id, external_uid)
);
CREATE INDEX calendar_import_events_import_id_idx ON calendar_import_events (calendar_import_id);

ALTER TABLE calendar_import_events ENABLE ROW LEVEL SECURITY;
ALTER TABLE calendar_import_events FORCE ROW LEVEL SECURITY;
CREATE POLICY calendar_import_events_isolation ON calendar_import_events
    USING (
        EXISTS (
            SELECT 1 FROM calendar_imports ci
            WHERE ci.id = calendar_import_events.calendar_import_id
              AND ci.group_id::text = current_setting('app.family_id', true)
        )
    );
