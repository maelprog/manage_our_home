# apps/web — Manage Our Home front-end

The web front-end for **Manage Our Home**. It is **server-side rendered**
(SSR): pages are built with Leptos's `view!` macro and rendered to HTML
strings inside plain [axum](https://github.com/tokio-rs/axum) handlers.

There is **no client-side hydration / WASM bundle**. Forms are plain HTML
`<form method="post">` submissions (progressive-enhancement style: the app
works with JavaScript disabled). This is a deliberate choice — it avoids
needing a `wasm32` toolchain / Trunk build step. See the top comment in
[`Cargo.toml`](./Cargo.toml) for the full rationale.

The browser only ever talks to this web app. This web app in turn calls
[`apps/api`](../api) server-to-server for all data and auth.

## What it covers today

Epic 1 — **Auth**. The routes:

- `/register` — create an account
- `/verify-email` — email-verification landing (from the emailed link)
- `/login`, `/logout`
- `/forgot-password`, `/reset-password`
- `/auth/google/callback` — Google OAuth return
- `/` — home (authenticated)

Unauthenticated visitors are redirected to `/login`; authenticated visitors
are redirected away from `/login` and `/register`.

## Running

You need [`apps/api`](../api) running first (see its README). Then:

```sh
API_INTERNAL_BASE_URL=http://localhost:8080 \
API_PUBLIC_BASE_URL=http://localhost:8080 \
  cargo run -p manage_our_home_web
```

Open **http://localhost:3000**.

### Environment variables

Copy [`.env.example`](./.env.example) to `.env` and adjust, or set them
inline as above. All have defaults baked into `main.rs`.

| Variable | Default | Meaning |
|----------|---------|---------|
| `API_INTERNAL_BASE_URL` | `http://localhost:8080` | Where this server reaches `apps/api` (server-to-server). In docker-compose: the internal service name `http://api:8080`. |
| `API_PUBLIC_BASE_URL` | `/api` | Base URL the **browser** uses for API-hosted links (e.g. the Google OAuth start endpoint). Behind Caddy the API lives under `/api`. |
| `WEB_BIND_ADDR` | `0.0.0.0:3000` | Address/port the web server binds to. |

## Tests

Handler-level tests live alongside the code (`cargo test -p
manage_our_home_web`). Full browser end-to-end tests that drive this app
against a real `apps/api` + Postgres live in [`../../e2e`](../../e2e).

## Where it fits

See the repo [root README](../../README.md) for the whole stack, and
[`docs/architecture.md`](../../docs/architecture.md) /
[`docs/idea.md`](../../docs/idea.md) for product & architecture context.
