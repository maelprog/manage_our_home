-- #427: the login lock of #178 (`apps/api/src/auth/throttle.rs`) counted in
-- the memory of the `api` process. With several replicas behind the ingress
-- an attacker got the threshold once per replica, and every deployment
-- reset it. The counters now live here, shared by every replica.
--
-- One row per (client address, email) pair, exactly the in-memory entry it
-- replaces — but neither the address nor the email is stored. `scope` is
-- the HMAC-SHA256 of the client address (`throttle::address_scope`: an IPv4
-- address, or the /64 of an IPv6 one), `pair` the HMAC-SHA256 of the
-- address and the typed email together, both under a key the API holds and
-- the database never sees (`throttle::Pseudonyms`). The per-address share
-- is counted on `scope`. A bare hash would not do: the IPv4 space is small
-- enough to hash whole.
--
-- UNLOGGED: no WAL, so a failed attempt costs no WAL write, and nothing is
-- replicated to a standby. A clean restart of Postgres keeps the rows; a
-- crash truncates the table, which only lifts every lock early — the cost
-- of the counters being a brake, not a record.
--
-- Not family-scoped: no `group_id`, no row-level policy. The runtime role
-- reads and writes it with the grants it holds on every table.

CREATE UNLOGGED TABLE login_throttle (
    scope        BYTEA NOT NULL CHECK (octet_length(scope) = 32),
    pair         BYTEA NOT NULL CHECK (octet_length(pair) = 32),
    attempts     INTEGER NOT NULL CHECK (attempts >= 0),
    window_start TIMESTAMPTZ NOT NULL,
    locked_until TIMESTAMPTZ,
    PRIMARY KEY (scope, pair)
);

