# apps/api — manage_our_home backend

Epic 1 — Auth + Groups. See `../../docs/architecture.md` and
`../../docs/idea.md` for product/architecture context.

## Setup

```
cp .env.example .env   # fill in real secrets
createdb manage_our_home
cargo sqlx prepare      # regenerate .sqlx query cache after schema changes
cargo run               # applies migrations through MIGRATION_DATABASE_URL
```

`MIGRATION_DATABASE_URL` has no default and no fallback — `cargo run` stops
before opening the runtime pool if it is unset. For a single-role local
database, point it at the same superuser role `DATABASE_URL` uses; see
"Deployment note on Row-Level Security" below for why real deployments must
not.

## Running tests

Tests need a reachable Postgres (used via `sqlx::test`, which provisions a
fresh throwaway database per test using `DATABASE_URL`'s server). The
connecting role must have `CREATEROLE` (RLS tests create/drop a scoped
throwaway role to prove isolation against a non-superuser connection —
see `tests/rls.rs`).

```
export DATABASE_URL=postgres://mhome:mhome@localhost:5432/postgres
cargo test
```

`DATABASE_URL`'s role is usually a superuser, which bypasses RLS, and by
default the handlers under test use it too. To drive them as the
`NOSUPERUSER NOBYPASSRLS` runtime role instead, as CI's `test-nobypassrls`
job does (#213), create that role with the default privileges the job
declares in `template1`, then set `FLOW_TEST_RUNTIME_ROLE` and
`FLOW_TEST_RUNTIME_ROLE_PASSWORD`. Only the handlers' pool switches role
(`runtime_pool` in `tests/common/mod.rs`): migrations, fixtures and
assertions stay on `DATABASE_URL`.

## Deployment note on Row-Level Security

RLS policies use `FORCE ROW LEVEL SECURITY`, but Postgres superusers (and,
without FORCE, table owners) always bypass RLS. The application's runtime
connection role must be a plain, non-superuser role that only owns the
privileges it needs — otherwise the RLS layer described in
`../../docs/architecture.md` is silently inert. Example role/grant for the
normal app connection (`DATABASE_URL`):

```sql
CREATE ROLE app_role LOGIN PASSWORD '...' NOSUPERUSER NOBYPASSRLS;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO app_role;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO app_role;
```

`app_role` no longer owns the tables (see `migration_role` below), so the
sequence grant is not optional: `audit_log.id` is `BIGSERIAL`, and an
`INSERT` by a non-owner without `USAGE` on its sequence fails on `nextval()`.

### `migration_role` (#105) — who applies the migrations

The role above is the one that serves requests. It is **not** the one that
applies the schema. Migrations run on their own connection,
`MIGRATION_DATABASE_URL`, as a third role that owns the tables and carries
`BYPASSRLS`:

```sql
CREATE ROLE migration_role LOGIN PASSWORD '...' NOSUPERUSER BYPASSRLS;
GRANT USAGE, CREATE ON SCHEMA public TO migration_role;
-- `0001_users_auth_groups.sql` opens on CREATE EXTENSION IF NOT EXISTS
-- pgcrypto. pgcrypto is a trusted extension, so a non-superuser may install
-- it — but only with CREATE on the *database*, which CREATE on the schema
-- does not confer. Without this line the very first migration stops on
-- "permission denied to create extension" and nothing is applied at all.
GRANT CREATE ON DATABASE manage_our_home TO migration_role;
-- Every table a migration creates belongs to migration_role, so the grants
-- the other two roles need must be declared as its default privileges —
-- otherwise the next migration ships a table nobody else can read.
ALTER DEFAULT PRIVILEGES FOR ROLE migration_role IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO app_role, admin_role;
ALTER DEFAULT PRIVILEGES FOR ROLE migration_role IN SCHEMA public
    GRANT USAGE, SELECT ON SEQUENCES TO app_role, admin_role;
```

If you would rather not hand the migration role `CREATE` on the database,
install the extension once as a superuser instead — `CREATE EXTENSION IF NOT
EXISTS pgcrypto;` — and drop that `GRANT` line. `0001`'s statement is then a
no-op. That is the route `infra/postgres/init/01-roles.sh` takes, because a
superuser is running there anyway at first boot.

Why it exists: every family-scoped table is `FORCE ROW LEVEL SECURITY` with
a policy keyed on `current_setting('app.family_id', true)`, which is `NULL`
outside an HTTP request. DDL is not subject to RLS — which is why
`0001..0012`, all pure DDL, never noticed — but the moment a migration does
DML, `app_role` reads its source tables back **empty**. The statement
applies to zero rows, reports `INSERT 0 0`, exits 0, and `sqlx::migrate!`
records the migration as applied in the same transaction without ever
looking at the row count. It never runs again. Nothing logs, nothing warns.
`0013_backfill_event_assignees.sql` is the first migration to do DML and the
one that made this visible.

Two things it will not let you get wrong, mirroring `reconcile-attachments`
below:

- **`MIGRATION_DATABASE_URL` is required, with no `DATABASE_URL` fallback.**
  A fallback would put the migration back on the runtime role — precisely
  the configuration this section exists to rule out — and it would do so in
  silence. Unset, or set to an empty string, the API refuses to start.
- **The connection is checked before anything is applied.** `migrations::apply`
  asserts `rolsuper OR rolbypassrls` for its own role and aborts if neither
  holds, so a `MIGRATION_DATABASE_URL` pointed at the wrong role fails loudly
  instead of recording a migration that did nothing. An unknown role fails
  closed too: the defect being guarded against leaves no trace, so an
  inconclusive answer is treated as a refusal.

The elevated connection is opened for the migration pass and closed again
before the server starts listening; it is not held for the life of the
process.

`infra/` sets this up for the shipped compose stack:
`postgres/init/01-roles.sh` creates both `migration_role` and `admin_role` at
first boot of the postgres volume, and `docker-compose.yml` passes
`MIGRATION_DATABASE_URL` to the api service.

### Epic #8 — `admin_role` (superadmin endpoints)

The three `/admin/*` endpoints (`src/user_admin/`) are a deliberate, narrow
exception to the RLS boundary above: a superadmin needs to list groups and
users across every family, which the normal RLS-scoped role can never do
by design. Rather than weaken the `groups`/`group_members` policies, that
one code path runs on a **second** connection pool (`AppState.admin_db`,
`ADMIN_DATABASE_URL` env var), authenticated as a dedicated role with
`BYPASSRLS`. Application code still gates access before any query ever
reaches this pool — see the `SuperAdminUser` extractor, which requires a
valid session *and* `users.is_superadmin = true`, else 403 — so `BYPASSRLS`
here is a controlled, audited exception rather than a general bypass.

Two other things run on this pool, and neither is a request handler:

- the **daily attachment reconcile** pass (#215, see the Ops section below),
  which needs the unscoped `event_attachments` read and deletes nothing in
  Postgres — only MinIO objects;
- the **hourly retention purge** (#138, `src/jobs/retention_purge.rs`),
  which `DELETE`s rows, across every family, from `audit_log`,
  `email_verification_tokens`, `password_reset_tokens`, `invitations` and
  `sessions`. It is the only unscoped writer of Postgres rows on this pool.
  Of those five tables only `invitations` is RLS'd at all, and it is `FORCE
  ROW LEVEL SECURITY`: with no `app.family_id` set, its `DELETE` on the
  runtime role matches **no row** and the pass would report a clean sweep
  having erased none of the invited third parties' addresses it exists to
  erase. So it calls the same `ensure_bypasses_rls` guard as the reconcile
  pass — `rolsuper OR rolbypassrls` on its own connection — and aborts the
  whole pass otherwise, the four unguarded tables included, logging
  `retention purge job failed` at ERROR once an hour and deleting nothing.

No request handler other than the three `/admin/*` ones touches `admin_db`.

```sql
CREATE ROLE admin_role LOGIN PASSWORD '...' NOSUPERUSER BYPASSRLS;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO admin_role;
```

If `ADMIN_DATABASE_URL` isn't set, `main.rs` falls back to `DATABASE_URL`
(convenient for local dev where a single role is fine); production
deployments should always set both to the distinct roles above.

## Ops: `reconcile-attachments` (#58)

Second binary in this crate. Finds objects in the MinIO attachments bucket
that no `event_attachments` row points at, and — only with `--apply` —
deletes them.

```
cargo run --bin reconcile-attachments -- --help
cargo run --bin reconcile-attachments               # dry run, 24h window
cargo run --bin reconcile-attachments -- --apply
```

Orphans come from three places. Two are historic and now closed: events
deleted before #56, and groups deleted before #59. The third is ongoing —
`upload_attachment` writes the object before the metadata row commits
(`src/agenda/attachments.rs`). #63 reordered that handler so a failed
`put_object` rolls the row back, which removed the old compensating
delete, but two gaps survive it: a failed `tx.commit()` leaves the object
with no row, and a client disconnect or process death drops the future so
the transaction rolls back while the object stays. Orphans therefore keep
accruing at a low rate.

**The API runs this pass itself, daily, with deletion on** (#215,
`src/jobs/attachment_reconcile.rs`): first pass at startup, then every 24h,
default 24h window, whole bucket. An orphan has no row, so neither account
nor group deletion ever reaches it; without the schedule a user's file
would stay in the bucket until someone ran the binary. The job runs on the
admin pool (`ADMIN_DATABASE_URL`): if that pool falls back to a
`DATABASE_URL` role without `BYPASSRLS`, the guard below refuses every pass
and the job logs `attachment reconcile job failed` at ERROR once a day,
deleting nothing. Each pass logs its counts at INFO, and each key at INFO
before deleting it. The binary stays for dry runs and `--prefix`-scoped
passes.

Two things it will not let you get wrong:

- **The binary requires `ADMIN_DATABASE_URL`, with no `DATABASE_URL`
  fallback.** (The scheduled job inherits `main.rs`'s fallback for the admin
  pool, described above; the RLS check below is what stops it there.)
  `event_attachments` is `FORCE ROW LEVEL SECURITY`, so an unscoped
  `SELECT storage_key` on a normal app connection returns **zero rows, not
  all rows** — and zero known keys means every object in the bucket
  classifies as an orphan. The pass checks `rolsuper OR rolbypassrls` for
  its own connection and aborts if neither holds, so a misconfigured run
  fails loudly instead of emptying the bucket. Point it at `admin_role`.
- **Objects newer than `--min-age-hours` (default 24) are never deleted.**
  `put_object` runs well before the row commits, so a freshly written
  object with no row may be a live upload mid-flight rather than garbage.

  Corollary: **`--min-age-hours 0` disables that protection entirely.** It
  makes every unreferenced object eligible the instant it is listed,
  including the ones belonging to uploads in flight right now — their row
  has not committed yet, so they are indistinguishable from garbage and
  the pass will delete the bytes out from under a request the user is
  watching succeed. It exists for tests, which pin the clock instead of
  sleeping out a real window. Don't use it against a live stack; if you
  need a narrower window there, give it a real one (`--min-age-hours 1`).

`--prefix` narrows the listing; keys are `{group_id}/{event_id}/{uuid}`, so
`--prefix <group-id>/` walks one family. Every deleted key is written to
stdout and logged at INFO before the delete — the delete is unrecoverable
and that record is the only trace left.

## Ops: where a login's time goes (`login_timing`, #113)

`POST /auth/login` splits its own wall time between the argon2 verification,
its two SQL statements and the remainder, and emits the split as one
structured line. The line is a `debug` event on its own tracing target, so
an **already-built** binary reports it — no rebuild, no patch:

```
RUST_LOG=login_timing=debug ./manage_our_home
```

```
DEBUG login_timing: login timing outcome="ok" total_us=271310 lookup_us=435
  verify_us=257946 session_us=13498 sql_us=13917 other_us=27
```

- `lookup_us` — `SELECT … FROM users WHERE email = $1`, plus the pool
  checkout it waits for first
- `verify_us` — argon2id verification (CPU, no I/O)
- `session_us` — `INSERT INTO sessions … RETURNING id`, its commit, and its
  own pool checkout
- `sql_us` — `lookup + session`
- `other_us` — `total` minus the three above: cookie building, the handler's
  own bookkeeping. It is the thinnest of the four, tens of microseconds, and
  nothing that blocks lives in it.

Two costs are **not** in `other_us`, and both surprise people:

- **Waiting for a database connection** is inside `lookup_us` or
  `session_us`, whichever asked for it — the handler goes through the pool,
  not a held connection. Kill the pool's connections
  (`pg_terminate_backend`) and those two jump by orders of magnitude while
  `other_us` does not move: 0,70 ms → 35,9 ms on `lookup_us`, 6,2 ms →
  39,2 ms on `session_us`, `other_us` 37 µs → 30 µs.
  A corollary for reading the numbers: `lookup_us` is checkout **plus**
  query, and the `SELECT` is the small half — an index scan on
  `users_email_key` whose `Execution Time` is a few hundredths of a
  millisecond, two orders of magnitude under a `lookup_us` of ~0,4 ms.
  The figures in this bullet are the ones reported in the body of #177,
  measured in that PR's own environment; they are not constants of the
  instrument, and another machine, another build profile or a loaded CI
  runner gives other values. What carries over is
  which phases move and which one does not, not by how much.
- **Request-body deserialization** is outside `total_us` entirely: axum's
  extractor runs it before the handler starts.

One line per request **that reaches the handler**, and `outcome` says which
of four endings produced these phases. Requests axum's extractor refuses
never reach it and emit nothing: malformed JSON (400), a missing `password`
(422), a wrong or absent `content-type` (415). A `/auth/login` request with
no line is one of those, not a lost measurement.

- `"ok"` — the login completed.
- `"rejected"` — 401: unknown email, Google-only account, wrong password, or
  unverified address. Since #178 every one of them pays `lookup_us` **and**
  `verify_us` — an account with no stored hash is checked against a decoy —
  so only `session_us` is zero. A `"rejected"` line with `verify_us=0` is
  the enumeration oracle back.
- `"throttled"` — 429: the (client address, email) pair is locked (see
  below). All four phases are zero: the lock is consulted before the lookup
  and before argon2id, which is the point of it.
- `"error"` — every error that is not one of the above. On today's login path those
  are 500s (a statement failed, or the hashing did), but read the label as
  "not a refusal" rather than as a status code: it is derived from the error
  type, so anything new on this path lands here instead of being mislabelled
  a refusal. Phases are zero here too, for the opposite reason — not "never
  needed" but "never finished". The two are kept apart on purpose: collapsed
  into one label, a crashed `INSERT` would read as a wrong password and the
  phase that actually broke would be invisible.

Numbers measured on this instrumentation are in the body of the PR that
added it. The short version: on the debug profile the e2e gate builds,
`verify_us` is ~95 % of the request and none of the three phases grows with
the size of `users` — so a login that gets slower as a database fills up is
not getting slower here.

**Which refusal was it?** Deliberately not on that line (#178). Whether a
401 came from an unknown email, a Google-only account, a wrong password or
an unverified address is exactly what the enumeration oracle leaked; a log
line saying so per request would hand it to whoever reads the journal. The
split is published instead as a cumulative aggregate on the same target, at
most once a minute and only once at least 20 new **refusals** have happened
since the previous line, with seven counts and nothing that identifies an
attempt:

```
DEBUG login_timing: login branches ok=41 unknown_email=3 no_password=0
  wrong_password=5 unverified=1 throttled=0 error=0
```

The counts are since the process started. To check the fix still holds, the
refusal counters should move while the per-request `verify_us` of
`"rejected"` lines stays in one population.

The batch is counted in refusals, not logins, because the per-request line
already says `ok`, `throttled` or `error`: with a batch of logins, 19 `ok`
and one `rejected` between two lines would let the difference name that
refusal's branch. What it guarantees is that at least 20 `"rejected"` lines
separate two aggregate lines and that their branches are only given in
total. That is not anonymity: if all 20 took the same branch the difference
says so for each, and someone who reads the journal *and* sends 19 of the
refusals with emails they know to be unknown can isolate the 20th.

## Login lock and client addresses (#178)

`POST /auth/login` admits at most 10 attempts per **(client address, email)**
pair within 15 minutes, then locks the pair for 15 minutes and answers `429
{"error": "too_many_attempts"}`. The pair, never the email alone: a lock per
account would let anyone who knows a household member's address lock them
out. Constants are in `src/auth/throttle.rs`.

An attempt is **counted when it is admitted**, in the same step as the lock
check and before any work; a success then clears the pair. So a locked
request costs no argon2id, and a burst of concurrent requests on one pair
gets 10 hashes, not one per request (counting refusals only once known let
40 concurrent attempts through as 40 × 401). An attempt that ends in a 500
counts too.

One address holds at most **50 pairs** — distinct emails tried from it that
have neither succeeded nor gone stale (50 per household address is a user
decision on #178). A 51st email from that address gets
the same 429, and only that address pays for it. Without that share, one
address could fill the table and churn new emails to evict, and so reset,
the pair it was attacking (measured against the previous version: 9 000
attempts admitted on one pair for 1 000 new emails).

The table holds at most 10 000 pairs, so a full table spans at least 200
addresses — 200 distinct /64 blocks, for IPv6 clients. When it is full of
live pairs, a new pair **evicts** one from the
addresses holding the most pairs — among all their pairs taken together, the
unlocked one with the oldest window; a locked one, soonest to expire, only
if none of those addresses holds an unlocked pair — rather than going
uncounted. A pair can only be evicted once no address holds more pairs than
its own: for an address holding `k` pairs that takes the table spread over
at least `10 000 / k` addresses (10 000 for an address holding one).

Each eviction resets up to 10 attempts on the evicted pair, its lock
included — a locked pair can be evicted, and then admits ten fresh attempts —
and **it can be repeated**: an attacker who controls enough addresses to keep the table full
replays it as often as they like within one window, and nothing bounds the
total. The review of #178 measured 27 000 attempts admitted on one pair for
3 000 cycles in one window (figure from the review, not reproduced here).
The share makes every cycle cost a full table of live pairs spread over at
least 200 addresses; it does not limit the number of cycles.

The counter is **in this process's memory**, which is correct only because
`infra/docker-compose.yml` runs one `api`. **If `api` ever runs as more than
one instance, it must move to a shared store (Redis/Valkey, not Postgres —
a database write per failed attempt is itself an amplification).**

**What counts as one address (#198).** An IPv4 address stands for itself. An
IPv6 one is reduced to its **/64** before it is used as a key: a residential
line is delegated a whole /64 and picks any address inside it, so the /128 on
the wire is a handle the client renews at will — keyed on it, neither the
lock nor the 50-pair share bounds anything. The grouping stops at /64, the
smallest block an end site is delegated; /56 and /48 go to different
subscribers, and grouping there would hand out the lockout as a weapon.
`::ffff:a.b.c.d` is folded back to `a.b.c.d` first (unreachable with the
`0.0.0.0` listener shipped here, but a `::` listener would deliver it, and
masking those to /64 would fold all of IPv4 into a single key).

This bounds the rotation, it does not end it: a subscriber delegated a /56
holds 256 /64s and a /48 holds 65 536, each a key of its own — at least
2 560 and 655 360 attempts per window on one email. Those are floors, not
ceilings: eviction from a full table resets pairs and nothing bounds how
often it is replayed (see above). Grouping wider is not the answer (those
blocks belong to different subscribers); a cap on the number of blocks
would have to sit above the key, and none exists today.

The /64 never groups two subscribers of the global unicast space together.
That is not an absolute over the whole address space: `64:ff9b::/96`
(NAT64) and the deprecated `::a.b.c.d` carry an IPv4 address in their low
bits, so every client behind such a translator would collapse onto one key.
Neither form reaches the `0.0.0.0` listener shipped here and neither is
canonicalised; a translator in front of a `::` listener would need the same
treatment as `::ffff:`.

The client address comes from `X-Forwarded-For`, which is only believed from
a peer listed in **`TRUSTED_PROXY_CIDRS`** (comma-separated CIDRs or bare
addresses; default `127.0.0.0/8,::1/128,172.16.0.0/12`). The entry used is
the rightmost one that is not itself a trusted peer. From any other peer the
header is ignored and the peer address is the client. The API refuses to
start on a malformed list.

Get this list wrong and the lock fails in one of two ways:

- **too narrow** (it misses `caddy`/`web`): every browser resolves to the
  proxy's address, and the lock becomes one lock for the whole household;
- **too wide** (it covers addresses browsers connect from): **if those
  clients can reach `web` or `api` directly** — not only through Caddy,
  which discards the header a browser sends — they choose their own
  address, dodge the lock, or aim it at someone else. With
  `infra/docker-compose.yml` as shipped only Caddy publishes a port, so this
  needs a port published on `web` or `api`, or a client on the Compose
  network itself.

apps/web relays the chain on its internal call and appends the address it
saw (`apps/web/src/client_ip.rs`). Caddy replaces any `X-Forwarded-For` a
browser sends with the address it saw (checked against `caddy:2`, see
`infra/Caddyfile`).

### When Caddy cannot see the browser's address

Everything above assumes the address Caddy accepts the connection from *is*
the browser's. With Compose's `ports: "80:80"` that holds only when Docker
forwards the port with NAT rules that keep the source address. It does
**not** hold when the connection goes through `docker-proxy`, Docker's
userland proxy, which reconnects to the container from the network's
gateway: every browser then reaches Caddy as the same `172.x.0.1`. That is
the case, among others, for

- an IPv6 client on a host whose Compose network is IPv4-only;
- Docker Desktop (macOS, Windows), where the port goes through its VM proxy;
- rootless Docker, whose port driver hides the source by default;
- a daemon started with `"userland-proxy": true` where the NAT rules do not
  apply (loopback traffic, for instance).

There is no error: the gateway sits inside `172.16.0.0/12`, so every hop is
trusted and every client resolves to the gateway. `client_ip::resolve` does
know when it falls back to a trusted address — no untrusted hop in the
chain — but nothing logs or acts on it today. The key becomes **(one
address, email)** — the email alone in practice — and ten wrong passwords
from anyone lock that account for everyone for 15 minutes — the denial of
service the pair exists to prevent. It gets worse with the per-address
share: 50 wrong logins on 50 made-up emails from anywhere use up the share
of the one address everybody appears to have, and every *new* email is
refused with a 429 until those pairs go stale. The attacker can keep it that
way indefinitely: one attempt on each of the 50 pairs right after they go
stale re-arms them, about 50 requests every 15 minutes.

To check a deployment, open the site in a browser from another machine
(not from the Docker host itself), then within a minute or two, while the
browser still holds its keep-alive connection, list Caddy's connections:

```
docker compose exec caddy netstat -tn
```

The `Foreign Address` column is what Caddy saw. `infra/Caddyfile` has no
`log` directive, so Caddy writes no access log and the connection table is
where to look; `netstat` ships in the `caddy:2` image. The browser machine's
own address means the chain works; `172.x.0.1`, the network's gateway
(`docker network inspect`), means it does not. Checked against `caddy:2`
v2.11.4 with this repository's Caddyfile: a keep-alive connection from a
container on the network showed that container's address, and one made
through the published port from the Docker host's loopback showed the
gateway. A browser on another machine was not part of that check.

The fix is on the deployment side, never by trusting a wider range:

- give Caddy the host's network (`network_mode: host`, then reach `web` and
  `api` through published loopback ports), so it sees real sources;
- or keep the IPv4 NAT path — IPv4-only listener, or IPv6 enabled on the
  Compose network — and `"userland-proxy": false` where that is supported.
  IPv6 clients are keyed on their /64 (#198), so a client rotating source
  addresses inside its own block keeps one lock and one share;
- or, if another proxy or load balancer terminates connections in front of
  Caddy, add its address to Caddy's `trusted_proxies` so the address it
  forwards is kept.
