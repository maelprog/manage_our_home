# Audit de conformité — 2026-09-09

Périmètre : dépôt `manage_our_home`, branche `main`, commit `1144b7f`.
Surfaces : **web uniquement** (Leptos SSR + axum, rendu serveur, pas de WASM).
Méthode : **lecture statique du markup et de la CSS** — le rendu est écrit en
clair dans des `format!`/`view!` Rust, donc le `grep` voit le HTML réel et non
des noms de composants ; contrastes mesurés par script sur `apps/web/src/style.css`.
Aucun rendu navigateur, aucun lecteur d'écran, aucune base peuplée n'ont été
utilisés (voir « Non vérifié »).

Cadre retenu, arbitré avec le porteur du projet pendant cet audit :
**service grand public ouvert** (inscription libre), **sans paiement**,
**avec des utilisateurs mineurs prévisibles** (application de gestion de foyer),
public **français / UE**, responsable de traitement : **une personne physique,
non encore nommée dans les documents**.

> Ce cadrage change des verdicts. Le dépôt, lui, est encore écrit pour l'autre :
> `infra/Caddyfile:7` porte « Public/internet exposure is still deferred past v1
> (local/VPN only) » et `docs/privacy-policy.md:14` décrit un « déploiement
> familial/home-lab ». Une partie des constats ci-dessous naissent exactement de
> cet écart entre le déploiement pour lequel le code a été écrit et celui qui est
> désormais visé.

## Cadre juridique retenu

| Régime | Applicable ? | Pourquoi |
|---|---|---|
| **RGPD** | Oui | Données personnelles de personnes en UE : `users` (email, nom), `messages`, `events`, `budget_entries`, etc. — `apps/api/migrations/0001_users_auth_groups.sql` et suivantes |
| **ePrivacy / cookies (art. 82 LIL)** | Oui, mais satisfait | Un seul dépôt terminal : le cookie de session `session_id` (`apps/api/src/auth/session.rs:103`). Strictement nécessaire → exempté de consentement. Aucun `localStorage`, `sessionStorage`, `indexedDB` ni service worker dans tout `apps/web` |
| **LCEN art. 6-III (mentions légales)** | **Oui**, du fait du cadrage public | Aucun document ni route ne les porte — constat n° 2 |
| **CGV / rétractation / remboursement** | **Non** | Aucun paiement : `grep -riE 'stripe\|paypal\|braintree\|adyen\|lemonsqueezy\|abonnement\|subscription'` sur `apps/`, `infra/`, `Cargo.toml` ne remonte que le mot « checkout » au sens « prix saisi en caisse » (`apps/api/src/budget/`) |
| **DSA** | **Non** | Le contenu (`messages`, `events`) circule dans un groupe fermé, jamais diffusé au public : `messages` est lu via `scoped_tx` + RLS `app.family_id` (`apps/api/src/messagerie/`). Pas d'hébergement de contenu public, donc ni dispositif de signalement ni rapport de transparence |
| **AI Act** | **Non** | Aucun système d'IA. `infra/docker-compose.yml:57` démarre bien un conteneur `ollama`, mais **aucun code Rust ne l'appelle** (`grep -riE 'ollama\|openai\|anthropic\|llm' apps/**/*.rs` : zéro occurrence hors commentaires). Les suggestions de recettes sont un tri par règles : `apps/api/src/recipes/suggestions.rs:15` |
| **EAA (directive 2019/882)** | **Non**, par catégorie | La directive vise des catégories énumérées de services aux consommateurs (commerce électronique, banque, transport, livre numérique, communications). Une application gratuite de gestion de foyer n'y figure pas, et l'exemption micro-entreprise couvrirait de toute façon un responsable personne physique. **À rouvrir si un paiement apparaît** : le service deviendrait du commerce électronique |
| **RGAA (art. 47 loi 2005-102)** | **Non** | Ni secteur public, ni délégataire, ni entreprise au-dessus du seuil de chiffre d'affaires. Donc pas de déclaration d'accessibilité ni de schéma pluriannuel obligatoires |
| **Art. 8 RGPD (mineurs)** | **Oui, non traité** | Voir constat n° 8 |
| **Conservation des données de connexion (décret 2021-1362)** | **À qualifier** | Voir constat n° 16 |
| **Directives post-mortem (art. 85 LIL)** | Oui, non traité | Constat n° 19 |
| **Autres juridictions (CCPA, UK GDPR, LPD suisse)** | Hors périmètre | Le service vise la France/UE (interface entièrement en français, `<html lang="fr">`, `chrono-tz` fixé sur Europe/Paris). S'ils devenaient applicables, ils **n'ont pas été audités ici** |

## Verdict

**Cinq constats bloquent une mise en ligne publique**, et aucun n'est un défaut
d'ingénierie : ce sont des documents. Le responsable de traitement n'est nommé
nulle part — la chaîne littérale `placeholder_name` est servie aux utilisateurs
à `/privacy-policy` —, aucune adresse de contact n'existe pour exercer un droit,
les mentions légales de la LCEN sont absentes, la politique omet des mentions
obligatoires de l'article 13 (droit de réclamation auprès de la CNIL, droits
d'opposition et de limitation, durées de conservation, qui sont renvoyées à un
fichier `docs/` que l'utilisateur ne peut pas ouvrir), et l'invitation par email
enrôle un tiers sans aucune des informations de l'article 14.

