# v1 scope tracker

Status snapshot of every service/epic needed for v1, per the dependency
order set in `architecture.md` (Groups → Agenda → Stocks → Recipes →
Grocery list → Budget; Messagerie and User admin independent). Update this
file as epics land — it's the single place to check "what's left."

| # | Epic / service | Status | Notes |
|---|---|---|---|
| 1 | Auth + Groups | **done** | Merged to `main` (PR #3, branch `spec/epic1-auth-groups-73929`). Moved into `apps/api/` (`git mv`, no code changes) now that it's landed. |
| 2 | Agenda (events, tasks-as-events, recurrence, reminders, files) | **done** | Implemented in `apps/api/` (`src/agenda/`, migrations `0002_agenda.sql`, `0003_event_occurrence_completions.sql`). RRULE-based recurrence (`rrule` crate), `scheduled_notifications` job-queue table + worker, MinIO wired for attachments (epic #10 pulled in as a dependency rather than stubbed). Recurring-task completion is tracked per occurrence, not on the shared event row. PR #4 (branch `epic2-agenda-review-fixes`) merged to `main`. |
| 3 | Stocks | **done** | Manual entry v1 (no scan/OCR). Implemented in `apps/api/` (`src/stocks/`, migration `0004_stocks.sql`). Family-scoped (`group_id`) like Agenda, same RLS + `scoped_tx`/`require_role` pattern. Reorder threshold is per-article, shared at the family level (`stock_items.reorder_threshold`); `low_stock` is derived on read, not stored. Any member can create/read/adjust; only the item's creator or a group admin/owner can update/delete. |
| 4 | Recipes (suggestion algorithm) | **done** | Implemented in `apps/api/` (`src/recipes/`, migration `0005_recipes.sql`). Family-scoped (`group_id`) with the same RLS + `scoped_tx`/`require_role` pattern as Stocks/Agenda. Rule-based (no ML): scores each recipe on stock match (0-100 pts, name-based against `stock_items`), a variety penalty if logged in `meal_history` within the last 14 days, and a small seasonal bonus per ingredient whose `seasonal_months` includes the current month. Emits a structured `missing_ingredients` list per suggestion (name/quantity/unit) for the Grocery-list epic. Any member may create/read a recipe or log a meal; only the recipe's creator or a group admin/owner may update/delete, same permission bar as Stocks. |
| 5 | Grocery list | **done** | Implemented in `apps/api/` (`src/grocery_list/`, migration `0006_grocery_list.sql`). Family-scoped (`group_id`) with the same RLS + `scoped_tx`/`require_role` pattern as Stocks/Recipes/Agenda. One shared list per family, fed by three sources: manual entry, a `POST /groups/:id/grocery-items/generate` endpoint that pulls in recipes' `missing_ingredients` (required ingredients not currently in stock) and stock items at/below their reorder threshold, deduplicating by name (case-insensitive, trimmed) against unchecked items so re-running it is idempotent. Any member may create/read/check off an item; only the item's creator or a group admin/owner may edit (name/quantity/unit) or delete it — same permission bar as Stocks/Recipes. |
| 6 | Budget | **done** | Implemented in `apps/api/` (`src/budget/`, migration `0007_budget.sql`). Family-scoped (`group_id`) with the same RLS + `scoped_tx`/`require_role` pattern as Stocks/Recipes/Grocery list. Tied to the grocery list, not a general expense tracker: manual price entry only (`POST /groups/:id/budget-entries`), plus `POST /groups/:id/grocery-items/:item_id/price` to associate a price with a grocery item once it's checked/bought (name denormalized onto the entry so it survives item deletion). Spend is cumulated per month at the family level via `GET /groups/:id/budget-entries/summary` (`date_trunc('month', spent_at)`, computed on read, not stored). Any member may create/read an entry or set a price; only the entry's creator or a group admin/owner may update/delete it, same permission bar as Stocks/Recipes/Grocery list. |
| 7 | Messagerie (one thread per family, text only) | **done** | Implemented in `apps/api/` (`src/messagerie/`, migration `0008_messagerie.sql`). Family-scoped (`group_id`) with the same RLS + `scoped_tx`/`require_role` pattern as Stocks/Recipes/Grocery list/Budget. Message content is stored encrypted via pgcrypto (`pgp_sym_encrypt`/`pgp_sym_decrypt`, dedicated `MESSAGE_ENCRYPTION_KEY`, isolated from the OAuth and calendar-feed keys — one key per secret class). REST CRUD (`/groups/:id/messages`) with cursor pagination (`before_created_at` + `before_id`, newest first, `has_more` via limit+1); writes go through REST only — the WS endpoint (`GET /groups/:id/messages/ws`) is push-only, fanning out `message.created`/`message.updated`/`message.deleted` over a per-family in-memory `tokio::sync::broadcast` channel (single API instance in v1, no LISTEN/NOTIFY). Any member may read/post; only the message's author or a group admin/owner may edit/delete, same permission bar as Stocks/Recipes/Grocery list/Budget. One deliberate deviation from the issue #10 spec, ratified in review: WS membership re-validation is a periodic 30s tick, **not** a per-event check before every broadcast send — one DB round-trip per event per connection on every fan-out was rejected on perf grounds; the cost is that a removed member keeps receiving pushes for ≤ 30s, a bounded window (rationale documented at the top of `src/messagerie/ws.rs`; the interval is configurable via `AppState.message_ws_recheck_interval` so the removal-disconnect flow test exercises the bound without sleeping 30s). Writes (POST/PATCH/DELETE) still re-run `require_role` on every request. The spec's E2E journey (login → open thread → message appears live on a second WS session) is deferred to front epic F8 (#23) with the rest of the UI-driven Playwright suites. |
| 8 | User admin (global technical superadmin) | **done** | Implemented in `apps/api/` (`src/user_admin/`, migration `0009_user_admin.sql` — a single `users.is_superadmin` flag, no new table, set manually via SQL since there's no signup flow for the role). Three support/maintenance endpoints only, no admin UI: `GET /admin/groups` (id/name/created_at/member_count across every family), `GET /admin/users` (id/email/email_verified/created_at/deleted_at/deletion_requested_at), and `POST /admin/users/:id/deactivate` (immediate support action — revokes every active session and sets `deleted_at`, distinct from the self-service `account/delete` grace-period flow). Cross-family visibility is a deliberate, narrow exception to the `groups`/`group_members` RLS boundary rather than a general bypass: a `SuperAdminUser` extractor (mirrors `AuthUser`, additionally requires `is_superadmin = true` else 403) gates a second connection pool (`AppState.admin_db`, `ADMIN_DATABASE_URL`) connected as a dedicated `admin_role` with `BYPASSRLS` — see `apps/api/README.md`'s role-setup section. Every successful admin action writes one `audit_log` row with the superadmin's `user_id` as actor. |
| 9 | Google Calendar import (one-way) | **done** | Implemented in `apps/api/` (`src/google_calendar/`, migration `0010_google_calendar_import.sql`). Family-scoped (`group_id`) with the same RLS + `scoped_tx`/`require_role` pattern as the other epics, but a **stricter** permission bar than usual: creating/deleting a `calendar_imports` connection requires admin/owner (the feed URL is a bearer credential for a member's Google account, not household data), while triggering an on-demand import and listing connections follows the normal any-member bar. Design decision: a per-calendar private ICS feed URL ("secret address in iCal format") rather than full OAuth2 + Calendar REST API — no consent screen/token refresh/Google Cloud project needed for a pull-only v1 mirror; tradeoff is up-to-a-few-hours staleness (Google-side ICS caching) and no webhooks, both acceptable since real-time and bidirectional sync are explicitly out of v1. The feed URL is stored encrypted via pgcrypto (`pgp_sym_encrypt`, dedicated `CALENDAR_FEED_ENCRYPTION_KEY`) and never echoed back by the API. `POST /groups/:id/calendar-imports/:import_id/import` fetches the feed fresh (`icalendar` crate, pure-logic parsing in `src/google_calendar/parse.rs`, unit-tested first per TDD) and upserts each VEVENT into `events` keyed by UID (`calendar_import_events` mapping table), skipping re-writes when `LAST-MODIFIED`/`DTSTAMP` hasn't changed — idempotent re-runs, no duplicate events. Recurring VEVENTs are imported as their DTSTART/DTEND occurrence only (Google already expands RECURRENCE-ID overrides into separate VEVENTs in its export); expanding a bare RRULE ourselves is a documented v1 limitation, not attempted. |
| 10 | Object storage (MinIO) wiring | **done** | Wired as part of epic #2 (Agenda): `src/storage.rs` (aws-sdk-s3 client pointed at MinIO), `infra/docker-compose.yml` minio service. Fridge-scan photos are post-v1. |
| 11 | Transactional email (Brevo/Mailjet via `lettre`) | partially designed | Stack decided (architecture.md); actual sending code is part of Auth epic (verification, password reset) — check epic #1 branch for implementation status. |
| 12 | RGPD features (export, erasure, privacy policy, registre des traitements) | **done** | Erasure (Art. 17) was already implemented as part of epic #1 (`apps/api/src/auth/mod.rs::delete_account` + `src/jobs/account_purge.rs`), including the sole-group-owner block (`owner_of_groups` 409) required by `notes-issue-1-qa.md` — verified still enforced, no gap found, nothing changed there. This epic adds the two missing pieces: **data export** (Art. 20) — `GET /account/export`, new `apps/api/src/rgpd/` module (`mod.rs` handler, `export.rs` pure `build_export`/`ExportCategories` JSON-shaping logic, unit-tested first per TDD). Self-service only (scoped by `AuthUser`, no target-user param). Iterates the requesting user's groups (`user_scoped_tx`, membership-based `groups` RLS fallback), then per group opens a `scoped_tx` and pulls every `created_by = user_id` row across events, stock_items, recipes, meal_history, grocery_items, budget_entries, messages (decrypted via `pgp_sym_decrypt` same as the Messagerie read path) and calendar_imports (metadata only — the encrypted feed URL is a bearer credential and is never decrypted for export, matching the existing `GET` endpoint's precedent). Writes an `account_data_exported` audit_log row. No new migration: export/privacy-policy needed no schema changes, so no `0011_*.sql` was added. **Privacy policy**: `GET /privacy-policy` (no auth) serves `docs/privacy-policy.md` compiled in via `include_str!`. **Registre des traitements**: `docs/registre-traitements.md`, one row per epic/data category with legal basis, retention, and recipients, per architecture.md's Art. 30 requirement and the data-controller/DPO stance from "Questions résolues" #3. Flow tests in `apps/api/tests/rgpd_flow.rs` (export scoping, cross-member leak check, auth requirement, public privacy-policy). Branch `feat/epic12-rgpd`. |
| 13 | CI (cargo audit, blocking on high/critical) | partially done | `cargo audit` job added to `ci.yml` (2026-07-08), covers the root workspace (`apps/api`). Frontend stack is now Rust/Leptos (`apps/web`, `apps/shared` — see `architecture.md` and Front epic #1, GH issue #15), so the same `cargo audit` job will cover it once those crates join the workspace; no separate `npm audit` job needed. |
| 14 | Deployment / infra (docker-compose, Caddy, secrets via sops) | scaffolded | `infra/docker-compose.yml` and `infra/Caddyfile` are skeletons pointing at services that don't exist yet (`apps/api` image, no `apps/web` build). Flesh out as those apps land. Public/internet exposure explicitly deferred past v1 per `notes-issue-1-qa.md` (#7) — local/VPN only for now. |
| — | Fridge-scan (OCR/vision via Ollama) | out of v1 | Explicitly scoped out, see architecture.md. |
| — | Bidirectional Google Calendar sync | out of v1 | Explicitly scoped out. |
| — | Local price lookup for Budget | out of v1 | Manual entry only for v1. |

**Immediate next step:** every backend epic row above is now **done** (epic #7's Messagerie row was the last one pending an update). Remaining work: the front epics below (F1 onwards, in dependency order), #11 (transactional email finish-up), and the two infra rows — #13 (CI already covers the Rust workspace via `cargo audit`; keep it covering `apps/web`/`apps/shared` as they join) and #14 (flesh out docker-compose/Caddy as the apps land).

## Frontend (apps/web, Leptos + Axum)

All backend epics above ship an API only — no client exists yet
(`apps/web`/`apps/shared` are not in the workspace). Front epics are
spec'd and filed one at a time, same discipline as the backend, in the
same dependency order (Groups → Agenda → Stocks → Recipes → Grocery
list → Budget; Messagerie and User admin independent, RGPD-facing
screens as their own pass). Stack: **Leptos SSR** (`leptos_axum`) +
shared `apps/shared` DTO/validation crate — supersedes the earlier
SvelteKit choice, see `architecture.md`'s "Web frontend" row and Front
epic #1 (GH issue #15) for the full rationale.

| # | Front epic | GH issue | Status | Notes |
|---|---|---|---|---|
| F1 | Auth (register/verify/login/forgot-reset password/Google OAuth/logout) | #15 | **done** | Stood up the `apps/web` + `apps/shared` workspace crates for the first time (Leptos SSR shell every other front epic builds on). `apps/web/src/routes/auth/` (`register`, `verify_email`, `login`, `forgot_password`, `reset_password`, logout) + the `GET /auth/me` session/cookie pattern F2–F10 reuse. Added the backend `GET /auth/me` prerequisite. E2E `e2e/tests/auth.spec.ts` (Google OAuth skipped — no test provider). Account-deletion UI and Capacitor/mobile carved out (→ F10 / v1.1). Merged as PR #28 (`2759e6b`). |
| F2 | Groups (create/join/invite, roles, switch active family) | #17 | **done** | `apps/web/src/routes/groups/` (`/groups`, `/groups/new`, `/groups/:id/members`, `/groups/:id/settings`, `/groups/invitations/:token/accept`) + the root-layout active-family switcher (persisted in the `active_group_id` cookie, resolved against `GET /groups` on every page — see `apps/web/src/family.rs`). DTOs in `apps/shared/src/dto/groups.rs`; pure validation/permission mirrors (group name, invitation-token parsing, owner/admin bar, `actor_can_act_on`) TDD'd in `apps/shared/src/validation/groups.rs`. Error UI maps apps/api's exact codes: 422 `too_many_groups`/`name_required`/`new_owner_id_required`/`cannot_transfer_to_self`, 409 `last_member_must_delete_group`, 410 consumed/expired invitation, 404 unknown invitation/group, 403 permission bar. |
| F3 | Agenda (calendar view, events, tasks-as-events, recurrence, reminders, file attachments) | #18 | **done** | `apps/web/src/routes/agenda/` (`/agenda` month/week calendar — widget hand-rolled per architecture.md's Leptos ecosystem-gap note, `/agenda/new`, `/agenda/:id` detail, `/agenda/:id/edit`, `/agenda/:id/{delete,complete}`, `/agenda/:id/reminders[/:rid/delete]`, `/agenda/:id/attachments[/:aid/{download,delete}]` — first front epic to wire MinIO for presigned attachment up/download). DTOs in `apps/shared/src/dto/agenda.rs`; pure logic (RRULE v1 subset, file size/extension caps, per-occurrence completion) TDD'd in `apps/shared/src/validation/agenda.rs`. Error UI maps apps/api's exact codes; 404 unknown event, 403 permission bar (a standard member gets no edit/delete on another member's event). Recurring tasks track completion per occurrence, not on the shared event row. Merged as PR #37 (`536143a`). |
| F4 | Stocks (list, manual entry, reorder threshold, low-stock indicator) | #19 | **done** | `apps/web/src/routes/stocks/` (`/stocks` list with low-stock badge + `?low_stock=1` filter delegated to the backend, `/stocks/new`, `/stocks/:id` detail with quantity-adjust, `/stocks/:id/edit`, `/stocks/:id/{adjust,delete}` POST actions). DTOs in `apps/shared/src/dto/stocks.rs` (double-`Option` PATCH mirror); pure `validate_item_form` + `is_low_stock` TDD'd in `apps/shared/src/validation/stocks.rs`. Error UI maps apps/api's exact codes: 400 `name_required`/`unit_required`/`quantity_must_be_non_negative`/`reorder_threshold_must_be_non_negative`, 404 unknown item, 403 permission bar. **Permission bar (#19 + follow-up #39, now landed):** the backend runs a two-tier bar — a quantity-only `PATCH` is open to any family member (shared inventory), while touching any full-record field, or delete, stays behind `can_modify` (creator/admin/owner). The front mirrors it: the adjust form renders for every member; the edit link and delete button only for `can_modify` users. The backend stays the authority (a forged full-edit/delete is still 403'd). |
| F5 | Recipes (list, suggestion view, missing-ingredients display, log a meal) | #20 | **done** | `apps/web/src/routes/recipes/` (`/recipes` list + ranked suggestions, `/recipes/new`, `/recipes/:id` detail with log-a-meal, `/recipes/:id/edit`, `/recipes/:id/{log,delete}` POST actions). Full spec at `docs/front-epic-5-recipes.md` (route table, per-page error tables, acceptance criteria). DTOs in `apps/shared/src/dto/recipes.rs` (double-`Option` PATCH mirror); pure ingredient textarea `parse_ingredients`/`format_ingredients` + `stock_summary` + `validate_recipe_name` TDD'd in `apps/shared/src/validation/recipes.rs`. **Decision:** the suggestion view renders the *ranked order* + derived human signals (stock summary, "déjà cuisiné récemment", missing-ingredients list), never the raw internal `score`. Missing ingredients carry an informational grocery-list marker (the cross-epic generate action is F6, out of scope). Error UI maps apps/api's exact codes: 400 `name_required`/`ingredient_name_required`/`ingredient_quantity_must_be_non_negative`/`invalid_seasonal_month`, 404 unknown recipe, 403 permission bar. Permission bar mirrors Stocks/Recipes backend `can_modify`: any member may create/read/log-a-meal; only the recipe's creator or a group admin/owner sees edit/delete (backend stays the authority). |
| F6 | Grocery list (shared list, manual entry, generate-from-recipes/stocks, check off) | #21 | **done** | `apps/web/src/routes/grocery_list/` (`/grocery-list` shared list with inline manual-add + generate button + per-row no-JS check-off, `/grocery-list/:id` edit screen, `/grocery-list/:id/{add,generate,check,edit,delete}` POST actions). Full spec at `docs/front-epic-6-grocery-list.md` (route table, per-page error tables, acceptance criteria). DTOs in `apps/shared/src/dto/grocery_list.rs` (double-`Option` PATCH mirror); pure `validate_item_form` + `can_modify` mirror + `quantity_label`/`fmt_num`/`source_label` formatting TDD'd in `apps/shared/src/validation/grocery_list.rs`. **Decisions:** one shared list per family (not per-user); manual add is inline on the list (no separate `/new` page); generate is idempotent so it's a plain no-confirmation button with a re-run-safe banner reporting the count added; check-off is a no-JS-safe per-row `<form>` (checkbox PE + always-present `Cocher`/`Décocher` button); the Budget price-on-checkout action is the F7 hook and is **out of scope** here (F6 renders the check-off trigger only, no price field, no dead `/budget` link — same way F5 left the generate action to F6). Error UI maps apps/api's exact codes: 400 `name_required`/`quantity_must_be_non_negative`, 404 unknown item, 403 permission bar. Permission bar mirrors the backend `can_modify`: any member may add/read/check off; only the item's creator or a group admin/owner sees the per-row Modifier link and the edit/delete screen (backend stays the authority). |
| F7 | Budget (manual entry, price-on-checkout, monthly summary) | #22 | **done** | `apps/web/src/routes/budget/` (`/budget` monthly summary + entry list, `/budget/new` manual price entry, `/budget/:id` edit screen, `/budget/:id/{edit,delete}` POST actions) plus the price-on-checkout hook F6 deferred: an inline `Renseigner le prix` form on each **checked** grocery item, posting to a new `POST /grocery-list/:id/price` handler (`routes/grocery_list/list.rs`, relaying to `POST /groups/:id/grocery-items/:item_id/price`) — no dedicated route, the "Budget hook out of scope" note F6 left in `routes/grocery_list/mod.rs` removed. Nav link added in `app.rs`. Full spec at `docs/front-epic-7-budget.md` (route table, per-page error tables, acceptance criteria). DTOs in `apps/shared/src/dto/budget.rs` — field-for-field, **no double-`Option`** (`spent_at` is `NOT NULL`, a plain `Option` = "leave as-is" only); the summary wire field is `total` (euros), not `total_cents`. Pure `validate_entry_form` + `can_modify` mirror + euros formatting (`format_euros` two-decimal French comma, `amount_input_value` for number-input pre-fill) + `format_period` monthly label TDD'd in `apps/shared/src/validation/budget.rs`. **Decisions:** Budget is strictly grocery-derived (manual price entry + price-on-checkout only, no category/payee/recurring; local price lookup stays out of v1); amount is euros at the API boundary (backend stores cents) so all rounding lives in one tested place; the monthly summary is read-only, computed server-side; set-price upserts on the grocery item so a re-tap re-tarifies rather than double-counting (the `/budget/new` manual path can 409 `grocery_item_already_priced`, but the form never sends a `grocery_item_id`, so it's defensive-only). Error UI maps apps/api's exact codes: 400 `name_required`/`amount_must_be_non_negative`, 409 `grocery_item_already_priced`, 404 unknown entry, 403 permission bar. Permission bar mirrors the backend `can_modify`: any member may create/read an entry or set a price; only the entry's creator or a group admin/owner sees the per-row Modifier link and the edit/delete screen (backend stays the authority). |
| F8 | Messagerie (one family chat thread, WebSocket) | #23 | **done** | `apps/web/src/routes/messagerie/` (`/messagerie` single family thread — live view with composer + inline edit/delete behind a permission bar, plus a windowed history page reached by a cursor URL — and `/messagerie/:id/{edit,delete}` POST actions). Nav link added in `app.rs` (between `/budget` and `/groups`). Full spec at `docs/front-epic-8-messagerie.md` (route table, per-page error tables, acceptance criteria). DTOs in `apps/shared/src/dto/messagerie.rs` (field-for-field mirror of the REST requests/responses + the WS `MessageEvent`); pure `validate_content` (trim/non-empty/≤4000 **chars**) + `can_modify` mirror + `format_message_time` (Europe/Paris) + `clamp_limit`/`older_page_query` (cursor pagination, microsecond-exact) + `message_ws_url` (http→ws / https→wss, relative base stays relative) + `author_name` (member-id→name, "Membre" fallback for someone who left) TDD'd in `apps/shared/src/validation/messagerie.rs`. **Decisions:** the WebSocket carries a bare "something changed" *signal*, not a view — the inline script re-fetches the current URL and swaps `#thread`'s HTML with the server-rendered fragment, so escaping/permission-bar/copy have one implementation (Rust) and the socket needs no client-side message model; no-JS is the baseline (send/edit/delete are plain form posts, editing is inline in a native `<details>` since the backend exposes no single-message GET), the WS is pure enhancement; the browser opens the socket straight against `apps/api` via `API_PUBLIC_BASE_URL` (same registrable domain → `SameSite=Lax` cookie rides the handshake, no proxy leg); connection lifecycle is bound to the document (full-page nav = teardown, so "switch family" needs no state machine); on close the client runs one authoritative refresh fetch — a landing on `/login` or a changed/absent `#thread[data-group-id]` stops reconnects with a visible banner (this resolves the API's ≤30 s membership-revalidation window, documented atop `ws.rs`, into UI state), otherwise exponential backoff (1→30 s, give-up banner after 5); pagination is a windowed history URL, not infinite scroll, so the URL fully describes what it shows. Error UI maps apps/api's exact codes: 400 `content_required`/`content_too_long`, 404 unknown/deleted message, 403 permission bar. Permission bar mirrors the backend `can_modify`: any member reads/posts/opens-the-socket; only the message's author or a group admin/owner sees `Modifier`/`Supprimer` (backend stays the authority). This is also where backend epic #7's deferred E2E journey lands (login → open thread → a message posted appears live on a second WS session). |
| F9 | User admin (superadmin support screens: groups/users list, deactivate) | #24 | **done** | `apps/web/src/routes/admin/` (`/admin/groups` read-only family table, `/admin/users` account table, `/admin/users/:id` detail + deactivate, `/admin/users/:id/deactivate` POST action). Full spec at `docs/front-epic-9-user-admin.md`. DTOs in `apps/shared/src/dto/user_admin.rs` (mirror the `{"groups":[…]}`/`{"users":[…]}` envelopes); pure `can_view_admin` (gate mirror) + `user_status_label` + `can_deactivate` + `format_admin_datetime[_opt]` (Europe/Paris) TDD'd in `apps/shared/src/validation/user_admin.rs`. **Backend prerequisite:** `GET /auth/me` now returns `is_superadmin` (added to the `AuthUser` extractor + `MeResponse`, the only backend change — the three `/admin/*` endpoints shipped with backend epic #8), so `apps/web` can gate the tree client-side. **Gate:** a `CurrentSuperAdmin` extractor redirects a non-superadmin to `/` (no session → `/login`) and the `Admin` nav link renders only for a superadmin — defense in depth over the backend's `SuperAdminUser` extractor, which stays the authority (a forged `/admin/*` call is still 403'd). No single-user API, so the detail page finds its user in the `GET /admin/users` list (same as F8 deriving one message from the paginated list). Deactivate is the immediate support action (revokes sessions + sets `deleted_at`), kept distinct in copy from F10's self-service grace-period deletion; the confirm form hides once the account is already deactivated. Error UI maps apps/api's exact codes: 204 → PRG banner, 404 → "Utilisateur introuvable", 403 → forbidden (defensive). |
| F10 | RGPD self-service (data export download, account deletion with owner-of-groups block, privacy-policy page) | #25 | **done** | `apps/web/src/routes/account/` (`/account` RGPD hub, `/account/export` explanation + `/account/export/download`, `/account/delete` + `/account/delete/cancel` POST actions) plus `apps/web/src/routes/privacy.rs` (`/privacy-policy`, kept outside `routes::account` because it must render **without a session** — linked from the login/register footers so a prospective user can read it before registering). `Mon compte` link added to the authenticated header in `app.rs`. Full spec at `docs/front-epic-10-rgpd.md`. DTOs in `apps/shared/src/dto/rgpd.rs` (`OwnerOfGroupsError`/`BlockingGroup` — the export document itself deliberately has **no** DTO); pure `validate_deletion_confirmation` + `can_request_deletion`/`can_cancel_deletion` mirrors + `deletion_deadline` + `format_rgpd_datetime`/`format_rgpd_date` (Europe/Paris) + `export_filename` + `render_markdown` TDD'd in `apps/shared/src/validation/rgpd.rs`. **Backend prerequisite:** `GET /auth/me` now returns `has_password` and `deletion_requested_at` (added to the `AuthUser` extractor + `MeResponse`, flow-tested in `rgpd_flow.rs`), and `DeleteAccountRequest` moved from a local `apps/api` struct into `apps/shared/src/dto/auth.rs` — the only backend changes, the four RGPD endpoints shipped with backend epic #12. **Decisions:** deletion is a **grace-period** flow (stamps `deletion_requested_at`, purged 30 days later by `jobs/account_purge.rs`), so every screen reads "programmée + annulable" and stays deliberately distinct in copy from F9's immediate superadmin `deactivate` — the 30-day horizon lives in one mirrored constant (`GRACE_PERIOD_DAYS`), never hardcoded in the copy; the pending state renders from `GET /auth/me` rather than a local flag, so requesting/cancelling takes effect on the next page load and `/account` + `/account/delete` show the same panel; confirmation asks for the password only when the account has one (`has_password` — a Google-only account has none) but requires an explicit consent tick in **both** cases, which is what discharges `delete_account`'s "re-consent is validated on the frontend flow" comment; the 409 `owner_of_groups` body is turned into actionable guidance (each blocking family named, linked to its F2 settings screen, with transfer-or-delete as the two ways out) rather than a raw error; the export is relayed **byte-for-byte** via a new `api_get_raw` helper (`state.rs`) with only a `Content-Disposition` added — `apps/web` never parses and re-serializes the user's own Art. 20 document — and split across an explanation route and a download route so a refresh never re-triggers it; the privacy policy is rendered from the single source document `apps/api` serves verbatim (`include_str!`) through a small escape-first `render_markdown` subset renderer rather than a markdown crate, with the shipped document itself run through it as a regression guard. Error UI maps apps/api's exact codes: local `password_required`/`consent_required` (nothing sent), 401 wrong password, 409 `owner_of_groups`, 404 nothing-to-cancel. This is also where F1's carved-out account-deletion UI lands (`notes-issue-1-qa.md` #6). |
| F11 | Google Calendar import UI (connect feed, trigger import, list connections) | #52 | **done** | `apps/web/src/routes/agenda/imports.rs` (`/agenda/imports` connections table + empty state, `/agenda/imports/new` connect form, `/agenda/imports/:id/import` pull-now action, `/agenda/imports/:id/delete` confirmation + POST). Placed under `/agenda/*` rather than `/groups/:id/settings` because the artefact is agenda data and the read/trigger bar is "any member" like the rest of Agenda; the family-settings page carries a cross-link, and an `Agendas Google` button sits in the `/agenda` nav row. Full spec at `docs/front-epic-11-google-calendar.md`. DTOs in `apps/shared/src/dto/google_calendar.rs` (mirrors — does not move — the backend's shapes, `user_admin.rs` convention; `CalendarImportResponse` deliberately has **no** `feed_url`); pure `validate_import_form` + `can_configure` mirror + `format_last_imported` (Europe/Paris, explicit "jamais importé") + `import_run_summary` (French pluralisation) TDD'd in `apps/shared/src/validation/google_calendar.rs`. **No backend change** — the four endpoints shipped with backend epic #9. **Decisions:** the copy has to make *pull-on-demand* legible (one shared `MODEL_EXPLAINER` on both screens — à sens unique, à la demande, Google cache l'adresse iCal — pinned by the E2E suite so it can't quietly vanish); the feed URL is treated as the bearer credential it is, submitted once by POST body only, rendered as a password field, **never** re-echoed into a form value after an error (the form re-asks; the label, which is not a secret, is kept), never in a query string or PRG parameter, and `feed_fetch_failed: {reqwest error}` is matched on its code *prefix* so the interpolated tail — which can embed the URL — is dropped before anything renders; the admin/owner bar is stricter than every other family-scoped epic and is mirrored client-side (controls hidden, `new`/`delete` routes bounced with `?error=forbidden`, backend still the authority); the delete confirmation spells out the two consequences the schema makes non-obvious (`calendar_import_events` cascades, `events` does not — so already-imported events survive the delete, and re-adding the same calendar re-imports everything as duplicates, which is also *why* there is no `PATCH`), and the post-delete banner repeats the first one; no in-place edit and no `/agenda/imports/:id` detail page (no backend `PATCH`, no single-import GET — the confirmation resolves its connection out of the list, same as F9's user detail page). Error UI maps apps/api's exact codes: 400 `label_required`/`feed_url_required`/`feed_url_must_be_http_or_https`, 404 unknown connection, 422 `feed_fetch_failed`/`feed_too_large`/`invalid_ics`, 403 permission bar. |
| — | Capacitor/mobile shell | not yet filed | out of this pass, v1.1 | Explicitly deferred in F1's Out of Scope; cross-origin cookie handling for the WebView is an open question to resolve when it's spec'd. |

**Current status (2026-07-28):** **every front epic F1–F11 is done** — the front
pass is complete. In dependency order: F1 stood up the `apps/web`/`apps/shared`
crates; F2 Groups, F3 Agenda, F4 Stocks, F5 Recipes, F6 Grocery list, F7 Budget,
F8 Messagerie followed. Each shipped a full spec at F1's depth (route table,
per-page error tables, acceptance criteria), `apps/shared` DTOs mirroring
`apps/api` request/response shapes, TDD unit tests for pure validation/formatting
logic, and a Playwright E2E merge-gate suite. F7 wired the price-on-checkout hook
F6 deferred (inline price form on a checked grocery item →
`POST /grocery-list/:id/price`); F8 added the first browser→apps/api
**WebSocket** (the family chat thread, live updates as progressive enhancement
over a no-JS-first SSR page) and, with it, landed the live-updates E2E journey
backend epic #7 had deferred. F9 (User admin) is independent of the family-scoped
epics — the superadmin support screens (`/admin/groups`, `/admin/users`,
deactivate), gated client-side by a `CurrentSuperAdmin` extractor over the
backend's `SuperAdminUser` authority; its only backend change was adding
`is_superadmin` to `GET /auth/me`. F10 (RGPD self-service) closed the pass with
the account hub, the Art. 20 export download, the grace-period deletion flow with
its owner-of-groups guidance, and the public privacy-policy page — landing the
account-deletion UI F1 had carved out and flagged as required before v1 ships.

F11 (Google Calendar import UI) closed the last unfiled front surface: backend
epic #9 had shipped with no UI at all, so a family could only wire a calendar in
by hand-crafting HTTP calls. It changes no API surface — it is the client for the
four `/groups/:id/calendar-imports` endpoints, built around making the
*pull-on-demand, one-way* model legible and around treating the ICS feed URL as
the bearer credential it is.

**Remaining for v1:** #11 (transactional email finish-up) and #14 (flesh out
docker-compose/Caddy). #13 (CI) already covers the whole Rust workspace.

**Playwright policy (2026-07-09):** every front epic's PR must ship a
Playwright E2E suite (TypeScript, driving the built `apps/web` app
against a real running stack) covering the user journeys that epic
introduces, and CI must run that suite as a merge gate — not a
follow-up task. F1 (#15) already has this in its acceptance criteria
(#11) and testing plan; F2-F10 (#17-#25) placeholders were updated
2026-07-09 to carry the same requirement so it isn't dropped when each
gets its full spec pass. Suites shipped so far, all run by `ci.yml`'s
`e2e` job as a merge gate:

- F1 (#15) — `e2e/tests/auth.spec.ts` (register/verify/login/logout,
  forgot/reset password, auth-gate redirects; Google OAuth skipped, no
  test provider).
- F2 (#17) — `e2e/tests/groups.spec.ts` (create group, join via
  invitation link incl. single-use/unknown/garbage-token errors,
  owner/admin permission bar, role change + member removal, rename,
  leave incl. owner-successor and last-member-409 paths, ownership
  transfer, delete group, active-family switcher persistence).
- F3 (#18) — `e2e/tests/agenda.spec.ts` (create event visible in
  month + week views, unknown-event 404; recurring task with
  per-occurrence completion — completing one occurrence leaves the
  others to do; one-off task complete/un-complete; edit + delete;
  permission bar — a standard member gets no edit/delete on another
  member's event; attachment upload/list/presigned-download + delete,
  plus the client-side wrong-type rejection). This is the first suite
  that needs MinIO, so `ci.yml`'s `e2e` job now starts a real MinIO and
  creates the attachments bucket (see the job's comment). The RRULE v1
  subset and the file size/extension caps are unit-tested in
  `apps/shared` (`validation::agenda`); the E2E suite drives the picker
  and the extension rejection end-to-end.
- F4 (#19 + #39) — `e2e/tests/stocks.spec.ts` (create item + list with the
  low-stock badge shown exactly at/below the reorder threshold;
  `?low_stock=1` filter; full edit; quantity adjust by a **standard
  member** on an item they created; permission bar — a standard member
  **can adjust** the quantity of another member's item (shared inventory,
  #39) but gets no edit/delete controls on it; delete; unknown-item 404;
  server-side empty-name rejection). The `validate_item_form` /
  `is_low_stock` logic is unit-tested in `apps/shared`
  (`validation::stocks`); the E2E suite drives the screens end-to-end.
- F5 (#20) — `e2e/tests/recipes.spec.ts` (create a recipe with a
  seasonal + an optional ingredient via the one-per-line textarea;
  `/recipes` shows the ranked **Suggestions** section with a stock
  summary derived against `stock_items` and the missing-required-ingredient
  list under the grocery-list marker, plus the **Toutes les recettes**
  list; detail shows ingredient/optional/seasonal markers; log-a-meal
  records a meal and the detail reflects the last-cooked state; full edit
  round-trips through the ingredient textarea; delete returns to the list;
  permission bar — a standard member gets no edit/delete on another
  member's recipe **but can log a meal** against it; unknown-recipe 404;
  server-side empty-name rejection). The `parse_ingredients` /
  `format_ingredients` / `stock_summary` / permission-mirror logic is
  unit-tested in `apps/shared` (`validation::recipes`); the E2E suite
  drives the screens end-to-end.
- F6 (#21) — `e2e/tests/grocery-list.spec.ts` (add a manual item inline;
  generate pulls a recipe's missing required ingredient (`Recette` badge)
  and a low-stock item (`Stock bas` badge) onto the one shared list, and a
  re-run adds nothing — idempotence asserted on the `added=0` banner; check
  an item off and back on; edit round-trips through the item form; delete
  returns to the list; permission bar — a standard member gets no
  **Modifier** link and the forbidden page on the edit screen of another
  member's item **but can still check it off**; unknown-item 404;
  server-side empty-name rejection). The `validate_item_form` / `can_modify`
  mirror / `quantity_label` / `source_label` logic is unit-tested in
  `apps/shared` (`validation::grocery_list`); the E2E suite drives the
  screens end-to-end.
- F7 (#22) — `e2e/tests/budget.spec.ts` (add a manual price entry and see it
  in both the entry list and the monthly **Résumé mensuel** total; price a
  **checked** grocery item inline and see the resulting entry — with its
  `Courses` badge — plus the monthly total on `/budget`, and re-setting the
  price on the same item re-tarifies the single entry rather than
  double-counting the total — idempotent upsert asserted; edit round-trips
  through the entry form; delete returns to the list; permission bar — a
  standard member gets no **Modifier** link and the forbidden page on another
  member's entry **but can still add their own entry and set a price** on a
  checked grocery item; unknown-entry 404; server-side empty-name and
  negative-amount rejection). The `validate_entry_form` / `can_modify` mirror /
  `format_euros` / `amount_input_value` / `format_period` logic is unit-tested
  in `apps/shared` (`validation::budget`); the E2E suite drives the screens
  end-to-end.
- F8 (#23) — `e2e/tests/messagerie.spec.ts` (post a message and see it in the
  thread with author + Paris-local timestamp; **a message posted in one session
  appears live in a second session's already-open thread over the WebSocket,
  with no reload** — the journey backend epic #7 deferred here; author edits a
  message inline via the `<details>` disclosure and sees the `modifié` marker,
  and deletes it; permission bar — a standard member sees neither `Modifier` nor
  `Supprimer` on another member's message and a forged edit POST hits the
  forbidden page, **but can still post and edit their own**; `Charger les
  messages plus anciens` opens the cursor history window with a link back to the
  recent view; error states — server-side empty-content rejection preserves the
  typed text, and an unknown/deleted message id → "Message introuvable"). The
  `validate_content` / `can_modify` mirror / `format_message_time` /
  `clamp_limit` / `older_page_query` / `message_ws_url` / `author_name` logic is
  unit-tested in `apps/shared` (`validation::messagerie`); the E2E suite drives
  the screens end-to-end. This is the first suite exercising a browser→apps/api
  **WebSocket**, so `ci.yml`'s `e2e` job keeps `API_PUBLIC_BASE_URL`
  browser-reachable and `MESSAGE_ENCRYPTION_KEY` set (see the job's comment).
- F9 (#24) — `e2e/tests/admin.spec.ts` (a superadmin sees the `Admin` nav and
  lists every family and account across tenants — including families they are
  not a member of, the gated cross-tenant exception; the account status column
  reads `Actif`/`Désactivé`; deactivating a user from their detail page shows the
  success banner, revokes the target's session (their next page → `/login`) and
  flips the row to `Désactivé`; deactivation is terminal — no button on an
  already-deactivated account, and an unknown user id → "Utilisateur
  introuvable"; the gate — a non-superadmin sees no `Admin` link and is bounced
  to `/` from every `/admin/*` route, an unauthenticated visitor to `/login`).
  The `can_view_admin` / `user_status_label` / `can_deactivate` /
  `format_admin_datetime` logic is unit-tested in `apps/shared`
  (`validation::user_admin`); the suite promotes a fresh account to superadmin by
  flipping `is_superadmin` straight in Postgres (`e2e/lib/db.ts`'s
  `makeSuperadmin`), the same trust boundary as the token reads.
- F10 (#25) — `e2e/tests/rgpd.spec.ts` (the public `/privacy-policy` is reachable
  **without a session** from both the login and register footers and renders the
  shipped markdown as HTML — headings, the legal-basis table, no raw `**` left
  over — with a way back into the app for an anonymous visitor; the account
  routes are all auth-gated; the export downloads a **dated** JSON attachment
  whose contents carry the caller's profile, their family membership with its
  role, and stable-shaped empty categories; the sole owner of a family is blocked
  on deletion with the family named and the transfer-or-delete guidance, and the
  guidance link leads to that family's settings where deleting it clears the
  block; requesting deletion shows the pending panel with both the request
  datetime and the purge deadline, replaces the request entry point, leaves the
  session working, and is cancellable from `/account/delete` back to a normal
  hub; the confirmation guards — consent unticked, missing password, wrong
  password → the backend's 401 — and none of them records a request. The copy
  assertions pin deletion as *deferred* so it can't drift back into F9's
  immediate `deactivate`.) The `validate_deletion_confirmation` /
  `can_request_deletion` / `can_cancel_deletion` / `deletion_deadline` /
  `format_rgpd_datetime` / `export_filename` / `render_markdown` logic is
  unit-tested in `apps/shared` (`validation::rgpd`) — including a guard that runs
  the **real** `docs/privacy-policy.md` through the renderer; the E2E suite drives
  the screens end-to-end.
- F11 (#52) — `e2e/tests/google-calendar.spec.ts` (an admin connects a calendar
  and imports it, and the feed's events land in the month view; a re-import of
  the unchanged feed reports everything unchanged and duplicates nothing —
  UID-keyed idempotence made visible; a mutated feed, same UID with a bumped
  `LAST-MODIFIED`, updates the event in place and the old title is gone; a
  standard member sees neither the add nor the delete control and is bounced from
  those routes with the permission copy, but can still pull the feed; a
  bad-scheme URL is rejected on the form, an unreachable feed renders the
  `feed_fetch_failed` copy and a non-ICS body `invalid_ics`, an unknown
  connection id reports not-found; deleting a connection removes it while its
  already-imported events stay in the agenda — the confirmation's promise; and
  two tests pin the credential handling: a failed submit leaves the feed-URL
  field empty with the token nowhere in the HTML or the URL, and a failing import
  never leaks the address it tried). The `validate_import_form` / `can_configure`
  / `format_last_imported` / `import_run_summary` logic is unit-tested in
  `apps/shared` (`validation::google_calendar`). **No Google dependency and no
  network egress:** the suite starts its own static ICS server inside the
  Playwright process (`e2e/lib/ics-server.ts`) — `validate_feed_url` accepts
  `http://` alongside `https://` precisely so a loopback feed works without TLS.
  apps/api is the client of that server, not the browser, so the advertised host
  is `ICS_FIXTURE_HOST` (default `127.0.0.1`, correct on the CI runner where both
  processes are local). No new CI service or secret —
  `CALENDAR_FEED_ENCRYPTION_KEY` was already set.

**Note (2026-07-08):** the fridge-scan epic (out of v1, row above) is now
also the designated trigger for the Version Y microservices/Kubernetes
trajectory — see `architecture.md` § "Version Y" and
`docs/version-y-microservices.md`. Its spec, when it happens, should
account for that (Ollama extraction as a separate service).
