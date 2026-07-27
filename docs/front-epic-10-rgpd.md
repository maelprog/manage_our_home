# Front epic F10 — RGPD self-service (issue #25)

SSR spec at the depth F1 (#15) established: route table, error tables mapping
the exact status codes of `apps/api/src/rgpd/` and
`apps/api/src/auth/mod.rs::delete_account` / `cancel_delete_account` to French
copy, and acceptance criteria. The backend is already shipped (backend epic #12,
v1-scope row #12: `GET /account/export`, `GET /privacy-policy`,
`POST /account/delete`, `POST /account/delete/cancel`, plus the purge job
`apps/api/src/jobs/account_purge.rs`) — this epic is the `apps/web` client only,
following the same pattern as every other front epic (Leptos SSR, plain
`<form method=post>`, PRG with `?notice=`/`?error=` codes, per-page error tables,
pure logic mirrored in `apps/shared` and TDD'd).

Depends only on F1 (Auth) for the session and F2 (Groups) for the settings
screens the deletion block links to. These are **account-level** screens: no
family context is needed, and no route here takes a `group_id`.

This is also where the account-deletion UI carved out of F1's scope lands — it
was flagged there as required before v1 ships (`notes-issue-1-qa.md` #6).

## Design decisions

### Deletion is a grace period, and the copy never says "supprimé"

`POST /account/delete` does not delete anything: it stamps
`users.deletion_requested_at`, and `jobs/account_purge.rs` anonymizes the account
30 days later. The account keeps working throughout, and the request is
cancellable at any point. Every screen therefore sells "programmée + annulable"
and states both dates (when it was requested, when the purge happens), never
"votre compte a été supprimé".

This keeps it deliberately distinct from F9's superadmin `deactivate`, which
*is* immediate and terminal. Two different actions, two different copies, on
purpose — the E2E suite pins the wording of both so they can't drift together.

The 30-day horizon lives in exactly one place per side:
`validation::rgpd::GRACE_PERIOD_DAYS` mirrors the backend's
`ACCOUNT_DELETION_GRACE_DAYS` / `PURGE_GRACE_DAYS`, and `deletion_deadline()`
derives the promised date from it rather than hardcoding a second number in the
copy.

### The pending state is rendered from `GET /auth/me`, not from a local flag

`MeResponse` gained `deletion_requested_at`, read live from the DB on every
request (the session lookup already joins `users`, same as `is_superadmin` in
F9). So requesting or cancelling takes effect on the very next page load, and
both `/account` and `/account/delete` render the *same* `pending_deletion_panel`
— the state is never hidden behind one route, and there is no client-side copy
of it to go stale.

`can_request_deletion` / `can_cancel_deletion` (pure, TDD'd) decide which of the
two the page shows. A second `POST /account/delete` would silently reset the
clock and postpone the purge, so the request form simply isn't rendered while a
request is pending.

### Confirmation: password when there is one, consent always

`delete_account` verifies `current_password` **only** for an account that has a
`password_hash`, and its comment states that for Google-only accounts
"re-consent is validated on the frontend flow before this endpoint is called".
So `MeResponse` also gained `has_password`, and the form asks for the password
only when there is one to check — asking a Google-only account for a password
would be a field nobody can fill.

The explicit consent checkbox is required for **both** kinds of account: it is
what discharges that backend comment for Google-only accounts, and for password
accounts it keeps an irreversible action off a single stray click. A native
`confirm()` on submit is progressive enhancement on top (the form posts fine
with JS off); the backend stays the authority.

`validate_deletion_confirmation` (pure, TDD'd) runs both guards before anything
is sent, and passes the password through **untrimmed** — spaces are legitimate
password characters and the backend compares against the stored hash.

### The 409 becomes guidance, not an error

`owner_of_groups` is the one API error body carrying more than `{"error": …}`:
it lists the groups the caller still owns. Rather than surface a raw error, the
page renders what blocks the deletion, names each blocking family, and gives the
two ways out — transfer ownership, or delete the family — with a direct link to
each family's settings screen (F2). That is the whole reason `apps/shared` has an
`OwnerOfGroupsError` DTO at all.

Wire-shape trap worth recording: the API's `BlockingGroup` struct names the
column `group_id` (`SELECT g.id as group_id`), but that struct is only the query
target — the response body is hand-built as `json!({"id": …, "name": …})`. The
DTO mirrors **`id`**, the wire name. Getting this wrong doesn't fail loudly: the
whole `OwnerOfGroupsError` fails to deserialize, `unwrap_or_default()` yields an
empty list, and the user silently gets the form back with no guidance at all.

### The export is relayed byte-for-byte, and split across two routes

`/account/export` explains what the document contains and what it deliberately
omits; `/account/export/download` performs the download. Two routes so the
explanation can be linked and bookmarked, and so a refresh never re-triggers a
download (each one writes an `account_data_exported` audit row server-side).

The download relays `GET /account/export`'s bytes **verbatim** and only adds the
`Content-Disposition` header — `apps/web` never parses and re-serializes the
document. It is the user's own data under Art. 20; nothing in the web tier
should be able to reshape it on the way out. Hence `api_get_raw` in `state.rs`
rather than the usual JSON helper, and no DTO mirroring the export shape (one
fewer place to drift as categories are added).

The filename is generated and dated (`mes-donnees-manage-our-home-YYYY-MM-DD.json`,
Paris calendar day) so a user who exports twice keeps both files, and so no user
input ever reaches that header.

### The privacy policy is public, and rendered from the one source document

`GET /privacy-policy` is the only page besides the auth entry points that renders
without a session: a prospective user must be able to read it *before*
registering, so it is linked from the login and register footers and lives
outside `routes::account`.

The content is not duplicated in `apps/web`. `apps/api` serves
`docs/privacy-policy.md` verbatim as `text/markdown` (compiled in via
`include_str!`), and the page renders that markdown with
`validation::rgpd::render_markdown` — so the document in source control, the API
response and the deployed page can never drift.

`render_markdown` is a deliberately small subset renderer (headings, paragraphs,
bullet lists with continuation lines, pipe tables, and inline `` `code` ``,
`**strong**`, `[text](url)`) rather than a markdown crate: the input is a single
trusted document that ships in the repo, and a full parser would be dependency
weight for one page. It escapes first and scans after, so no raw HTML can pass
through, and a link whose scheme isn't `http(s)`/`mailto`/local stays literal
text. The `renders_the_real_privacy_policy_without_leftover_markup` test runs the
**actual shipped document** through it and asserts no markdown markers survive —
that is the regression guard keeping the document inside the supported subset.

### Out of scope

Editing profile fields from `/account` (this epic is RGPD self-service, not an
account-settings pass — password change already lives in F1's flow), an
in-app audit-log viewer, admin-initiated export or erasure on someone else's
behalf (`/account/*` is strictly self-service, scoped by `AuthUser` with no
target-user parameter — F9's `deactivate` is the separate support action), and
any UI for the `registre des traitements` (`docs/registre-traitements.md` is an
internal Art. 30 document, not a user-facing page).

## Route table (`apps/web`)

| Method | Path | Handler | Purpose | API call(s) |
|---|---|---|---|---|
| GET | `/privacy-policy` | `privacy::get` | Public privacy policy, rendered from the API's markdown | `GET /privacy-policy` |
| GET | `/account` | `account::get` | RGPD hub: identity, export entry point, deletion entry point or pending panel, policy link | — (uses `GET /auth/me` via `CurrentUser`) |
| GET | `/account/export` | `account::export::get` | What the export contains / omits, + the download button | — |
| GET | `/account/export/download` | `account::export::download` | Relays the export bytes as a dated JSON attachment | `GET /account/export` |
| GET | `/account/delete` | `account::delete::get` | Deletion explanation + confirmation form, or the pending panel | — |
| POST | `/account/delete` | `account::delete::post` | Requests deletion (after the local guards) | `POST /account/delete` |
| POST | `/account/delete/cancel` | `account::delete::cancel` | Cancels a pending request | `POST /account/delete/cancel` |

Every `/account/*` route is gated by `CurrentUser` (no session → `/login`).
`/privacy-policy` uses `CurrentUserOpt`: an authenticated visitor keeps the app
chrome, an anonymous one gets a "Retour à la connexion" link instead. The
`Mon compte` link sits in the authenticated header next to the logout button
(`app.rs`), so the hub is reachable from every page.

## Error tables (per page)

Backend codes are `apps/api/src/error.rs` bodies (`{"error": "<code>"}`); each
row is the exact `(status, code)` → French UI state.

### `GET /privacy-policy` — `rgpd::privacy_policy`

| Status | Code | UI |
|---|---|---|
| 200 | — | the rendered policy inside `article.prose` |
| — | other status | "Le document est momentanément indisponible" notice, header still rendered |
| — | transport error | same notice (the page itself always renders — it must stay readable to a logged-out visitor) |

### `GET /account` — the hub

| Status | Code | UI |
|---|---|---|
| — | `?notice=deletion_requested` | success banner: request recorded, still cancellable |
| — | `?notice=deletion_cancelled` | success banner: account stays active |
| — | `?error=no_pending_deletion` | error banner: nothing to cancel |
| — | `?error=unavailable` | error banner: service momentarily unavailable |
| — | no session | `/login` (via `CurrentUser`) |

### `GET /account/export/download` — `rgpd::export_account`

| Status | Code | UI |
|---|---|---|
| 200 | — | JSON attachment, `mes-donnees-manage-our-home-YYYY-MM-DD.json` |
| 401 | `unauthorized` | `/login` (the session died between the two requests) |
| — | other status | PRG → `/account/export?error=unavailable` |
| — | transport error | service-unavailable page |

### `POST /account/delete` — `auth::delete_account`

| Status | Code | UI |
|---|---|---|
| — | `password_required` (local) | "Saisissez votre mot de passe actuel…", nothing sent to the API |
| — | `consent_required` (local) | "Cochez la case de confirmation…", nothing sent to the API |
| 200 | — | PRG → `/account?notice=deletion_requested` |
| 401 | `unauthorized` | "Mot de passe incorrect." on the form |
| 409 | `owner_of_groups` | the block + each blocking family + the two ways out, one settings link per family |
| — | other status | "La demande… n'a pas pu être enregistrée." |
| — | transport error | service-unavailable page |

### `POST /account/delete/cancel` — `auth::cancel_delete_account`

| Status | Code | UI |
|---|---|---|
| — | nothing pending (local) | PRG → `/account?error=no_pending_deletion`, nothing sent to the API |
| 200 | — | PRG → `/account?notice=deletion_cancelled` |
| 404 | `not_found` | PRG → `/account?error=no_pending_deletion` (e.g. a double submit) |
| — | other status | PRG → `/account?error=unavailable` |
| — | transport error | service-unavailable page |

## Acceptance criteria

1. `/privacy-policy` is readable **without a session**, reachable from the login
   and register footers, and renders the shipped `docs/privacy-policy.md` as
   HTML (headings, the legal-basis table, bullet lists) with no raw markdown
   left over.
2. From the header's `Mon compte` link, a user reaches `/account` and downloads
   their data as a dated JSON file containing their profile, their group
   memberships and the content they created — and not other members' content.
3. Requesting deletion schedules it: the hub shows the pending panel with the
   request date and the purge deadline, the request entry point is replaced, and
   the session keeps working (the account is only *scheduled*).
4. A pending request can be cancelled from either `/account` or
   `/account/delete`, returning the hub to its normal state.
5. The sole owner of a family is blocked (409) with actionable guidance naming
   each blocking family and linking to its settings; once ownership is
   transferred or the family deleted, the request goes through.
6. The confirmation is guarded: consent unticked → nothing sent; missing
   password on a password account → nothing sent; wrong password → the
   backend's 401 surfaced on the form; and in none of those cases is a request
   recorded.
7. The pure `validate_deletion_confirmation` / `can_request_deletion` /
   `can_cancel_deletion` / `deletion_deadline` / `format_rgpd_datetime` /
   `format_rgpd_date` / `export_filename` / `render_markdown` logic is
   unit-tested in `apps/shared` (`validation::rgpd`), written test-first per
   `.claude/CLAUDE.md`; the E2E suite (`e2e/tests/rgpd.spec.ts`) drives every
   journey above end-to-end against the real stack as a CI merge gate.

## Backend prerequisite

`GET /auth/me` gained two fields — `has_password`
(`users.password_hash IS NOT NULL`) and `deletion_requested_at` — on the
`AuthUser` extractor, the `MeResponse` DTO and the `me` handler, with a flow test
(`rgpd_flow.rs::auth_me_reports_password_and_pending_deletion`) asserting both
across a fresh account, a pending deletion request, and a cancellation. This is
the only backend change; the four RGPD endpoints were already shipped by backend
epic #12.

`DeleteAccountRequest` also moved from a local `apps/api` struct into
`apps/shared/src/dto/auth.rs`, so the request body has one definition shared by
both sides — the same consolidation F1 did for the other auth DTOs.
