# e2e — Playwright suite for apps/web's Auth screens

Drives a real running stack: `apps/web` + `apps/api` + Postgres. Token
retrieval (email verification / password reset) reads directly off
Postgres — the exact same mechanism `apps/api/tests/*_flow.rs`'s
integration tests already use (`SELECT token FROM
email_verification_tokens ...`), see `lib/db.ts`. apps/api has no
dev/test HTTP hook for tokens, and this doesn't add one — a direct DB
read is no weaker a trust boundary than what the Rust integration tests
already rely on.

## Running against docker-compose

```
docker compose -f ../infra/docker-compose.yml up -d
npm install
DATABASE_URL=postgres://mom:<password>@localhost:5432/manage_our_home \
WEB_BASE_URL=http://localhost \
  npx playwright test
```

## Running against a manually started stack

```
# Terminal 1 — apps/api (see apps/api/README.md for env vars)
cargo run -p manage_our_home

# Terminal 2 — apps/web
API_INTERNAL_BASE_URL=http://localhost:8080 \
API_PUBLIC_BASE_URL=http://localhost:8080 \
  cargo run -p manage_our_home_web

# Terminal 3
cd e2e
npm install
npx playwright install --with-deps chromium
DATABASE_URL=postgres://<role>:<password>@localhost:5432/<db> \
WEB_BASE_URL=http://localhost:3000 \
  npm test
```

## Coverage

- Register → verify-email → login → home → logout.
- Wrong-password and duplicate-email error states (issue #15's error table).
- Forgot-password → reset-password → login-with-new-password.
- Unauthenticated redirect to `/login`; authenticated redirect away from
  `/login`/`/register`.
- Google OAuth: **skipped**, no test provider/credentials available in
  this environment — see the skip reason in `tests/auth.spec.ts`.
