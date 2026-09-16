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
accruing at a low rate. Run this dry first to measure the backlog and the
drip; if the numbers justify a schedule, `src/jobs/` already has the
polling-worker shape (`account_purge.rs`).

Two things it will not let you get wrong:

- **`ADMIN_DATABASE_URL` is required, with no `DATABASE_URL` fallback.**
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
most once a minute, with seven counts and nothing that identifies an
attempt:

```
DEBUG login_timing: login branches ok=41 unknown_email=3 no_password=0
  wrong_password=5 unverified=1 throttled=0 error=0
```

The counts are since the process started. To check the fix still holds, the
refusal counters should move while the per-request `verify_us` of
`"rejected"` lines stays in one population.

## Login lock and client addresses (#178)

`POST /auth/login` locks a **(client address, email)** pair for 15 minutes
after 10 failures within 15 minutes, and answers `429 {"error":
"too_many_attempts"}` while locked. The pair, never the email alone: a lock
per account would let anyone who knows a household member's address lock
them out. A success clears the pair. The lock is checked before any work, so
a locked request costs no argon2id. Constants are in `src/auth/throttle.rs`.

The counter is **in this process's memory**, which is correct only because
`infra/docker-compose.yml` runs one `api`. **If `api` ever runs as more than
one instance, it must move to a shared store (Redis/Valkey, not Postgres —
a database write per failed attempt is itself an amplification).**

The client address comes from `X-Forwarded-For`, which is only believed from
a peer listed in **`TRUSTED_PROXY_CIDRS`** (comma-separated CIDRs or bare
addresses; default `127.0.0.0/8,::1/128,172.16.0.0/12`). The entry used is
the rightmost one that is not itself a trusted peer. From any other peer the
header is ignored and the peer address is the client. The API refuses to
start on a malformed list.

Get this list wrong and the lock fails in one of two ways:

- **too narrow** (it misses `caddy`/`web`): every browser resolves to the
  proxy's address, and the lock becomes one lock for the whole household;
- **too wide** (it covers addresses browsers connect from): those clients
  choose their own address, dodge the lock, or aim it at someone else.

apps/web relays the chain on its internal call and appends the address it
saw (`apps/web/src/client_ip.rs`). Caddy replaces any `X-Forwarded-For` a
browser sends with the address it saw (checked against `caddy:2`, see
`infra/Caddyfile`).
