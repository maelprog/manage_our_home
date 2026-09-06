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
DATABASE_URL=postgres://mhome:<password>@localhost:5432/manage_our_home \
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
docker run --name mhome-e2e-runner --network mhome-e2e ... \
  -e ICS_FIXTURE_HOST=mhome-e2e-runner ...
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

npm run test:scripts   # pure logic: the two scripts, lib/, and the gate's own
                       # floor — no stack needed
```

Both are TypeScript run straight by `node` (no build step, no new
dependency): the only runtime import outside `node:` is `pg`, already a
dependency here, and `node:zlib` ships both gzip **and** zstd.

That needs **Node ≥ 22.18** — `≥ 22.15` for `zstdCompressSync`, `≥ 22.18`
to run `.ts` without a flag. It is declared in `engines`, enforced by npm
(`.npmrc` sets `engine-strict=true`, so `npm ci` on an older Node fails
with `EBADENGINE` instead of only warning), and enforced again at startup
by `scripts/run.mjs`, which both npm scripts go through: a `.ts`
entrypoint on an older Node dies with `ERR_UNKNOWN_FILE_EXTENSION`, which
names neither the required version nor the fix. A
`mcr.microsoft.com/playwright:*-noble` image carries Node 24 at the tag
matching the pinned Playwright version (`v1.61.1-noble` → Node 24.17) —
but not at every tag: `v1.55.0-noble` still ships Node 22.18. Pick the
tag `package-lock.json` pins.

The `e2e` job in `.github/workflows/ci.yml` runs on **Node 24** and calls
`npm run test:scripts` right after `npm ci`, before the stack is built —
these tests are pure logic, so running them there keeps their verdict
readable even when the stack fails to come up (#91). `npm run measure` is
still **not** a merge gate: the pinned version that blocked it is raised,
but turning a budget report into a gate is a separate decision, left out
of #91 on purpose.

A shell script was the
alternative for the measurement half, but it could not reuse `lib/db.ts`,
would need `zstd`/`curl` binaries present, and none of its guardrails
could be unit-tested — so both halves are Node, and the guardrail logic
is shared and covered by `npm run test:scripts`.

### The floor under `npm run test:scripts` (`scripts/run-script-tests.mjs`)

The script goes through `scripts/run-script-tests.mjs` rather than calling
`node --test` directly, because **`node --test` exits 0 when its globs
match no file at all** (checked on Node 24.20.0: `tests 0 / pass 0 / fail
0`, exit 0). A renamed directory, a moved `lib/`, a changed extension
would take the whole suite out of CI while the job stayed green — the very
failure mode #91 fixed, except CI would call it green (#123).

The wrapper runs the same `node --test`, with two reporters: `spec` on
stdout (the readable output, unchanged) and `tap` into a temp file outside
the repo, which is the machine-readable source for the count. It then
refuses to exit 0 on fewer than **one executed test**
(`scripts/lib/test-floor.ts`, unit-tested by `test-floor.test.ts` — which
the globs match, so the floor covers itself).

It counts **executed tests** (`pass + fail`), not matched files. What that
catches, at a threshold of 1: the glob that matches nothing (`tests 0`); a
suite whose cases are all `skip`/`todo` — tests exist, none runs its body;
and a file cut down to a bare `describe(...)`/`suite(...)` shell, which
`node --test` reports as `tests 0 / suites 1`. A matched-file count would
clear the last two; this floor is red on them.

What it does **not** catch, and the distinction matters because it is the
whole gap between the two forms of floor:

- **a file emptied until it registers nothing at all**. `node --test`
  counts a matched file that registers no test as *one passing test* —
  checked on Node 24.20.0: two files cut down to
  `import test from "node:test";` give `tests 2 / pass 2 / fail 0`, exit 0,
  with the `spec` reporter printing `✔ lib/dates.test.ts`. On that family,
  counting executed tests is worth no more than counting files; covering it
  would mean counting the `test(...)` calls actually registered, which is
  outside #123. Mind the nuance: emptied down to a `describe` shell the
  file is *red* (above) — it is the file registering nothing whatsoever
  that slips through;
- a suite that **shrinks**: three files down to one, or the whole suite
  down to a single test, stays green. Catching that needs a number kept up
  to date on every test added, and #123 says "at least 1" is enough.

So at threshold 1 this floor is equivalent to a matched-file floor
**except** on the families where `node --test` counts no executed test —
all-skipped/`todo`, or a file reduced to a `describe`/`suite` shell —
where only it bites. That is a real advantage, but a narrow one.

Two more properties, independent of the above:

- `node --test`'s own exit code is checked **first**. A real test failure
  exits with that code and never reaches the floor, so the floor cannot
  mask a failure or relabel it;
- `test-floor.test.ts` also reads `package.json` and checks its
  `test:scripts` line against `run-script-tests.mjs`. Putting `node --test
  …` back on that line is a one-line way to reopen #123 while every other
  test keeps passing, since the rest of them exercise the pure logic and
  not the wiring. That check has a condition and two error directions,
  spelled out below.

#### What the wiring check can and cannot do

**Start with the condition it runs under**, because everything else
depends on it and the code does not show it: the check is asserted **from
inside the suite it protects**. Only `test-floor.test.ts` calls
`wiringViolation`, and it runs only if `test:scripts`'s globs match it. So
**it bites only while the suite still runs.** An unwiring that also
empties the globs never executes it: the pre-#123 line pointed at paths
that no longer match gives `tests 0` and **exit 0** (measured) — which is
exactly the state #123 describes, guardrail included. No test living
inside a suite can guard that suite's invocation; closing this would need
a check outside npm, e.g. a `grep` step in `ci.yml`. Out of scope for
#123, and accepted as such.

Everything below therefore holds **only while the suite runs**.

**What the check does** then: a text heuristic, not a command parser — a
substring search for the runner's name, plus a refusal of a bare `--test`
token, unquoted and standalone (`--test-reporter` and friends do not
count). On the lines probed it refuses the pre-#123 line, a gate split
into two halves where only one goes through the runner — the other half is
#123 verbatim — and a line that names the runner only inside a shell
comment.

**A heuristic errs in both directions**, and both are real here:

- it **accepts broken wiring**: any line that *names* the runner without
  invoking it and without writing `--test` — `bash foo.sh #
  run-script-tests.mjs`, or a second gate launched by some other wrapper
  with no floor of its own;
- it **refuses correct wiring**: a correct line whose *comment* contains
  the `--test` token. Not a contrived case —
  `node scripts/run-script-tests.mjs "…" # replaces the old direct node
  --test` is refused (measured), and that is the most likely comment a
  maintainer would write here. The failure is loud and names the offending
  line, which beats a silent hole, but it is a false positive.

Both lists are what was probed, not a demonstrated boundary.

### Dates de test : `lib/dates.ts`

Les specs qui fabriquent un événement « aujourd'hui » passent par `parisDay` /
`parisDayOfMonth`, jamais par `new Date()` directement. L'app affiche en
**Europe/Paris** (fuseau v1 figé) alors que le runner GitHub tourne en UTC :
entre 22 h et minuit UTC les deux ne désignent plus le même jour, et un
événement « du jour » selon le runner est déjà terminé selon l'app. Le test
`home.spec.ts` « un événement toute la journée aujourd'hui est bien affiché »
échouait sur cette seule fenêtre — reproductible à l'heure, invisible le reste
du temps. `lib/dates.test.ts` fige les instants concernés, changements d'heure
compris ; ils tournent sous `npm run test:scripts`.

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
  finished by running it again. It never deletes anything. Events are
  counted **inside the window `/agenda` renders**, not over a wider span:
  when those two windows differed, a stack whose events had aged out of the
  current month counted as complete, so `npm run seed` — the repair the
  measurement tells you to run — created nothing and the stack had no
  supported way back.
- **Deterministic**: one seeded PRNG, no `Math.random()`, so the same
  seed produces the same bytes and two measurements are comparable. Both
  the event slots and the "already ticked" grocery draws are computed for
  the whole target and then indexed, so a run resumed halfway produces the
  same rows as a single pass.
- **Dated rows are anchored on today.** `/agenda` renders the 42-day grid
  of the *current month* (`month_grid`: the Monday on or before the 1st,
  plus 41 days), so events seeded into another month render partly or not
  at all — and the page keeps its normal size while doing it. Seeding into
  the next month and measuring today rendered 18 of 40 events here and cost
  **36 % of the document**, with nothing else out of place.
  `SEED_REFERENCE_DATE` exists to reproduce that, not to use.
- **The corpus is real French household text, and that is load-bearing.**
  gzip crushes repetitive labels, so a lorem-ipsum seed hands you a falsely
  reassuring budget: an earlier manual campaign (issue #83, a different
  dataset from this one) put 50 near-identical messages at 13 741 bytes,
  under budget, against 14 990 for 50 ordinary ones, over — the verdict
  flipping on the quality of the seeded text alone. Those two numbers are
  cited for the mechanism, not as expectations for this seed. The
  measurement prints which dataset it ran against for the same reason.

Useful env: `SEED_GROUP_NAME`, `SEED_OWNER_EMAIL`, `SEED_PARTNER_EMAIL`,
`SEED_RNG_SEED`, and `SEED_EVENTS` & co. for the targets — enough for two
people to seed the same database side by side.

### `npm run measure` — `scripts/measure-page-weight.ts`

Weighs the eight nav routes raw, gzipped and zstd-compressed, for the
whole response **and for the document alone** (response minus the inlined
stylesheet). That last column is the one that was missing: the sheet used
to weigh the same on all eight routes and hid what a page actually costs.
Since #89 the sheet is linked rather than inlined, so the two columns
coincide on an application page — and the sheet is fetched and weighed
once, on its own line, because it is now paid once per deploy instead of
once per page view. A response that carries neither a `<style>` nor a
`<link rel=stylesheet>` still aborts the run: a page with no CSS at all
weighs beautifully and is broken.
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
3. Per route, **rendered rows are compared to the rows the API holds inside
   the window that page displays** — for `/agenda`, the same 42-day
   `month_grid` window the seed counts into. Nothing in the window → "run
   the seeding script first" (and it says so when rows exist elsewhere).
   Some rendered but **fewer than owed** → refused too: a half-filled page
   weighs a perfectly plausible intermediate amount, so a zero-check is not
   enough. The only route allowed to render less than it stores is
   `/messagerie`, which pages at 50; that tolerance is declared per route,
   never globally. The byte floors are a leftover belt over these braces —
   they only fire on a wholly blank shell.
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