La **technique, elle, est nettement au-dessus de la moyenne** de ce que cet
audit rencontre, et il faut le dire aussi précisément que les manques : aucun
traceur, aucun CDN, aucun SDK tiers, polices auto-hébergées, un seul cookie et
il est strictement nécessaire, chiffrement applicatif des colonnes sensibles,
isolation multi-tenant doublée par des politiques RLS, purge de compte réellement
implémentée et planifiée, nettoyage des objets S3 câblé sur les suppressions en
cascade. **Aucun bandeau cookies n'est requis, et en ajouter un serait une
régression.**

Ce qui peut suivre la mise en ligne : la conservation bornée des journaux
d'audit et des jetons, l'export qui ne rend pas tout ce que l'app détient, et la
passe de contraste déjà tracée sous l'issue #74.

## Constats

| # | Gravité | Domaine | Constat | Preuve |
|---|---|---|---|---|
| 1 | **Bloquant** | Documents légaux | Le responsable de traitement n'est pas nommé : la chaîne `placeholder_name` est servie telle quelle aux utilisateurs. Aucune adresse de contact n'existe pour exercer un droit — la politique renvoie « au responsable de traitement (voir en-tête) », où figure le placeholder | `docs/privacy-policy.md:8`, `:82`, `docs/registre-traitements.md:9`, `docs/architecture.md:216` |
| 2 | **Bloquant** | Documents légaux | Aucunes mentions légales (éditeur, hébergeur, contact), obligatoires pour un service en ligne accessible publiquement | Aucune route ni fichier : `grep -ril 'mentions.légal' .` et `apps/web/src/main.rs:40-90` (table des routes) |
| 3 | **Bloquant** | Documents légaux | La politique omet des mentions obligatoires de l'art. 13 : droit de réclamation auprès de la CNIL, droits d'opposition et de limitation, et les durées de conservation par catégorie — ces dernières sont déléguées à `docs/registre-traitements.md`, un chemin de dépôt que l'utilisateur ne peut pas ouvrir | `docs/privacy-policy.md:24` (« Voir `docs/registre-traitements.md` »), `:73-84` (la liste des droits s'arrête à 4 sur 6) |
| 4 | **Bloquant** | Emails / art. 14 | L'invitation par email fait entrer dans le traitement une personne qui n'a rien demandé, et l'email ne porte **aucune** des informations de l'art. 14 : ni qui traite, ni pourquoi son adresse est connue, ni comment s'y opposer, ni lien vers la politique. Son adresse est de plus stockée en clair | Corps complet : `apps/api/src/email.rs:71-73` ; stockage : `apps/api/src/groups/mod.rs:371-384`, colonne `invitations.invited_email` (`migrations/0001`) |
| 5 | **Bloquant** | Documents légaux | Aucunes CGU. Pour un service ouvert, rien ne définit le service, la propriété du contenu déposé dans un groupe partagé, la responsabilité, ni les conditions de fermeture d'un compte — alors même que le contenu d'un membre **survit** à la suppression de son compte (constat n° 11) | Aucun fichier ni route ; comparer à `apps/api/src/jobs/account_purge.rs:12-16` |
| 6 | Majeur | Registre / politique | La politique et le registre décrivent une application plus ancienne que le code. **Absents des deux** : la connexion Google (`/auth/google/start`, un flux distinct de l'import iCal — le registre affirme même « Aucun compte Google n'est requis »), les rappels d'événements par email, les invitations, les jetons de vérification et de réinitialisation, `message_read_state`, `event_assignees` | `apps/api/src/lib.rs:83-84` vs `docs/registre-traitements.md:19-21` ; `apps/api/src/jobs/scheduled_notifications.rs:82` vs `docs/privacy-policy.md:44-49` |
| 7 | Majeur | Sous-traitants | Le sous-traitant SMTP n'est pas choisi (« Brevo ou Mailjet »), aucun DPA n'existe, et le registre restreint son usage à la vérification et à la réinitialisation — alors qu'il reçoit aussi les invitations et les rappels. « Basé UE » est une intention : l'hôte vient de `SMTP_HOST`, que le code ne contraint pas | `docs/registre-traitements.md:15-18`, `docs/privacy-policy.md:52-56` vs `apps/api/src/main.rs` (`SMTP_HOST` lu de l'environnement) |
| 8 | Majeur | Mineurs | Une application de gestion de foyer aura des comptes d'enfants. Aucun âge n'est collecté, aucun consentement parental n'est prévu, et ni la politique ni le registre ne mentionnent les mineurs — alors que l'art. 8 fixe le seuil à 15 ans en France pour les traitements fondés sur le consentement (ici : l'import calendrier) | Aucune colonne d'âge dans `migrations/0001` ; `apps/web/src/routes/auth/register.rs:36-45` (3 champs : email, nom, mot de passe) |
| 9 | Majeur | Conservation | Les journaux d'audit n'ont **aucune durée** et **aucune purge**. Le registre l'admet en toutes lettres. La table grandit indéfiniment | `docs/registre-traitements.md`, ligne « Logs d'audit » : « Non défini en v1 » ; aucun `DELETE FROM audit_log` dans `apps/api/src/` |
| 10 | Majeur | Conservation | Les jetons consommés ou expirés ne sont jamais supprimés : `email_verification_tokens`, `password_reset_tokens`, et les `invitations` (qui portent l'email d'un tiers). Seul `consumed_at` est renseigné | `apps/api/src/auth/mod.rs` (`consumed_at` posé, jamais de suppression) ; la liste exhaustive des `DELETE FROM` de `apps/api/src/` ne contient aucune de ces trois tables |
| 11 | Majeur | Droit d'accès | L'export ne rend pas tout ce que l'app détient sur la personne, alors que la politique promet « l'intégralité des données que vous avez créées ». **Manquent** : les pièces jointes qu'elle a téléversées (`event_attachments`), ses rappels, ses sessions, son identité Google liée, ses invitations émises, et les données la concernant créées par autrui — être **assigné** à un événement est une donnée personnelle (art. 15), mais l'export filtre partout sur `created_by = auth.user_id` | `apps/api/src/rgpd/mod.rs:20-215` (9 catégories, aucune n'est `event_attachments`) vs `docs/privacy-policy.md:75` |
| 12 | Majeur | Sécurité / art. 32 | Le jeton de réinitialisation de mot de passe voyage en **query string**. Il atterrit dans l'historique du navigateur et dans tout journal d'accès du reverse-proxy. Aucun `Referrer-Policy` n'est posé (la fuite par `Referer` reste théorique ici : la page ne charge aucune ressource externe) | `apps/api/src/auth/mod.rs:249` (`/reset-password?token={token}`) ; aucun en-tête de sécurité dans `infra/Caddyfile` ni dans `apps/*/src/` |
| 13 | Majeur | Sécurité / art. 32 | Aucun en-tête de sécurité n'est émis : ni `Content-Security-Policy`, ni `Strict-Transport-Security`, ni `X-Frame-Options`/`frame-ancestors`, ni `X-Content-Type-Options`, ni `Referrer-Policy` | `grep -riE 'Content-Security-Policy\|X-Frame-Options\|Strict-Transport\|Referrer-Policy' apps/ infra/` : aucun résultat |
| 14 | Majeur | Sécurité / art. 32 | Le `Caddyfile` écoute sur `:80` en clair. En l'état, une mise en ligne publique servirait tout le trafic — identifiants compris — sans TLS ; et comme `SECURE_COOKIES` vaut `true` par défaut, le cookie de session serait rejeté et la connexion cassée. Le fichier documente lui-même l'échange à faire | `infra/Caddyfile:8` (`:80 {`), commentaire `:1-7` ; `apps/api/src/main.rs:96` |
| 15 | Majeur | Organisation | Aucune procédure de violation de données écrite, et aucun registre des violations. La notification CNIL sous 72 h est citée comme une **responsabilité de rôle**, jamais comme une marche à suivre : qui décide, à quel seuil les personnes sont informées, où l'incident est consigné | `docs/privacy-policy.md:20`, `docs/architecture.md:76` et `:230` — les trois seules occurrences de « 72h » du dépôt |
| 16 | Majeur | Conservation | Aucune donnée de connexion n'est journalisée (pas d'IP, pas d'user-agent — ce qui est excellent pour la minimisation). Devenu fournisseur de service de communication au public en ligne, le projet doit **qualifier** si le décret n° 2021-1362 lui impose une conservation, et arbitrer explicitement | Aucune colonne d'IP dans `migrations/` ; `sessions` ne porte que `created_at`, `last_seen_at`, `expires_at` |
| 17 | Majeur | Accessibilité | Aucun lien d'évitement. Sur chaque page authentifiée, la navigation (`<header>` puis `<nav class="tabs">`) précède `<main>` : un utilisateur au clavier retraverse tous les liens à chaque page. WCAG 2.4.1, **niveau A** | `apps/web/src/app.rs:83` (`<header>…</header>` avant `<main>`), `:244` ; `grep -riE 'skip-link\|aller au contenu' apps/web/src/` : aucun résultat |
| 18 | Majeur | Accessibilité | Deux paires de couleurs réellement déclarées passent sous 4,5:1 en thème clair : `.notice.success` **4,43:1** et `.notice.warning` **4,06:1**. Écart à l'objectif AA que `DESIGN.md` se fixe (aucun régime ne l'impose ici, cf. tableau). La CSS le documente et l'attribue déjà à l'issue **#74** | `apps/web/src/style.css:476-477`, valeurs et renvoi à #74 en commentaire `:31-39` ; confirmé par `scripts/contrast.py` |
| 19 | Mineur | Documents légaux | Rien sur le sort des données après le décès (directives post-mortem, art. 85 LIL). Une ligne suffit, et elle manque | `docs/privacy-policy.md` (section « Vos droits », `:73-84`) |
| 20 | Mineur | Politique | La politique décrit un traitement qui n'existe pas : « les modèles IA de suggestion de recettes / OCR sont exécutés localement via Ollama ». Aucun code n'appelle Ollama, et les suggestions sont un tri par règles. Une politique doit décrire ce qui est fait, pas ce qui est prévu | `docs/privacy-policy.md:58-60` vs `apps/api/src/recipes/suggestions.rs:15` ; conteneur inutilisé en `infra/docker-compose.yml:57` |
| 21 | Mineur | Emails | L'objet des rappels recopie le titre de l'événement (`Rappel : <titre>`). Un titre peut être sensible (« IRM », « rendez-vous Dr X ») et l'objet transite en clair chez le relais et dans la boîte du destinataire. Arbitrage de minimisation à poser, pas à subir. L'expéditeur n'est par ailleurs identifié que par `SMTP_FROM` | `apps/api/src/jobs/scheduled_notifications.rs:95-99` |
| 22 | Mineur | Politique | Le registre annonce un hachage « bcrypt/argon2 » ; le code utilise argon2 seul | `docs/registre-traitements.md`, ligne 1 du tableau vs `apps/api/Cargo.toml` (`argon2 = "0.5"`, pas de bcrypt) |
| 23 | Mineur | Accessibilité | Les `<th>` ne portent pas de `scope="col"`, et le tableau du calendrier n'a pas de `<caption>`. Toléré par l'inférence des navigateurs sur un tableau simple, mais c'est le genre d'implicite qui casse dès qu'une colonne d'en-tête apparaît | `apps/web/src/routes/admin/users.rs:113`, `admin/groups.rs:67`, `agenda/calendar.rs:295` et `:321` |
| 24 | Mineur | Accessibilité | Les champs en erreur ne portent pas `aria-invalid`. L'erreur *est* annoncée — le `<span class="field-error">` est placé **dans** le `<label>`, donc il rejoint le nom accessible du champ — mais l'état d'invalidité, lui, n'est pas exposé | `apps/web/src/routes/auth/register.rs:36-45` |

## Détail par domaine

### Documents légaux

**Ce qui est conforme, et pourquoi on le sait.** La politique de confidentialité
est **réellement publiée**, ce qui est rare et mérite d'être constaté
précisément : elle est routée (`apps/web/src/main.rs:66`), servie **sans
session** (`apps/web/src/routes/privacy.rs:26-30` accepte un `CurrentUserOpt`),
et liée depuis les pieds de page de connexion et d'inscription
(`routes/auth/login.rs:38`, `routes/auth/register.rs:56`) — donc lisible **avant**
de consentir. Un test en interdit la régression (`apps/web/src/app.rs:822`).
Le contenu n'est pas dupliqué : `apps/api` sert `docs/privacy-policy.md` verbatim
via `include_str!` (`apps/api/src/rgpd/mod.rs:236-247`), si bien que le document
déployé et celui du contrôle de version ne peuvent pas diverger. Le rendu
markdown ne laisse pas passer de HTML brut.

**Ce qui ne l'est pas.** Le document publié porte `placeholder_name` à la place
du responsable de traitement (constats 1 et 3), sa liste de droits s'arrête à
quatre sur six, il ne mentionne pas la CNIL, et il délègue les durées de
conservation — mention obligatoire — à un fichier du dépôt. Sa date de dernière
mise à jour (2026-07-08) est antérieure à des fonctionnalités qu'il décrit mal
ou pas (constat 6). Il n'existe ni mentions légales ni CGU (constats 2 et 5).

**Non applicable.** Les CGV, le droit de rétractation et la politique de
remboursement : il n'y a aucun paiement, aucune dépendance de paiement, aucun
endpoint de facturation. Une politique cookies séparée n'est pas non plus
requise — voir ci-dessous.

### Cookies, stockage terminal et consentement

**Conforme, et c'est le point fort du dossier.** L'inventaire est complet parce
qu'il est court. Un seul dépôt terminal existe : le cookie de session
`session_id`, posé en `apps/api/src/auth/session.rs:103-111` avec
`http_only(true)`, `same_site(Lax)`, `path("/")`, `max_age` de 30 jours, et
`secure` piloté par `SECURE_COOKIES` — qui vaut `true` par défaut
(`apps/api/src/main.rs:96`). Il est strictement nécessaire au service demandé,
donc **exempté de consentement**. La déconnexion le révoque réellement, côté
navigateur et côté base (`expired_session_cookie`, `revoke_session`).

`grep -riE 'localStorage|sessionStorage|indexedDB|caches\.open'` sur tout
`apps/` ne remonte **rien** : pas de stockage local, pas de service worker, pas
de cache applicatif, pas d'empreinte de navigateur.

**Conclusion à retenir : aucun bandeau cookies n'est requis, et en ajouter un
serait une régression.** Un bandeau qui demande le consentement pour un cookie
strictement nécessaire habitue à cliquer « accepter » sans lire, et c'est
sanctionné comme trompeur. Ce qu'il faut à la place : une section « cookies »
de quelques lignes dans la politique, qui nomme `session_id`, sa finalité et sa
durée. Elle n'y est pas aujourd'hui.

**Consentement des formulaires.** Le formulaire d'inscription ne collecte que
trois champs (email, nom affiché, mot de passe), tous nécessaires à l'exécution
du contrat. **Il n'y a aucune case de consentement, et c'est le bon choix** :
la base légale est le contrat, pas le consentement ; une case « j'accepte »
serait un consentement factice. L'information au point de collecte est assurée
par le lien vers la politique, présent dans le formulaire lui-même.

Le seul consentement au sens strict est l'import calendrier — l'utilisateur
colle volontairement une URL de flux. Il est retirable : la suppression de
l'import efface le flux et, en option, les événements importés
(`apps/api/src/google_calendar/imports.rs:214-228`).

### Traceurs, tiers et transferts hors UE

**Aucun traceur, et la vérification est complète.** Aucun script tiers, aucune
iframe, aucune URL absolue dans le markup ou la CSS : le seul résultat du
`grep` sur les origines externes est le test qui **interdit** le motif
(`apps/web/src/app.rs:1511-1516` : « No CDN, no `@import`, no absolute URL of
any kind »). Les polices sont auto-hébergées et servies depuis le binaire
(`apps/web/assets/fonts/*.woff2`, `@font-face` en `style.css:117-130`), ce qui
évite d'exposer l'IP des visiteurs à un tiers — c'est une règle du `CLAUDE.md`
du projet, et elle est tenue. Aucune télémétrie : ni Sentry, ni PostHog, ni
Datadog, ni OpenTelemetry, ni Matomo, ni Plausible — les manifestes
`apps/*/Cargo.toml` et `e2e/package.json` ne portent aucun SDK de ce type.

**Les seuls flux sortants sont déclenchés par une action volontaire**, et cette
distinction est importante : aucun tiers n'est appelé sur une page vue.

| Tiers | Déclencheur | Ce qui part | Statut |
|---|---|---|---|
| Google (OAuth) | Clic sur « Continuer avec Google » | Identifiants du flux OAuth | **Absent du registre et de la politique** (constat 6) |
| Google (flux iCal) | L'utilisateur colle une URL | Requête serveur→serveur vers l'URL fournie | Au registre, base légale consentement |
| Relais SMTP | Envoi d'un email | Adresse du destinataire, objet, corps | **Sous-traitant non choisi, sans DPA** (constat 7) |
| MinIO, Postgres, Ollama | — | Rien | Auto-hébergés, pas de tiers |

**Transferts hors UE.** Aucun n'est établi par le code, mais aucun n'est
**exclu** non plus, et c'est le point à trancher : l'hôte SMTP est une variable
d'environnement, et Google reçoit des requêtes lors des deux flux ci-dessus.
Le registre affirme « basé UE » sans nommer de fournisseur ni de mécanisme de
transfert. Pour Google, ni décision d'adéquation, ni certification EU-US Data
Privacy Framework, ni clauses contractuelles types ne sont citées. **Nommer le
mécanisme, fournisseur par fournisseur**, est le travail qui reste.

### Données collectées, conservation et purge

**Minimisation : bonne.** Le schéma (13 migrations, `apps/api/migrations/`) ne
porte aucun champ dont la finalité soit introuvable : pas de date de naissance,
pas de téléphone, pas d'adresse postale, pas de genre, pas d'IP, pas
d'user-agent. `users` tient en huit colonnes.

**Catégories particulières (art. 9) : par la bande, et c'est à assumer.**
Aucun champ ne nomme une donnée sensible, mais plusieurs peuvent en recevoir, et
c'est ce qui compte : `events.title` et `events.description` (un rendez-vous
médical), `messages.content` (tout), `recipes` et `grocery_items` (un régime
révélant une conviction religieuse ou une allergie), et surtout
`event_attachments` — un téléversement libre acceptant `image/png`, `image/jpeg`,
`image/webp` et **`application/pdf`** (`apps/web/src/routes/agenda/detail.rs:454`).
Le dépôt lui-même le sait sans le dire : le test
`apps/api/tests/google_calendar_flow.rs:617` nomme sa pièce jointe
`ordonnance.png`. **Des données de santé entreront par cette porte.** Ce n'est
pas un défaut en soi — les personnes les déposent elles-mêmes, dans leur propre
espace — mais cela doit être écrit dans la politique et pesé dans l'AIPD, et
aujourd'hui ni l'un ni l'autre.

**Fuites par les à-côtés : aucune.** C'est la source la plus fréquente de
non-conformité réelle, et elle est propre ici. Les cinq seuls journaux touchant
à un email n'enregistrent que l'erreur de transport, jamais le destinataire ni
le corps (`apps/api/src/auth/mod.rs:119`, `:259`, `:334`, `:479`,
`apps/api/src/groups/mod.rs:403`), ce que `apps/api/src/email.rs:55` documente
explicitement (« Never log recipient/body content »). Les métadonnées d'audit ne
contiennent que des identifiants et des compteurs — vérifié sur les dix sites
d'appel de `audit::record`.

**Chiffrement.** Trois colonnes sont chiffrées applicativement via `pgcrypto`,
avec des clés distinctes : `messages.content`, `oauth_identities.refresh_token_encrypted`,
`calendar_imports.feed_url`. L'URL du flux iCal, qui est un porteur d'accès,
n'est jamais rendue déchiffrée, pas même dans l'export — arbitrage juste et
documenté (`apps/api/src/rgpd/mod.rs:196-199`).

**Conservation : le point faible.** Le compte a une durée et elle est
**implémentée** : `POST /account/delete` pose `deletion_requested_at`, un worker
horaire purge après 30 jours (`apps/api/src/jobs/account_purge.rs`), la demande
est annulable, et un propriétaire unique ne peut pas se supprimer en laissant un
groupe orphelin. C'est une promesse tenue par du code, ce qui est rare. En
revanche les journaux d'audit (constat 9) et les jetons et invitations
(constat 10) n'ont **aucune** durée effective.

**Suppression : ce qu'elle fait et ce qu'elle ne fait pas.** La purge supprime
les identités OAuth et les sessions, et anonymise la ligne `users`. Elle **ne
supprime pas** le contenu créé : événements, messages, dépenses, recettes, et
les objets S3 des pièces jointes restent. **C'est un arbitrage assumé et
documenté** dans les deux documents et dans le code — le contenu appartient
fonctionnellement au groupe partagé. Il se défend, et il doit être **confirmé
par le responsable de traitement** plutôt que hérité : l'art. 17 admet la
conservation lorsque d'autres personnes ont un intérêt légitime au contenu
partagé, mais l'arbitrage doit être posé, pas subi. À noter que les CGU qui
devraient le porter n'existent pas (constat 5).

Le nettoyage du stockage objet, lui, est correctement câblé : les clés S3 sont
collectées **avant** la suppression en cascade, à la suppression d'un événement
comme d'un groupe (`apps/api/src/agenda/events.rs:637-641`,
`apps/api/src/groups/mod.rs:328-333`), et un binaire de réconciliation existe
pour les orphelins (`apps/api/src/bin/reconcile_attachments.rs`).

