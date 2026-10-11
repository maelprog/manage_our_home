-- #427: the login lock of #178 (`apps/api/src/auth/throttle.rs`) counted in
-- the memory of the `api` process. With several replicas behind the ingress
-- an attacker got the threshold once per replica, and every deployment
-- reset it. The counters now live here, shared by every replica.
--
-- One row per (client scope, email) pair, exactly the in-memory entry it
-- replaces: `scope` is `throttle::address_scope` (an IPv4 address, or the
-- /64 of an IPv6 one), `email` the key's normalised email, truncated to 320
-- bytes by `throttle::key` (the CHECK repeats that bound for any other
-- writer).
--
-- UNLOGGED: no WAL, so a failed attempt costs no WAL write, and nothing is
-- replicated to a standby. A clean restart of Postgres keeps the rows; a
-- crash truncates the table, which only lifts every lock early — the cost
-- of the counters being a brake, not a record.
--
-- Not family-scoped: no `group_id`, no row-level policy. The runtime role
-- reads and writes it with the grants it holds on every table.

CREATE UNLOGGED TABLE login_throttle (
    scope        INET NOT NULL,
    email        TEXT NOT NULL CHECK (octet_length(email) <= 320),
    attempts     INTEGER NOT NULL CHECK (attempts >= 0),
    window_start TIMESTAMPTZ NOT NULL,
    locked_until TIMESTAMPTZ,
    PRIMARY KEY (scope, email)
);
