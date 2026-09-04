# Étude — faut-il réécrire le front en JS/TS ?

État au 2026-08-28. Ce document est un **constat et un arbitrage**, pas un plan
d'implémentation. Il répond à une question posée en une phrase — « et si on
passait le front à une techno JS/TS, notamment pour le paquetage mobile ? » —
et il existe pour que la prochaine personne qui la repose n'ait pas à refaire
l'enquête.

La cible visuelle est dans [`DESIGN.md`](../DESIGN.md) ; les contraintes
techniques du front sont rappelées dans
[`docs/design-audit.md`](design-audit.md) § 1 ; la décision mobile déjà prise
est dans [`docs/architecture.md`](architecture.md) l. 93.

---

## 0. Réponse courte

**Ne pas réécrire.** L'argument principal — le paquetage mobile — ne tient pas :
Capacitor emballe une WebView, pas un framework JS, et dans sa variante la moins
chère il fait tourner `apps/web` tel quel. La « question ouverte » des cookies
laissée par `architecture.md` n'est pas un problème que le JS résout, c'est un
problème que le JS crée.

Deux arguments restent sérieux — le hors-ligne et la vélocité de design — et
aucun des deux n'est aujourd'hui un goulot mesuré. Les seuils qui les rendraient
décisifs sont listés en § 6.

---

## 1. Ce qui est en place, en chiffres

| Composant | Taille | Nature |
|---|---|---|
| `apps/web` | 13 216 l. Rust | Leptos SSR, zéro WASM, formulaires HTML natifs, **plus ~170 l. de JS écrit à la main** |
| `apps/api` | 8 458 l. Rust | API JSON axum, cookie de session HttpOnly 30 j, RLS Postgres |
| `apps/shared` | 4 325 l. Rust | 12 modules DTO + 13 modules de validation, partagés api ↔ web |
| `style.css` | 544 l. | Écrite à la main, servie sous `/assets/style-<sha256>.css`, budget en octets compressés |
| `e2e` | 13 specs | Playwright / TypeScript — TS est déjà dans le dépôt, côté tests |

Trois faits sortent de ce comptage et décident presque tout le reste.

### 1.1 L'API est déjà une API JSON

Un front JS/TS s'y brancherait sans toucher au backend. Le coût d'une migration
est intégralement du côté front. C'est la bonne nouvelle, et elle est réelle —
c'est ce qui rend la question légitime plutôt qu'absurde.

### 1.2 `apps/shared` n'est pas du code partagé « en passant »

25 modules de DTO et de validation, écrits en Rust parce que les deux bouts sont
en Rust, développés en TDD selon la règle du projet (`.claude/CLAUDE.md`
§ Development process). Un front TypeScript les perd. Deux issues, aucune
gratuite :

- **dupliquer** la validation en TS — deux sources de vérité pour les mêmes
  règles métier, c'est-à-dire exactement ce que `apps/shared` a été créé pour
  éviter ;
- **générer** les types (`ts-rs`, ou `utoipa` → OpenAPI → `openapi-typescript`)
  — donc un pipeline de build, dans un dépôt où la livraison du CSS a été
  conçue en #89 pour n'en avoir **aucun** (« ni étape de build, ni fichier
  généré, ni manifeste », `DESIGN.md` § Livraison du CSS).

### 1.3 Le front n'est pas « sans JavaScript » — il est « sans bundler »

**C'est le fait qui a le plus changé la conclusion de cette étude, et il est
facile à rater.** Le doc-comment de `apps/web/Cargo.toml` dit « no client-side
hydration/WASM bundle », ce qui est exact, et se lit facilement comme « pas de
JS », ce qui est faux.

Ce qui existe aujourd'hui :

