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
`../../docs/architecture.md` is silently inert.
