[![CI](https://github.com/maelprog/manage_our_home/actions/workflows/ci.yml/badge.svg)](https://github.com/maelprog/manage_our_home/actions/workflows/ci.yml)

# manage_our_home (MHome)

**Manage Our Home** is a family-management app: a shared space where the
members of a household (a "group") organize their life together — accounts &
groups, messaging, a shared calendar with Google import, file attachments,
and RGPD-compliant data handling.

This is a monorepo containing the backend API, the web front-end, shared
code, and the infrastructure to run it all.

---

## What's in here

| Path | What it is |
|------|-----------|
| `apps/api` | Backend HTTP API — Rust (axum + sqlx + Postgres). Handles auth, groups, messaging, calendar, files, admin. |
| `apps/web` | Web front-end — Rust (Leptos **server-side rendering**, no WASM). Plain HTML forms, works with JS disabled. Talks to `apps/api`. |
| `apps/shared` | Rust code shared between `api` and `web`. |
| `e2e` | End-to-end tests (Playwright / TypeScript) driving the real running stack. |
| `infra` | `docker-compose.yml` + `Caddyfile` to run the whole stack (Postgres, MinIO, Ollama, api, web, Caddy reverse-proxy). |
| `docs` | Architecture, product spec, and scope docs (see below). |

Everything Rust lives in one Cargo workspace (`Cargo.toml` at the root).

---

## Documentation map

- [`docs/architecture.md`](docs/architecture.md) — stack, repo layout,
  security/RGPD posture.
- [`docs/idea.md`](docs/idea.md) — feature spec / epic clarifications.
- [`docs/v1-scope.md`](docs/v1-scope.md) — per-epic status for v1: done,
  in progress, missing.
- [`apps/api/README.md`](apps/api/README.md) — backend setup, tests,
  Row-Level-Security deployment notes.
- [`e2e/README.md`](e2e/README.md) — end-to-end test setup.

---

## Prerequisites

- **Rust** (stable toolchain — `rustup`, `cargo`).
- **PostgreSQL 16** — either locally installed, or via the Docker Compose
  stack below.
- **Docker + Docker Compose** — only needed if you want the one-command
  full stack (Postgres, object storage, reverse-proxy).
- **Node.js** — only needed to run the end-to-end tests in `e2e/`.

---

## Quickest way to run everything: Docker Compose

From a clean clone to a running application with seeded data. No secrets to
invent, nothing to fill in by hand:

```sh
git clone https://github.com/maelprog/manage_our_home.git
cd manage_our_home/infra
cp .env.example .env
docker compose up -d
```

The first run compiles the Rust workspace inside Docker and takes a few
minutes; later runs start in seconds.

Then open **http://localhost** and sign in with one of the accounts the stack
seeds for you:

| Email | Password | Role |
|-------|----------|------|
| `alice.dev@example.test` | `devpassword` | owner of the group "Foyer Dev" |
| `bob.dev@example.test` | `devpassword` | member of that group |

`.env.example` is a throwaway local-test environment: fixed passwords, seeded
logins, `SECURE_COOKIES=false` (there is no TLS on `http://localhost`), and a
Mailpit container that catches the verification, reset and invitation emails
so that nothing leaves the machine — read them at **http://localhost:8025**.
The Google OAuth values are dummies, so `/auth/google/*` will not complete;
everything else works.

Caddy puts the whole stack on one URL:

- `/` → the web front-end (`apps/web`, internally on port 3000)
- `/api/*` → the backend API (`apps/api`, internally on port 8080)

The browser only ever talks to the web app; the web app calls the API
server-to-server over the internal Docker network.

Stop everything with `docker compose down` (add `-v` to also wipe the
database and storage volumes).

### Running it for real: `generate-env.sh`

Never deploy `.env.example`: its passwords are in this repository, and
`DEV_SEED_USERS` creates accounts whose password is public knowledge. For a
real deployment, generate the same variable set with random secrets instead:

```sh
cd infra
./generate-env.sh
```

It fills `POSTGRES_PASSWORD`, `ADMIN_ROLE_PASSWORD`, the three
`*_ENCRYPTION_KEY` values and `MINIO_ROOT_PASSWORD` with `openssl rand`
output, and deliberately leaves the dev-only knobs (`COMPOSE_PROFILES`,
`SMTP_PORT`, `SMTP_ALLOW_INSECURE`, `DEV_SEED_USERS`) out.

What it cannot invent, you have to fill in yourself:

- **`SMTP_HOST` / `SMTP_USERNAME` / `SMTP_PASSWORD` / `SMTP_FROM`** — a real
  relay. `SMTP_FROM` must parse as a mailbox (`no-reply@example.com`) or the
  API exits at startup, so a generated `.env` used as-is leaves the `api`
  container restarting in a loop. This is the one value that blocks boot.
- **`GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET`** — only for Google sign-in
  and calendar import. Empty values boot fine; just those two flows fail.
- **`PUBLIC_BASE_URL`** — the public origin, no trailing slash.

<details>
<summary>Writing the <code>.env</code> by hand instead</summary>

`docker-compose.yml` reads exactly these variables:

| Variable | Required | What it is |
|----------|----------|------------|
| `POSTGRES_PASSWORD` | yes | Password for the application role `mhome`. |
| `ADMIN_ROLE_PASSWORD` | yes | Password for the `BYPASSRLS` `admin_role` created at first Postgres boot by `postgres/init/01-admin-role.sh`. |
| `OAUTH_ENCRYPTION_KEY` | yes | `openssl rand -base64 32` |
| `MESSAGE_ENCRYPTION_KEY` | yes | `openssl rand -base64 32` |
| `CALENDAR_FEED_ENCRYPTION_KEY` | yes | `openssl rand -base64 32` |
| `SMTP_HOST`, `SMTP_USERNAME`, `SMTP_PASSWORD`, `SMTP_FROM` | yes | Mail relay. `SMTP_FROM` must parse as a mailbox. |
| `MINIO_ROOT_USER`, `MINIO_ROOT_PASSWORD` | yes | Object-storage credentials; also used as the API's S3 access/secret key. |
| `PUBLIC_BASE_URL` | no — defaults to `http://localhost` | Public origin, no trailing slash. |
| `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET` | no | Google sign-in and calendar import. |
| `SECURE_COOKIES` | no — defaults to `true` | Set `false` for plain-http local testing. |
| `COMPOSE_PROFILES` | no | `dev` starts the Mailpit mail catcher. |
| `SMTP_PORT`, `SMTP_ALLOW_INSECURE` | no | Dev-only: plaintext SMTP to Mailpit. |
| `DEV_SEED_USERS` | no — defaults to `false` | Dev-only: seeds the two accounts above at API startup. |

Postgres passwords end up embedded in `postgres://` URLs, so keep them
URL-safe: that is why `generate-env.sh` uses `openssl rand -hex` for passwords
and reserves `-base64` for the encryption keys, whose `+`, `/` and `=` would
break URL parsing.

</details>

### Upgrading a stack created before the `mhome` rename

The application's Postgres role was renamed `mom` → `mhome`. `POSTGRES_USER`
is only read when the data volume is first initialized, so an existing
`postgres_data` volume still holds a role named `mom`, which the new
`DATABASE_URL` will not find.

Simplest fix, if the data is disposable: `docker compose down -v`, then
`docker compose up -d` re-initializes the volume with the new role.

To keep the data, rename the role in place. `mom` is the bootstrap superuser
created by `initdb`, so it is both the only superuser and the role you would
connect as — and Postgres refuses to rename the session user. The rename
therefore needs a temporary superuser to run from:

```sh
cd infra
docker compose up -d postgres          # api/web will fail to connect until this is done

docker compose exec postgres \
  psql -U mom -d manage_our_home -c \
  "CREATE ROLE tmp_rename SUPERUSER LOGIN PASSWORD 'tmp';"

docker compose exec -e PGPASSWORD=tmp postgres \
  psql -h 127.0.0.1 -U tmp_rename -d manage_our_home -c \
  "ALTER ROLE mom RENAME TO mhome;"

docker compose exec postgres \
  psql -U mhome -d manage_our_home -c "DROP ROLE tmp_rename;"

docker compose up -d
```

Verified on `postgres:16`: the password survives the rename (it is hashed with
`scram-sha-256`, which — unlike the legacy `md5` scheme — does not use the
role name as salt), database ownership follows the role, and the default
privileges granted `FOR ROLE mom` to `admin_role` carry over, since Postgres
stores them against the role's OID. `admin_role` itself is untouched, so
`ADMIN_DATABASE_URL` needs no change.

---

## Running the pieces by hand (local dev)

Useful when you're actively developing and want fast rebuilds without Docker.

### 1. Database

Either use the Postgres from the Compose stack, or a local install:

```sh
createdb manage_our_home
```

### 2. Backend API — `apps/api`

```sh
cd apps/api
cp .env.example .env     # then fill in real secrets (DB URL, encryption keys, SMTP, Google OAuth...)
cargo run -p manage_our_home
```

The API listens on **http://localhost:8080** by default.

See [`apps/api/README.md`](apps/api/README.md) for the full env-var list,
the `cargo sqlx prepare` step (after schema changes), and the important
Row-Level-Security role setup for production.

### 3. Web front-end — `apps/web`

In a second terminal, pointing it at the API you just started:

```sh
cd apps/web
API_INTERNAL_BASE_URL=http://localhost:8080 \
API_PUBLIC_BASE_URL=http://localhost:8080 \
  cargo run -p manage_our_home_web
```

The web app listens on **http://localhost:3000** by default. Open that in
your browser.

**Web app environment variables:**

| Variable | Default | Meaning |
|----------|---------|---------|
| `API_INTERNAL_BASE_URL` | `http://localhost:8080` | Where the web server reaches the API (server-to-server). |
| `API_PUBLIC_BASE_URL` | `/api` | Base URL the browser uses for API-hosted links (e.g. Google OAuth start). |
| `WEB_BIND_ADDR` | `0.0.0.0:3000` | Address/port the web server binds to. |

---

## Running the tests

### Rust unit + integration tests

Integration tests need a reachable Postgres; they provision throwaway
databases per test. The connecting role must have `CREATEROLE` (the RLS
tests create/drop a scoped role to prove isolation).

```sh
export DATABASE_URL=postgres://mhome:mhome@localhost:5432/postgres
cargo test
```

### End-to-end tests (`e2e/`)

Playwright driving the real running stack (web + api + Postgres). See
[`e2e/README.md`](e2e/README.md) — in short:

```sh
cd e2e
npm install
npx playwright install --with-deps chromium
DATABASE_URL=postgres://<role>:<password>@localhost:5432/<db> \
WEB_BASE_URL=http://localhost:3000 \
  npm test
```

---

## Pre-commit checks

This repo ships a git hook that runs `cargo fmt --check`, `cargo clippy`,
and `cargo build` before each commit. Enable it once per clone with:

```sh
git config core.hooksPath .githooks
```