| Où | Quoi |
|---|---|
| `routes/messagerie/thread.rs` → `live_script` | **169 lignes**, client WebSocket : reconnexion avec backoff, coalescence, et réconciliation du DOM par `data-message-id` pour qu'une édition en cours survive au message d'un autre membre (issue #48) |
| `app.rs:289` | Bascule d'affichage du mot de passe (`onclick`) |
| `routes/admin/users.rs:164`, `routes/account/delete.rs:89` | Deux gardes `confirm()` sur des actions destructrices |

`live_script` est couvert par un test E2E à deux sessions
(`e2e/tests/messagerie.spec.ts` § « Messagerie — live updates (WebSocket) »).

**Conséquence directe :** la messagerie en direct n'est pas un manque à combler,
elle est livrée. Et le modèle « SSR + un peu de JS écrit à la main, sans
bundler » n'est pas une proposition à évaluer — c'est l'architecture en place,
déjà éprouvée sur son cas le plus dur.

---

## 2. Le paquetage mobile

La décision existe déjà : `architecture.md` l. 93 retient **Capacitor
enveloppant l'app Leptos** pour la v1.1, « réutilise le build `apps/web` plutôt
que de maintenir une seconde UI », coquille native réservée à ce qui l'exige
vraiment (push, caméra pour le scan de frigo). Une question est laissée
ouverte : les cookies cross-origin dans la WebView.

Cette question ouverte n'a pas une réponse, elle en a deux, selon le modèle de
Capacitor retenu — et c'est là que se joue tout l'arbitrage.

### Modèle A — `server.url`

La WebView pointe sur l'origine https réelle. L'origine de la WebView **est**
celle du site : les cookies se comportent comme dans Safari, `session_id`
fonctionne, le flux OAuth Google fonctionne.

- Coût de mise en œuvre côté front : **zéro**. `apps/web` tourne tel quel.
- Ce qu'on n'a pas : aucun hors-ligne, chaque page étant rendue serveur. Et
  c'est le profil que l'App Store rejette le plus volontiers (cf. § 2.1).

### Modèle B — bundle local

L'app est un paquet d'assets statiques servi depuis `capacitor://localhost`,
qui appelle l'API en cross-origin. **C'est ici que vit le problème de cookie** :
`SameSite`, blocage des cookies tiers par WKWebView et ITP.

Il faut alors :

- passer aux jetons (`Authorization: Bearer`, stockage Keychain / Keystore),
  donc rouvrir `apps/api/src/auth/session.rs` ;
- refaire le flux OAuth Google, dont l'état CSRF est un cookie
  (`apps/api/src/auth/oauth_google.rs:32`), via `ASWebAuthenticationSession`
  ou Custom Tabs.

> **Le modèle B est celui qu'impose une réécriture JS/TS.** La question ouverte
> de `architecture.md` n'existe que dans ce modèle-là. En SSR, aujourd'hui,
> elle ne se pose pas.

### 2.1 La règle 4.2 de l'App Store

Apple rejette les emballages de site : une app qui n'apporte rien qu'un
navigateur ne donnerait pas n'est pas un produit distinct à ses yeux, et les
revues se sont durcies. Mais ce qui satisfait la règle — push, hors-ligne,
caméra — ce sont des **capacités natives ou PWA, pas des propriétés du framework
front**. Un SPA React dans une WebView reste un emballage de site s'il n'apporte
rien de plus.

Le scan de frigo, déjà au programme, est le meilleur argument 4.2 dont dispose
ce projet. Il passe par un plugin natif quelle que soit la techno du front.

### 2.2 La PWA, et le push sur iOS dans l'UE

Le dépôt n'a **ni manifeste ni service worker** — `apps/web/assets/` ne contient
que deux `.woff2` et leurs licences. Les ajouter est petit.

Conditions d'iOS : installation manuelle via Partager → Sur l'écran d'accueil,
aucune invite automatique, push réservé aux web apps installées (pas dans un
onglet Safari), et ni Background Sync ni Background Fetch.

**Le push web fonctionne bien dans l'UE**, contrairement à ce qu'affirment
encore beaucoup d'articles — y compris des articles datés 2026. Chronologie
vérifiée :

1. Février 2024 — dans la bêta d'iOS 17.4, Apple retire les web apps d'écran
   d'accueil dans l'UE, au nom du DMA et du moteur alternatif.
2. **1er mars 2024 — Apple revient sur cette décision**, avant la sortie de
   17.4 : « Home Screen web apps continue to be built directly on WebKit and
   its security architecture ».
3. Aujourd'hui — `caniuse.com/push-api` donne la Push API en support partiel
   sur Safari iOS de 16.4 jusqu'aux versions 26.x courantes, **sans réserve
   géographique**.

Les billets qui annoncent l'inverse recyclent l'annonce annulée sans mentionner
le rétropédalage. À re-vérifier si un jour la roadmap mobile en dépend
vraiment, mais la réponse actuelle est : le push PWA est disponible en France.

**En revanche**, un service worker *utile* suppose des pages cachables — or le
HTML de cette application dépend de la session et des données, ce que
`DESIGN.md` § Livraison du CSS énonce noir sur blanc (« incachable par
construction »). Un hors-ligne réel demande donc un rendu client. C'est le seul
argument mobile qui tienne vraiment pour la réécriture.

### 2.3 Rappels d'événements en local, hors ligne (2026-09-03)

Exigence posée comme **sine qua non** : une notification de rappel doit se
déclencher sur le mobile même si l'appareil est hors ligne au moment prévu.
Vérifié : elle ne repousse **pas** vers une réécriture JS/TS, parce qu'elle ne
se joue pas au niveau du front mais au niveau du wrapper mobile.

Le mécanisme qui répond à ça est le plugin natif **Local Notifications** de
Capacitor : il programme le rappel au niveau de l'OS (`UNUserNotificationCenter`
iOS, `AlarmManager` Android), qui le déclenche sans réseau et sans que l'app
tourne. L'appel au plugin passe par le pont JS de Capacitor — un script du même
genre que `live_script` (§ 1.3), pas une réécriture. Que le HTML derrière soit
rendu par Leptos SSR ou par React ne change rien à cet appel : c'est le
wrapper (Capacitor Modèle A, § 2) qui porte la capacité, pas le framework front
— exactement le principe déjà posé en § 2.1 pour le scan de frigo.

