# Front epic F6 — Grocery list (issue #21)

SSR spec at the depth F1 (#15) established: route table, error tables
mapping `apps/api/src/grocery_list/`'s exact status/error codes to French
copy, and acceptance criteria. Backend is already shipped
(`apps/api/src/grocery_list/`, migration `0006_grocery_list.sql`, v1-scope
row #5) — this epic is the `apps/web` client only, following the
`apps/web/src/routes/recipes/` and `apps/web/src/routes/stocks/` pattern
exactly (Leptos SSR, `<form method=post>`, PRG with `?notice=`/`?error=`
codes, `FamilyContext`/`family_context`, per-page error tables, `can_modify`
mirrored front-side for control visibility — the backend stays the
authority).

Depends on F4 (Stocks) and F5 (Recipes): both feed the `generate` endpoint
— it pulls recipes' missing required ingredients (not currently in stock)
and stock items at/below their reorder threshold into the one shared list.

## Design decisions

### One shared list per family, three sources

There is a single grocery list per family (`grocery_items` scoped by
`group_id`), **not** a list per user. Every member reads and writes the same
list. Each row carries a `source` (`manual` / `recipe` / `low_stock`)
recording which of the three sources created it — purely informational once
created (all three behave identically). The UI shows a small provenance
badge on generated rows (`Recette` / `Stock bas`) so a member understands
where an auto-added item came from; manual rows carry no badge.

### Manual add is inline on the list (no separate `/new` page)

Unlike Recipes/Stocks (which have a dedicated `/…/new` form page), grocery
entry is a lightweight, high-frequency action, so the manual add form is
rendered **inline** at the top of `/grocery-list` (name + optional
quantity + optional unit), posting to `POST /grocery-list/add`. This
matches the issue's "ajout manuel inline" and keeps the round trip to a
single screen. Only the name is required; quantity and unit are optional
(mirrors the backend's `CreateGroceryItemRequest`, where both are
`Option`).

### Generate is idempotent — safe re-run, no confirmation

The "Générer depuis les recettes et les stocks" button posts to
`POST /grocery-list/generate`, which the backend deduplicates by name
(case-insensitive, trimmed) against existing **unchecked** items, so
re-running it never creates duplicates. Because it is idempotent, the UI
offers it as a plain button with **no confirmation dialog** and a
re-run-safe success banner that reports how many items were added
(`?notice=generated&added=N`). Any member may trigger it (same permission
bar as manual add).

### Check-off — no-JS SSR checkbox

The list renders each item as a checkbox ("cases à cocher"), but `apps/web`
ships no client hydration bundle, so a bare `<input type=checkbox>` cannot
submit on its own. Each row is therefore a small `<form method=post>` to
`POST /grocery-list/:id/check` carrying a hidden `checked` field set to the
**target** (toggled) state, with:

- a real `<input type=checkbox>` reflecting the current state that
  auto-submits on change (`onchange="this.form.submit()"`) — progressive
  enhancement, same PE approach as the password-visibility toggle in
  `app.rs`; and
- an always-present submit button (`Cocher` / `Décocher`) that works with
  JS disabled and is the deterministic E2E target.

The backend orders the list `checked, name`, so checked items sink to the
bottom; the UI strikes them through. Any member may check/uncheck any item
(lighter bar than edit — see the backend's `check_grocery_item` note).

### Edit/delete on a per-item screen, behind `can_modify`

`/grocery-list/:item_id` is the item's edit screen (name / quantity / unit)
with a delete action, both reserved for the item's **creator or a group
admin/owner** (`can_modify`, mirrored from
`apps/api/src/grocery_list/mod.rs`). A non-permitted member GETting it sees
the forbidden page; on the list, the per-row "Modifier" link renders only
for items they may modify. The backend stays the authority — a forged
PATCH/DELETE is still `403`'d and mapped defensively. Editing uses the
backend's double-`Option` PATCH contract: a blank quantity/unit field
**clears** it (`Some(None)`), a filled one sets it.

### Budget (F7) price-on-checkout hook — out of scope here

Checking an item off is the entry point of the Budget epic's
price-on-checkout flow (`POST /groups/:id/grocery-items/:item_id/price`,
which associates a bought price with a checked item). That price action —
and any `/budget` screen — belongs to **F7 (#22)** and is **out of scope**
for F6, exactly as F5 carried the `missing_ingredients` marker but left the
`generate` action to F6. F6 ships the check-off itself (the trigger); it
renders **no** price field and **no** dead link to a budget screen that
doesn't exist yet. The backend `price` endpoint already exists; F7 will add
the UI.

### Out of scope

The Budget price-on-checkout action and any `/budget` screen (F7, #22).
Categorised/aisle-grouped lists, quantity aggregation across sources, and
any barcode/scan capture (post-v1, not in the backend).

## Route table (`apps/web`)

| Method | Path | Handler | Purpose | API call(s) |
|---|---|---|---|---|
| GET | `/grocery-list` | `grocery_list::list::get` | Shared list (unchecked then checked), inline manual-add form, generate button | `GET /groups/:id/grocery-items` |
| POST | `/grocery-list/add` | `grocery_list::list::add` | Add a manual item | `POST /groups/:id/grocery-items` |
| POST | `/grocery-list/generate` | `grocery_list::list::generate` | Generate from recipes + stocks (idempotent) | `POST /groups/:id/grocery-items/generate` |
| POST | `/grocery-list/:id/check` | `grocery_list::list::check` | Check/uncheck an item (any member) | `POST /groups/:id/grocery-items/:item_id/check` |
| GET | `/grocery-list/:id` | `grocery_list::edit::get` | Item edit screen (creator/admin/owner only) | `GET /groups/:id/grocery-items/:item_id` |
| POST | `/grocery-list/:id/edit` | `grocery_list::edit::post` | Update name/quantity/unit | `PATCH /groups/:id/grocery-items/:item_id` |
| POST | `/grocery-list/:id/delete` | `grocery_list::edit::delete` | Delete an item (creator/admin/owner only) | `DELETE /groups/:id/grocery-items/:item_id` |

No active family → every route redirects to `/groups/new` (same as
Stocks/Recipes).

## Error tables (per page)

Backend codes are `apps/api/src/error.rs` bodies (`{"error": "<code>"}`);
each row is the exact `(status, code)` → French UI state.

### `/grocery-list` (list) — `list_grocery_items`

| Status | UI |
|---|---|
| 200 | render list + inline add + generate button |
| other (403 non-member — unreachable once family resolved) | render empty, no JSON leak |
| transport error | service-unavailable page |

### `POST /grocery-list/add` (manual add) — `create_grocery_item`

| Status | Code | UI |
|---|---|---|
| 201 | — | PRG → `/grocery-list?notice=item_added` |
| 400 | `name_required` | PRG → `/grocery-list?error=name_required` (inline banner) |
| 400 | `quantity_must_be_non_negative` | PRG → `/grocery-list?error=quantity_must_be_non_negative` |
| 403 | `forbidden` | PRG → `/grocery-list?error=forbidden` (not reachable via UI — any member may add; mapped defensively) |
| — | transport / other | PRG → `/grocery-list?error=unavailable` |

Name (non-empty) and quantity (non-negative) are pre-validated by the shared
`validate_item_form` (no round trip when they fail); the backend 400s are
the defensive fallback.

### `POST /grocery-list/generate` — `generate_grocery_items`

| Status | Code | UI |
|---|---|---|
| 201 | — | PRG → `/grocery-list?notice=generated&added=N` (N = items created; `0` → "aucun nouvel article") |
| 403 | `forbidden` | PRG → `/grocery-list?error=forbidden` (defensive) |
| — | transport / other | PRG → `/grocery-list?error=unavailable` |

Idempotent: re-running when nothing new qualifies returns `201` with an
empty `items` list → the banner reads "Aucun nouvel article à ajouter."

### `POST /grocery-list/:id/check` — `check_grocery_item`

| Status | Code | UI |
|---|---|---|
| 200 | — | PRG → `/grocery-list?notice=item_checked` / `item_unchecked` |
| 404 | `not_found` | "Article introuvable" page |
| 403 | `forbidden` | PRG → `/grocery-list?error=forbidden` (defensive — any member may check) |
| — | transport / other | PRG → `/grocery-list?error=unavailable` |

### `GET /grocery-list/:id` (edit screen) — `get_grocery_item`

| Status | Code | UI |
|---|---|---|
| 200 | — | edit form (only if `can_modify`, else forbidden page) |
| 404 | `not_found` | "Article introuvable" page |
| — | transport / parse | service-unavailable page |

`?error=` (`forbidden`, `unavailable`, `name_required`,
`quantity_must_be_non_negative`) banners rendered on load / after a failed
edit round trip.

### `POST /grocery-list/:id/edit` — `update_grocery_item`

| Status | Code | UI |
|---|---|---|
| 200 | — | PRG → `/grocery-list?notice=item_updated` |
| 400 | `name_required` | inline on the edit form |
| 400 | `quantity_must_be_non_negative` | inline on the edit form |
| 403 | `forbidden` | PRG → `/grocery-list/:id?error=forbidden` |
| 404 | `not_found` | "Article introuvable" page |
| — | transport / other | inline "Service momentanément indisponible…" |

GET renders the form only for `can_modify` (creator/admin/owner); a
non-permitted user GETting it gets the forbidden page. Name + quantity are
pre-validated by the shared `validate_item_form`.

### `POST /grocery-list/:id/delete` — `delete_grocery_item`

| Status | Code | UI |
|---|---|---|
| 204 | — | PRG → `/grocery-list?notice=item_deleted` |
| 403 | `forbidden` | forbidden page |
| 404 | `not_found` | "Article introuvable" page |
| — | transport / other | service-unavailable page |

## Permission bar (front mirror of `grocery_list::can_modify`)

Backend bar (`apps/api/src/grocery_list/mod.rs::can_modify`): the item's
creator, or a group **owner/admin**, may edit (name/quantity/unit) or delete
it; **any** member may create, read, or **check off** an item. The front
mirrors it (`manage_our_home_shared::validation::grocery_list::can_modify`,
unit-tested, re-exported by `routes/grocery_list`):

- The inline add form, the generate button, and every row's check control
  render for **every** member.
- The per-row **Modifier** link and the edit screen's edit/delete controls
  render only when `can_modify(role, is_creator)`; a standard member sees
  no "Modifier" link on another member's item, and GETting its edit screen
  gets the forbidden page.

The backend stays the authority: a forged edit/delete is still `403`'d and
mapped defensively per the tables above.

## Acceptance criteria

1. A member can add an item manually inline (name + optional quantity/unit)
   and it appears on the shared list with a success banner.
2. "Générer depuis les recettes et les stocks" pulls a recipe's missing
   required ingredient and a low-stock item onto the list; re-running it
   adds nothing (idempotent) and the banner says so.
3. Checking an item off marks it done (strikethrough, sunk to the bottom);
   unchecking restores it. Any member can check any item.
4. The item's creator (or an admin/owner) can edit its name/quantity/unit
   (round-trips through the edit form) and delete it, returning to the list
   with a banner.
5. A standard member gets no "Modifier" link on another member's item and
   the forbidden page if they GET its edit screen — but **can** still check
   it off.
6. Documented error states render their exact French copy: server-side
   empty-name rejection on add, and unknown-item 404. The pure
   `validate_item_form` / `can_modify` / formatting logic is unit-tested in
   `apps/shared` (`validation::grocery_list`); the E2E suite
   (`e2e/tests/grocery-list.spec.ts`) drives every journey above end-to-end
   against the real stack as a CI merge gate.

## Cross-epic note — Budget (F7)

Checking an item off is the **trigger** of F7's price-on-checkout flow, but
the price action (`POST /groups/:id/grocery-items/:item_id/price`) and any
`/budget` screen are out of scope here (see the "Budget hook" decision
above). F6 renders the check-off; F7 adds the price capture on top of it.
