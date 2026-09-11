# Manage Our Home — Architecture Proposal (v2)

Status: draft for discussion, nothing implemented yet. This document only
challenges and revises the stack; it does not replace the feature specs
(those come later, one epic at a time via `/spec`).

**Target bar: enterprise-deployable in a French company, not a home-lab
project.** Every choice below must hold up to RGPD compliance and to the
security level expected of a product actually sold/deployed to French
businesses — not just "good enough for one household." Concretely this
means: defense in depth at the DB level (not app-code isolation alone),
a real legal basis and documentation for every category of personal data
processed, data subject rights (access/export/erasure/rectification)
implemented as real features not TODOs, breach-notification capability,
and security practices (dependency scanning, secrets hygiene, encryption
at rest/in transit, audit logging) that would pass a client security
questionnaire.

## Corrections applied from review

1. **Messagerie needs real-time transport.** Added explicitly: Axum's native
   WebSocket support. No separate pub/sub broker needed at household scale —
   Postgres `LISTEN/NOTIFY` fans out messages between Axum instances if we
   ever run more than one.
2. **File attachments (event files, fridge photos) need object storage.**
   Added **MinIO** (self-hosted, S3-compatible). Fridge-scan vision model is
   split out as its own future epic, not bundled into core storage.
3. **Google Calendar sync scoped down.** v1 is **one-way import** (Google →
   MHome), not bidirectional real-time sync. Bidirectional sync is a
   substantially harder problem (conflict resolution, webhook subscriptions,
   token refresh at scale) and isn't justified until import is proven useful.
4. **Reminders must survive restarts/deploys.** Dropped the in-process
   `tokio-cron-scheduler` as the source of truth. Replaced with a **persisted
   job queue table** (`scheduled_notifications`), polled by a worker on an
   interval. The in-memory scheduler crate is now optional (only for very
   short-lived, non-critical ticks) — reminders themselves are DB-backed.
5. **Recipe suggestions are a recommendation feature, not CRUD.** Called out
   as its own scoped sub-problem with a concrete (if simple) v1 algorithm,
   customizable per user.
6. **Budget: local price research.** Added as an explicit capability, scoped
   as v2 (needs a data source decision — see Open Questions).
7. **RLS enabled from v1, not deferred.** Given the enterprise-deployable
   bar, app-code-only isolation (a missed `WHERE family_id = ...` away from
   a cross-tenant data leak) is not an acceptable long-term posture — it's
   the kind of finding that fails a client security review. Postgres
   Row-Level Security is turned on from the start as defense in depth
   alongside app-code scoping: every connection sets `app.family_id` via
   `SET LOCAL` at the start of a request/transaction, and RLS policies on
   every tenant-scoped table enforce that boundary at the DB layer even if
   application code has a bug. The extra migration/debugging overhead is
   accepted as the cost of doing this correctly, not deferred as a "v2
   hardening pass."
8. **Local LLM via Ollama** for any AI-assisted feature (recipe suggestions,
   fridge-scan OCR/detection later, message summarization if ever wanted).
   Self-hosted, no data leaves your server, no per-call API cost.
9. **Every technology below has a one-line justification** and is either (a)
   a Tier-1 maintained project with an active release cadence and no open
   critical CVEs, or (b) infrastructure you already run. Nothing obscure.
10. **RGPD compliance is a first-class v1 requirement, not a later pass.**
    Every table storing personal data (users, family members, messages,
    fridge photos, calendar events) is mapped in a data processing register
    (registre des traitements) with its legal basis (contrat / intérêt
    légitime / consentement). v1 ships: a real privacy policy (politique de
    confidentialité) covering what's collected, why, retention period, and
    who it's shared with (Google, for calendar import); a data export
    endpoint (portabilité, Art. 20); an account+data deletion flow that
    actually purges personal data within a defined delay (droit à l'effacement,
    Art. 17); explicit consent capture for Google OAuth data access; and a
    documented data retention/deletion policy per data category (e.g.
    messages, fridge photos) rather than "keep forever by default."