**Ce qui est explicitement écarté : le PWA pur, sans coquille Capacitor.**
L'API web qui aurait permis de programmer une notification à l'avance côté
navigateur, *Notification Triggers* (`Notification.showTrigger`), a été
abandonnée par Chrome en décembre 2021 — deux origin trials (Chrome 80-83
puis 86-88, 2020-2021) sans jamais atteindre le canal stable, jamais portée
sur les autres navigateurs ni sur Safari iOS :

> « The development of Notification Triggers API […] has ended. It wasn't
> clear that we could provide consistent and reliable experiences across
> platforms. » — Chrome for Developers

Le Web Push classique ne sauve pas le coup non plus : il dépend d'un
aller-retour réseau au moment de l'émission, donc échoue précisément au test
« hors ligne ». Conclusion : cette exigence **confirme** le choix déjà pris
en § 2 (Capacitor Modèle A) plutôt que de le remettre en cause, et n'ajoute
aucun argument pour l'option 3.

Contraintes techniques à cadrer quand l'epic sera spécifiée (indépendantes du
choix Rust vs JS, donc à ne pas confondre avec cette étude) :

- **Plafond iOS de 64 notifications programmées par app**, limite système
  sans contournement — il faut synchroniser seulement les prochaines
  échéances et reprogrammer à chaque ouverture de l'app avec réseau.
- **Android 12+** : permission `SCHEDULE_EXACT_ALARM` requise pour un
  déclenchement à l'heure exacte. **Android 13+** : permission runtime
  `POST_NOTIFICATIONS`.
- La table `scheduled_notifications` (`architecture.md` l. 34, canal email
  actuel) doit gagner un canal qui pousse la donnée du rappel vers le client
  pour programmation locale, avec annulation/reprogrammation à l'édition ou
  la suppression de l'événement.

Voir `architecture.md`, Questions résolues #4, pour la décision actée.

---

## 3. Ce qu'une réécriture achèterait

- **Le hors-ligne**, structurellement impossible en SSR pur.
- **Les interactions sans aller-retour serveur** — cocher un article de
  courses, ajuster une quantité de stock — là où le JS écrit à la main devient
  coûteux à multiplier.
