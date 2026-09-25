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
| 9 | Backups: restore tested | missing | Blocking — must be proven before go-live, not after an incident. **Restore Postgres and MinIO to the same point.** The API deletes, once a day, every attachment object older than 24h that no `event_attachments` row points at (#215, `apps/api/src/jobs/attachment_reconcile.rs`). A Postgres dump older than the bucket leaves every attachment uploaded since the dump without a row, and the next pass after they turn 24h old deletes those files for good. Before such a restore, either restore the bucket to the same point, or keep the API from running the job (start it with `ADMIN_DATABASE_URL` on a role without `BYPASSRLS`, which makes every pass refuse, and also breaks the `/admin/*` endpoints and stops the hourly retention purge (#138), both of which share that pool) until the rows are reconciled. |
| 10 | CI: `cargo audit` | missing | Same as v1 tracker item #13, still not in `ci.yml`. |
| 11 | CD: deploy pipeline (build → push → deploy to VPS) | missing | |
| 12 | Monitoring: uptime check | missing | Not yet designed anywhere in `architecture.md`. |
| 13 | Monitoring: centralized/queryable logs | missing | |
| 14 | Rate-limiting on `/login`, `/register` | missing | Called out in `architecture.md` security section as "once exposed to internet" — that condition is now met. |
| 15 | Secrets via sops in production | missing | Scaffolding exists conceptually in `architecture.md`; not yet wired to a real deployment. |
| 16 | RGPD: nom et adresse de contact du responsable de traitement | missing | **À remplacer avant la mise en ligne** — bloquant (#131). Art. 13(1)(a) exige l'identité *et* les coordonnées du responsable. Le porteur du projet (personne physique) fournit son nom et une adresse relevée par une personne — pas un `noreply@` — au moment de l'ouverture publique. Voir la procédure ci-dessous. |
| 17 | LCEN: éditeur, directeur de la publication et hébergeur des mentions légales | missing | **À remplacer avant la mise en ligne** — bloquant (#132). Les mentions légales existent et sont servies (`docs/legal-notice.md`, `GET /legal-notice`), mais cinq valeurs y sont encore des placeholders. L'hébergeur dépend de l'item #1 : auto-hébergement (l'éditeur est alors son propre hébergeur) ou VPS. Voir la procédure ci-dessous. |
| 18 | RGPD: cadre contractuel du sous-traitant email et transferts hors UE | missing | **À faire avant la mise en ligne** — bloquant (#136). Le fournisseur est arrêté (Mailjet), mais ni le cadre contractuel opposable ni les transferts hors UE — pour lui comme pour Google — ne sont établis : trois placeholders les portent dans la politique et le registre. Rien dans le code ne contraint `SMTP_HOST`. Voir la procédure ci-dessous. |

**Immediate next step:** none of the above are done yet. Given the ~1 week
horizon, items 4-9 (RGPD + backups) and 14 (rate-limiting) are the hard
blockers for a responsible first deployment; 1-2 and 10-13 support them.

## Item #16 — remplacer les placeholders du responsable de traitement

`docs/privacy-policy.md` est compilé dans le binaire de `apps/api`
(`include_str!`) et servi tel quel sur `GET /privacy-policy`, page publique
liée depuis les pieds de page de connexion et d'inscription. Cinq valeurs y
sont des placeholders, sous la forme
`[<quoi> — à renseigner avant la mise en ligne]`. Deux relèvent de cet item :

- `nom du responsable de traitement`
- `adresse de contact`

Les trois autres (`cadre contractuel du sous-traitant email`,
`transferts hors UE du sous-traitant email`, `transferts hors UE de Google`)
relèvent de l'item #18 ci-dessous. Les cinq figurent à l'identique, et dans le
même ordre de lecture, dans `docs/registre-traitements.md` — c'est ce que la
suite de tests épingle. `docs/architecture.md` ("Questions résolues" #3)
renvoie aux deux premiers.

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
   - `pending_release_values` est leur source unique, et elle porte les cinq
     placeholders, ceux de l'item #18 compris : en retirer deux au pas 1 ne
     la vide donc pas, et `the_internal_rgpd_documents_carry_the_same_placeholders`
     n'exige que `docs/architecture.md` ait perdu son annonce que le jour où
     les deux items sont faits et où la liste est vide ;
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

## Item #18 — établir le cadre contractuel du sous-traitant email et les transferts

Le fournisseur est arrêté depuis l'arbitrage du 2026-09-19 : **Mailjet**
(Mailjet SAS, groupe Sinch), nommé dans `docs/registre-traitements.md`,
`docs/privacy-policy.md`, `docs/architecture.md` et `README.md`. Ce qui n'est
pas arrêté, ce sont les deux choses qu'aucun de ces documents n'affirme : le
cadre contractuel opposable et les transferts hors UE. Trois placeholders les
portent, à remplir ensemble, dans la politique **et** dans le registre (même
libellé, même ordre de lecture) :

- `cadre contractuel du sous-traitant email`
- `transferts hors UE du sous-traitant email`
- `transferts hors UE de Google`

Au moment de l'ouverture publique :

1. **Arrêter le cadre contractuel.** Le DPA du groupe s'impose par
   l'acceptation des conditions, il n'y a pas de signature séparée à obtenir :
   ce qu'il faut établir, c'est quelle version est opposable, à quelle date
   elle l'est devenue et pour quel compte d'envoi. C'est cela qui remplace le
   premier placeholder.
2. **Relever la liste des sous-traitants ultérieurs**, publiée à une URL
   publique (`sinch.com/legal/data-protection-agreement-sub-processors/`) et
   non annexée au contrat. Au 2026-09-21 elle donne le stockage du flux email
   chez Google Cloud France SARL, centres en Allemagne et en Belgique, pour
   les clients européens — mais elle nomme aussi des entités hors UE pour le
   support, et le DPA réserve des transferts intra-groupe à l'échelle
   mondiale. La question à trancher est donc : quels transferts ont
   effectivement lieu pour l'envoi transactionnel, et sous quel mécanisme
   (art. 44-49) ? La réponse remplace le deuxième placeholder ; si elle est
   « aucun transfert », l'écrire, mais seulement une fois établie.
3. **Faire le même travail pour Google** (connexion et flux iCal) et remplir
   le troisième placeholder. La liste officielle du cadre de confidentialité
   des données n'était pas consultable le 2026-09-21 (site en erreur) : la
   déclaration de Google, qui porte la réserve « sauf exclusion explicite »,
   ne suffit pas à nommer un mécanisme.
4. **Pointer la configuration de production sur le fournisseur retenu** :
   `SMTP_HOST` est lu de l'environnement par `apps/api/src/main.rs`, sans
   contrainte. Poser l'hôte d'envoi du fournisseur, avec `SMTP_FROM` sur un
   domaine dont les enregistrements SPF/DKIM/DMARC sont en place.
5. Rafraîchir la date de dernière mise à jour en tête des deux documents, et
   retirer les trois entrées correspondantes de `pending_release_values` dans
   `apps/shared/src/validation/rgpd.rs` — la suite reste rouge tant que la
   liste et les documents ne disent pas la même chose.

Le jour où le relais self-hosted de `docs/architecture.md` (« Transactional
email (long-term) ») remplace le fournisseur, c'est cet item qu'il faut
rouvrir : le sous-traitant disparaît du registre, et la ligne de transfert
avec lui.

Rien de tout cela ne laisse de trace dans le dépôt tant que les placeholders
restent en place : aucun test ne dira si la configuration de production pointe
ailleurs que là où le registre le croit. C'est pourquoi le pas 4 est ici plutôt
que dans un commentaire de code.