**Accès et portabilité.** `GET /account/export` existe, retourne du JSON, est
tracé à l'audit, et ne peut jamais exporter les données d'un tiers — il n'y a
pas de paramètre d'utilisateur cible. Mais il est incomplet (constat 11).

### Emails et notifications

Quatre emails partent, et **les quatre sont transactionnels ou demandés par la
personne**. Aucune prospection : aucune relance, aucune newsletter, aucun résumé
non sollicité. **Il n'y a donc ni consentement préalable à recueillir au titre
de l'art. L34-5 CPCE, ni lien de désinscription à ajouter** — et c'est à écrire,
parce qu'un lecteur ne distingue pas « non applicable » de « oublié ».

| Email | Nature | Déclencheur | Réserve |
|---|---|---|---|
| Vérification d'adresse | Transactionnel | Inscription | — |
| Réinitialisation de mot de passe | Transactionnel | Demande de la personne | Jeton en query string (constat 12) |
| Invitation à un groupe | **Tiers non consentant** | Un membre saisit une adresse | **Art. 14 non respecté (constat 4)** |
| Rappel d'événement | Notification demandée | Rappel créé par l'utilisateur | Titre recopié en objet (constat 21) |

Les rappels sont désactivables — ils n'existent que si la personne les a créés,
et `DELETE` sur le rappel les retire (`apps/api/src/agenda/reminders.rs:140`).
Ils ne partent qu'au créateur de l'événement, pas aux assignés
(`scheduled_notifications.rs:82-90`, jointure sur `e.created_by`) : cohérent
avec la minimisation, mais à vérifier côté produit — un assigné qui ne reçoit
rien est peut-être un défaut fonctionnel plutôt qu'un choix.