- **Un modèle d'état client unique**, plutôt qu'un script par page à maintenir
  séparément.
- **L'accès à l'outillage de design par IA** : v0, Lovable, Superdesign visent
  React et Tailwind d'abord ; leur sortie n'a aujourd'hui aucun point
  d'atterrissage dans `view!`.

La messagerie en direct ne figure **pas** dans cette liste (cf. § 1.3).

---

## 4. Ce qu'elle coûterait

- **13 216 lignes à réécrire** à comportement égal, sans gain fonctionnel pour
  la majorité d'entre elles.
- **La perte de `apps/shared` côté front** : 25 modules dupliqués ou générés
  (cf. § 1.2).
- **Un second graphe de dépendances.** `architecture.md` l. 341 revendique
  littéralement « with no second dependency graph to scan ». La politique du
  projet est `cargo audit` **sans jamais d'`ignore`** (`.claude/CLAUDE.md`
  § Dependency policy) ; npm apporte des centaines de dépendances transitives
  à tenir à la même règle.
- **Un delta RGPD.** Les polices sont auto-hébergées parce qu'un CDN exposerait
  l'IP des visiteurs à un tiers, à déclarer au registre. Un cache client de
  données de foyer, c'est des données personnelles sur l'appareil : les
  messages, chiffrés en base par pgcrypto (`MESSAGE_ENCRYPTION_KEY`),
  atterriraient en clair dans un store client. Nouvelle analyse à porter au
  registre.
- **La fin de « fonctionne sans JS »**, et du modèle d'erreurs qui va avec
  (« no fetch()/promise rejections to handle client-side », `apps/web/Cargo.toml`).
