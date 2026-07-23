# Front epic F5 — Recipes (issue #20)

SSR spec at the depth F1 (#15) established: route table, error tables
mapping `apps/api/src/recipes/`'s exact status/error codes to French copy,
and acceptance criteria. Backend is already shipped (`apps/api/src/recipes/`,
migration `0005_recipes.sql`, v1-scope row #4) — this epic is the
`apps/web` client only, following the `apps/web/src/routes/stocks/` pattern
exactly (Leptos SSR, `<form method=post>`, PRG with `?notice=`/`?error=`
codes, `FamilyContext`/`family_context`, per-page error tables, `can_modify`
mirrored front-side for control visibility — the backend stays the
authority).

Depends on F4 (Stocks): the suggestion display is built around the
stock-match signal (`missing_ingredients`, matched/total ingredients), which
the backend derives name-based against `stock_items`.

## Design decisions

### Score: ranked order, not the raw number

The suggestion endpoint returns a raw `score` (`match_ratio * 100 −
recency_penalty + seasonal_bonus`). **We do not render that number.** It is
an internal heuristic that can be negative and means nothing to a family
user. Instead we render the *ranked order* the score produces, plus the
human-meaningful signals the score is built from, each surfaced as its own
badge/label derived by a pure function (`stock_summary`,
`recently_eaten`):

- **Stock match** — `stock_summary(matched, total)` → "Tous les ingrédients
  en stock" / "3/4 ingrédients en stock" / "Aucun ingrédient requis".
- **Variété** — a "Déjà cuisiné récemment" badge when `recently_eaten`
  (logged in `meal_history` within the backend's 14-day variety window),
  with the `last_eaten_on` date.
- **Ingrédients manquants** — the `missing_ingredients` list per suggestion
  (name + quantity/unit), which is exactly what feeds the grocery-list
  generate endpoint.

This mirrors Stocks, which derives a human "Stock bas" badge from
`low_stock` rather than printing the raw threshold arithmetic.

### `missing_ingredients` → grocery-list hook (F6, out of scope here)

Each suggestion lists its missing required ingredients under a marker
("À ajouter à la liste de courses") stating these ingredients feed the
family grocery list. The actual cross-epic wiring (a button that calls
`POST /groups/:id/grocery-items/generate`, or a link to a `/grocery`
screen) belongs to F6 (#21) and is **out of scope** for F5 — no `/grocery`
route exists yet, so we render an honest informational marker, not a dead
link. The backend already emits the structured `missing_ingredients` the
generate endpoint consumes; F6 will add the action.

### Out of scope

Any ML/Ollama suggestion swap (v1 is rules-only, per architecture.md and
the backend's own doc comment). Fridge-scan/OCR ingredient capture
(post-v1). The grocery-list generate action (F6).

### Ingredient entry: one-per-line textarea + pure parser

No-JS SSR, same as the rest of `apps/web` (no client bundle). Ingredients
are entered as a textarea, **one ingredient per line**, pipe-delimited
positional fields — parsed by the pure, TDD'd
`manage_our_home_shared::validation::recipes::parse_ingredients`:

```
Nom | quantité | unité | mois de saison | optionnel
```

- **Nom** (field 1) — required, trimmed. Empty line → skipped; a non-empty
  line with an empty name → `ingredient_name_required`.
- **Quantité** (field 2) — optional `f64`; empty → none; negative →
  `ingredient_quantity_must_be_non_negative`; unparseable →
  `ingredient_quantity_invalid` (front-only: cannot be sent as JSON).
- **Unité** (field 3) — optional free text.
- **Mois de saison** (field 4) — optional comma-separated month numbers
  `1..=12` (an optional `saison:` prefix is tolerated); out of range →
  `invalid_seasonal_month`.
- **Optionnel** (field 5) — `optionnel`/`facultatif`/`opt`/`o`/`oui`/`x`
  (case-insensitive) marks the ingredient optional (excluded from the
  required-ingredient stock match); anything else → required.

Trailing fields may be omitted (`Farine` = name only; `Farine | 2 | kg`).
The reverse, `format_ingredients` (used to pre-fill the edit form), is the
round-trip inverse and is unit-tested as such. The parser mirrors the
backend's `validate_ingredients` check order so the first error the form
surfaces is the one a forged request would hit.

## Route table (`apps/web`)

| Method | Path | Handler | Purpose | API call(s) |
|---|---|---|---|---|
| GET | `/recipes` | `recipes::list::get` | Suggestions (ranked, with missing-ingredients) + full recipe list | `GET /groups/:id/recipes/suggestions`, `GET /groups/:id/recipes` |
| GET | `/recipes/new` | `recipes::new::get` | Create form | — |
| POST | `/recipes/new` | `recipes::new::post` | Create a recipe | `POST /groups/:id/recipes` |
| GET | `/recipes/:id` | `recipes::detail::get` | Detail: ingredients, instructions, last-cooked, log-meal form, edit/delete controls | `GET /groups/:id/recipes/:rid`, `GET /groups/:id/recipes/meal-history` |
| GET | `/recipes/:id/edit` | `recipes::edit::get` | Edit form (creator/admin/owner only) | `GET /groups/:id/recipes/:rid` |
| POST | `/recipes/:id/edit` | `recipes::edit::post` | Update a recipe | `PATCH /groups/:id/recipes/:rid` |
| POST | `/recipes/:id/log` | `recipes::detail::log` | Log this meal (feeds the variety penalty) | `POST /groups/:id/recipes/:rid/meal-history` |
| POST | `/recipes/:id/delete` | `recipes::detail::delete` | Delete a recipe (creator/admin/owner only) | `DELETE /groups/:id/recipes/:rid` |

No active family → every route redirects to `/groups/new` (same as Stocks).

## Error tables (per page)

Backend codes are `apps/api/src/error.rs` bodies (`{"error": "<code>"}`);
each row is the exact `(status, code)` → French UI state.

### `/recipes/new` (create) — `create_recipe`

| Status | Code | UI |
|---|---|---|
| 201 | — | PRG → `/recipes/:id?notice=recipe_created` (new recipe detail) |
| 400 | `name_required` | inline "Le nom est obligatoire." |
| 400 | `ingredient_name_required` | inline "Chaque ingrédient doit avoir un nom." |
| 400 | `ingredient_quantity_must_be_non_negative` | inline "La quantité d'un ingrédient ne peut pas être négative." |
| 400 | `invalid_seasonal_month` | inline "Les mois de saison doivent être compris entre 1 et 12." |
| 403 | `forbidden` | forbidden page (not reachable via UI — any member may create; mapped defensively) |
| — | transport / other | inline "Service momentanément indisponible…" |

Name + all ingredient rules are pre-validated by the shared
`validate_recipe_name` / `parse_ingredients` (inline error, no round trip);
the backend 400s are the defensive fallback.

### `/recipes` (list + suggestions) — `list_recipes` / `suggest_recipes`

| Status | UI |
|---|---|
| 200 | render suggestions + list |
| other (403 non-member — unreachable once family resolved) | render empty, no JSON leak |
| transport error | service-unavailable page |

### `/recipes/:id` (detail) — `get_recipe`

| Status | Code | UI |
|---|---|---|
| 200 | — | detail |
| 404 | `not_found` | "Recette introuvable" page |
| — | transport / parse | service-unavailable page |

`?notice=` (`recipe_created`, `recipe_updated`, `meal_logged`) and
`?error=` (`forbidden`, `unavailable`) banners rendered on load.

### `/recipes/:id/edit` — `update_recipe`

| Status | Code | UI |
|---|---|---|
| 200 | — | PRG → `/recipes/:id?notice=recipe_updated` |
| 400 | (same four codes as create) | inline on the edit form |
| 403 | `forbidden` | PRG → `/recipes/:id?error=forbidden` |
| 404 | `not_found` | "Recette introuvable" page |
| — | transport / other | inline "Service momentanément indisponible…" |

GET renders the form only for `can_modify` (creator/admin/owner); a
non-permitted user GETting it gets the forbidden page.

### `/recipes/:id/log` (log a meal) — `log_meal`

| Status | Code | UI |
|---|---|---|
| 201 | — | PRG → `/recipes/:id?notice=meal_logged` |
| 404 | `not_found` | "Recette introuvable" page |
| 403 | `forbidden` | PRG → `/recipes/:id?error=forbidden` (defensive — any member may log) |
| — | transport / other | PRG → `/recipes/:id?error=unavailable` |

### `/recipes/:id/delete` — `delete_recipe`

| Status | Code | UI |
|---|---|---|
| 204 | — | PRG → `/recipes?notice=recipe_deleted` |
| 403 | `forbidden` | forbidden page |
| 404 | `not_found` | "Recette introuvable" page |
| — | transport / other | service-unavailable page |

## Permission bar (front mirror of `recipes::can_modify`)

Backend bar (`apps/api/src/recipes/mod.rs::can_modify`): the recipe's
creator, or a group **owner/admin**, may update or delete it; **any**
member may create, read, or log a meal. The front mirrors it
(`routes/recipes::can_modify`, unit-tested):

- Create, read, and the **log-a-meal** form render for every member.
- The **edit link** and **delete button** render only when
  `can_modify(role, is_creator)`; a standard member viewing another
  member's recipe sees a muted "Seul le créateur ou un administrateur…"
  note instead.

The backend stays the authority: a forged edit/delete/log is still
`403`'d and mapped defensively per the tables above.

## Acceptance criteria

1. A member can create a recipe (name + instructions + ingredient lines,
   including a seasonal + an optional ingredient) and is redirected to its
   detail with a success banner.
2. `/recipes` shows a **Suggestions** section, ranked, each with a stock
   summary derived from what's in stock, and its missing required
   ingredients listed under the grocery-list marker; and a **Toutes les
   recettes** alphabetical list. A recipe whose required ingredients are
   all in stock ranks above / is summarised as fully stocked; one whose
   ingredients are missing lists them.
3. The recipe detail shows ingredients (with optional/seasonal markers),
   instructions, and the last-cooked state; "logger ce repas" records a
   meal and the detail then reflects it (feeding the future variety
   penalty).
4. The creator (or an admin/owner) can edit and delete a recipe; edits
   round-trip through the ingredient textarea; delete returns to the list
   with a banner.
5. A standard member viewing another member's recipe gets no edit/delete
   controls (only the muted note) but **can** log a meal against it.
6. Documented error states render their exact French copy: server-side
   empty-name rejection on create, unknown-recipe 404, and the permission
   bar. Pure ingredient parsing/formatting and the permission mirror are
   unit-tested in `apps/shared` (`validation::recipes`) /
   `routes/recipes`; the E2E suite (`e2e/tests/recipes.spec.ts`) drives
   every journey above end-to-end against the real stack as a CI merge
   gate.