11. **Security practices for an enterprise bar.** Dependency vulnerability
    scanning (e.g. `cargo audit` across the whole Rust workspace in CI), TLS enforced
    everywhere (including internal service-to-service where feasible),
    audit logging of sensitive actions (auth events, data export/deletion,
    cross-family admin actions), and a documented breach-notification
    process (CNIL notification within 72h if a breach affecting personal
    data occurs) are treated as v1 requirements, not backlog items.

## Revised stack

| Layer | Choice | Justification |
|---|---|---|
| API | **Rust + Axum** | Tokio-based, actively maintained by the Tokio team, first-class typed extractors, native WebSocket support (covers messagerie) — avoids adding a second framework for real-time. |
| DB access | **SQLx** | Compile-time query checking against a real schema catches drift before runtime; no ORM behavior magic to audit for security. Widely used in production Rust services. |
| DB | **PostgreSQL 16** | Mature, `pgcrypto` for column-level encryption of PII, `LISTEN/NOTIFY` for lightweight pub/sub if we scale past one API instance, JSONB for flexible fields (event notes, recipe metadata). |
| Tenant isolation (v1) | **Postgres Row-Level Security policies on every tenant-scoped table** (via `SET LOCAL app.family_id` per request), backed by app-code scoping + integration tests as a second layer | Defense in depth from day one — a bug in application code can no longer leak data across families, which is required to pass a client security review at enterprise deployment scale. |
| Auth | **argon2 (password hashing) + custom email/password + Google OAuth2 via the `oauth2` crate** | `argon2` is the current OWASP-recommended password hash; `oauth2` crate is the de facto standard Rust OAuth2 client, actively maintained. |
| Real-time messaging | **Axum WebSockets** | Native to the framework already in use; no extra dependency for the messagerie feature. |
| Object storage | **MinIO (self-hosted, S3 API)** | Needed for event file attachments and (later) fridge-scan photos; S3 API means any Rust S3 client (`aws-sdk-s3`) works, and it's swappable for real S3 later without an app rewrite. |
| Scheduled reminders | **Postgres-backed job queue table + polling worker** | Survives restarts/deploys by design — the queue lives in the DB, not process memory. A cron-style in-memory scheduler is optional only for non-critical periodic ticks (e.g. cache refresh), never for user-facing reminders. |
| Local AI (recipes, later OCR) | **Ollama**, self-hosted | Keeps all data (what you ate, fridge photos) local — matches the RGPD/self-hosted stance already established; no third-party API cost or data exposure. |
| Web frontend | **Leptos (SSR via `leptos_axum`)** | Superseded 2026-07-08 (see Front epic #1, GH issue #15): keeps the whole stack in Rust — shared DTOs/validation between `apps/api` and `apps/web` via a new `apps/shared` crate compiled natively and to `wasm32-unknown-unknown`, no duplicated request/response types or a second language's tooling/CI/audit surface to maintain. Ecosystem gaps (calendar widgets, rich components) are accepted and closed by hand-rolling rather than pulling in a JS framework, in line with the "one Rust monolith" posture already used for the backend. |
| Mobile client | **Capacitor wrapping the Leptos web app** | Targeted for v1.1 (not the Auth front epic). Reuses the `apps/web` build instead of maintaining a second UI; native shell only for what genuinely needs it (push notifications, camera for fridge-scan, **local reminder notifications that must fire with no network** — see Questions résolues #4). Cross-origin cookie handling for the Capacitor WebView is an open question, to be resolved in that future epic. |
| Reverse proxy / TLS | **Caddy** | Automatic TLS renewal, minimal config, fits a home-hosted single-server deployment. |
| Deployment | **Docker Compose** on your home server | Matches "home hosted"; one compose file for Axum, Postgres, MinIO, Ollama, Caddy; straightforward volume backup for Postgres + MinIO data. |
| Secrets / encryption keys | **sops** (age-backed) for encrypting secrets at rest in the repo, keys injected as env vars at container start, never committed | Standard practice for home-hosted secret management without a full vault service. |
| PII encryption at rest | **`pgcrypto`** for sensitive columns (e.g. message content, fridge photo metadata) in addition to disk-level encryption on the Postgres/MinIO volumes | RGPD Art. 32 ("mesures techniques appropriées"); protects data even if a DB backup or disk is exfiltrated. |
| Dependency / vuln scanning | **`cargo audit`** in CI, blocking on high/critical, across the whole workspace (`apps/api`, `apps/web`, `apps/shared`) | Baseline expected by any enterprise security questionnaire; catches known CVEs before deploy. No `npm audit` needed — the frontend is Rust/Leptos, not a JS framework, so there's no separate `package.json` dependency graph to scan. |
| Transactional email (v1) | **SMTP relay via a EU transactional provider (Brevo or Mailjet)**, sent from Rust via the `lettre` crate | Email verification, password reset, and email-channel notifications (from `scheduled_notifications`) need reliable deliverability (SPF/DKIM/DMARC, IP reputation) that's impractical to run well as a single maintainer. Both providers are EU-based (FR), so they slot into the same documented-subprocessor model already used for Google OAuth (DPA, registre des traitements entry). Free tier covers household-scale volume (~200-300 emails/day) at €0; only becomes a paid line item if usage scales beyond that. |
| Transactional email (long-term) | **Self-hosted SMTP server** (e.g. Postfix or Mailu, in the Docker Compose stack) | Planned migration once the operational burden (IP reputation, SPF/DKIM/DMARC, anti-spam/blocklist monitoring) is justified — brings mail fully in-house, consistent with the self-hosted/no-third-party-data-leaves-the-server posture used for Ollama and MinIO. Deferred because getting deliverability right without one is a real risk of landing in spam; kept as an explicit target so the app-level email code (via `lettre`, SMTP-based) needs no rewrite — only a config/endpoint swap when migrating. |

## Scoping change: what's now explicitly out of v1 core

These were folded into your original list but need their own spec later —
calling them out now so they aren't silently lost:

- **Fridge scan (OCR/object detection on photos)** — needs a vision model
  decision (local via Ollama-compatible vision model vs. cloud API) and its
  own accuracy/cost tradeoff discussion.
- **Bidirectional real-time Google Calendar sync** — v1 ships one-way import
  only; export/sync-back is a separate future epic.
- **Recipe recommendation algorithm** — needs its own spec: inputs (last-2-weeks
  meals, season, per-user customization), a concrete v1 rule (not ML) with room
  to swap in an Ollama-backed suggestion later.
- **Local price lookup for budget** — manual entry for v1 (decided); scraper/
  API integration is a future update once the feature proves useful.

## Epic scoping clarifications (2026-07-07)

Resolved ambiguities between overlapping/undefined epics from `idea.md`:

- **Groups**: a user can belong to multiple groups/families — not previously
  stated; must be reflected in the Auth+Groups data model (no single
  `family_id` on the user row).
- **Tasks with reminder**: a task is an agenda event type, not a separate
  entity — same reminder/recurrence/assignment mechanics as regular events.
- **Stocks**: manual entry in v1 (fridge scan automates/enriches entry later,
  as its own epic). Reorder threshold is defined per article, shared at the
  family level (not per-user).
- **Grocery list**: one shared list per family (not per user). Auto-populated
  from (a) missing ingredients of a chosen recipe compared against stock
  levels, and (b) recurring items to rebuy (custom per user) based on stock
  levels. This makes Stocks and Recipes (which must emit a structured
  ingredient list) hard dependencies before Grocery List can be spec'd.
- **User admin**: a global technical superadmin, distinct from the
  owner/admin/standard group roles — overlaps with the "administrateur
  technique / mainteneur" role already named in the RGPD section below. To be
  scoped as its own epic.
- **Messagerie**: one discussion thread per family (no user-to-user DMs).
  Text only in v1; attachments deferred.
- **Budget**: tied to the grocery list — manually entered price per item,
  cumulated per period. Not a general expense tracker (rent, bills, etc.).

Recommended spec order given these dependencies: Groups (multi-family) →
Agenda → Stocks → Recipes → Grocery list → Budget. Messagerie and User admin
are independent and can be spec'd any time after Groups.

## Sécurité

RLS est activé dès v1 (voir correction #7 plus haut) : au niveau de fiabilité
attendu pour un déploiement en entreprise française, l'isolation applicative
seule n'est pas un niveau de défense suffisant — une requête mal scopée dans
un handler ne doit jamais pouvoir fuiter des données inter-familles. RLS
apporte cette garantie au niveau de la base, en complément (pas en
remplacement) de l'isolation applicative ci-dessous.

**Isolation multi-tenant (RLS + app-code, v1)**
- Policies RLS sur chaque table scoped par famille, activées via
  `SET LOCAL app.family_id` au début de chaque requête/transaction.
- En complément, un helper de requête centralisé qui injecte `family_id` sur
  toute requête scoped — jamais de SQL avec scoping manuel dispersé dans les
  handlers.
- Tests d'intégration qui prouvent qu'un user de la famille A ne peut ni lire
  ni écrire les données de la famille B, sur chaque endpoint sensible, y
  compris en simulant un bug applicatif (scoping manquant) pour vérifier que
  RLS bloque quand même l'accès.

**Auth**
- argon2id, paramètres mémoire/temps calibrés sur le matériel cible (~250ms
  par hash).
- Cookies de session : `HttpOnly`, `Secure`, `SameSite=Lax` (ou `Strict` si
  pas de flux cross-site requis).
- Rate-limiting sur `/login` et `/register` dès que l'app est exposée sur
  internet (brute force, credential stuffing).
- OAuth2 Google : validation du `state` (CSRF), tokens de refresh chiffrés
  via `pgcrypto`, jamais stockés en clair.

**Données en base**
- `pgcrypto` sur les PII sensibles (emails ; notes de messagerie si jugées
  sensibles).
- Contraintes `NOT NULL` / `CHECK` en DB en plus de la validation
  applicative — la DB est le dernier rempart.
- Backups (Postgres + MinIO) chiffrés et testés par restauration régulière.

**Transport / infra**
- TLS partout via Caddy, y compris en interne si les services ne sont pas
  co-localisés.
- Secrets exclusivement via sops ; vérifier qu'aucun secret/token n'atterrit
  dans les logs `tracing` (payloads complets à surveiller).
- WebSockets : authentifier la connexion et revalider l'appartenance à la
  famille à chaque message, pas seulement au handshake.

**Uploads (MinIO)**
- Validation du type MIME réel (pas de l'extension) et de la taille avant
  stockage.
- URLs présignées à durée courte, pas de bucket public.
- Restriction des types de fichiers acceptés (photos frigo, pièces jointes
  events).

**Général**
- `cargo audit` régulier (RustSec advisory DB), cohérent avec l'exigence
  "pas de lib obscure ou compromise".
- Logs d'audit sur les actions sensibles (changement de rôle, suppression de
  compte, export de données) — utile aussi pour le RGPD.

## Questions résolues

1. **Fridge-scan vision model : IA locale.** Décision confirmée — modèle de
   vision via Ollama, self-hosted. Aucune photo de frigo ne quitte le
   serveur ; cohérent avec la posture RGPD/self-hosted déjà retenue pour le
   reste de la stack. Pas d'accord de sous-traitance tiers à négocier.
2. **Budget, source des prix : saisie manuelle pour v1.** Le scraping/API
   d'un site tiers est reporté à une future mise à jour, une fois la
   fonctionnalité de recherche de prix jugée utile — évite l'exposition
   légale (ToS) tant que ce n'est pas justifié.
3. **Responsable RGPD (data controller) : placeholder_name, contributeur
   unique du projet.** placeholder_name porte donc l'ensemble des rôles
   nécessaires au déploiement :
   - **Data controller / responsable de traitement** — responsable du
     registre des traitements, de la politique de confidentialité, et des
     décisions sur la base légale de chaque catégorie de données.
   - **Contact vie privée / DPO de fait** — à l'échelle actuelle (déploiement
     familial/home-lab à visée enterprise-ready), un contact vie privée
     documenté suffit ; un DPO formel n'est pas requis tant que le volume et
     la nature des traitements ne l'imposent pas légalement (à réévaluer si
     le produit est commercialisé/déployé chez des tiers).
   - **Administrateur technique / mainteneur** — seul commiteur, donc seul
     responsable de la sécurité applicative, des migrations DB, de la
     rotation des secrets (sops), et de la réponse à incident (notification
     CNIL sous 72h en cas de breach).
4. **Rappels mobiles hors ligne (2026-09-03) : exigence sine qua non,
   compatible avec le front Rust actuel.** La notification d'un rappel
   d'événement doit se déclencher même sans réseau au moment prévu. Vérifié :
   ça ne dépend pas du langage du front (Rust/Leptos vs JS), mais du wrapper
   mobile — c'est le plugin natif **Local Notifications** de Capacitor
   (ligne « Mobile client » ci-dessus) qui programme le rappel au niveau de
   l'OS (`UNUserNotificationCenter` iOS, `AlarmManager` Android) ; une fois
   programmé, il se déclenche sans réseau et sans que l'app tourne. L'appel au
   plugin se fait par un pont JS, du même genre que `live_script`
   (`apps/web/src/routes/messagerie/thread.rs`) — pas un changement
   d'architecture front. Écarté explicitement : le PWA pur, sans coquille
   Capacitor. L'API web qui aurait permis ça côté navigateur (*Notification
   Triggers*, `showTrigger`) a été abandonnée par Chrome en décembre 2021
   après deux origin trials, sans jamais atteindre le canal stable, jamais
   standardisée ni portée ailleurs ; le Web Push classique redemande un
   aller-retour réseau au moment de l'émission, donc échoue au même test.
   Voir `front-stack-study.md` § 2.3 pour le détail et les contraintes qui en
   découlent (plafond iOS de 64 notifications programmées par app, permission
   `SCHEDULE_EXACT_ALARM` sur Android 12+, resynchronisation à l'ouverture de
   l'app).

Epic #1 (Auth + Groups) has landed on `main` (`apps/api/`). Next step is
spec'ing the remaining epics one at a time via `/spec`, in the order given
above, starting with Agenda.

## v2 — Déploiement multi-famille (2026-07-08)

Décidé le 2026-07-08 : v1 restait volontairement local/VPN (voir
`v1-scope.md` #14). L'objectif change — déploiement à **10-15 familles
externes**, cible **la semaine prochaine** une fois v1 validé. Ça fait
passer plusieurs points de "nice to have" à **bloquant avant le premier
déploiement**.

**Hébergement**
- Un VPS (Hetzner/Scaleway, ~5-10€/mois, 2 vCPU/4 Go suffisent pour cette
  échelle) plutôt qu'un cloud managé gratuit — même stack Docker Compose
  qu'en local (`infra/docker-compose.yml`), pas de divergence dev/prod, et
  c'est le pont naturel vers le self-host définitif chez soi plus tard.
- Exposition publique réelle : TLS via Caddy devient obligatoire (déjà
  scaffoldé dans `infra/Caddyfile`), plus optionnel comme en v1 local.

**Superadmin (item #8 du tracker v1)**
- Un rôle superadmin technique global, distinct des rôles
  owner/admin/standard par groupe — support/maintenance uniquement, un seul
  compte (vous) pour l'instant. Pas de gestion d'équipe support à prévoir
  tant que c'est le cas.

**RGPD (item #12 du tracker v1) — bloquant dès le premier déploiement**
- Export de données (Art. 20), suppression de compte effective (Art. 17),
  politique de confidentialité réelle, base légale documentée par
  catégorie de donnée. Ne peut plus être différé une fois que des données
  appartenant à d'autres familles sont hébergées.

**Backups**
- Postgres + MinIO chiffrés et **testés par restauration** avant le
  premier déploiement — pas de première restauration en situation de
  crise réelle.

**CI/CD**
- Compléter la CI actuelle (`ci.yml` : fmt/clippy/test) avec `cargo audit`
  (item #13 du tracker, encore manquant), puis un pipeline de déploiement
  (build image → push → déployer sur le VPS).

**Monitoring minimal**
- Pas encore dans l'architecture : au moins un check d'uptime et des logs
  centralisés/consultables, pour savoir qu'une instance servant d'autres
  familles est down avant qu'un utilisateur ne le signale.

Détail par item : voir `docs/v2-deployment.md`.

## Version Y — trajectoire microservices/Kubernetes (2026-07-08)

Décidé le 2026-07-08 : au-delà de v1/v2, une trajectoire de formation vers
une architecture microservices orchestrée par Kubernetes est actée —
**stratégie monolith-first, migration progressive**, pas un big-bang.
Objectif explicitement pédagogique (se former à une architecture
d'entreprise), pas une réponse à un besoin de charge : à l'échelle
10-15 familles rien ne justifierait ce virage sur des critères de charge
seuls.

Principe : le monolithe modulaire (`apps/api`) reste la plateforme de
référence pour v1/v2. Chaque extraction de service se fait une par une, sur
justification concrète (contrainte matérielle différente, pas juste
"séparation par domaine"). Le seul candidat identifié pour l'instant est
**Ollama** (futur epic fridge-scan/vision), déjà noté ci-dessus comme "le
candidat pour un vrai service séparé". Kubernetes n'est introduit qu'à ce
moment-là, pas avant — Docker Compose reste l'outil de déploiement de
v1/v2.

Stack cible, séquencement, discipline à respecter dès maintenant dans le
monolithe (frontières de module, pas de SQL cross-module, stateless par
requête) et questions non tranchées (choix du cluster, GPU pour Ollama,
cas d'usage Kafka, stateful sur K8s ou non, mesh, registry) : voir
`docs/version-y-microservices.md`.

## Repo layout (monorepo)

This repo is a monorepo: it holds every service around, and including,
the MHome application itself — not just the API crate. One repo, one CI,
one docker-compose stack, coherent versioning across services.

```
manage_our_home/
├── apps/
│   ├── api/          # Rust + Axum core backend (Auth, Groups, Agenda,
│   │                 # Stocks, Recipes, Grocery list, Budget, Messagerie).
│   │                 # Also hosts the scheduled_notifications worker as a
│   │                 # second [[bin]] target in the same crate — it shares
│   │                 # the SQLx models and DB pool, no need to split it out
│   │                 # as a separate service at this scale.
│   ├── web/          # Leptos SSR frontend (calendar, chat, recipes UI, ...)
│   ├── shared/       # DTOs + pure validation shared between apps/api and
│   │                 # apps/web (native + wasm32-unknown-unknown)
│   └── mobile/       # Capacitor shell wrapping apps/web (v1.1)
├── infra/
│   ├── docker-compose.yml   # api, postgres, minio, ollama, caddy
│   ├── Caddyfile
│   └── postgres/            # init scripts, extensions (pgcrypto)
├── docs/
│   ├── architecture.md      # this file
│   ├── idea.md
│   ├── v1-scope.md          # per-service status tracker for v1
│   └── notes-issue-1-qa.md
└── README.md
```

**Workspace root:** a single root `Cargo.toml` (virtual workspace, `members
= ["apps/api", "apps/web", "apps/shared"]`) ties every service together —
`apps/web` and `apps/shared` are Rust crates like `apps/api`, not a
separate npm workspace, so `cargo audit` runs once across the whole repo
with no second dependency graph to scan. `apps/mobile` (Capacitor, v1.1)
will need its own minimal `package.json` when it lands, since Capacitor
itself is a JS tool, but it wraps the built `apps/web` output rather than
housing app logic.

**`apps/api` history:** the Auth+Groups epic (issue #1) was started before
this layout was settled, as a flat crate at repo root. It has since been
moved into `apps/api/` (plain `git mv`, no code changes) now that it's
merged to `main`.

**Why not split every future service into its own top-level crate now:**
at this stage (one maintainer, household scale) most "services" listed in
`docs/v1-scope.md` are features inside `apps/api`, not deployable units —
splitting them out is unjustified operational overhead (more compose
services, more inter-service auth, more network hops) until one of them
needs independent scaling or a different language runtime (e.g. the
future Ollama-backed fridge-scan/OCR epic, which is the one candidate for
a real separate service since it's GPU/model-bound).
