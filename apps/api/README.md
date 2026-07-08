# apps/api — manage_our_home backend

Epic 1 — Auth + Groups. See `../../docs/architecture.md` and
`../../docs/idea.md` for product/architecture context.

## Setup

```
cp .env.example .env   # fill in real secrets
createdb manage_our_home
cargo sqlx prepare      # regenerate .sqlx query cache after schema changes
cargo run
```

## Running tests

Tests need a reachable Postgres (used via `sqlx::test`, which provisions a
fresh throwaway database per test using `DATABASE_URL`'s server). The
connecting role must have `CREATEROLE` (RLS tests create/drop a scoped
throwaway role to prove isolation against a non-superuser connection —
see `tests/rls.rs`).

```
export DATABASE_URL=postgres://mom:mom@localhost:5432/postgres
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
```

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
