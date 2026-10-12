-- #428: the three rate limits #427 left in the memory of the `api` process
-- — recipe imports per member, reads sent to Open Food Facts, CSP reports
-- per client address — now count here, like the login lock
-- (`0028_login_throttle.sql`): shared by every replica, kept across
-- restarts, decided under a Postgres advisory lock, on the database's
-- clock. Same choices as `login_throttle`:
--
-- - neither a member's id nor an address is stored: the keys are
--   HMAC-SHA256 pseudonyms under the key the login lock already derives
--   from `LOGIN_THROTTLE_KEY` (`auth::throttle::Pseudonyms::limit`), each
--   limit under a label of its own;
-- - UNLOGGED: no WAL for a counter, and a crash of Postgres only empties
--   it, which lifts every limit early;
-- - not family-scoped: no `group_id`, no row-level policy.

-- The sliding windows (`rate_limit::SlidingWindow`): the admissions still
-- inside the window, oldest first, one row per counted subject — a member
-- for the recipe import, the whole server for Open Food Facts.
CREATE UNLOGGED TABLE rate_limit_log (
    kind     TEXT NOT NULL CHECK (kind IN ('recipe_import', 'openfoodfacts')),
    bucket   BYTEA NOT NULL CHECK (octet_length(bucket) = 32),
    admitted TIMESTAMPTZ[] NOT NULL CHECK (cardinality(admitted) > 0),
    PRIMARY KEY (kind, bucket)
);

-- `POST /csp-report` per client address (`csp_report_throttle`): a fixed
-- window per address, at most `MAX_TRACKED` addresses.
CREATE UNLOGGED TABLE csp_report_throttle (
    scope        BYTEA PRIMARY KEY CHECK (octet_length(scope) = 32),
    requests     INTEGER NOT NULL CHECK (requests > 0),
    window_start TIMESTAMPTZ NOT NULL
);
