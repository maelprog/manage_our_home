# manage_our_home (MoM)

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

This brings up the whole stack — database, object storage, API, web, and a
Caddy reverse-proxy that puts everything on one URL.

```sh
cd infra

# Create a .env file next to docker-compose.yml with the required secrets
# (see the variables it references: POSTGRES_PASSWORD, ADMIN_ROLE_PASSWORD,
# PUBLIC_BASE_URL, GOOGLE_CLIENT_ID/SECRET, the *_ENCRYPTION_KEY values,
# SMTP_*, MINIO_ROOT_USER/PASSWORD, ...).
# Generate encryption keys with:  openssl rand -base64 32

docker compose up -d
```

Then open **http://localhost** in your browser. Caddy routes:

- `/` → the web front-end (`apps/web`, internally on port 3000)
- `/api/*` → the backend API (`apps/api`, internally on port 8080)

The browser only ever talks to the web app; the web app calls the API
server-to-server over the internal Docker network.

Stop everything with `docker compose down` (add `-v` to also wipe the
database and storage volumes).

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
export DATABASE_URL=postgres://mom:mom@localhost:5432/postgres
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
