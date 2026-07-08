-- Epic 7: Messagerie.
--
-- One text-only discussion thread per family (docs/v1-scope.md epic #7),
-- real-time via Axum WebSockets. `content` is stored encrypted via
-- pgcrypto (pgp_sym_encrypt), same technique already used for OAuth
-- refresh tokens (0002_auth.sql / src/auth/oauth_google.rs), but with a
-- dedicated `message_encryption_key` isolated from `oauth_encryption_key`.

CREATE TABLE messages (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id            UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    created_by          UUID NOT NULL REFERENCES users(id),
    content             BYTEA NOT NULL,           -- pgp_sym_encrypt(plaintext, key)
    edited_at           TIMESTAMPTZ,               -- set on update, NULL if never edited
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Covers both plain `WHERE group_id = $1` lookups (leading column) and the
-- cursor-paginated history query's `ORDER BY created_at DESC, id DESC`.
CREATE INDEX messages_group_id_created_at_id_idx
    ON messages (group_id, created_at DESC, id DESC);

ALTER TABLE messages ENABLE ROW LEVEL SECURITY;
ALTER TABLE messages FORCE ROW LEVEL SECURITY;
CREATE POLICY messages_isolation ON messages
    USING (group_id::text = current_setting('app.family_id', true));
