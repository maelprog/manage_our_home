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

## The ICS fixture server (`ICS_FIXTURE_HOST`)

`tests/google-calendar.spec.ts` (front epic F11) starts a throwaway HTTP
server inside the Playwright process and serves `.ics` fixtures from it,
so the Google Calendar import runs against no real Google account and no
network egress. The client of that server is **apps/api**, not the
browser: it is apps/api that fetches the feed URL when an import is
triggered.

The advertised host therefore has to be whatever apps/api can reach the
Playwright process at. It defaults to `127.0.0.1`, which is correct
whenever both run on the same machine (the `e2e` job in `ci.yml`, and
both recipes above). When the stack runs on a docker network and
Playwright runs in its own container, give the container a resolvable
name and pass it through:

```
docker run --name mom-e2e-runner --network mom-e2e ... \
  -e ICS_FIXTURE_HOST=mom-e2e-runner ...
```

## `scripts/` — seeding a stack and weighing its pages

Two committed tools, replacing the ad-hoc terminal work that produced a
wrong performance budget (the incident is told in full at the top of
each script):

```
cd e2e
npm ci

# 1. fill a group with a month of ordinary household data, via the API
API_BASE_URL=http://localhost:8080 \
DATABASE_URL=postgres://<role>:<password>@localhost:5432/<db> \
  npm run seed

# 2. weigh the eight nav routes, authenticated, through Caddy
WEB_BASE_URL=http://localhost \
  npm run measure

npm run test:scripts   # the pure logic behind both, no stack needed
```

Both are TypeScript run straight by `node` (no build step, no new
dependency): the only runtime import outside `node:` is `pg`, already a
dependency here, and `node:zlib` ships both gzip **and** zstd. That
needs **Node ≥ 22.15** (`zstdCompressSync`) and **≥ 22.18** (running
`.ts` without a flag); the `mcr.microsoft.com/playwright:*-noble` image
used by the docker recipe above carries Node 24. A shell script was the
alternative for the measurement half, but it could not reuse `lib/db.ts`,
would need `zstd`/`curl` binaries present, and none of its guardrails
could be unit-tested — so both halves are Node, and the guardrail logic
is shared and covered by `npm run test:scripts`.

### `npm run seed` — `scripts/seed-perf-data.ts`

Creates two accounts, a group, and tops it up to ~40 events, 40 stock
items, 30 grocery items, 30 expenses, 25 recipes and 50 messages. Every
row goes through the HTTP API, so it is valid by construction and the
script survives a schema change; the one direct Postgres read is the
email-verification token, the same trust boundary `lib/db.ts` documents
above. Every request declares the statuses it accepts and the first
unexpected one aborts the run with the method, URL, status and body.

- **Idempotent**: it *tops each collection back up* to its target rather
  than doubling it, so re-running is a no-op and an interrupted run is
  finished by running it again. It never deletes anything.
- **Deterministic**: one seeded PRNG, no `Math.random()`, so the same
  seed produces the same bytes and two measurements are comparable.
- **Dated rows are anchored on today.** `/agenda` renders the grid of the
  *current month* (`apps/web/src/routes/agenda/calendar.rs`, falling back
  to `today_paris`), so events seeded into another month render **not at
  all** — the page keeps its normal size and simply shows nothing, which
  measured 1 881 bytes of document instead of 2 807 without looking
  wrong. `SEED_REFERENCE_DATE` exists to reproduce that, not to use.
- **The corpus is real French household text, and that is load-bearing.**
  gzip crushes repetitive labels: 50 near-identical messages weighed
  13 741 bytes (under budget) where 50 ordinary ones weigh 14 990 (over).
  The verdict flips on the quality of the seeded text alone, so a
  lorem-ipsum seed would hand you a falsely reassuring budget. The
  measurement prints which dataset it ran against for this reason.

Useful env: `SEED_GROUP_NAME`, `SEED_OWNER_EMAIL`, `SEED_PARTNER_EMAIL`,
`SEED_RNG_SEED`, and `SEED_EVENTS` & co. for the targets — enough for two
people to seed the same database side by side.

### `npm run measure` — `scripts/measure-page-weight.ts`

Weighs the eight nav routes raw, gzipped and zstd-compressed, for the
whole response **and for the document alone** (response minus the inlined
stylesheet). That last column is the one that was missing: the sheet
weighs the same on all eight routes and hides what a page actually costs.
Compressed sizes are computed locally with `node:zlib`, so they are
reproducible whether or not the server in front compresses; what the
server really negotiated is reported separately, and its absence is a
warning, not a crash.

It refuses to print a number it cannot vouch for:

1. The measured route list is **checked against the real nav** parsed out
   of `apps/web/src/app.rs`. Any divergence aborts. `/admin/users` is a
   declared, deliberately unmeasured conditional slot (it 404s for
   everyone but the technical superadmin); a *new* conditional nav entry
   also aborts, so none can slip in unweighed.
2. Non-200, empty body, absurdly small body, or the login screen served
   as a 200 in place of the page → abort.
3. Per route, **rendered rows are compared to rows stored in the API**.
   Nothing stored → "run the seeding script first". Rows stored but none
   rendered → the data is outside the window the page displays. A byte
   floor cannot catch that second case, which is exactly why it exists.
4. `--budget=<bytes>` (default 14 336, the 14 KiB response budget
   `apps/web/src/app.rs` already uses) sets a **non-zero exit code** when
   a route's gzipped response exceeds it.

`--json` emits the same run as machine-readable JSON, `--base-url` and
`--api-base-url` override the defaults. Measuring **through Caddy** is the
default because that is the deployed path and the thing that compresses;
pointing `WEB_BASE_URL` at `web:3000` directly works too, and then
`API_BASE_URL` must be given separately since apps/web does not serve
`/api`.

Wiring `npm run measure` into `.github/workflows/` as a merge gate is
possible and deliberately **not done here** — that needs the CI job to
stand up a seeded stack, and changing CI was out of scope for this
change.

## Coverage

- Register → verify-email → login → home → logout.
- Wrong-password and duplicate-email error states (issue #15's error table).
- Forgot-password → reset-password → login-with-new-password.
- Unauthenticated redirect to `/login`; authenticated redirect away from
  `/login`/`/register`.
- Google OAuth: **skipped**, no test provider/credentials available in
  this environment — see the skip reason in `tests/auth.spec.ts`.
