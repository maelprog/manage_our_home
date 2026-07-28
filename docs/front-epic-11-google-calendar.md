# Front epic F11 — Google Calendar import UI (issue #52)

SSR spec at the depth F1 (#15) established: route table, error tables mapping
the exact status codes of `apps/api/src/google_calendar/` to French copy, and
acceptance criteria. The backend is already shipped (backend epic #9, v1-scope
row #9: `apps/api/src/google_calendar/`, migration
`0010_google_calendar_import.sql`) and had **no UI at all** — a family could only
wire a calendar in by hand-crafting HTTP calls. This epic is the `apps/web`
client only, and changes no API surface: same pattern as every other front epic
(Leptos SSR, plain `<form method=post>`, PRG with `?notice=`/`?error=` codes,
per-page error tables, pure logic mirrored in `apps/shared` and TDD'd).

Depends on F1 (Auth) for the session, F2 (Groups) for the active-family cookie
and the role in `GroupSummary`, and F3 (Agenda) for `family_context`, the
Europe/Paris display convention, and the month/week views the imported events
show up in. It closes the front pass: F1–F10 were already done, this was the last
unfiled front surface for v1.

## Design decisions

### The screens have to make "pull-on-demand" legible

Import is one-way (Google → us) and pulled **only when someone presses the
button**: a per-calendar private ICS feed URL, not OAuth2 + the Calendar API. No
background polling, no webhooks, and Google's own ICS cache means the feed itself
lags by up to a few hours. That is a deliberate v1 tradeoff (see `v1-scope.md`
row 9) — but it is also exactly the kind of thing a UI can silently misrepresent,
because "connect your Google Calendar" reads as "sync" to everyone.

So a single `MODEL_EXPLAINER` paragraph states it — à sens unique, à la demande,
Google met son adresse en cache — and is rendered on both the list and the
connect form. The E2E suite asserts that copy is on the page, so it can't quietly
disappear. The list's "Dernier import" column reinforces it: an explicit
`jamais importé` for a connection nobody has pulled yet, rather than a blank
cell.

### Placed under `/agenda/*`, cross-linked from the family settings

The artefact this produces is agenda data, and the read/trigger bar is "any
member" like the rest of Agenda, so the screens live at `/agenda/imports` and
`routes/agenda/imports.rs`, reusing `family_context` and the Agenda shared
helpers. But "connecter un agenda Google" is also the kind of thing people go
looking for in the family settings, so `/groups/:id/settings` carries a
cross-link section (a link, no controls — one implementation, one place).

### The feed URL is a credential, and is treated as one end to end

`feed_url` is a bearer token: anyone holding it can read that calendar, with no
password. The backend already refuses to hand it back (`CalendarImportResponse`
has no `feed_url` field — "write-only once stored", same principle as never
returning a password hash). The front upholds the same property:

- it is submitted **once**, in a POST body — never in a query string, never in a
  PRG parameter, never logged;
- the input is `type="password"` with `autocomplete="off"` and
  `spellcheck="false"`;
- after a validation error the form **re-asks** for it rather than re-rendering
  it into a `value=` attribute. The label, which is not a secret, is preserved;
- `apps/api`'s `feed_fetch_failed: {reqwest error}` interpolates an error whose
  `Display` can embed the URL it was given, so `import_error_code` matches on the
  code *prefix* and drops the tail. The `feed_fetch_failed` copy is a fixed
  string; nothing from the backend's message reaches the page.

Two E2E tests pin this: a failed submit must leave `input[name=feed_url]` empty
and the token nowhere in the HTML or the URL, and a failing import must not leak
the address it tried.

The connect form also warns, before the field, that the address grants read
access to that calendar to anyone holding it and cannot be re-displayed
afterwards — a user who pastes the wrong link should know that the recovery path
is "reset it in Google Agenda", not "edit it here".

### The stricter permission bar, mirrored

Unlike every other family-scoped epic, `apps/api` puts creating and deleting a
connection behind **admin/owner** (`can_configure`), because the feed URL is a
credential for a member's personal Google account rather than household data.
Listing connections and triggering an import stay on the normal any-member bar.

`validation::google_calendar::can_configure` mirrors it, in the same spirit as
F9's `can_view_admin`: it decides whether the "Ajouter" button and the per-row
"Supprimer" link render, and the `new`/`delete` handlers bounce a standard member
to `/agenda/imports?error=forbidden` rather than showing a form they cannot
submit. The backend stays the authority — a forged POST is still 403'd, and the
403 is mapped defensively.

### The delete confirmation states two consequences the schema makes non-obvious

Verified against `0010_google_calendar_import.sql`: `calendar_import_events`
cascades from `calendar_imports`, but `events` does not. So:

1. **Already-imported events stay in the agenda.** Deleting a connection drops
   only the UID→event mapping rows; the `events` rows survive and behave like any
   manual event. The intuitive reading is the opposite ("removing the connection
   removes its events"), so the confirmation says it outright — and the
   post-delete banner repeats it, because that is the moment the user finds out.
2. **Re-adding the same calendar re-imports everything as duplicates.** With the
   mapping gone, the next import has no UID to match on and inserts fresh rows
   next to the old ones.

Consequence 2 is also *why* there is no `PATCH`: changing a label or a URL means
delete + recreate, which means a full re-import. The confirmation steers anyone
who only wanted to fix the name toward that fact before they click.

Both are v1 behaviour to **surface**, not to fix here. If we would rather offer
"supprimer aussi les événements importés", that is a separate issue with a
backend change behind it.

### There is no in-place edit, and no `/agenda/imports/:id` detail page

No backend `PATCH`, so no edit screen. And no single-import `GET` either, so the
delete confirmation resolves its connection out of the list — the same approach
F9's user detail page takes for the same reason.

### Out of scope

- Bidirectional sync, background/scheduled polling, webhooks (out of v1 per
  `architecture.md`).
- Editing a connection in place (no backend `PATCH`; delete + recreate is the
  documented path).
- Deleting the events an import produced when the connection is removed (see
  consequence 1 — file separately if wanted).
- Expanding bare `RRULE`s from the feed (documented v1 backend limitation:
  Google already expands `RECURRENCE-ID` overrides into separate VEVENTs, so a
  plain RRULE VEVENT imports as its first occurrence only).

## Route table (`apps/web`)

| Method | Path | Handler | Bar | Purpose | API call(s) |
|---|---|---|---|---|---|
| GET | `/agenda/imports` | `agenda::imports::get` | any member | Connections table (label, dernier import, ajouté par/le) + per-row actions, or the empty state | `GET /groups/:gid/calendar-imports`, `GET /groups/:gid` (member names) |
| GET | `/agenda/imports/new` | `agenda::imports::new_get` | **admin/owner** | Connect form + where-to-find-the-address help + the credential warning | — |
| POST | `/agenda/imports` | `agenda::imports::create` | **admin/owner** | Creates the connection | `POST /groups/:gid/calendar-imports` |
| POST | `/agenda/imports/:id/import` | `agenda::imports::run` | any member | Pulls the feed now | `POST /groups/:gid/calendar-imports/:id/import` |
| GET | `/agenda/imports/:id/delete` | `agenda::imports::delete_get` | **admin/owner** | Confirmation page (the two consequences) | `GET /groups/:gid/calendar-imports` |
| POST | `/agenda/imports/:id/delete` | `agenda::imports::delete_post` | **admin/owner** | Removes the connection | `DELETE /groups/:gid/calendar-imports/:id` |

Every route is gated by `CurrentUser` (no session → `/login`) and by
`family_context` (no active family → `/groups/new`), the F1/F3 patterns. The
`Agendas Google` button sits in the `/agenda` navigation row next to
`Nouvel événement`; `/groups/:id/settings` carries the cross-link.

## Error tables (per page)

Backend codes are `apps/api/src/error.rs` bodies (`{"error": "<code>"}`); each
row is the exact `(status, code)` → French UI state.

### `POST /agenda/imports` — `imports::create_calendar_import`

| Status | Code | UI |
|---|---|---|
| — | `label_required` (local) | "Donnez un nom à cet agenda.", nothing sent to the API |
| — | `feed_url_required` (local) | "L'adresse iCal est obligatoire.", nothing sent |
| — | `feed_url_must_be_http_or_https` (local) | "L'adresse doit commencer par https:// — copiez l'adresse secrète au format iCal depuis Google Agenda.", nothing sent |
| 201 | — | PRG → `/agenda/imports?notice=import_created` |
| 400 | any of the three above | same copy, re-rendered on the form (defensive: `validate_import_form` mirrors them) |
| 403 | `forbidden` | PRG → `/agenda/imports?error=forbidden` |
| — | other status / transport error | "Service momentanément indisponible…" on the form |

In every failure case the form is re-rendered with the **label** pre-filled and
the **feed URL blank**.

### `POST /agenda/imports/:id/import` — `imports::trigger_calendar_import`

| Status | Code | UI |
|---|---|---|
| 200 | — | PRG → `?notice=imported&imported=…&updated=…&skipped=…`, rendered as "Import terminé : 3 événements importés, 1 mis à jour, 12 inchangés." |
| 404 | `not_found` | PRG → `?error=not_found` — "Cet agenda connecté n'existe plus." |
| 422 | `feed_fetch_failed: …` | PRG → `?error=feed_fetch_failed` — "Google n'a pas répondu, ou l'adresse n'est plus valide…". **The interpolated tail is dropped**, it can contain the feed URL |
| 422 | `feed_too_large` | "Cet agenda dépasse la taille maximale acceptée (5 Mo)." |
| 422 | `invalid_ics` | "Le contenu récupéré n'est pas un agenda iCal valide." |
| 403 | `forbidden` | `?error=forbidden` (defensive — the bar here is any member) |
| — | other status / transport error | `?error=unavailable` |

### `GET`/`POST /agenda/imports/:id/delete` — `imports::delete_calendar_import`

| Status | Code | UI |
|---|---|---|
| — | not admin/owner (local) | PRG → `?error=forbidden`, nothing sent to the API |
| — | id absent from the list (GET) | PRG → `?error=not_found` |
| 204 | — | PRG → `?notice=import_deleted` — "Agenda Google retiré. Les événements déjà importés restent dans l'agenda." |
| 404 | `not_found` | `?error=not_found` (e.g. a double submit) |
| 403 | `forbidden` | `?error=forbidden` |
| — | other status / transport error | `?error=unavailable` |

### `GET /agenda/imports` — the list

| Status | Code | UI |
|---|---|---|
| 200 | — | the table, or the empty state explaining the feature |
| — | any `?notice=`/`?error=` above | the matching banner |
| — | list call fails | service-unavailable page (the group-detail call for member names does **not** fail the page — `author_name` falls back to "Membre") |

## `apps/shared`

`dto/google_calendar.rs` mirrors the backend's shapes rather than moving them
(the `user_admin.rs` convention; this epic changes no API surface):
`CreateCalendarImportRequest`, `CalendarImportResponse` — deliberately with **no**
`feed_url` field — the `{"imports": […]}` envelope `CalendarImportsResponse`, and
`ImportRunResponse`.

`validation/google_calendar.rs`, written test-first per `.claude/CLAUDE.md`:

| Function | Mirrors / does |
|---|---|
| `validate_import_form(label, feed_url)` | `create_calendar_import`'s three guards, in the backend's own order. Returns only the verdict — it never holds or copies the credential |
| `can_configure(role)` | `google_calendar::mod::can_configure` — the admin/owner bar |
| `format_last_imported(Option<DateTime<Utc>>)` | Europe/Paris (F3's `DISPLAY_TZ`), or `jamais importé` |
| `import_run_summary(imported, updated, skipped)` | The French pluralised sentence; zero and one take the singular |

`import_error_code` (the code-prefix mapper for the 422 bodies) stays in
`apps/web` with the rest of the error table and is unit-tested there, including a
case asserting a URL embedded in a `feed_fetch_failed` message never reaches the
copy.

## Acceptance criteria

1. `/agenda/imports` lists the active family's connections with the last-import
   time in Europe/Paris and an explicit "jamais importé" state.
2. An admin/owner can add a connection from the UI and it appears in the list; a
   standard member sees neither the add nor the delete control, and direct
   navigation to those routes is bounced with the permission copy.
3. Any member (including standard) can trigger "Importer maintenant" and gets the
   imported/updated/skipped counts back as a notice.
4. A second import of an unchanged feed reports everything as unchanged, and the
   month view gains no duplicate.
5. Imported events are visible in the F3 month/week views; a changed upstream
   event (bumped `LAST-MODIFIED`) is updated in place rather than added twice.
6. The delete confirmation states both consequences above; after deletion the
   connection is gone and the previously imported events are still in the agenda.
7. Every error code in the tables renders its mapped French copy, and the feed
   URL never appears in a URL, in a re-rendered form, or in an error message.
8. The pure `validate_import_form` / `can_configure` / `format_last_imported` /
   `import_run_summary` logic is unit-tested in `apps/shared`
   (`validation::google_calendar`), written test-first per `.claude/CLAUDE.md`;
   the E2E suite (`e2e/tests/google-calendar.spec.ts`) drives every journey above
   end-to-end against the real stack as a CI merge gate.

## Testing

`e2e/tests/google-calendar.spec.ts` runs as a merge gate in `ci.yml`'s `e2e` job,
per the Playwright policy in `v1-scope.md`.

**No Google dependency, no network egress.** `validate_feed_url` accepts `http://`
alongside `https://` precisely so a loopback feed can be used without TLS (its
doc comment says so), so the suite starts its own static ICS server inside the
Playwright process (`e2e/lib/ics-server.ts`) and points the connection at it. The
client of that server is **apps/api**, not the browser, so the advertised host is
`ICS_FIXTURE_HOST` (default `127.0.0.1` — correct on the CI runner, where both
processes are local; set to the Playwright container's name when the stack runs
on a docker network, see `e2e/README.md`). Ports are ephemeral and no new CI
service or secret is needed — `CALENDAR_FEED_ENCRYPTION_KEY` was already set.

Journeys covered: admin connects → imports → the fixture's events appear in the
month view; re-import of the unchanged fixture reports everything unchanged with
no duplicates; a mutated fixture (bumped `LAST-MODIFIED`, same UID) updates the
event in place and the old title is gone; a standard member sees no add/delete
controls, is bounced from those routes, but can pull the feed; a bad-scheme URL
is rejected on the form; a failed submit re-asks for the address and leaks
nothing; an unreachable feed and a non-ICS body render their mapped copy; an
unknown connection id reports not-found; and deleting a connection removes it
while leaving its already-imported events in the agenda.
