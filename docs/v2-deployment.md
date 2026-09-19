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
| 9 | Backups: restore tested | missing | Blocking — must be proven before go-live, not after an incident. **Restore Postgres and MinIO to the same point.** The API deletes, once a day, every attachment object older than 24h that no `event_attachments` row points at (#215, `apps/api/src/jobs/attachment_reconcile.rs`). A Postgres dump older than the bucket leaves every attachment uploaded since the dump without a row, and the next pass after they turn 24h old deletes those files for good. Before such a restore, either restore the bucket to the same point, or keep the API from running the job (start it with `ADMIN_DATABASE_URL` on a role without `BYPASSRLS`, which makes every pass refuse, and also breaks the `/admin/*` endpoints that share that pool) until the rows are reconciled. |
| 10 | CI: `cargo audit` | missing | Same as v1 tracker item #13, still not in `ci.yml`. |
| 11 | CD: deploy pipeline (build → push → deploy to VPS) | missing | |
| 12 | Monitoring: uptime check | missing | Not yet designed anywhere in `architecture.md`. |
| 13 | Monitoring: centralized/queryable logs | missing | |
| 14 | Rate-limiting on `/login`, `/register` | missing | Called out in `architecture.md` security section as "once exposed to internet" — that condition is now met. |
| 15 | Secrets via sops in production | missing | Scaffolding exists conceptually in `architecture.md`; not yet wired to a real deployment. |
| 16 | RGPD: nom et adresse de contact du responsable de traitement | missing | **À remplacer avant la mise en ligne** — bloquant (#131). Art. 13(1)(a) exige l'identité *et* les coordonnées du responsable. Le porteur du projet (personne physique) fournit son nom et une adresse relevée par une personne — pas un `noreply@` — au moment de l'ouverture publique. Voir la procédure ci-dessous. |

**Immediate next step:** none of the above are done yet. Given the ~1 week
horizon, items 4-9 (RGPD + backups) and 14 (rate-limiting) are the hard
blockers for a responsible first deployment; 1-2 and 10-13 support them.

## Item #16 — remplacer les placeholders du responsable de traitement

`docs/privacy-policy.md` est compilé dans le binaire de `apps/api`
(`include_str!`) et servi tel quel sur `GET /privacy-policy`, page publique
liée depuis les pieds de page de connexion et d'inscription. Deux valeurs y
sont encore des placeholders, sous la forme
`[<quoi> — à renseigner avant la mise en ligne]` :

- `nom du responsable de traitement`
- `adresse de contact`

Les mêmes deux placeholders figurent dans `docs/registre-traitements.md`, et
`docs/architecture.md` ("Questions résolues" #3) y renvoie.

Au moment de l'ouverture publique :

1. Remplacer les deux placeholders dans `docs/privacy-policy.md` et
   `docs/registre-traitements.md`, et retirer le renvoi de
   `docs/architecture.md` #3.
2. Reporter la même identité dans les mentions légales une fois qu'elles
   existent (issue « ni mentions légales ni CGU »).
3. Rafraîchir la date de dernière mise à jour en tête des deux documents.
4. Mettre à jour `release_placeholders` et ses attentes dans
   `apps/shared/src/validation/rgpd.rs` : le test y épingle la liste exacte
   des placeholders restants, donc la suite reste rouge tant que le pas 1
   n'est pas reflété — c'est le rappel mécanique, pas seulement écrit.
