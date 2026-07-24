# Front epic F9 — User admin (issue #24)

SSR spec at the depth F1 (#15) established: route table, error tables mapping
`apps/api/src/user_admin/`'s exact status codes to French copy, and acceptance
criteria. Backend is already shipped (`apps/api/src/user_admin/`, migration
`0009_user_admin.sql`, v1-scope row #8) — this epic is the `apps/web` client
only, following the same pattern as the family-scoped front epics (Leptos SSR,
plain `<form method=post>`, PRG with `?notice=`/`?error=` codes, per-page error
tables, pure logic mirrored in `apps/shared` and TDD'd).

Independent of every family-scoped epic: it depends only on F1 (Auth) for the
session and the `is_superadmin` flag — no group/family context is needed.
Cross-family visibility (listing families/accounts the superadmin isn't a member
of) is the one deliberate, gated exception to the `groups`/`group_members` RLS
boundary, see `architecture.md`'s RLS/superadmin section; the backend enforces
it via a `SuperAdminUser` extractor + a dedicated `BYPASSRLS` pool, and this
epic never touches that — it only calls the three finished endpoints.

## Design decisions

### The whole `/admin` tree is gated twice, and never revealed

The backend already 403s every `/admin/*` call from a non-superadmin (the
`SuperAdminUser` extractor). The front adds defense in depth: a
`CurrentSuperAdmin` extractor resolves the session like `CurrentUser`, then
additionally requires `MeResponse::is_superadmin`. A visitor with no session
goes to `/login`; an authenticated **non**-superadmin goes to `/` — the admin
area is never shown to them, not even as a 403 page, and the `Admin` nav link is
only rendered for a superadmin (`app::app_header`). So an ordinary user never
sees a door they can't open.

This needed one backend prerequisite: `GET /auth/me` now returns
`is_superadmin` (added to the `AuthUser` extractor + `MeResponse`), the same way
F1 added `GET /auth/me` itself. The flag is read live from the DB on every
request (the session lookup joins `users`), so promoting an account takes effect
on its next page load with no re-login — and, symmetrically, a demotion would.

### No single-user API, so the detail page reuses the list

`apps/api` exposes `GET /admin/users` (the full list) but **no**
`GET /admin/users/:id`. Rather than add an endpoint, `/admin/users/:id` finds
its user in the same list the table renders — exactly how Messagerie (F8)
derives one message from the paginated list when there is no single-message GET.
An id that isn't in the list → the "Utilisateur introuvable" page.

### Deactivate is immediate and terminal — kept distinct from self-service deletion

`POST /admin/users/:id/deactivate` is a support action: it revokes every active
session and sets `deleted_at` **now**, atomically with the audit-log write. It is
deliberately **not** the self-service account deletion (F10), which is a
grace-period flow keyed on `deletion_requested_at`. The copy keeps them apart
("Action immédiate de support… À distinguer de la suppression de compte en
libre-service"), and the confirm form only renders while the account is still
active — the backend guards with `AND deleted_at IS NULL` and 404s a second
attempt, so `can_deactivate` (pure, TDD'd) hides the button once it is gone. A
native `confirm()` guards the click (progressive enhancement — with JS off the
form still posts; the confirmation is a convenience, not a security control,
since the backend is the authority).

### Read-only otherwise, and no audit-log viewer

`/admin/groups` and `/admin/users` are pure look-up tables — no mutation, so no
PRG there. Every successful admin action (including the two list reads) already
writes an `audit_log` row server-side; issue #24 explicitly scopes a separate
audit-log viewer UI out of this pass, and nothing here adds one.

### Out of scope

Team/role management (v1 has a single expected superadmin — issue #24, and
`architecture.md`'s "Questions résolues" #3), an audit-log viewer, editing any
user/group field, and creating/promoting a superadmin from the UI (the flag is
set manually via SQL, matching the backend's stance).

## Route table (`apps/web`)

| Method | Path | Handler | Purpose | API call(s) |
|---|---|---|---|---|
| GET | `/admin/groups` | `admin::groups::get` | Read-only table of every family (id/name/created/member count) across all tenants | `GET /admin/groups` |
| GET | `/admin/users` | `admin::users::get` | Read-only table of every account (email/verified/created/status), each linking to the detail | `GET /admin/users` |
| GET | `/admin/users/:id` | `admin::users::detail` | One account's detail + the deactivate confirm form (when still active) | `GET /admin/users` (found in the list) |
| POST | `/admin/users/:id/deactivate` | `admin::users::deactivate` | Immediate deactivate (revoke sessions + set `deleted_at`) | `POST /admin/users/:id/deactivate` |

Every route is gated by `CurrentSuperAdmin`: no session → `/login`;
authenticated non-superadmin → `/`. The `Admin` nav link (→ `/admin/users`) sits
after `Groupes` in `app.rs`, rendered only for a superadmin.

## Error tables (per page)

Backend codes are `apps/api/src/error.rs` bodies (`{"error": "<code>"}`); each
row is the exact `(status, code)` → French UI state.

### `GET /admin/groups` — `list_groups`

| Status | Code | UI |
|---|---|---|
| 200 | — | table of families (empty-state note when there are none) |
| 403 | `forbidden` | empty table, no JSON leak (unreachable — the route is superadmin-gated; defensive) |
| — | other status | empty table, no JSON leak |
| — | transport error | service-unavailable page |

### `GET /admin/users` — `list_users`

| Status | Code | UI |
|---|---|---|
| 200 | — | table of accounts with status (empty-state note when there are none) |
| 403 | `forbidden` | empty table, no JSON leak (defensive) |
| — | other status | empty table, no JSON leak |
| — | transport error | service-unavailable page |

### `GET /admin/users/:id` — detail (via `list_users`)

| Status | Code | UI |
|---|---|---|
| 200 | — | user detail; deactivate form when `deleted_at` is null, else "déjà désactivé" note |
| — | id not in the list | "Utilisateur introuvable" page |
| — | transport error | service-unavailable page |

### `POST /admin/users/:id/deactivate` — `deactivate_user`

| Status | Code | UI |
|---|---|---|
| 204 | — | PRG → `/admin/users?notice=user_deactivated` |
| 404 | `not_found` | "Utilisateur introuvable" page (unknown or already deactivated) |
| 403 | `forbidden` | forbidden page (unreachable once gated; defensive) |
| — | other status | PRG → `/admin/users?error=unavailable` |
| — | transport error | service-unavailable page |

## Gate (front mirror of `user_admin::is_superadmin`)

Backend gate (`apps/api/src/user_admin/mod.rs::is_superadmin` + the
`SuperAdminUser` extractor): a request is allowed iff `users.is_superadmin`. The
front mirrors it as `manage_our_home_shared::validation::user_admin::can_view_admin`
(pure, unit-tested), used both to render the `Admin` nav link and — via the
`CurrentSuperAdmin` extractor — to gate the `/admin/*` handlers. The backend
stays the authority: a forged call from a non-superadmin session is still 403'd.

## Acceptance criteria

1. A superadmin sees an `Admin` nav link and can open `/admin/groups` and
   `/admin/users`; both list entities from **families the superadmin does not
   belong to** (the gated cross-tenant exception).
2. `/admin/users` shows each account's status derived from its timestamps
   (`Actif` / `Suppression demandée` / `Désactivé`), and each row links to the
   detail page.
3. From a user's detail page a superadmin deactivates the account: a success
   banner, the target's existing session stops working (their next page → 
   `/login`), and the row now reads `Désactivé`.
4. Deactivation is terminal: revisiting the detail of an already-deactivated
   account shows no deactivate button, and an unknown user id → "Utilisateur
   introuvable".
5. An authenticated **non**-superadmin sees no `Admin` link and is redirected to
   `/` on any `/admin/*` route; an unauthenticated visitor is redirected to
   `/login`.
6. The pure `can_view_admin` / `user_status_label` / `can_deactivate` /
   `format_admin_datetime` logic is unit-tested in `apps/shared`
   (`validation::user_admin`), written test-first per `.claude/CLAUDE.md`; the
   E2E suite (`e2e/tests/admin.spec.ts`) drives every journey above end-to-end
   against the real stack as a CI merge gate.

## Backend prerequisite

`GET /auth/me` gained an `is_superadmin` field (`AuthUser` extractor +
`MeResponse` DTO + `me` handler), with a flow test
(`user_admin_flow.rs::auth_me_reports_superadmin_flag`) asserting it is `false`
for a plain account and `true` once the flag is set. This is the only backend
change; the three `/admin/*` endpoints were already shipped by backend epic #8.
