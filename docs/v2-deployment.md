# v2 deployment scope tracker — multi-family rollout

Companion to `v1-scope.md`, same format. Target: first external deployment
next week (once v1 is validated), for 10-15 families. Decisions and
rationale in `architecture.md` ("v2 — Déploiement multi-famille").

| # | Item | Status | Notes |
|---|---|---|---|
| 1 | VPS provisioning | missing | Hetzner/Scaleway, 2 vCPU/4 Go class. Same Docker Compose stack as local dev. |
| 2 | TLS via Caddy in production | scaffolded | `infra/Caddyfile` exists as a skeleton; needs a real domain + cert issuance config. |
| 3 | Superadmin role | missing | Global technical role, distinct from group owner/admin/standard. Single account (maintainer) for now — no support team to model. |
| 4 | RGPD: data export (Art. 20) | missing | Blocking before first external deployment. |
| 5 | RGPD: account/data deletion (Art. 17) | missing | Blocking. Depends on group-ownership transfer rules (see `notes-issue-1-qa.md`). |
| 6 | RGPD: privacy policy | missing | Blocking. Must cover what's collected, why, retention, sharing (Google OAuth). |
| 7 | RGPD: legal basis documentation per data category | missing | Blocking (registre des traitements). |
| 8 | Backups: Postgres + MinIO, encrypted | missing | Blocking. |
| 9 | Backups: restore tested | missing | Blocking — must be proven before go-live, not after an incident. |
| 10 | CI: `cargo audit` | missing | Same as v1 tracker item #13, still not in `ci.yml`. |
| 11 | CD: deploy pipeline (build → push → deploy to VPS) | missing | |
| 12 | Monitoring: uptime check | missing | Not yet designed anywhere in `architecture.md`. |
| 13 | Monitoring: centralized/queryable logs | missing | |
| 14 | Rate-limiting on `/login`, `/register` | missing | Called out in `architecture.md` security section as "once exposed to internet" — that condition is now met. |
| 15 | Secrets via sops in production | missing | Scaffolding exists conceptually in `architecture.md`; not yet wired to a real deployment. |

**Immediate next step:** none of the above are done yet. Given the ~1 week
horizon, items 4-9 (RGPD + backups) and 14 (rate-limiting) are the hard
blockers for a responsible first deployment; 1-2 and 10-13 support them.