### Organisation — AIPD, violations, registre

**Registre (art. 30) : il existe**, ce qui vaut d'être noté, et il est structuré
par finalité avec base légale, durée et destinataires. Il est en revanche daté du
2026-07-08 et incomplet (constat 6), son sous-traitant principal n'est pas choisi
(constat 7), et il laisse une durée « non définie » (constat 9).

**AIPD (art. 35) : à trancher, et l'audit conclut qu'elle est probablement
requise.** Le test est le croisement de deux critères au moins ; trois sont
plausiblement réunis dès lors que le service est **ouvert au grand public** :

1. **Données sensibles** — pas par un champ qui les nomme, mais par les pièces
   jointes libres et le texte libre (voir plus haut). Critère matériellement
   rempli, quelle que soit l'intention.
2. **Personnes vulnérables** — des mineurs, structurellement, dans une
   application de foyer (constat 8).
3. **Collecte à grande échelle** — non rempli aujourd'hui, mais c'est
   précisément ce que le cadrage « service grand public ouvert » vise.

Les deux premiers suffisent. **Recommandation : mener l'AIPD avant l'ouverture
publique**, pas après. C'est le genre d'obligation qui se décide avant la mise en
ligne ou jamais.

**DPO : non requis.** Ni organisme public, ni suivi systématique à grande
échelle, ni traitement de données sensibles à grande échelle **à titre
d'activité de base** — les données sensibles arrivent incidemment, elles ne sont
pas l'objet du service. La position actuelle (« contact documenté, pas de DPO
formel », `docs/privacy-policy.md:14-18`) est **correcte**, à condition que le
contact existe — il n'existe pas (constat 1).

