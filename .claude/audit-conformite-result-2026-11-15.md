# Audit de conformité — 2026-11-15 (volet accessibilité)

Périmètre : dépôt `manage_our_home`, `apps/web`, commit `a477ec7` (`main`),
après la refonte visuelle (#523 à #540) et le choix du thème (#585). **Volet
accessibilité seul** (WCAG 2.1 AA / RGAA), demandé par #543 : les volets
juridique, RGPD et natif ne sont pas rouverts ici — le dernier audit complet
reste celui du 2026-09-09 (`.claude/audit-conformite-result-2026-09-09.md`).
Surfaces : **web** (Leptos SSR + axum, rendu serveur). Le shell mobile
(`apps/mobile/`) embarque l'app web ; ses deux pages locales
(`www/hors-ligne.html`, `www/choix-du-site.html`) n'ont pas été auditées.

Méthode : **mixte**.

- **Rendu** : la pile complète (api + web + Postgres 16 + MinIO) montée en
  local depuis `a477ec7`, parcourue par Playwright 1.61.1 (Chromium) avec
  **axe-core 4.10.3** injecté page par page (règles des étiquettes `wcag2a`,
  `wcag2aa`, `wcag21a`, `wcag21aa`, `wcag22aa`), plus des sondes écrites pour
  cet audit : parcours Tab arrêt par arrêt avec lecture de l'indicateur de
  focus, mesure des cibles, reflow à 320 px, ouverture des fenêtres modales au
  clavier, noms accessibles. Un compte neuf est ensemencé (deux familles, des
  événements dont un sur la journée et une demi-heure, deux pièces jointes, un
  article en stock, une recette prévue à laquelle il manque un ingrédient, une
  dépense, un message, un article de courses). Les pages sont celles d'une
  liste fixe complétée par les liens internes qu'elles portent, dédoublonnés
  par forme d'URL : **61 pages connectées** et **8 pages publiques**.
  Le harnais n'est pas versionné : c'est un outil d'audit, pas un garde-fou
  (voir « Non vérifié » pour ce qu'il faudrait en garder).
- **Statique** : `scripts/contrast.py` du skill d'audit sur
  `apps/web/src/style.css`, lecture du markup (`apps/web/src/routes/`) et de
  `apps/web/src/enhance.js`.

Les quatre configurations de thème de #543 et de son commentaire ont été
rendues, et le thème effectivement appliqué relevé sur chaque page
(`data-theme` et `--bg` de `<html>`) :

| Configuration | Appareil (émulé) | Choix sur `/account/appearance` | Relevé |
|---|---|---|---|
| Automatique, appareil clair | `prefers-color-scheme: light` | Automatique | pas de `data-theme`, `--bg` `#fbfaf7` |
| Automatique, appareil sombre | `dark` | Automatique | pas de `data-theme`, `--bg` `#1a1917` |
| **Clair sur un appareil en sombre** | `dark` | Clair | `data-theme="light"`, `--bg` `#fbfaf7` |
| **Sombre sur un appareil en clair** | `light` | Sombre | `data-theme="dark"`, `--bg` `#1a1917` |

Chaque configuration est passée à **390 px** (téléphone) et **1 280 px**
(PC). Les **8 teintes de famille** ont été choisies une par une sur la page
de paramètres de la famille, puis vérifiées sur `<html>` (`data-accent` et
`--accent` calculé, tableau au § Couleur) avant d'être mesurées.

## Cadre retenu

**Aucun régime contraignant**, comme au 2026-09-09 : ni EAA (pas de service
marchand, aucun paiement), ni RGAA (ni secteur public, ni délégataire, ni
seuil de chiffre d'affaires). **Les constats ci-dessous sont des écarts à
l'objectif que le projet s'est fixé** — `DESIGN.md` vise WCAG AA et des cibles
de 44 px — et non des manquements légaux. Les deux critères WCAG 2.2 cités
(2.4.11, 2.5.8) sont hors de la cible 2.1 de #543 ; ils sont signalés parce que
la norme EN 301 549 les reprendra et qu'axe les mesure déjà.

## Verdict

Rien ne bloque. L'état est nettement meilleur qu'au 2026-09-09 : des quatre
constats d'accessibilité de cet audit-là, le lien d'évitement (#144) et les
contrastes (#74) sont levés, le calendrier a sa `<caption>` et 13 lignes de
`apps/web/src/routes/` posent `scope="col"`, et `aria-invalid` est posé sur
l'inscription (`routes/auth/register.rs`) et ailleurs (4 fichiers de
`apps/web/src/` le nomment). **axe ne relève aucune violation WCAG 2.0/2.1 A
ou AA sur les 69 pages** — les 61 pages connectées dans les quatre
configurations de thème, les 8 pages publiques dans les deux configurations
automatiques (le choix du thème est une page de compte) — aux deux largeurs, ni
aucune violation de contraste pour les 8 teintes de famille dans les deux
thèmes. Les fenêtres modales sont natives (`showModal`) et se comportent comme
il faut au clavier.

Deux écarts AA réels restent : **le focus des cases à cocher et des boutons
radio est quasi invisible** (anneau à 1,00–1,36:1 sur son fond, sur 18 des
61 pages connectées, dans les quatre configurations), et **un champ fichier
déborde à 320 px** sur `/stocks/new`. Sur téléphone, **le focus disparaît sous
la barre d'onglets** en bas de 7 pages (WCAG 2.2). Le reste est mineur.

## Constats

| # | Gravité | Domaine | Constat | Preuve |
|---|---|---|---|---|
| 1 | Majeur | Focus (2.4.7, 1.4.11) | Le focus d'une **case à cocher** ou d'un **bouton radio** n'est porté que par l'anneau `box-shadow: 0 0 0 3px var(--accent-soft)` : la règle des champs pose `outline: 2px solid transparent` et `border-color: var(--accent)`, mais une case native ne dessine pas de bordure. Mesuré : anneau à **1,12 / 1,17:1** en clair, **1,22 / 1,36:1** en sombre (1,00:1 sur une carte `--accent-soft`), mêmes valeurs en thèmes forcés ; capture : un liseré à peine perceptible. 18 pages sur 61 touchées (sélecteur de familles de l'agenda, formulaire d'événement, `/budget/new`, `/budget/settings`, une dépense, `/account`, `/account/appearance`, `/account/notifications`, paramètres de famille…). Remède : donner aux cases et radios l'anneau des cliquables (`input:is([type=checkbox], [type=radio]):focus-visible { outline: 2px solid var(--accent); outline-offset: 2px }`). `e2e/tests/interaction.spec.ts` ne l'a pas vu : il accepte tout `box-shadow` peint comme indicateur, sans en mesurer le contraste | `apps/web/src/style.css:1349-1353` ; `e2e/tests/interaction.spec.ts:98` |
| 2 | Majeur | Reflow (1.4.10) | À 320 px, `/stocks/new` défile horizontalement de **14 px** : le champ fichier « Photographier le code » garde sa largeur intrinsèque (298 px, bord droit à 334 px). Remède : borner le champ fichier à la colonne (`input[type="file"] { max-width: 100% }` ou la largeur des autres champs). Aucune autre des 61 pages ne déborde à 320 px | `apps/web/src/routes/stocks/new.rs:403` |
| 3 | Majeur | Focus masqué (2.4.11, WCAG 2.2) | Sur téléphone, un élément focalisable en bas de page reçoit le focus **entièrement sous la barre d'onglets fixe** : le navigateur le fait défiler jusqu'au bord de la fenêtre, que la barre recouvre. Mesuré sur « Se déconnecter » de `/account` : bouton de 795 à 843 px, barre à partir de 787 px, fenêtre de 844 px. 7 pages : « Stock bas » de l'accueil, « Se déconnecter » de `/account`, et un lien du pied ou de fin de page sur `/legal-notice`, `/account/notifications`, les paramètres de famille et deux mois de `/budget`. Mêmes résultats dans les quatre configurations. Gravité : critère 2.2, mais le clavier y perd le focus de vue sans recours, et la disposition téléphone s'applique aussi à une fenêtre étroite ou zoomée. Remède : `scroll-padding-bottom` de la hauteur de la barre sur `html` dans la disposition téléphone | `apps/web/src/style.css:248` (`.tabs`, `position: fixed`) ; aucun `scroll-padding` dans la feuille |
| 4 | Mineur | Cibles (2.5.8, WCAG 2.2) | Seules violations axe des 69 pages : les barres du mois sur téléphone (**49 × 19 px**) et les puces de la semaine sur PC (**81 × 22 px**), sous le plancher de 24 px. **Arbitrées** : barres « à la taille de la maquette » (#836), puces « trois par jour de 6,5 rem, disposition pointeur » — exclusions écrites de `touch-targets.spec.ts`. Constat consigné pour le jour où la cible deviendra WCAG 2.2, pas à corriger sans nouvel arbitrage | `e2e/tests/touch-targets.spec.ts:137-155` ; `DESIGN.md` → « La grille du mois en barres (#836) » |
| 5 | Mineur | Cibles (objectif 44 px) | Des liens de texte **seuls sur leur ligne** restent à 21–22 px de haut : « ← Retour à mon compte » (4 pages), « Gérer les rôles des membres », « Gérer les agendas Google », « Retour aux recettes prévues », « Aide Google : intégrer un agenda » ; et le badge-lien « Il manque 1 élément » à 24 px. Ils passent 2.5.8 par l'exception d'espacement (axe ne les relève pas) et 2.5.5 est AAA, mais ils manquent l'objectif de 44 px de `DESIGN.md`. `touch-targets.spec.ts` les écarte expressément (« not in the issue's list and are not closed by it »). Remède : la boîte des liens de `.links`, ou un arbitrage qui les déclare liens de texte. Les liens **dans une phrase** (pied de page, mentions, notices) relèvent de l'exception en ligne et ne sont pas comptés | `apps/web/src/routes/account/notifications.rs:504`, `account/sessions.rs:44`, `account/delete.rs:115`, `account/export.rs:48`, `budget/settings.rs:195`, `groups/settings.rs:396`, `recipes/missing.rs:186` et `:55`, `agenda/imports.rs:360` |
| 6 | Mineur | Noms accessibles (1.1.1) | La flèche de « ← Retour à mon compte » est un caractère du texte du lien : un lecteur d'écran lit « flèche vers la gauche, Retour à mon compte ». Même chose pour les « → » des plages horaires du détail d'un événement et du texte d'aide de l'import Google. Toutes les icônes SVG, elles, sont `aria-hidden` et aucun bouton n'a un nom fait d'un seul symbole (sondé sur les 69 pages). Remède : la flèche dans un `<span aria-hidden="true">`, ou l'icône du jeu (`icons.rs`) | `apps/web/src/routes/account/notifications.rs:504`, `account/sessions.rs:44`, `account/delete.rs:115`, `account/export.rs:48` |
| 7 | Mineur | Formulaires (3.3.1) | Dette héritée de #474 : sur `/grocery-list/reception`, le message d'erreur d'une ligne (`<p class="field-error">`) est posé en tête de son `<fieldset>`, sans `aria-describedby` vers le champ fautif ni `aria-invalid`. La légende nomme l'article, pas le champ. Remède : la forme de `recipes/outcome.rs:1267` (`id` sur le message, `aria-describedby` et `aria-invalid` sur le champ) | `apps/web/src/routes/grocery_list/reception.rs:292`, `:317` |

## Détail

### Contrastes et couleur (1.4.1, 1.4.3, 1.4.11)

**Statique.** `scripts/contrast.py` lit `style.css` et résout deux thèmes. Les
**38 paires réellement déclarées** (une règle qui pose une couleur et un fond)
passent toutes 4,5:1 dans les deux : la plus basse est **4,61:1**
(`.notice.success`, clair), puis 4,62:1 (`.notice.warning`, `.badge.soon`,
`.tile.warn`, clair). Les thèmes forcés n'ont pas de valeurs propres : depuis
#585 les valeurs sombres sont écrites une fois sous `:root[data-theme="dark"]`
et le bloc `prefers-color-scheme` en est dérivé au démarrage
(`theme::with_device_theme`), ce que le rendu ci-dessous confirme.
La matrice de jetons appariés par leur nom n'a été retenue nulle part : ses KO
(`--accent-fg` sur les fonds `-soft`) ne correspondent à aucune règle, comme au
2026-09-09.

**Ce que la mesure statique ne couvre pas** — 16 valeurs non résolues : le
fond `transparent` de `.pw-toggle` et des créneaux `.slots button`, les
fonds `none` de `.slots button` et de `.mm > *`, le
`color-mix()` de `.ghost`, le `currentColor` de `.dot`, le `var(--accent)` en
`content-box` du jour courant. Ces règles sont couvertes par le rendu
ci-dessous, sauf ce que liste « Non vérifié ». **Couleurs écrites hors des
feuilles** : 23, aucune dans l'interface de `apps/web` — des tests
(`budget_flow.rs`, `agenda.spec.ts`), le leurre de `assets.rs`, le gabarit
d'e-mail (`apps/api/src/email/template.rs:80`), les pages locales et l'écran
de lancement du shell mobile.

**Rendu.** axe `color-contrast` ne relève **aucune violation** sur les 69 pages
dans les quatre configurations, aux deux largeurs. Les teintes de famille :

| Teinte | `--accent` clair | `--accent` sombre | Violations (16 pages × 2 largeurs × 2 thèmes) |
|---|---|---|---|
| Sarcelle | `#14706e` | `#4fb3af` | 0 |
| Océan | `#1f5f99` | `#67a6e0` | 0 |
| Prune | `#6b4a9b` | `#ad97cd` | 0 |
| Forêt | `#3d6b35` | `#6db162` | 0 |
| Terracotta | `#a64b25` | `#de8c6a` | 0 |
| Framboise | `#a3305a` | `#dc88a7` | 0 |
| Ardoise | `#475569` | `#95a3b7` | 0 |
| Miel | `#875800` | `#de9100` | 0 |

Valeurs relevées sur `<html>` après enregistrement de chaque teinte, la couleur
du lien de navigation courant suivant. Règles passées : `color-contrast` et
`link-in-text-block` (le lien de texte sans soulignement au repos, #752, est
distingué par sa graisse : axe ne le relève sur aucune teinte). Pages : accueil,
mois, semaine, jour, formulaire d'événement, détail avec pièces jointes,
courses, messagerie, stocks, recettes (toutes et prévues), budget, familles,
paramètres de famille, compte, notifications. L'anneau de focus (`outline`
`--accent`) reste au-dessus de 3:1 sur son fond pour les 8 teintes dans les
deux thèmes (sondé sur trois pages).

**La couleur n'est jamais seule** : l'onglet courant a aussi la graisse 600,
l'initiale d'un membre accompagne sa couleur, les badges portent un texte.

### Clavier et focus (2.1.1, 2.1.2, 2.4.3, 2.4.7)

- **Parcours Tab** : jusqu'à 70 arrêts par page, sur les 61 pages connectées,
  quatre configurations, deux largeurs — 7 420 arrêts en tout. **Aucun arrêt
  sans indicateur** (ni `outline` peint ni `box-shadow`), **aucun anneau
  `outline` sous 3:1**. Les champs texte, nombre, date, fichier et zones de
  texte ont l'anneau pâle **et** leur bordure passe en `--accent` (5,63 à
  7,03:1 sur le fond) : l'indicateur est visible. Les cases et radios n'ont que
  l'anneau pâle (constat 1).
- **Contrastes forcés** (`forced-colors: active`, émulé) : chaque arrêt porte
  une `outline` — l'`outline` transparente des champs y est repeinte, comme
  `DESIGN.md` l'a voulu.
- **Lien d'évitement** : premier arrêt de chaque page, « Aller au contenu »
  vers `<main id="main">` (`app.rs:168`).
- **Aucun `tabindex` positif** dans `apps/web/src/` ; `enhance.js` ne pose que
  `0`/`-1` (le mois du choix d'une date, la liste `.combo`).
- **Focus masqué** : constat 3.

### Fenêtres modales et listes déroulantes

Les quatre fenêtres de l'application sont des `<dialog>` ouverts par
`showModal()` : le navigateur y met le focus, l'y garde et les ferme sur
Échap. Chacune a été ouverte **au clavier** (focus sur le déclencheur, Entrée
ou Espace), à 1 280 et 390 px, en clair automatique et en Sombre forcé (le choix
dans le calendrier : en clair automatique seulement) :

| Fenêtre | Nom accessible | Focus à l'ouverture | Tab | Échap | Focus au retour |
|---|---|---|---|---|---|
| Éléments manquants d'une recette (#560) | « Il manque à « Crêpes » » (`aria-labelledby`) | non mesuré ici (lu trop tôt) ; « Ajouter à la liste de courses » selon `e2e/tests/recipes.spec.ts:437` | 25 arrêts, tous dans la fenêtre | ferme | le badge |
| Choisir dans le calendrier (#795) | « Choisir dans le calendrier » (`aria-label`) | « Fermer » | parcourt Fermer, mois, jours, bandeau, heures | ferme | le bouton |
| Visionneuse d'une pièce jointe (#776) | le nom du fichier | non mesuré ici (lu trop tôt) | 25 arrêts, tous dans la fenêtre | ferme | la tuile |
| Dernière famille affichée (#750) | le message (`aria-labelledby`) | « Fermer » | alterne « Fermer » et la chrome du navigateur (comportement natif d'un modal) | ferme | la case |

axe sur la fenêtre ouverte : aucune violation. Dans le choix d'une date, la
zone des heures (`div.hours-scroll`) prend le focus au clavier avec l'anneau
du navigateur et non celui de l'application — visible, sans écart au critère.

Liste `.combo` (#803, « Voir les unités ») : à l'ouverture le focus passe sur
la `listbox` (`aria-activedescendant` sur la première option), ↓ déplace
l'option active, Échap ferme et rend le focus au bouton ▾.

Les « feuilles » de #523 (`.card.sheet` : `stocks/new.rs:433`, `:499`,
`agenda/new.rs:931`, `agenda/pick.rs:409`) sont des sections de la page, pas
des fenêtres : elles n'ont rien à piéger et sont couvertes par le parcours Tab.

### Noms accessibles des icônes (1.1.1, 4.1.2)

`icons.rs` émet chaque icône en `<svg class="icon" … aria-hidden="true">` ; la
sonde sur les 69 pages ne trouve **aucun `<svg>` exposé** sans nom et **aucun
lien, bouton ou option dont le nom ne contient ni lettre ni chiffre** (le ▾ de
`.combo` porte « Voir les unités », les boutons du mois du choix d'une date
« Mois précédent » et « Mois suivant »). axe `button-name`, `link-name`, `svg-img-alt` : aucune
violation. Reste la flèche en caractère du texte (constat 6).

### Cibles (objectif 44 px)

Mesure de chaque lien, bouton, champ, `summary` et case (par son `<label>`) à
390 px sur les 69 pages. Hors liens dans une phrase, les cibles sous 44 px sont
celles déjà arbitrées — demi-heure `.slots a` (28 px), ligne « Journée » `.ad`
(32 px), bloc court `.blk` (26 px), bandeau `.slot` (35 px), barres du mois —
et les liens du constat 5. Sous 24 px (WCAG 2.2) : constat 4.

### Structure et formulaires (1.3.1, 2.4.2, 3.1.1, 3.3.x, 4.1.3)

axe ne relève ni titre de page manquant, ni `lang`, ni étiquette de champ, ni
`aria-*` invalide sur les 69 pages. Le champ e-mail de l'inscription porte
désormais `aria-invalid` en erreur (`routes/auth/register.rs`), là où le
constat n° 24 du 2026-09-09 le relevait. Exception : constat 7.

### Médias temporels (1.2.x)

Nouveau depuis le 2026-09-09 : la visionneuse lit une **vidéo jointe** par un
utilisateur (`<video controls>`, #776). Le contenu est déposé par la famille
elle-même ; ni sous-titres ni transcription ne sont attendus d'un service qui
ne produit pas ces vidéos. Rien d'autre (ni `<audio>`, ni lecteur embarqué).

### Mouvement (2.3.3)

`prefers-reduced-motion: reduce` neutralise transitions et transition de page
(`style.css:1423`, `:1429`). Tout le rendu a tourné en mouvement réduit (voir
« Non vérifié »).

## Non vérifié

- **Lecteur d'écran** : aucun (NVDA, VoiceOver, TalkBack) n'a été utilisé. Les
  annonces de la messagerie en direct (`aria-live`), des erreurs de formulaire
  et des fenêtres ne sont établies que par le DOM.
- **Navigateurs** : Chromium seul. Firefox et Safari (dont le rendu du focus
  des cases natives, constat 1) n'ont pas été vus.
- **Contraste que axe n'a pas pu trancher** : sur la vue jour, les en-têtes
  d'heure (`th`) et le titre du jour sont recouverts par les liens de
  création `.slots` (« background could not be determined… overlapped ») ; sur
  la semaine, la puce et son avatar `aria-hidden` ; les `<textarea>` des
  formulaires d'événement, de recette et de la messagerie. Ni axe ni le script
  statique ne les mesurent : à vérifier à la pipette.
- **Mouvement complet** : tout le rendu a tourné sous
  `prefers-reduced-motion: reduce`, comme la suite e2e du dépôt (la transition
  de page fige le headless shell, voir `e2e/playwright.config.ts`). La
  transition de page n'a donc pas été auditée.
- **Zoom texte à 200 %** et **espacement du texte** (1.4.4, 1.4.12) : non
  testés ; seul le reflow à 320 px l'a été.
- **Contrastes forcés réels** : seule l'émulation de Chromium
  (`forced-colors: active`) ; pas de mode Contraste élevé de Windows.
- **Pages non visitées** : celles qui demandent un état que l'ensemencement
  n'a pas créé — administration (`/admin/*`, compte non administrateur),
  réception des courses avec erreur (constat 7 lu dans le code), invitation à
  rejoindre, réinitialisation du mot de passe par jeton, scan d'un code-barres
  (`/stocks/new/scan`), page de suppression du compte en délai de grâce.
- **Pages du shell mobile** (`apps/mobile/www/*.html`) et e-mails HTML
  (#831) : hors de ce passage.
- **Harnais** : les sondes de cet audit (parcours Tab avec contraste de
  l'indicateur, reflow à 320 px, axe sur chaque page et chaque thème) ne sont
  pas versionnées. Les garder comme garde-fous serait une issue à part :
  `interaction.spec.ts` ne couvre que quatre pages et accepte tout anneau
  peint (constat 1), et le dépôt n'a aucune dépendance axe.
