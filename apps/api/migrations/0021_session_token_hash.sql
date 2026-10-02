-- Issue #222. The session cookie used to carry `sessions.id` itself, so
-- whoever could read this table — a leaked dump or backup, a read-only SQL
-- injection, a query log with its parameters — held sessions good for up
-- to 30 days. From here on the cookie carries a random token (32 bytes
-- from the OS CSPRNG, base64url), and the table keeps only its SHA-256.
-- The api computes the hash itself (`auth::session::session_token_hash`),
-- so the raw token never travels in a SQL statement. `id` stays the
-- internal identifier: revocation, `AuthUser.session_id`, the messagerie
-- WebSocket's recheck.
--
-- A fast hash is enough: the token has 256 bits of entropy, nothing to
-- brute-force, and a slow hash would be paid on every request.
--
-- Existing sessions: arbitrated 2026-10-02, every one is revoked here, with
-- no transition code accepting the old cookie. Their `token_hash` is drawn
-- at random, a hash no cookie can match. The retention purge
-- (`jobs::retention_purge`) deletes the revoked rows on its next run. The
-- cookie's name changes in the same release (#224, `__Host-session_id`
-- behind `SECURE_COOKIES`), so everyone logs in again once, not twice.
ALTER TABLE sessions ADD COLUMN token_hash BYTEA;

UPDATE sessions SET revoked_at = now() WHERE revoked_at IS NULL;
UPDATE sessions SET token_hash = gen_random_bytes(32);

ALTER TABLE sessions
    ALTER COLUMN token_hash SET NOT NULL,
    ADD CONSTRAINT sessions_token_hash_key UNIQUE (token_hash),
    ADD CONSTRAINT sessions_token_hash_is_sha256 CHECK (octet_length(token_hash) = 32);

COMMENT ON COLUMN sessions.token_hash IS
    'SHA-256 of the token the session cookie carries (issue #222). The token itself is stored nowhere; sessions.id is an internal identifier and opens nothing.';
