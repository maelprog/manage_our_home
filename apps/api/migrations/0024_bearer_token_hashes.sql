-- Issue #335. Since #222 the session token is stored only by its SHA-256;
-- the three other bearer tokens were still stored as they are mailed out
-- (UUIDs, 0001), so a copy of the database was enough to use them: an
-- invitation let any account into the group for 7 days (the invited
-- address is not checked at acceptance), a password reset token took the
-- account for an hour, a verification token validated an address for 24
-- hours without owning it.
--
-- From here on each is built like the session token
-- (`auth::token::new_token`): 32 bytes from the OS CSPRNG handed out in
-- unpadded base64url, and the table keeps only their SHA-256, in
-- `token_hash`. The api computes the hash itself, so the token never
-- travels in a SQL statement.
--
-- Tokens still pending are invalidated, not converted (a UUID is not a
-- 32-byte token, and its hash would keep the 128 bits it had): each row
-- gets a random `token_hash` no link can match, and an `expires_at` no
-- later than now, so the rows say what they now are — expired — in the
-- account export, and the retention purge (`jobs::retention_purge`) takes
-- them on its usual schedule. The cost: an invitation to send again, a
-- reset or verification link to ask for again.

-- email_verification_tokens -------------------------------------------------
ALTER TABLE email_verification_tokens ADD COLUMN token_hash BYTEA;
UPDATE email_verification_tokens
    SET token_hash = gen_random_bytes(32),
        expires_at = least(expires_at, now());
ALTER TABLE email_verification_tokens
    DROP CONSTRAINT email_verification_tokens_pkey,
    DROP COLUMN token,
    ALTER COLUMN token_hash SET NOT NULL,
    ADD CONSTRAINT email_verification_tokens_pkey PRIMARY KEY (token_hash),
    ADD CONSTRAINT email_verification_tokens_token_hash_is_sha256
        CHECK (octet_length(token_hash) = 32);
COMMENT ON COLUMN email_verification_tokens.token_hash IS
    'SHA-256 of the token the verification link carries (issue #335). The token itself is stored nowhere.';

-- password_reset_tokens -----------------------------------------------------
ALTER TABLE password_reset_tokens ADD COLUMN token_hash BYTEA;
UPDATE password_reset_tokens
    SET token_hash = gen_random_bytes(32),
        expires_at = least(expires_at, now());
ALTER TABLE password_reset_tokens
    DROP CONSTRAINT password_reset_tokens_pkey,
    DROP COLUMN token,
    ALTER COLUMN token_hash SET NOT NULL,
    ADD CONSTRAINT password_reset_tokens_pkey PRIMARY KEY (token_hash),
    ADD CONSTRAINT password_reset_tokens_token_hash_is_sha256
        CHECK (octet_length(token_hash) = 32);
COMMENT ON COLUMN password_reset_tokens.token_hash IS
    'SHA-256 of the token the password reset link carries (issue #335). The token itself is stored nowhere.';

-- invitations ---------------------------------------------------------------
-- The policy reads `token`: it goes first, and comes back on the hash.
DROP POLICY invitations_isolation ON invitations;

ALTER TABLE invitations ADD COLUMN token_hash BYTEA;
UPDATE invitations
    SET token_hash = gen_random_bytes(32),
        expires_at = least(expires_at, now());
ALTER TABLE invitations
    DROP COLUMN token,
    ALTER COLUMN token_hash SET NOT NULL,
    ADD CONSTRAINT invitations_token_hash_key UNIQUE (token_hash),
    ADD CONSTRAINT invitations_token_hash_is_sha256
        CHECK (octet_length(token_hash) = 32);
COMMENT ON COLUMN invitations.token_hash IS
    'SHA-256 of the token the invitation link carries (issue #335). The token itself is stored nowhere.';

-- Accepting an invitation only has the token, not the group_id, so the
-- policy also lets a row be seen when its own hash matches
-- `app.invitation_token`, set via SET LOCAL for that one lookup
-- (`auth::session::token_scoped_tx`). The setting carries the hash in
-- lowercase hex (`auth::token::hash_hex`), never the token: the api hashes
-- what the link carries first. Knowing a row's hash opens it here, but the
-- hash is only ever derived from a token of 256 bits of entropy, so this
-- does not weaken isolation between tenants. `encode` rather than `decode`
-- on the setting: a malformed setting then matches nothing instead of
-- raising.
CREATE POLICY invitations_isolation ON invitations
    USING (
        group_id::text = current_setting('app.family_id', true)
        OR encode(token_hash, 'hex') = current_setting('app.invitation_token', true)
    );
