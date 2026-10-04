# v2 deployment scope tracker — multi-family rollout

Companion to `v1-scope.md`, same format. Target: first external deployment
next week (once v1 is validated), for 10-15 families. Decisions and
rationale in `architecture.md` ("v2 — Déploiement multi-famille").

| # | Item | Status | Notes |
|---|---|---|---|
| 1 | VPS provisioning | missing | Hetzner/Scaleway, 2 vCPU/4 Go class. Same Docker Compose stack as local dev. |
| 2 | TLS via Caddy in production | done | Configured since #141 (`infra/Caddyfile`, site `{$SITE_ADDRESS::80}`): `infra/generate-env.sh <domain>` puts the public domain name in `SITE_ADDRESS`, which turns on Caddy's automatic HTTPS — certificate obtained and renewed by Caddy (kept in the `caddy_data` volume), port 80 only redirecting to 443, `Strict-Transport-Security` on every response. Takes effect on the host of item #1, provided the name resolves to it and its ports 80 and 443 are reachable from the Internet (README, "Running it for real"). The `:80` fallback is the local plain-HTTP stack only. `docs/registre-traitements.md` (« Mesures de sécurité communes ») refers to this setup (#315). |
| 3 | Superadmin role | missing | Global technical role, distinct from group owner/admin/standard. Single account (maintainer) for now — no support team to model. |
| 4 | RGPD: data export (Art. 20) | missing | Blocking before first external deployment. |
| 5 | RGPD: account/data deletion (Art. 17) | missing | Blocking. Depends on group-ownership transfer rules (see `notes-issue-1-qa.md`). |
| 6 | RGPD: privacy policy | missing | Blocking. Must cover what's collected, why, retention, sharing (Google OAuth). |
| 7 | RGPD: legal basis documentation per data category | missing | Blocking (registre des traitements). |
| 8 | Backups: Postgres + MinIO, encrypted | missing | Blocking. |
| 9 | Backups: restore tested | missing | Blocking — must be proven before go-live, not after an incident. **Restore Postgres and MinIO to the same point.** The API deletes, once a day, every attachment object older than 24h that no `event_attachments` row points at (#215, `apps/api/src/jobs/attachment_reconcile.rs`). A Postgres dump older than the bucket leaves every attachment uploaded since the dump without a row, and the next pass after they turn 24h old deletes those files for good. Before such a restore, either restore the bucket to the same point, or keep the API from running the job (start it with `ADMIN_DATABASE_URL` on a role without `BYPASSRLS`, which makes every pass refuse, and also breaks the `/admin/*` endpoints and stops the hourly retention purge (#138) and the hourly account purge (#139), all of which share that pool) until the rows are reconciled. |
| 10 | CI: `cargo audit` | missing | Same as v1 tracker item #13, still not in `ci.yml`. |
| 11 | CD: deploy pipeline (build → push → deploy to VPS) | missing | |
| 12 | Monitoring: uptime check | missing | Not yet designed anywhere in `architecture.md`. |
| 13 | Monitoring: centralized/queryable logs | missing | |
| 14 | Rate-limiting on `/login`, `/register` | missing | Called out in `architecture.md` security section as "once exposed to internet" — that condition is now met. |
| 15 | Secrets via sops in production | missing | Scaffolding exists conceptually in `architecture.md`; not yet wired to a real deployment. |
| 16 | RGPD: nom et adresse de contact du responsable de traitement | missing | **À remplacer avant la mise en ligne** — bloquant (#131). Art. 13(1)(a) exige l'identité *et* les coordonnées du responsable. Le porteur du projet (personne physique) fournit son nom et une adresse relevée par une personne — pas un `noreply@` — au moment de l'ouverture publique. Voir la procédure ci-dessous. |
| 17 | LCEN: hébergeur des mentions légales et identité de l'éditeur confiée à l'hébergeur | missing | **À remplacer avant la mise en ligne** — bloquant (#132, #314). Les mentions légales existent et sont servies (`docs/legal-notice.md`, `GET /legal-notice`), mais trois valeurs y sont encore des placeholders. L'éditeur use de l'anonymat de la LCEN art. 1-1, II (arbitrage du 2026-10-04) : son identité est communiquée à l'hébergeur, pas publiée. L'hébergeur dépend de l'item #1. Voir la procédure ci-dessous. |
| 18 | RGPD: cadre contractuel du sous-traitant email et transferts hors UE | missing | **À faire avant la mise en ligne** — bloquant (#136, #328). Le fournisseur est arrêté (Scaleway Transactional Email, arbitrage du 2026-10-02 qui remplace Mailjet) et ne transfère rien hors UE, relevé daté dans le registre. Restent à établir le cadre contractuel opposable (DPA accepté depuis la console Scaleway) et les transferts hors UE de Google et des services de notification des navigateurs (#306) : trois placeholders les portent dans la politique et le registre. Rien dans le code ne contraint `SMTP_HOST`. Voir la procédure ci-dessous. |
| 19 | RGPD: AIPD signée et registre des violations ouvert | missing | **À faire avant la mise en ligne** — bloquant (#143). L'AIPD est rédigée (`docs/aipd.md`) mais sa conclusion n'est qu'une proposition : le responsable de traitement la relit et la signe, ce qui remplit son placeholder (`conclusion de l'AIPD, date et signature`). Elle conditionne l'ouverture aux items #2, #8, #9, #12, #13, #15, #16 et #18. Le registre des violations est tenu hors du dépôt, qui est public (`docs/registre-violations.md` en fixe la forme) : l'ouvrir et en noter l'emplacement à la place du placeholder de ce fichier (`emplacement du registre des violations`). `docs/procedure-violation.md` porte le placeholder `adresse de contact`, rempli par l'item #16. Les trois sont épinglés par `the_breach_and_aipd_documents_carry_only_the_placeholders_pinned_here` (`apps/shared/src/validation/rgpd.rs`) : retirer l'attente correspondante en remplissant chacun. |

**Immediate next step:** apart from item 2, none of the above are done
yet. Given the ~1 week horizon, items 4-9 (RGPD + backups) and 14
(rate-limiting) are the hard blockers for a responsible first deployment;
1-2 and 10-13 support them.

## Item #16 — remplacer les placeholders du responsable de traitement

`docs/privacy-policy.md` est compilé dans le binaire de `apps/api`
(`include_str!`) et servi tel quel sur `GET /privacy-policy`, page publique
liée depuis les pieds de page de connexion et d'inscription. Cinq valeurs y
sont des placeholders, sous la forme
`[<quoi> — à renseigner avant la mise en ligne]`. Deux relèvent de cet item :

- `nom du responsable de traitement`
- `adresse de contact`

Les trois autres (`cadre contractuel du sous-traitant email`,
`transferts hors UE de Google`,
`transferts hors UE des services de notification`)
relèvent de l'item #18 ci-dessous. Les cinq figurent à l'identique, et dans le
même ordre de lecture, dans `docs/registre-traitements.md` — c'est ce que la
suite de tests épingle. `docs/architecture.md` ("Questions résolues" #3)
renvoie aux deux premiers. `adresse de contact` figure aussi, seule, dans
`docs/procedure-violation.md` (#143).

Au moment de l'ouverture publique :

1. Remplacer les deux placeholders dans `docs/privacy-policy.md` et
   `docs/registre-traitements.md`, l'adresse de contact dans
   `docs/procedure-violation.md`, et retirer le renvoi de
   `docs/architecture.md` #3.
2. Reporter la même adresse de contact dans les mentions légales et
   communiquer l'identité de l'éditeur à l'hébergeur, sans la publier
   (anonymat LCEN art. 1-1, II) : voir l'item #17 ci-dessous, qui se traite
   dans le même passage.
3. Rafraîchir la date de dernière mise à jour en tête des deux documents.
4. Mettre à jour les tests de `apps/shared/src/validation/rgpd.rs` qui
   épinglent ces valeurs, car tant qu'ils ne le sont pas la suite reste rouge
   — c'est le rappel mécanique, pas seulement écrit :
   - `pending_release_values` est leur source unique, et elle porte les six
     placeholders, ceux de l'item #18 compris : en retirer deux au pas 1 ne
     la vide donc pas, et `the_internal_rgpd_documents_carry_the_same_placeholders`
     n'exige que `docs/architecture.md` ait perdu son annonce que le jour où
     les deux items sont faits et où la liste est vide ;
   - `renders_the_real_privacy_policy_without_leftover_markup` porte en plus
     deux attentes littérales à retirer à la main : les `html.contains(…)`
     posés sur `[nom du responsable de traitement` et sur
     `[adresse de contact`.
   - `the_breach_and_aipd_documents_carry_only_the_placeholders_pinned_here`
     attend l'adresse de contact dans `docs/procedure-violation.md` et exige
     qu'elle disparaisse le même jour que celle de la politique.

## Item #17 — remplacer les placeholders des mentions légales

`docs/legal-notice.md` et `docs/terms-of-service.md` sont compilés dans le
binaire de `apps/api` (`include_str!`) et servis tels quels sur
`GET /legal-notice` et `GET /terms-of-service`, pages publiques liées depuis
les pieds de page de connexion et d'inscription et depuis `/account`. Les CGU
ne portent aucun placeholder — elles renvoient aux mentions légales.

Le régime appliqué est l'anonymat de l'éditeur non professionnel (LCEN
art. 1-1, II, ancien art. 6-III-2 ; arbitrage du 2026-10-04) : l'article
1-1, I exige de l'éditeur personne physique ses nom, prénoms, domicile et
numéro de téléphone, et de l'hébergeur son nom, son adresse et son numéro de
téléphone ; le II dispense l'éditeur non professionnel de publier les siens,
à condition de les avoir communiqués à l'hébergeur. Les mentions publiées
ne portent donc que l'identité de l'hébergeur et l'adresse de contact. Trois
valeurs y sont des placeholders, dans la même forme
`[<quoi> — à renseigner avant la mise en ligne]` que l'item #16 :

- `adresse de contact` — la **même** que celle de l'item #16, écrite à
  l'identique pour qu'une seule valeur soit à décider
- `nom et adresse de l'hébergeur`
- `numéro de téléphone de l'hébergeur`

Les nom, prénoms, domicile et numéro de téléphone de l'éditeur ne sont
**jamais** écrits dans le dépôt, qui est public : ils vont à l'hébergeur
seul (pas 1 ci-dessous).

L'hébergeur dépend de l'item #1 et n'est pas choisi (arbitrage du
2026-09-19 : auto-hébergement envisagé, VPS possible ensuite). Les deux
branches, au moment de la mise en ligne :

- **VPS** : inscrire la raison sociale, l'adresse et le numéro de téléphone
  du prestataire retenu ;
- **auto-hébergement** : l'éditeur est son propre hébergeur, et l'anonymat
  du II ne tient plus — le nom, l'adresse et le numéro de téléphone de
  l'hébergeur à publier sont alors ceux de l'éditeur. Cette branche demande
  un nouvel arbitrage avant la mise en ligne, et la réécriture des sections
  « Éditeur du service » et « Directeur de la publication ».

Au moment de l'ouverture publique :

1. Communiquer à l'hébergeur retenu les nom, prénoms, domicile et numéro de
   téléphone de l'éditeur (LCEN art. 1-1, II), par le moyen qu'il prévoit,
   et en garder la trace hors du dépôt. Sans ce pas, l'anonymat n'est pas
   ouvert et les mentions publiées sont incomplètes.
2. Remplacer les trois placeholders dans `docs/legal-notice.md`, en même
   temps que les deux de l'item #16 — le responsable de traitement et
   l'éditeur sont la même personne physique, et l'adresse de contact est la
   même.
3. Retirer, dans la section « Hébergeur », le paragraphe qui explique que
   l'hébergement n'est pas arrêté.
4. Rafraîchir la date de dernière mise à jour en tête de
   `docs/legal-notice.md` (les CGU ne changent pas).
5. Mettre à jour les tests de `apps/shared/src/validation/rgpd.rs` qui
   épinglent ces valeurs, faute de quoi la suite reste rouge :
   - `pending_legal_notice_values` est leur source unique ; la vider reflète
     le pas 2 ;
   - `the_public_legal_documents_carry_only_the_placeholders_pinned_here`
     exige que les mentions légales et la politique de confidentialité soient
     remplies **le même jour** — c'est ce qui empêche d'en remplir une et
     d'oublier l'autre ;
   - `renders_the_real_legal_notice_without_leftover_markup` boucle sur
     `pending_legal_notice_values` et n'a donc rien à retirer à la main ; il
     exige aussi que le texte cite l'article 1-1 et l'anonymat, ce que la
     branche auto-hébergement devra revoir.

## Item #18 — établir le cadre contractuel du sous-traitant email et les transferts

Le fournisseur est arrêté depuis l'arbitrage du 2026-10-02 (#328), qui remplace
celui du 2026-09-19 (#136, Mailjet) : **Scaleway Transactional Email**
(Scaleway SAS, Paris), nommé dans `docs/registre-traitements.md`,
`docs/privacy-policy.md`, `docs/architecture.md` et `README.md`. Ses
transferts sont établis : aucun hors UE, relevé daté et sourcé dans le
registre (FAQ du service et liste des sous-traitants de Scaleway, consultées le
2026-10-04). Ce qui n'est pas arrêté, ce sont le cadre contractuel opposable
et les transferts des autres destinataires. Trois placeholders les portent, à
remplir ensemble, dans la politique **et** dans le registre (même libellé,
même ordre de lecture) :

- `cadre contractuel du sous-traitant email`
- `transferts hors UE de Google`
- `transferts hors UE des services de notification` (#306)

Au moment de l'ouverture publique :

1. **Ouvrir le compte d'envoi et accepter le DPA.** Dans la console
   Scaleway, créer le projet qui portera Transactional Email et accepter
   l'accord de traitement des données (DPA) de Scaleway, qui définit les
   conditions de traitement au titre de l'article 28 ; il n'y a pas de
   signature séparée. Noter la version acceptée, sa date et le projet
   qu'elle couvre : c'est cela qui remplace le premier placeholder.
2. **Revérifier l'absence de transfert.** Relire la FAQ du service
   (`scaleway.com/en/docs/transactional-email/faq/`, questions sur la TIA
   et sur les sous-traitants hors UE) et la liste des sous-traitants
   ultérieurs (`scaleway.com/en/subprocessorlist/`), publiée à une URL
   publique et non annexée au contrat. Si l'une ou l'autre nomme désormais
   un traitement hors UE pour Transactional Email, la ligne Scaleway du
   tableau des transferts du registre et la politique ne tiennent plus :
   les corriger avant d'ouvrir. Sinon, rafraîchir la date de consultation
   dans les deux documents.
3. **Vérifier le domaine d'envoi.** Ajouter le domaine de `SMTP_FROM` dans
   Transactional Email, publier les enregistrements SPF et DKIM que la
   console fournit, puis un enregistrement DMARC (`_dmarc.<domaine>`, en
   `p=none` le temps de lire les rapports, puis durci), et lancer la
   vérification du domaine depuis l'onglet de vérification DNS. Un
   enregistrement MX sur le domaine est recommandé par Scaleway pour la
   délivrabilité.
4. **Pointer la configuration de production sur Scaleway** :
   `SMTP_HOST=smtp.tem.scaleway.com`, `SMTP_USERNAME` = l'identifiant du
   projet Scaleway qui porte le domaine, `SMTP_PASSWORD` = la clé secrète
   d'une clé d'API IAM de ce projet, générée pour l'envoi SMTP selon la
   documentation Scaleway,
   `SMTP_FROM` sur le domaine vérifié. Le code se connecte en TLS implicite
   sur le port 465 (`AsyncSmtpTransport::relay` de `lettre`,
   `apps/api/src/main.rs`) ; `SMTP_PORT` n'est lu qu'en mode de
   développement non chiffré (`SMTP_ALLOW_INSECURE=true`, Mailpit) et ne
   s'applique pas ici. Scaleway accepte aussi le port 587 en STARTTLS, que
   ce code n'utilise pas. `SMTP_HOST` n'est contraint par rien d'autre que
   cette configuration.
5. **Faire le travail de transferts pour Google** (connexion et flux iCal)
   et remplir le deuxième placeholder. La liste officielle du cadre de
   confidentialité des données n'était pas consultable le 2026-09-21 (site
   en erreur) : la déclaration de Google, qui porte la réserve « sauf
   exclusion explicite », ne suffit pas à nommer un mécanisme.
   Puis pour les **services de notification des navigateurs** (Google pour
   Chrome, Mozilla, Apple, Microsoft), qui reçoivent l'adresse d'abonnement
   d'un appareil et l'heure d'un message vide à chaque rappel par
   notification : le service ne les choisit pas, le navigateur du membre le
   fait. La réponse remplace le troisième placeholder.
6. Rafraîchir la date de dernière mise à jour en tête des deux documents, et
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