**Violation de données : rien (constat 15).**

### Mobile, desktop et magasins

**Non applicable, parce qu'aucune cible native n'existe, et la vérification a
été faite plutôt que supposée.** La recherche de `capacitor.config.*`,
`AndroidManifest.xml`, `Info.plist`, `*.xcodeproj`, `app.json`, `pubspec.yaml`,
`build.gradle*`, `tauri.conf.json`, `electron-builder.*` et `forge.config.*` sur
tout le dépôt ne remonte rien, et aucun répertoire `android/`, `ios/`,
`src-tauri/` ou `electron/` n'existe. Le workspace Cargo ne déclare que trois
membres, tous serveur ou partagés (`Cargo.toml`).

**Ce n'est pas non plus une PWA** : aucun `manifest.webmanifest`, aucun
`manifest.json`, aucun service worker, et aucun appel à
`Notification.requestPermission` ni à une API de géolocalisation. Il n'y a donc
ni stockage hors cookies, ni permission de terminal à déclarer.

Aucune déclaration magasin (Privacy Labels d'Apple, Data safety de Google),
aucune obligation de suppression de compte in-app, aucune sécurité de shell
desktop ne s'appliquent. **Le jour d'un portage, tout ce volet est à rouvrir**,
et le volet accessibilité serait à refaire si l'app native était distincte
plutôt qu'un shell embarquant le web.

### Accessibilité

**Régime : aucun régime contraignant** (voir le tableau du cadre juridique :
ni EAA par catégorie, ni RGAA). **Les constats ci-dessous sont donc des écarts à
un objectif que le projet s'est lui-même donné** — `DESIGN.md` fixe AA — et non
des manquements légaux. Un seul relève du niveau A du WCAG et mérite d'être
traité comme tel : le lien d'évitement.

**Méthode : lecture statique**, légitime ici parce que le HTML est écrit en
clair dans des `format!` et des `view!` Leptos rendus côté serveur, sans
hydratation ni bibliothèque de composants tierce : le `grep` voit le DOM réel.
La seule surface pilotée par JavaScript est le fil de messagerie (WebSocket).

**Ce qui est conforme, et pourquoi on le sait.**

- **Structure** : `<html lang="fr">`, un `<title>` unique par page, `<main>`,
  `<header>`, `<nav>` (`apps/web/src/app.rs:107-118`, `:83`, `:244`). Un test
  vérifie que `<header>` n'est plus **dans** `<main>` (`app.rs:1825-1832`).
- **Formulaires** : les 71 champs de `apps/web/src/routes/` sont enveloppés dans
  un `<label>` — vérifié champ par champ, les deux seuls candidats suspects
  (`groups/list.rs:91`, `groups/members.rs:162`) le sont bien. Le seul
  `aria-label` autonome est justifié (`grocery_list/list.rs:107`, un champ de
  prix répété par ligne). Les groupes de cases sont en `<fieldset>`/`<legend>`
  (`agenda/new.rs:103-126`). Les `placeholder` ne servent jamais d'étiquette.
- **Erreurs** : la saisie est repopulée, et le message est rendu **au niveau du
  champ** — placé dans le `<label>`, il rejoint le nom accessible du champ et
  est donc annoncé avec lui (`routes/auth/register.rs:36-45`). C'est un montage
  qui marche.
- **Clavier** : aucun `tabindex`, aucun `onClick` sur un élément non
  interactif, aucune modale maison, aucun contenu révélé au seul `:hover`. Le
  seul script de page est un `<button type="button">` avec `aria-label` qui bascule
  son propre libellé (`app.rs:289`).
- **Focus** : `outline: none` n'apparaît nulle part. Les cliquables prennent une
  `outline` de 2 px, et les champs une `outline` **transparente** doublée d'un
  `box-shadow` — précisément pour survivre au mode contrastes forcés, qui ignore
  les ombres (`style.css:534-547`). C'est une attention rarement vue.
- **Mouvement** : `prefers-reduced-motion` est respecté en une déclaration
  (`style.css:574`), et aucune animation d'entrée n'existe.
- **Médias temporels** : **non applicable**, il n'y a ni `<video>`, ni
  `<audio>`, ni lecteur embarqué, ni GIF porteur d'information.
- **Images** : une seule image dans toute l'application, un SVG décoratif
  correctement neutralisé par `aria-hidden="true"` dans un bouton qui porte son
  nom accessible (`app.rs:289`). Aucun `<img>`, aucun `background-image`.
- **Couleur seule** : traitée explicitement — l'initiale d'un membre est
  doublée de son nom (`style.css:395-397`), le badge « Stock bas » porte du
  texte, les notices ont un texte et une bordure.
- **Régions live** : le fil de messagerie déclare `aria-live="polite"` sur un
  élément **présent dans le DOM avant** toute mise à jour
  (`routes/messagerie/thread.rs:426`, `:799`) — la condition que presque tout le
  monde rate. Ailleurs, chaque navigation est un rechargement complet, donc il
  n'y a pas de contenu injecté à annoncer.

**Ce qui ne l'est pas** : le lien d'évitement (17), les deux paires de contraste
sous AA en thème clair (18), l'absence de `scope`/`<caption>` (23) et
d'`aria-invalid` (24).

**Sur les contrastes, ce que la mesure couvre exactement.** Le script a lu
`apps/web/src/style.css` et résolu **deux thèmes** (clair et sombre — le thème
sombre est piloté par `prefers-color-scheme`, pas par JavaScript, donc il n'a pas
échappé à la mesure). Sur les **11 paires réellement déclarées**, 9 passent, 2
échouent, et les deux échecs sont **en thème clair** — l'inverse du cas habituel,
parce que ce dépôt interdit les replis `var(--x, #hex)` qui cassent
silencieusement les thèmes sombres. Les ratios du thème sombre sont tous au-dessus
de 4,8:1.

La **matrice de jetons appariés par leur nom est une présomption**, et elle a été
vérifiée avant d'être écartée plutôt que recopiée : les KO qu'elle affiche sont
du bruit. `--accent-fg` (blanc) et `--accent-soft` ne sont jamais peints
ensemble : `--accent-fg` est le texte des surfaces pleines
(`style.css:261`, `:273`, `:379`), `--accent-soft` un fond qui reçoit `--accent`
(`:555`, mesuré 5,01:1 et 4,88:1). De même `--muted` sur `--accent-soft`
(4,38:1 en sombre) ne correspond à aucune règle : `.badge` est peint sur
`--border`, et il **fixe explicitement sa couleur** pour ne pas hériter du
`--muted` d'une ligne barrée — la CSS documente ce piège et s'en défend
(`style.css:364-375`).

**Ce que la mesure n'a pas couvert**, et qui se cite même quand c'est court :
8 valeurs non résolues, toutes le même cas — un fond `transparent` sur
`.pw-toggle` et les boutons secondaires, dont le contraste réel dépend de la
surface qui les porte et n'est donc mesurable qu'au rendu. Et **une seule**
couleur écrite hors des feuilles : `apps/web/src/assets.rs:476`, un
`body{background:#f00}` qui est un **leurre de test** (« the decoy »), pas de
l'interface. Aucune couleur d'interface n'échappe donc au basculement de thème.

**Aucune vérification automatisée n'a eu lieu, et il n'existe aucun harnais pour
en faire** : `e2e/package.json` ne contient ni `axe-core`, ni `pa11y`, ni
`lighthouse`. La suite Playwright existante serait le chemin le plus court pour
en injecter un — c'est un travail à proposer, pas un acquis.

## Non vérifié

Ce qui suit demanderait un rendu, un environnement ou une décision hors de portée
de cet audit, et n'a **pas** été contrôlé :

- **Tout ce qui exige un navigateur** : l'ordre de tabulation réel, la
  visibilité du focus à chaque étape, le comportement d'un lecteur d'écran sur
  la messagerie en direct et sur les erreurs de formulaire, le reflow à 320 px,
  le zoom texte à 200 %, et le rendu effectif en mode contrastes forcés. Le
  serveur MCP Playwright n'a pas démarré dans cette session (`npx` introuvable),
  et aucune capture n'a été prise.
- **Le contraste réel des quatre règles à fond `transparent`** (`.pw-toggle`,
  boutons secondaires), qui dépend de la surface qui les porte.
- **La configuration de production** : la valeur effective de `SECURE_COOKIES`,
  l'hôte SMTP réellement retenu et sa localisation, la présence de TLS, la
  politique de journalisation de Caddy, la rétention et le chiffrement des
  sauvegardes Postgres et MinIO. Rien de tout cela n'est décidable depuis le
  dépôt : ce sont des variables d'environnement et des choix d'exploitation.
- **Les sauvegardes** : leur existence, leur horizon, et si une purge de compte
  les atteint. Aucun dispositif de sauvegarde n'apparaît dans `infra/`.
- **L'accessibilité des emails** (HTML/texte, contrastes) : les corps sont en
  texte brut, ce qui écarte l'essentiel du sujet, mais aucun n'a été rendu.
- **Les décisions qui appartiennent au responsable de traitement** : la
  qualification au regard du décret n° 2021-1362, la conclusion de l'AIPD, le
  maintien du contenu après suppression de compte, et le choix du sous-traitant
  SMTP.
- **Le droit étranger** : si des utilisateurs hors UE s'inscrivent, le CCPA/CPRA,
  le UK GDPR ou la LPD suisse pourraient s'appliquer. **Ils n'ont pas été
  audités.**
