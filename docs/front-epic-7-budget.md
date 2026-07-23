# Front epic F7 — Budget (issue #22)

SSR spec at the depth F1 (#15) established: route table, error tables mapping
`apps/api/src/budget/`'s exact status/error codes to French copy, and
acceptance criteria. Backend is already shipped (`apps/api/src/budget/`,
migration `0007_budget.sql`, v1-scope row #6) — this epic is the `apps/web`
client only, following the `apps/web/src/routes/grocery_list/*` and
`apps/web/src/routes/recipes/*` pattern exactly (Leptos SSR, plain
`<form method=post>`, PRG with `?notice=`/`?error=` codes,
`FamilyContext`/`family_context`, per-page error tables, `can_modify`
mirrored front-side for control visibility — the backend stays the authority).

Depends on F6 (Grocery list): Budget is **tied to the grocery list**, not a
standalone expense tracker. The check-off F6 ships is the entry point of the
price-on-checkout flow this epic finally wires (`POST
/groups/:id/grocery-items/:item_id/price`), exactly as F6 promised (it left the
price action to F7, the same way F5 left `generate` to F6).

## Design decisions

### Strict scope — grocery-derived spend only, manual price entry

Budget in v1 is **not** a general-purpose expense tracker. Every entry is a
price attached to something the family buys: either a **manual price entry**
(`POST /groups/:id/budget-entries`, name + amount + optional date) or a price
**set on a checked grocery item** at checkout (`POST
/groups/:id/grocery-items/:item_id/price`). There is no category, no payee, no
recurring-expense modelling, no local price lookup (out of v1, see
`docs/v1-scope.md`). The `/budget/new` form is deliberately minimal (name,
amount, date) and never sends `grocery_item_id` — the only path that links an
entry to a grocery item is the checkout action on the grocery list.

### Euros at the boundary, cents in the DB — formatting is pure and tested

The API speaks **euros** as `f64` (`amount`), while the backend stores
`amount_cents` (BIGINT) so the monthly summary can sum without binary
floating-point drift (`apps/api/src/budget/entries.rs::to_cents`). The front
never does money maths — it only *displays* the euro `f64` the API returns and
*parses* the euro `f64` a form submits. Display goes through the pure,
unit-tested `format_euros` (two decimals, French comma + `€`, e.g.
`"12,50 €"`); number-input pre-fill goes through `amount_input_value` (two
decimals, dot, e.g. `"12.50"`, valid for `<input type=number>`). Both live in
`apps/shared/src/validation/budget.rs` so the UI and the fixed backend can't
drift on rounding.

### Monthly summary is read-only and computed server-side

`GET /groups/:id/budget-entries/summary` returns one row per month (`{ period:
first-of-month, total: euros }`, newest first), computed on read via
`date_trunc('month', spent_at)` — never stored, same as Stocks' derived
low-stock. `/budget` renders it as a **Résumé mensuel** section above the entry
list; the period label goes through the pure `format_period` (French month +
year, e.g. `"juillet 2026"`). The front adds no client-side aggregation.

### Price-on-checkout is inline on the grocery list, no new route needed

The price-capture UI lives **inline on `/grocery-list`**, on each **checked**
item's row — an amount field + a `Renseigner le prix` button posting to a new
`POST /grocery-list/:id/price` handler (which relays to the backend's
`POST /groups/:id/grocery-items/:item_id/price`). No dedicated `/grocery-list/:id/price`
GET screen: the checkout action is a one-field, high-frequency gesture, so an
inline form (same lightweight approach as F6's inline manual-add) beats a
full page round-trip. The field appears only once an item is checked (bought),
matching the "prix au checkout" framing. Any member may set a price (same bar
as the check-off itself), so it renders for everyone; the backend upsert makes
a re-tap idempotent (re-tarifies, never double-counts — see below), so it needs
no confirmation and stays visible even after a price is set. F7 also removes the
"Budget hook out of scope" note F6 left in `routes/grocery_list/mod.rs`, since
the hook is now wired.

### Set-price is idempotent (upsert); manual create can 409

Two endpoints create a `BudgetEntryResponse`, with deliberately different
conflict semantics that the UI mirrors:

- `POST …/grocery-items/:item_id/price` **upserts** on the item's
  `grocery_item_id` — a double-tap re-tarifies the same entry rather than
  creating a second, double-counted one. So the inline price form has no
  "already priced" error state; re-submitting just updates.
- `POST /budget-entries` with a `grocery_item_id` already priced returns **409
  `grocery_item_already_priced`**. The `/budget/new` form never sends a
  `grocery_item_id`, so this is unreachable from the UI — it's mapped
  defensively only.

### Edit/delete on a per-entry screen, behind `can_modify`

`/budget/:entry_id` is the entry's edit screen (name / amount / date) with a
delete action, both reserved for the entry's **creator or a group admin/owner**
(`can_modify`, mirrored from `apps/api/src/budget/mod.rs`). A non-permitted user
GETting it sees the forbidden page; on the list, the per-row `Modifier` link
renders only for entries they may modify. The backend stays the authority — a
forged PATCH/DELETE is still `403`'d and mapped defensively. The PATCH contract
has **no double-`Option`**: `spent_at` is `NOT NULL` in the DB, so the update
request's `spent_at` is a plain `Option` meaning "leave as-is" only (no clear).
The edit form always sends every field, so a round trip preserves the date.

### Out of scope

Categorised/tagged expenses, budgets/limits/alerts, recurring expenses, any
payee/account modelling, charts, and local price lookup (all post-v1 or
explicitly out of v1 per `docs/v1-scope.md`). Budget stays strictly
grocery-derived.

## Route table (`apps/web`)

| Method | Path | Handler | Purpose | API call(s) |
|---|---|---|---|---|
| GET | `/budget` | `budget::list::get` | Monthly summary + entry list; per-row `Modifier` for `can_modify` | `GET /groups/:id/budget-entries/summary`, `GET /groups/:id/budget-entries` |
| GET | `/budget/new` | `budget::new::get` | Manual price-entry form | — |
| POST | `/budget/new` | `budget::new::post` | Create a manual entry | `POST /groups/:id/budget-entries` |
| GET | `/budget/:id` | `budget::edit::get` | Entry edit screen (creator/admin/owner only) | `GET /groups/:id/budget-entries/:entry_id` |
| POST | `/budget/:id/edit` | `budget::edit::post` | Update name/amount/date | `PATCH /groups/:id/budget-entries/:entry_id` |
| POST | `/budget/:id/delete` | `budget::edit::delete` | Delete an entry (creator/admin/owner only) | `DELETE /groups/:id/budget-entries/:entry_id` |
| POST | `/grocery-list/:id/price` | `grocery_list::list::price` | Set a price on a checked grocery item (any member) | `POST /groups/:id/grocery-items/:item_id/price` |

No active family → every route redirects to `/groups/new` (same as
Grocery list / Stocks / Recipes).

## Error tables (per page)

Backend codes are `apps/api/src/error.rs` bodies (`{"error": "<code>"}`); each
row is the exact `(status, code)` → French UI state.

### `/budget` (list) — `list_budget_entries` + `budget_summary`

| Status | UI |
|---|---|
| 200 / 200 | render Résumé mensuel + entry list + `Ajouter une dépense` link |
| other (403 non-member — unreachable once family resolved) | render empty section, no JSON leak |
| transport error | service-unavailable page |

### `POST /budget/new` (manual entry) — `create_budget_entry`

| Status | Code | UI |
|---|---|---|
| 201 | — | PRG → `/budget?notice=entry_created` |
| 400 | `name_required` | inline on the form |
| 400 | `amount_must_be_non_negative` | inline on the form |
| 409 | `grocery_item_already_priced` | inline (unreachable — the form sends no `grocery_item_id`; mapped defensively) |
| 403 | `forbidden` | forbidden page (unreachable — any member may create; defensive) |
| 404 | `not_found` | inline (unreachable — the form sends no `grocery_item_id`; defensive) |
| — | transport / other | inline "Service momentanément indisponible…" |

Name (non-empty) and amount (finite, non-negative) are pre-validated by the
shared `validate_entry_form` (no round trip when they fail); the backend 400s
are the defensive fallback.

### `GET /budget/:id` (edit screen) — `get_budget_entry`

| Status | Code | UI |
|---|---|---|
| 200 | — | edit form (only if `can_modify`, else forbidden page) |
| 404 | `not_found` | "Dépense introuvable" page |
| — | transport / parse | service-unavailable page |

### `POST /budget/:id/edit` — `update_budget_entry`

| Status | Code | UI |
|---|---|---|
| 200 | — | PRG → `/budget?notice=entry_updated` |
| 400 | `name_required` | inline on the edit form |
| 400 | `amount_must_be_non_negative` | inline on the edit form |
| 403 | `forbidden` | PRG → `/budget/:id?error=forbidden` |
| 404 | `not_found` | "Dépense introuvable" page |
| — | transport / other | inline "Service momentanément indisponible…" |

GET renders the form only for `can_modify` (creator/admin/owner); a
non-permitted user GETting it gets the forbidden page. Name + amount are
pre-validated by the shared `validate_entry_form`.

### `POST /budget/:id/delete` — `delete_budget_entry`

| Status | Code | UI |
|---|---|---|
| 204 | — | PRG → `/budget?notice=entry_deleted` |
| 403 | `forbidden` | forbidden page |
| 404 | `not_found` | "Dépense introuvable" page |
| — | transport / other | service-unavailable page |

### `POST /grocery-list/:id/price` (price-on-checkout) — `set_grocery_item_price`

| Status | Code | UI |
|---|---|---|
| 201 | — | PRG → `/grocery-list?notice=price_set` (upsert — idempotent, a re-tap re-tarifies) |
| 400 | `amount_must_be_non_negative` | PRG → `/grocery-list?error=amount_must_be_non_negative` |
| 404 | `not_found` | "Article introuvable" page |
| 403 | `forbidden` | PRG → `/grocery-list?error=forbidden` (unreachable — any member may set a price; defensive) |
| — | transport / other | PRG → `/grocery-list?error=unavailable` |

Amount (finite, non-negative) is pre-validated by the shared
`validate_entry_form` reused with a placeholder name (only the amount arm can
fire here — the price form has no name field); the backend 400 is the
defensive fallback.

## Permission bar (front mirror of `budget::can_modify`)

Backend bar (`apps/api/src/budget/mod.rs::can_modify`): the entry's creator, or
a group **owner/admin**, may edit (name/amount/date) or delete it; **any**
member may create an entry, read entries/summary, or **set a price** on a
grocery item. The front mirrors it
(`manage_our_home_shared::validation::budget::can_modify`, unit-tested,
re-exported by `routes/budget`):

- The `Ajouter une dépense` link, the `/budget/new` form, the monthly summary,
  the entry list, and the grocery-list price form render for **every** member.
- The per-row `Modifier` link and the edit screen's edit/delete controls render
  only when `can_modify(role, is_creator)`; a standard member sees no
  `Modifier` link on another member's entry, and GETting its edit screen gets
  the forbidden page.

The backend stays the authority: a forged edit/delete is still `403`'d and
mapped defensively per the tables above.

## Acceptance criteria

1. A member can add a manual price entry (`/budget/new`, name + amount +
   optional date) and it appears on `/budget` in both the entry list and the
   monthly **Résumé mensuel** total, with a success banner.
2. From `/grocery-list`, checking an item off reveals an inline `Renseigner le
   prix` field; setting a price records a budget entry that shows up on
   `/budget` (list + monthly summary). Re-setting the price on the same item
   re-tarifies the single entry (idempotent upsert) — the monthly total does
   not double-count.
3. The entry's creator (or an admin/owner) can edit its name/amount/date
   (round-trips through the edit form) and delete it, returning to `/budget`
   with a banner.
4. A standard member gets no `Modifier` link on another member's entry and the
   forbidden page if they GET its edit screen — but **can** still set a price on
   a grocery item and add their own manual entry.
5. Documented error states render their exact French copy: server-side
   empty-name rejection and negative-amount rejection on add, and unknown-entry
   404. The pure `validate_entry_form` / `can_modify` / `format_euros` /
   `amount_input_value` / `format_period` logic is unit-tested in `apps/shared`
   (`validation::budget`); the E2E suite (`e2e/tests/budget.spec.ts`) drives
   every journey above end-to-end against the real stack as a CI merge gate.

## Cross-epic note — Grocery list (F6)

F7 is where F6's deferred price-on-checkout hook lands. F6 ships the check-off
(the trigger, no price field); F7 adds the inline price capture on top of it and
the whole `/budget` surface. The grocery-list check-off behaviour itself is
unchanged — the price form is purely additive, rendered only on already-checked
rows.
