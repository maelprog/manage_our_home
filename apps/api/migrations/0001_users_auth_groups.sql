-- Epic 1: Auth + Groups foundation.
CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE users (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email           TEXT NOT NULL UNIQUE,
    email_verified  BOOLEAN NOT NULL DEFAULT FALSE,
    password_hash   TEXT,
    display_name    TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    deletion_requested_at TIMESTAMPTZ,
    deleted_at      TIMESTAMPTZ
);

CREATE TABLE oauth_identities (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider        TEXT NOT NULL CHECK (provider = 'google'),
    provider_user_id TEXT NOT NULL,
    refresh_token_encrypted BYTEA,
    linked_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (provider, provider_user_id)
);

-- users must have at least one auth method (password or an oauth identity).
-- Enforced with a deferred trigger rather than a CHECK subquery (Postgres
-- disallows subqueries referencing other tables in CHECK constraints).
CREATE OR REPLACE FUNCTION check_user_has_auth_method() RETURNS trigger AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM users u
        WHERE u.id = NEW.id
          AND (u.password_hash IS NOT NULL OR EXISTS (
                SELECT 1 FROM oauth_identities oi WHERE oi.user_id = u.id
              ))
    ) THEN
        RAISE EXCEPTION 'user % has no auth method (password or oauth identity)', NEW.id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE CONSTRAINT TRIGGER users_has_auth_method_trigger
    AFTER INSERT OR UPDATE ON users
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION check_user_has_auth_method();

CREATE OR REPLACE FUNCTION check_user_has_auth_method_from_identity() RETURNS trigger AS $$
DECLARE
    target_user_id UUID;
BEGIN
    target_user_id := COALESCE(NEW.user_id, OLD.user_id);
    IF NOT EXISTS (
        SELECT 1 FROM users u
        WHERE u.id = target_user_id
          AND (u.password_hash IS NOT NULL OR EXISTS (
                SELECT 1 FROM oauth_identities oi WHERE oi.user_id = u.id
              ))
    ) THEN
        RAISE EXCEPTION 'user % has no auth method (password or oauth identity)', target_user_id;
    END IF;
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

CREATE CONSTRAINT TRIGGER oauth_identities_has_auth_method_trigger
    AFTER INSERT OR DELETE ON oauth_identities
    DEFERRABLE INITIALLY DEFERRED
    FOR EACH ROW EXECUTE FUNCTION check_user_has_auth_method_from_identity();

CREATE TABLE sessions (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at      TIMESTAMPTZ NOT NULL,
    revoked_at      TIMESTAMPTZ
);
CREATE INDEX sessions_user_id_active_idx ON sessions (user_id) WHERE revoked_at IS NULL;

CREATE TABLE email_verification_tokens (
    token           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at      TIMESTAMPTZ NOT NULL,
    consumed_at     TIMESTAMPTZ
);

CREATE TABLE password_reset_tokens (
    token           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at      TIMESTAMPTZ NOT NULL,
    consumed_at     TIMESTAMPTZ
);

CREATE TABLE groups (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name            TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_by      UUID NOT NULL REFERENCES users(id)
);

CREATE TYPE group_role AS ENUM ('owner', 'admin', 'standard');

CREATE TABLE group_members (
    group_id        UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    user_id         UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role            group_role NOT NULL,
    joined_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (group_id, user_id)
);

-- Exactly one owner per group, enforced at the DB layer (AC #10).
CREATE UNIQUE INDEX one_owner_per_group ON group_members (group_id) WHERE role = 'owner';

CREATE TABLE invitations (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    group_id        UUID NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
    token           UUID NOT NULL UNIQUE DEFAULT gen_random_uuid(),
    invited_email   TEXT,
    created_by      UUID NOT NULL REFERENCES users(id),
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at      TIMESTAMPTZ NOT NULL,
    consumed_at     TIMESTAMPTZ,
    consumed_by     UUID REFERENCES users(id)
);

CREATE TABLE audit_log (
    id              BIGSERIAL PRIMARY KEY,
    occurred_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    actor_user_id   UUID REFERENCES users(id),
    action          TEXT NOT NULL,
    target_type     TEXT NOT NULL,
    target_id       TEXT NOT NULL,
    metadata        JSONB
);

-- Row-Level Security: every tenant-scoped table is isolated by
-- `app.family_id`, set via `SET LOCAL` at the start of each request
-- transaction (see src/auth/session.rs). Policies use `current_setting`
-- with the `missing_ok` flag so an absent/invalid setting resolves to no
-- rows matching, rather than an error (AC #15).
ALTER TABLE group_members ENABLE ROW LEVEL SECURITY;
ALTER TABLE group_members FORCE ROW LEVEL SECURITY;
CREATE POLICY group_members_isolation ON group_members
    USING (group_id::text = current_setting('app.family_id', true));

ALTER TABLE invitations ENABLE ROW LEVEL SECURITY;
ALTER TABLE invitations FORCE ROW LEVEL SECURITY;
-- Accepting an invitation only has the token, not the group_id, so the
-- policy also allows a row to be seen when its own token matches
-- `app.invitation_token` (set via SET LOCAL for that one lookup). A UUID
-- token is 128 bits of unguessable entropy, so this doesn't weaken
-- isolation between tenants.
CREATE POLICY invitations_isolation ON invitations
    USING (
        group_id::text = current_setting('app.family_id', true)
        OR token::text = current_setting('app.invitation_token', true)
    );

-- `groups` itself needs a different policy: a user must be able to list
-- "my groups" before app.family_id is known for any single one of them.
-- Membership-based visibility instead of a family_id setting.
ALTER TABLE groups ENABLE ROW LEVEL SECURITY;
ALTER TABLE groups FORCE ROW LEVEL SECURITY;
CREATE POLICY groups_membership_isolation ON groups
    USING (
        id::text = current_setting('app.family_id', true)
        OR EXISTS (
            SELECT 1 FROM group_members gm
            WHERE gm.group_id = groups.id
              AND gm.user_id::text = current_setting('app.user_id', true)
        )
    );

-- The application connects as a non-superuser role for tenant-scoped
-- queries so RLS is actually enforced (superusers/table owners bypass RLS
-- unless FORCE is set and the role isn't BYPASSRLS). See README for the
-- role-setup snippet run as part of deployment.
