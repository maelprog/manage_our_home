# v1 scope tracker

Status snapshot of every service/epic needed for v1, per the dependency
order set in `architecture.md` (Groups → Agenda → Stocks → Recipes →
Grocery list → Budget; Messagerie and User admin independent). Update this
file as epics land — it's the single place to check "what's left."

| # | Epic / service | Status | Notes |
|---|---|---|---|
| 1 | Auth + Groups | **done** | Merged to `main` (PR #3, branch `spec/epic1-auth-groups-73929`). Moved into `apps/api/` (`git mv`, no code changes) now that it's landed. |
| 2 | Agenda (events, tasks-as-events, recurrence, reminders, files) | missing | Depends on Auth + Groups. Needs the `scheduled_notifications` job-queue table + worker (see architecture.md). |
| 3 | Stocks | missing | Manual entry v1. Depends on Groups (family-scoped). |
| 4 | Recipes (suggestion algorithm) | missing | Depends on Stocks (for "what's missing") + needs its own v1 rule-based spec (not ML yet). |
| 5 | Grocery list | missing | Hard-depends on Stocks + Recipes (structured ingredient list). One shared list per family. |
| 6 | Budget | missing | Depends on Grocery list (manual price entry, cumulated per period). |
| 7 | Messagerie (one thread per family, text only) | missing | Independent of 2-6; can be spec'd any time after Groups. Needs Axum WebSockets. |
| 8 | User admin (global technical superadmin) | missing | Independent epic; distinct from group owner/admin/standard roles. |
| 9 | Google Calendar import (one-way) | missing (v1) | Depends on Agenda. Bidirectional sync explicitly deferred past v1. |
| 10 | Object storage (MinIO) wiring | missing | Needed by Agenda (event files) once that epic starts; fridge-scan photos are post-v1. |
| 11 | Transactional email (Brevo/Mailjet via `lettre`) | partially designed | Stack decided (architecture.md); actual sending code is part of Auth epic (verification, password reset) — check epic #1 branch for implementation status. |
| 12 | RGPD features (export, erasure, privacy policy, registre des traitements) | missing | Called out as v1-required in architecture.md; not yet implemented in any epic. Needs its own pass once Auth+Groups lands (account deletion depends on group-ownership transfer rules from `notes-issue-1-qa.md`). |
| 13 | CI (cargo audit / npm audit, blocking on high/critical) | missing | No `.github/workflows` yet. Add once `apps/api` (and later `apps/web`) exist, so the audit has something to scan. |
| 14 | Deployment / infra (docker-compose, Caddy, secrets via sops) | scaffolded | `infra/docker-compose.yml` and `infra/Caddyfile` are skeletons pointing at services that don't exist yet (`apps/api` image, no `apps/web` build). Flesh out as those apps land. Public/internet exposure explicitly deferred past v1 per `notes-issue-1-qa.md` (#7) — local/VPN only for now. |
| — | Fridge-scan (OCR/vision via Ollama) | out of v1 | Explicitly scoped out, see architecture.md. |
| — | Bidirectional Google Calendar sync | out of v1 | Explicitly scoped out. |
| — | Local price lookup for Budget | out of v1 | Manual entry only for v1. |

**Immediate next step:** start epic #2 (Agenda) inside `apps/api/`.
