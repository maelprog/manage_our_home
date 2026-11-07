-- #669: Google sign-in from the Android app's native SDK, by ID token
-- (`auth::google_id_token`).
--
-- The nonces `POST /auth/google/nonce` issues, each spent at most once by
-- `POST /auth/google/id-token`. The browser holds the nonce in a flow
-- cookie; this table holds only its SHA-256, so a row says nothing of the
-- nonce nor of whom it was issued to. No account, no family: no
-- `group_id`, no row-level policy. UNLOGGED: a crash of Postgres only
-- empties it, which fails the sign-ins in progress, and they start over.
-- Each issuance deletes the expired rows.
CREATE UNLOGGED TABLE google_sign_in_nonces (
    nonce_hash BYTEA PRIMARY KEY CHECK (octet_length(nonce_hash) = 32),
    expires_at TIMESTAMPTZ NOT NULL
);

-- Both routes count against a per-address limit
-- (`auth::google_id_token::RATE`) in `rate_limit_log`, like the other
-- limits (`0029_rate_limits.sql`, `0043_account_exists_notice_limit.sql`).
-- Only the list of kinds the table accepts changes; 0043 is left as it was.
ALTER TABLE rate_limit_log DROP CONSTRAINT rate_limit_log_kind_check;
ALTER TABLE rate_limit_log ADD CONSTRAINT rate_limit_log_kind_check
    CHECK (kind IN ('recipe_import', 'openfoodfacts', 'account_exists_notice', 'google_sign_in'));
