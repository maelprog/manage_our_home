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
| 7 | Messagerie (one thread per family, text only) | missing | Independent of 2-6; can be spec'd any time after Groups. Needs Axum WebSockets. |
| 8 | User admin (global technical superadmin) | missing | Independent epic; distinct from group owner/admin/standard roles. |
| 9 | Google Calendar import (one-way) | missing (v1) | Depends on Agenda. Bidirectional sync explicitly deferred past v1. |
| 10 | Object storage (MinIO) wiring | **done** | Wired as part of epic #2 (Agenda): `src/storage.rs` (aws-sdk-s3 client pointed at MinIO), `infra/docker-compose.yml` minio service. Fridge-scan photos are post-v1. |
| 11 | Transactional email (Brevo/Mailjet via `lettre`) | partially designed | Stack decided (architecture.md); actual sending code is part of Auth epic (verification, password reset) — check epic #1 branch for implementation status. |
| 12 | RGPD features (export, erasure, privacy policy, registre des traitements) | missing | Called out as v1-required in architecture.md; not yet implemented in any epic. Needs its own pass once Auth+Groups lands (account deletion depends on group-ownership transfer rules from `notes-issue-1-qa.md`). |
| 13 | CI (cargo audit / npm audit, blocking on high/critical) | partially done | `cargo audit` job added to `ci.yml` (2026-07-08), covers the root workspace (`apps/api`). `npm audit` job still to add once `apps/web`/`apps/mobile` (npm workspace) exist — no `package.json` yet to scan. |
| 14 | Deployment / infra (docker-compose, Caddy, secrets via sops) | scaffolded | `infra/docker-compose.yml` and `infra/Caddyfile` are skeletons pointing at services that don't exist yet (`apps/api` image, no `apps/web` build). Flesh out as those apps land. Public/internet exposure explicitly deferred past v1 per `notes-issue-1-qa.md` (#7) — local/VPN only for now. |
| — | Fridge-scan (OCR/vision via Ollama) | out of v1 | Explicitly scoped out, see architecture.md. |
| — | Bidirectional Google Calendar sync | out of v1 | Explicitly scoped out. |
| — | Local price lookup for Budget | out of v1 | Manual entry only for v1. |

**Immediate next step:** epic #6 (Budget) has landed; next up is epic #7 (Messagerie) or #8 (User admin), both independent of the Groups→Budget dependency chain.

**Note (2026-07-08):** the fridge-scan epic (out of v1, row above) is now
also the designated trigger for the Version Y microservices/Kubernetes
trajectory — see `architecture.md` § "Version Y" and
`docs/version-y-microservices.md`. Its spec, when it happens, should
account for that (Ollama extraction as a separate service).
