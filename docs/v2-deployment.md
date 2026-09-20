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
| 17 | LCEN: éditeur, directeur de la publication et hébergeur des mentions légales | missing | **À remplacer avant la mise en ligne** — bloquant (#132). Les mentions légales existent et sont servies (`docs/legal-notice.md`, `GET /legal-notice`), mais cinq valeurs y sont encore des placeholders. L'hébergeur dépend de l'item #1 : auto-hébergement (l'éditeur est alors son propre hébergeur) ou VPS. Voir la procédure ci-dessous. |

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
2. Reporter la même identité dans les mentions légales — elles existent
   depuis #132 : voir l'item #17 ci-dessous, qui se traite dans le même
   passage.
3. Rafraîchir la date de dernière mise à jour en tête des deux documents.
4. Mettre à jour les deux tests de `apps/shared/src/validation/rgpd.rs` qui
   épinglent ces valeurs, car tant qu'ils ne le sont pas la suite reste rouge
   — c'est le rappel mécanique, pas seulement écrit :
   - `pending_release_values` est leur source unique : la vider reflète le
     pas 1, et `the_internal_rgpd_documents_carry_the_same_placeholders`
     exige alors que `docs/architecture.md` ait bien perdu son annonce ;
   - `renders_the_real_privacy_policy_without_leftover_markup` porte en plus
     deux attentes littérales à retirer à la main : les `html.contains(…)`
     posés sur `[nom du responsable de traitement` et sur
     `[adresse de contact`.

## Item #17 — remplacer les placeholders des mentions légales

`docs/legal-notice.md` et `docs/terms-of-service.md` sont compilés dans le
binaire de `apps/api` (`include_str!`) et servis tels quels sur
`GET /legal-notice` et `GET /terms-of-service`, pages publiques liées depuis
les pieds de page de connexion et d'inscription et depuis `/account`. Les CGU
ne portent aucun placeholder — elles renvoient aux mentions légales. Cinq
valeurs des mentions légales en sont, dans la même forme
`[<quoi> — à renseigner avant la mise en ligne]` que l'item #16 :

- `nom de l'éditeur`
- `adresse postale de l'éditeur`
- `adresse de contact` — la **même** que celle de l'item #16, écrite à
  l'identique pour qu'une seule valeur soit à décider
- `nom du directeur de la publication`
- `nom et adresse de l'hébergeur`

L'hébergeur dépend de l'item #1 et n'est pas choisi (arbitrage du
2026-09-19 : auto-hébergement envisagé, VPS possible ensuite). Les deux
branches, au moment de la mise en ligne :

- **auto-hébergement** : l'éditeur est son propre hébergeur ; inscrire son
  nom et l'adresse où le serveur est exploité ;
- **VPS** : inscrire la raison sociale, l'adresse et le moyen de contact du
  prestataire retenu.

Au moment de l'ouverture publique :

1. Remplacer les cinq placeholders dans `docs/legal-notice.md`, en même temps
   que les deux de l'item #16 — le responsable de traitement et l'éditeur
   sont la même personne physique.
2. Retirer, dans la section « Hébergeur », le paragraphe qui explique que
   l'hébergement n'est pas arrêté.
3. Rafraîchir la date de dernière mise à jour en tête des deux documents.
4. Mettre à jour les tests de `apps/shared/src/validation/rgpd.rs` qui
   épinglent ces valeurs, faute de quoi la suite reste rouge :
   - `pending_legal_notice_values` est leur source unique ; la vider reflète
     le pas 1 ;
   - `the_public_legal_documents_carry_only_the_placeholders_pinned_here`
     exige que les mentions légales et la politique de confidentialité soient
     remplies **le même jour** — c'est ce qui empêche d'en remplir une et
     d'oublier l'autre ;
   - `renders_the_real_legal_notice_without_leftover_markup` boucle sur
     `pending_legal_notice_values` et n'a donc rien à retirer à la main.