- **Le budget CSS perd son objet.** Il a été calibré contre une base sans
  bundle, et c'est lui qui « interdit de fait un framework utilitaire »
  (`DESIGN.md`, bénéfice n° 4 de l'inlining).
- **Node devient une dépendance de développement.** L'hôte n'en a pas ; tout
  passerait par Docker, en local comme en CI.

---

## 5. Les options, par engagement croissant

### Option 0 — statu quo + coquille mobile

Capacitor en modèle A, plus un manifeste PWA et des icônes. Quelques jours,
aucun changement de code applicatif. Ne règle ni le hors-ligne, ni le risque 4.2.

### Option 1 — continuer le modèle actuel *(recommandé)*

Ce n'est pas une nouvelle direction, c'est celle déjà prise. `live_script` prouve
que le modèle tient sur le cas le plus dur. Il reste deux ajouts du même genre :
le manifeste PWA avec service worker minimal (coquille d'installation, pas de
cache de données), et les interactions optimistes là où l'aller-retour se voit.

On garde Leptos, `apps/shared`, les cookies, le budget CSS, la progressive
enhancement, et un seul graphe de dépendances.

### Option 2 — Leptos islands (0.8) *(si l'option 1 sature)*

Même langage, mêmes DTO, hydratation partielle : seuls les composants
interactifs partent en WASM. **Un seul graphe de dépendances conservé** — c'est
son mérite décisif face à l'option 3.

Coût : chaîne `wasm32` et `cargo-leptos` que l'environnement n'a pas, de l'ordre
de 50 à 100 Ko de WASM, et la fin du « sans JS » sur les îlots concernés.

### Option 3 — réécriture JS/TS complète *(pas maintenant)*

Justifiée seulement si la cible est une vraie application native, hors-ligne et
poussée, **et** si la vélocité de design par IA devient un objectif assumé.

| Candidat | Ajustement |
|---|---|
| **SvelteKit** | Meilleur ajustement technique : plus petit runtime, SSR et progressive enhancement natifs (`<form use:enhance>` est presque exactement le modèle actuel), CSS ordinaire bienvenu, `adapter-node` et Docker comme aujourd'hui. |
| **Next.js / React** | Pire ajustement technique, meilleur ajustement outillage : v0, shadcn et Superdesign ciblent React d'abord. |
| **Nuxt / Vue** | Au milieu, sans avantage décisif ici. |
| **Expo / React Native** | Meilleure réponse à la règle 4.2 (vraie UI native), mais impose une **seconde UI à maintenir** : exactement ce que `architecture.md` a refusé. |

> L'arbitrage à dire tout haut : **Svelte optimise pour les valeurs de cette
> stack, React pour la promesse d'outillage IA.** On ne peut pas avoir les deux
> à plein.

---

## 6. Ce qui rouvrirait la question

Dans cet ordre, d'abord :

1. Ajouter le manifeste PWA, les icônes et un service worker minimal.
2. Poser la coquille Capacitor en modèle A. On apprend ce que le mobile demande
   vraiment avant de payer pour le deviner ; si elle suffit, la question ne se
   pose plus.
3. Ajouter les interactions optimistes dans le style de `live_script`, et
   seulement là où l'usage les réclame.

Puis ne rouvrir la réécriture que si **l'un** de ces seuils est franchi :

- [ ] le hors-ligne devient une exigence produit, pas un confort ;
- [ ] un rejet 4.2 réel arrive, et la coquille du modèle A ne suffit pas à le
      lever ;
- [ ] le JS écrit à la main dépasse ~5 scripts de la taille de `live_script` —
      on paierait alors le coût d'un framework sans en avoir les garanties ;
- [ ] la vélocité de design devient le goulot **mesuré** du projet — mesuré,
      pas supposé.

---

## Annexe — l'outillage de design par IA, et pourquoi il ne colle pas

Question posée en amont de celle-ci, et qui l'a motivée. Résumé :

**Ce qui ne colle pas.** v0, Lovable, Figma Make, Superdesign, Locofy/Anima
produisent du React + Tailwind + shadcn, ou de la soupe de `<div>` issue de
Figma. Aucun de ces codes ne peut atterrir dans `view!`. Pire, leur esthétique
par défaut — cartes flottantes, ombres portées, dégradés, sans géométrique
arrondi — est **exactement la liste de ce que `DESIGN.md` refuse**
(§ Direction esthétique). Utilisables comme source d'images, jamais comme source
de code.

**Ce qui colle.**

1. **Donner des yeux à l'agent.** `.mcp.json` déclare déjà Playwright MCP, mais
   il ne démarre pas : `npx` est introuvable dans le `PATH` (tout passe par
   Docker sur cette machine). Le réparer est le plus gros gain disponible pour
   un coût quasi nul — l'agent pourrait ouvrir une route en 375 px, en thème
   sombre, et *voir* le rendu, au lieu de raisonner à l'aveugle sur du CSS.
   C'est précisément le mode de défaillance du bug `var(--x, #hex)` qui « a
   cassé le thème sombre de l'agenda sans que personne le voie ».
   Chrome DevTools MCP est le complément orienté audit (perf, réseau).
2. **Idéation en amont du code** : Google Stitch sort du HTML/CSS plutôt que du
   React ; les skills gstack (`design-shotgun`, `design-review`) sont déjà
   installées. La sortie sert de référence visuelle, `DESIGN.md` reste
   l'autorité.
3. **Garde-fous** : régression visuelle Playwright (`toHaveScreenshot()`) par
   route × thème × largeur, et `@axe-core/playwright` pour contraste et focus.
   La suite E2E existe déjà.

---

## Journal

| Date | Événement |
|---|---|
| 2026-08-28 | Étude initiale. Correction en cours de route : `live_script` (169 l., WebSocket) invalide l'hypothèse « le front n'a pas de JS » et le manque « messagerie temps réel » qui en découlait. Vérification du push PWA iOS dans l'UE : disponible, l'annonce de retrait de février 2024 ayant été annulée le 1er mars 2024. |
| 2026-09-03 | Exigence sine qua non testée : rappels de notification mobile devant se déclencher hors ligne (§ 2.3). Vérifié compatible avec le front Rust actuel — la capacité vient du plugin natif Capacitor (Local Notifications), pas du framework front ; le PWA pur est écarté (*Notification Triggers* abandonnée par Chrome en décembre 2021, après deux origin trials sans passage en stable). Ne change pas la conclusion § 0. |
