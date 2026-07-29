# Audit d'ergonomie et d'apparence — `apps/web`

État des lieux au 2026-07-29, après la livraison des épics front F1→F11.
Objectif : cadrer ce qu'il faut reprendre pour que l'interface soit utilisable
sur PC sans rien perdre de son ergonomie mobile.

Ce document est un **constat**, pas un plan d'implémentation. La cible visuelle
(typographie, palette, tokens) est décrite dans [`DESIGN.md`](../DESIGN.md) ; le
découpage en lots livrables est en fin de document et se retrouve dans les
issues GitHub.

---

## 1. Contraintes techniques à respecter

Toute reprise doit tenir dans le cadre existant — ce ne sont pas des défauts,
ce sont les règles du jeu :

- **Rendu serveur pur.** `apps/web` est un serveur axum qui rend du HTML via
  `leptos::ssr::render_to_string`. Pas d'hydratation, pas de bundle WASM
  (cf. le doc-comment de `apps/web/Cargo.toml` et de `src/app.rs`). Toute
  solution doit donc être **CSS-first** : pas de framework JS, pas de state
  client. Le peu de JS existant est de l'amélioration progressive inline
  (le toggle mot de passe dans `src/app.rs:141`, le rafraîchissement de la
  messagerie).
- **CSS inliné dans le document.** `shell()` injecte `include_str!("style.css")`
  dans un `<style>` à chaque réponse (`src/app.rs:22`). Le budget taille compte :
  la feuille est envoyée en entier sur chaque page. Cela plaide pour un fichier
  unique, tokenisé et sans redondance — pas pour un framework utilitaire.
- **Pas de dépendance CSS externe.** Rien à installer, rien à builder : la
  feuille est du CSS écrit à la main.
- **Les tests e2e sont peu couplés au markup.** Les sélecteurs Playwright de
  `e2e/tests/*.spec.ts` ciblent des rôles, des labels et des `input[name=...]`,
  pas des classes ni des structures de `<div>`. Une refonte visuelle est donc
  **à faible risque de régression** tant qu'on préserve les rôles ARIA, les
  `name` de formulaire et les libellés visibles.

---

## 2. Constat global, en chiffres

| Mesure | Valeur | Commentaire |
|---|---|---|
| Lignes de CSS (`src/style.css`) | **103** | pour 11 domaines fonctionnels |
| Lignes de front (`src/**/*.rs`) | **10 640** | ratio CSS/code ≈ 1 % |
| Attributs `style="..."` inline | **173** | dont 31 dans `messagerie/thread.rs` |
| Variables CSS définies | **6** | `--fg --bg --muted --error --accent --border` |
| Variables CSS *utilisées mais jamais définies* | **2** | `--accent-bg`, `--chip-bg` → bug, cf. §4.1 |
| Media queries | **1** | et c'est `prefers-color-scheme`, pas un breakpoint |
| Breakpoints responsive | **0** | |
| Règles `:hover` / `:focus-visible` / `:active` | **0** | |
| Règles `transition` / `animation` | **0** | |
| Échelle d'espacement | *aucune* | valeurs ad hoc : 0.3/0.4/0.5/0.6/0.75/0.85/1/1.25/1.5 rem |
| Largeur max du contenu | **28 rem (448 px)** | identique pour toutes les pages |

La conclusion tient en une phrase : **il n'y a pas de système de design, il y a
une feuille de style de maquette** — écrite pour l'épic d'authentification
(des formulaires étroits et centrés), puis jamais réévaluée alors que l'app
gagnait un calendrier, des tables d'administration, une messagerie et des
listes à actions multiples.

---

## 3. Les quatre défauts structurels

### 3.1 Une colonne de 448 px pour tout le monde — le vrai blocage PC

```css
.container { max-width: 28rem; margin: 3rem auto; padding: 0 1.25rem; }
```

`src/style.css:25-29`. Cette règle s'applique à **toutes** les pages, parce que
`shell()` enveloppe systématiquement le corps dans `<main class="container">`
(`src/app.rs:25`). Conséquences concrètes :

- **Agenda, vue mois** (`routes/agenda/calendar.rs:255`) : une table de 7
  colonnes en `table-layout:fixed`, chaque cellule à `width:14.28%` et
  `height:6.5rem`, comprimée dans 448 px → **~57 px de large par jour**. Le
  titre d'un événement n'a aucune chance d'être lisible ; les pastilles
  (`calendar.rs:307`) se réduisent à quelques caractères tronqués.
- **Agenda, vue semaine** (`calendar.rs:275`) : les colonnes sont en
  `flex:1;min-width:8rem` — 7 × 8 rem = 56 rem de minimum dans un conteneur de
  28 rem. Le `flex-wrap` fait retomber les jours les uns sous les autres :
  la vue « semaine » n'est plus une semaine, c'est une liste verticale.
- **Admin utilisateurs / familles** (`routes/admin/users.rs:112`) : des tables
  multi-colonnes en `white-space:nowrap` (`style.css:66`) dans un
  `.table-wrap { overflow-x:auto }`. Sur un écran 27 pouces, l'écran est vide à
  80 % et l'utilisateur scrolle horizontalement dans une fenêtre de 448 px.
  Le commentaire de `style.css:61-63` documente d'ailleurs le contournement,
  ce qui montre que le problème était déjà connu au moment de l'épic F9.
- **Messagerie** (`routes/messagerie/thread.rs`) : le fil et le champ de saisie
  sont dans la même colonne étroite ; le `textarea` d'édition force un
  `min-width:16rem` (`thread.rs:112`) qui frôle la largeur disponible.
- **Toutes les pages de liste** (stocks, recettes, budget, courses) : chaque
  ligne est un `flex` avec le libellé à gauche et 2-3 boutons à droite
  (`stocks/list.rs:83`, `budget/list.rs:85`, `recipes/list.rs:101`…). À 448 px,
  les boutons mangent la moitié de la ligne et le libellé se tronque.

L'espace horizontal disponible sur un poste de travail n'est jamais exploité,
et les écrans qui en auraient le plus besoin (calendrier, tables) sont
précisément les plus pénalisés.

**Le symétrique existe aussi sur téléphone.** En rendant la maquette du système
de design à 390 px de large, la grille mensuelle reste illisible même une fois
le layout corrigé : 7 colonnes donnent ~50 px par jour. Élargir le conteneur ne
suffit donc pas — **sous 861 px, l'agenda doit basculer par défaut sur une vue
jour**, avec un bandeau de semaine tappable qui conserve l'orientation, la
grille mensuelle restant accessible par un bouton « Mois ». C'est un changement
de contenu, pas de CSS, et il ne se voyait pas à la lecture du code.

Bonne nouvelle pour le coût : l'agenda expose déjà `?view=week|month&date=…`
avec prev/next et « Aujourd'hui » (`agenda/calendar.rs:68,189`). `view=day`
s'insère comme troisième valeur du même schéma, sans JS — chaque jour est un
lien. Spec détaillée dans `DESIGN.md` → Layout → L'agenda sur téléphone,
et dans l'issue [#71](../../issues/71).

### 3.2 Aucun système de composants — 173 styles inline dupliqués

Les motifs visuels récurrents n'existent pas comme classes CSS : ils sont
recopiés à la main dans chaque route. Recensement des duplications exactes :

| Motif | Occurrences | Fichiers |
|---|---|---|
| En-tête de page (`justify-content:space-between;align-items:center;gap:0.75rem;flex-wrap:wrap` + `h1 style="margin:0"`) | **6** | `grocery_list/list.rs`, `recipes/list.rs`, `budget/list.rs`, `stocks/list.rs`, `agenda/calendar.rs`, `agenda/imports.rs` |
| Ligne de liste (`padding:0.6rem 0;border-bottom:1px solid var(--border)`) | **9** | `messagerie/thread.rs` (×2), `recipes/list.rs` (×2), `groups/list.rs`, `groups/members.rs`, `budget/list.rs`, `stocks/list.rs`, `grocery_list/list.rs` |
| Carte / encadré (`margin-top:1.5rem;padding:1rem;border:1px solid var(--border);border-radius:6px`) | **3** | `account/mod.rs` (×2), `admin/users.rs` |
| Sous-titre de section (`h2` avec `font-size` inline en 1.05 ou 1.1 rem) | **14** | `groups/settings.rs` (×6), `agenda/detail.rs` (×3), `account/mod.rs` (×3), `groups/list.rs`, `groups/members.rs` |
| Pastille / badge (`font-size:0.8rem;padding:0.1rem 0.4rem;border-radius:3px`) | **4** | `stocks/list.rs`, `stocks/detail.rs`, `grocery_list/list.rs`, `agenda/calendar.rs` |

Effets :

- **Dérive garantie.** Deux tailles différentes coexistent déjà pour le même
  niveau de titre (`1.05rem` dans `account/mod.rs:128` vs `1.1rem` dans
  `agenda/detail.rs:288`). Le rayon de bordure oscille entre `3px` (badges) et
  `6px` (boutons, champs, cartes).
- **Toute correction visuelle est une opération à 9 endroits.** Changer
  l'apparence d'une ligne de liste demande d'éditer 7 fichiers Rust.
- **Le CSS ne dit pas la vérité sur l'app.** En lisant `style.css`, on ne
  soupçonne pas qu'il existe des cartes, des badges, des barres d'actions ou
  un calendrier.

### 3.3 Aucun retour d'interaction — ce qui « fait rudimentaire »

La feuille de style ne contient **aucune** règle `:hover`, `:focus-visible`,
`:active`, ni `transition`. Concrètement :

- **Rien ne réagit au survol.** Sur PC, où le pointeur est le mode
  d'interaction principal, aucun bouton, lien, ligne de liste ou cellule de
  calendrier ne signale qu'il est cliquable. C'est le premier facteur du
  ressenti « interface rudimentaire ».
- **La navigation au clavier repose sur l'anneau de focus par défaut du
  navigateur**, jamais stylé — et donc à contraste non maîtrisé sur les
  boutons pleins (`--accent` en fond) comme en thème sombre.
- **Aucune transition**, y compris là où l'état change de façon abrupte
  (ouverture des `<details>` d'édition dans la messagerie, `thread.rs:109`).
- **Aucun état actif dans la navigation** : `aria-current` n'apparaît nulle
  part dans le code. Les 8 liens de la nav (`src/app.rs:89-99`) sont rendus
  identiques quelle que soit la page consultée — l'utilisateur ne sait pas où
  il se trouve.

### 3.4 Une navigation qui ne passe pas à l'échelle

L'en-tête (`src/app.rs:86-113`) empile, dans une colonne de 448 px :

1. une `<nav>` en `display:flex;gap:0.75rem` de **8 liens textuels** (Accueil,
   Agenda, Stocks, Recettes, Liste de courses, Budget, Messagerie, Groupes,
   + Admin pour le superadmin) ;
2. sur la même ligne, le nom de l'utilisateur, « Mon compte » et un bouton
   « Se déconnecter » ;
3. en dessous, le sélecteur de famille active (un `<select>` + un bouton
   « Changer »).

Sans `flex-wrap`, ces 8 liens plus le bloc compte débordent largement de 448 px.
Le tout est stylé en `.muted` (`src/app.rs:88`) — donc la navigation
principale de l'application est rendue en **gris secondaire, à 0.85 rem**,
hiérarchiquement en dessous du contenu qu'elle dessert.

Le sélecteur de famille est un formulaire qui exige **deux gestes** (choisir
dans le `<select>`, puis cliquer « Changer ») et provoque un rechargement
complet, alors qu'il s'agit du commutateur de contexte le plus structurant de
l'app.

Enfin, la page d'accueil (`routes/home.rs:31-36`) affiche littéralement
« Bienvenue / Vous êtes connecté. » — aucun tableau de bord, aucune entrée
vers les 8 domaines, alors que c'est la première page vue après connexion.

---

## 4. Bugs visuels concrets (corrigeables immédiatement)

### 4.1 Deux variables CSS utilisées mais jamais définies → thème sombre cassé

`--accent-bg` et `--chip-bg` sont référencées avec une valeur de repli claire,
mais ne sont **définies nulle part** dans `style.css`. Le repli s'applique donc
*toujours*, y compris en thème sombre :

| Emplacement | Code | Effet en thème sombre |
|---|---|---|
| `agenda/calendar.rs:233` | `background:var(--accent-bg,#eef)` | cellule du jour en `#eef` (quasi-blanc) sous un texte `--fg: #eaeaea` → **illisible** |
| `agenda/calendar.rs:269` | `background:var(--accent-bg,#eef)` | idem, vue semaine |
| `agenda/detail.rs:353` | `background:var(--accent-bg,#eef)` | idem, occurrence mise en avant |
| `agenda/calendar.rs:307` | `background:var(--chip-bg,#f0f0f5)` | pastilles d'événements en `#f0f0f5` sous texte `#eaeaea` → **illisible** |

C'est le défaut le plus grave du lot : en thème sombre, **le contenu de
l'agenda devient invisible**. Contraste estimé ≈ 1.1:1 là où WCAG AA exige
4.5:1.

### 4.2 `textarea` absent du sélecteur de champs

`style.css:33` stylise `input, select` — **pas `textarea`**. Les 14 `textarea`
de l'app (`messagerie/thread.rs` ×8, `recipes/new.rs` ×4, `agenda/new.rs`,
`agenda/edit.rs`) tombent donc sur les styles par défaut du navigateur :
police monospace, bordure en relief, **fond blanc et texte noir en thème
sombre**. Dans le formulaire de nouvel événement, un `<input>` sombre et un
`<textarea>` blanc se retrouvent côte à côte.

### 4.3 Cibles tactiles sous le seuil

Les boutons sont en `padding:0.6rem 0.9rem` avec `font-size:1rem`
(`style.css:41-51`), soit ≈ 40 px de haut — sous les 44 px recommandés
(WCAG 2.5.5 / iOS HIG). Plus critique, le `.pw-toggle` (`style.css:74-85`) est
en `padding:0.25rem 0.4rem` autour d'une icône de 18 px → **~28 px**, difficile
à atteindre au pouce. Même problème pour les flèches « ◀ / ▶ » du calendrier
(`calendar.rs:195,197`), qui sont des caractères Unicode dans un `.button`.

### 4.4 Bordure de bouton incohérente

`button, .button` porte `border:1px solid var(--border)` **et**
`background:var(--accent)` (`style.css:41-51`) : un bouton primaire bleu est
donc cerclé d'un gris clair qui ne correspond à rien. Sur `.danger`
(`style.css:56-60`) la bordure est explicitement recolorée, sur le primaire non
— l'oubli est visible.

### 4.5 Notification de succès en bleu d'accent

`.notice.success` (`style.css:90`) utilise `--accent` (bleu) faute d'une
couleur sémantique de succès. Un message de confirmation est donc
visuellement identique à une information neutre, et rien dans la palette ne
couvre l'avertissement.

### 4.6 Pas de `color-scheme` sur les champs en thème sombre

`:root` déclare `color-scheme: light dark` (`style.css:2`), ce qui est correct,
mais les `<select>` et les `<input type="number">` / `type="date"` conservent
des widgets natifs dont l'apparence n'est pas vérifiée en thème sombre —
notamment le sélecteur de famille dans l'en-tête et les champs de date de
l'agenda.

---

## 5. Audit page par page

| Écran | Fichier | Problèmes spécifiques |
|---|---|---|
| **Accueil** | `routes/home.rs` | Page vide (« Bienvenue / Vous êtes connecté »). Aucun tableau de bord, aucune entrée vers les domaines. Premier écran après connexion. |
| **Agenda — mois** | `agenda/calendar.rs:255` | 7 colonnes dans 448 px (~57 px/jour) ; cellule du jour et pastilles illisibles en thème sombre (§4.1) ; hauteur fixe `6.5rem` qui tronque les journées chargées sans indicateur de débordement. |
| **Agenda — semaine** | `agenda/calendar.rs:275` | `min-width:8rem` × 7 dans 28 rem → les jours passent à la ligne, la vue perd son sens. |
| **Agenda — détail** | `agenda/detail.rs` | 3 sous-titres `h2` stylés inline ; sections (rappels, occurrences, pièces jointes) empilées sans hiérarchie ni encadrement ; largeur inutilisée sur PC. |
| **Agenda — imports Google** | `agenda/imports.rs` | En-tête de page dupliqué ; 13 styles inline. |
| **Stocks — liste** | `stocks/list.rs` | Lignes flex à 448 px avec badge « Stock bas » + actions ; badge en `var(--error,#c0392b)` avec repli codé en dur. |
| **Recettes — liste** | `recipes/list.rs` | Deux motifs de ligne différents dans le même fichier (`:83` et `:101`) ; bloc « ingrédients manquants » sans traitement visuel distinct. |
| **Liste de courses** | `grocery_list/list.rs` | 15 styles inline ; formulaire de prix inline dans chaque ligne (`input width:7rem` + bouton) → ligne saturée sur mobile ; rayé via `text-decoration:line-through` sans autre signal d'état. |
| **Budget** | `budget/list.rs` | Résumé mensuel et liste des dépenses partagent le même traitement visuel ; aucune mise en valeur des totaux. |
| **Messagerie** | `messagerie/thread.rs` | **31 styles inline** — le pire fichier. Fil de discussion rendu comme une liste de `<li>` à bordure basse, sans distinction visuelle entre ses propres messages et ceux des autres. Formulaire d'édition en `<details>` dont le `<summary>` porte la classe `.button` (détournement). Pas de zone de saisie fixe. |
| **Groupes — réglages** | `groups/settings.rs` | 6 sous-titres `h2` stylés inline ; page la plus dense en sections sans structure de carte. |
| **Groupes — membres** | `groups/members.rs` | Lignes de liste dupliquées ; actions de rôle sans hiérarchie. |
| **Mon compte (RGPD)** | `account/mod.rs` | Motif de carte dupliqué 2× ; la section « suppression programmée » réutilise `.notice.error` avec une bordure ajoutée inline — un cas d'usage qui mériterait un composant. |
| **Admin — utilisateurs / familles** | `admin/users.rs`, `admin/groups.rs` | Tables larges en scroll horizontal dans 448 px ; sous-nav (`users.rs:123`) stylée inline sans état actif. |
| **Auth (login/register/reset)** | `auth/*.rs` | **Les seules pages correctement servies** par le layout actuel : formulaires étroits et centrés, c'est ce pour quoi la feuille a été écrite. À préserver tel quel. |
| **Politique de confidentialité** | `routes/privacy.rs` | `.prose` est correct sur le fond, mais la ligne de texte à 448 px est *trop étroite* pour du long texte (optimum ≈ 60-75 caractères). |

---

## 6. Direction retenue

Décision prise en amont de cet audit : **navigation latérale persistante sur
grand écran, contenu à largeur variable selon la page**.

```
┌────────────┬──────────────────────────────┐
│ 🏠 Accueil │  Agenda — Juillet 2026       │
│ 📅 Agenda  │  ┌────┬────┬────┬────┬────┐  │
│ 📦 Stocks  │  │ L  │ M  │ M  │ J  │ V  │  │
│ 🍲 Recettes│  ├────┼────┼────┼────┼────┤  │
│ 🛒 Courses │  │  1 │  2 │  3 │  4 │  5 │  │
│ 💶 Budget  │  │    │▣RDV│    │▣Gym│    │  │
│ 💬 Messages│  └────┴────┴────┴────┴────┘  │
│────────────│                              │
│ Famille ▾  │                              │
│ Mon compte │                              │
└────────────┴──────────────────────────────┘
```

Principes qui en découlent :

1. **Trois largeurs de contenu, pas une.** Une classe par intention :
   étroite (formulaires, auth — le `28rem` actuel), lisible (texte long,
   détail — ~65 rem), pleine (calendrier, tables, messagerie — 100 % avec une
   marge). Le `shell()` doit accepter la largeur en paramètre plutôt que
   d'imposer `.container`.
2. **La sidebar devient une barre d'onglets basse sur mobile**, ce qui améliore
   aussi l'ergonomie téléphone : les 8 liens gris de 0.85 rem actuels
   deviennent des cibles tactiles atteignables au pouce.
3. **Le CSS reste un fichier unique, écrit à la main, sans dépendance** — la
   contrainte du §1 est structurante, pas négociable.
4. **Aucune modification des rôles ARIA, des `name` de formulaire ni des
   libellés visibles**, pour que la suite e2e continue de passer (§1).

---

## 7. Découpage en lots

Ordonné par rapport valeur/risque. Chaque lot correspond à une issue GitHub.

| Issue | Lot | Portée | Risque |
|---|---|---|---|
| [#65](../../issues/65) | Corriger les bugs de thème sombre | Définir `--accent-bg` / `--chip-bg` dans les deux thèmes ; ajouter `textarea` au sélecteur de champs (§4.1, §4.2) | Très faible — quelques lignes de CSS |
| [#66](../../issues/66) | Poser les tokens du design system | Échelle d'espacement, échelle typographique, rayons, couleurs sémantiques (succès/avertissement), tokens de surface. Cible : `DESIGN.md` | Faible — additif, aucune régression visuelle attendue |
| [#67](../../issues/67) | Servir les polices auto-hébergées | `apps/web` ne sert **aucun** fichier statique aujourd'hui : ajouter `tower-http` + `ServeDir`, les deux `.woff2` variables sous-ensemblés, `Cache-Control: immutable`. Pas de CDN — cela exposerait l'IP des visiteurs à un tiers et créerait un traitement à déclarer au registre RGPD | Faible — nouvelle route, isolée |
| [#68](../../issues/68) | Extraire les composants dupliqués en classes | `.page-header`, `.list-row`, `.card`, `.badge`, `.field`, `.btn` → supprimer les 173 styles inline (§3.2) | Moyen — touche 20 fichiers, mais mécanique |
| [#69](../../issues/69) | États d'interaction | `:hover`, `:focus-visible`, transitions, `aria-current` sur la nav (§3.3) | Faible |
| [#70](../../issues/70) | Layout responsive + sidebar | Breakpoints, `shell()` paramétré par largeur, sidebar desktop / barre d'onglets mobile (§3.1, §3.4, §6) | Élevé — c'est la refonte structurelle |
| [#71](../../issues/71) | Reprendre l'agenda avec la largeur retrouvée | Vue mois et semaine exploitant l'espace, débordement des journées chargées, **et bascule automatique en vue liste sous 861 px** (§3.1, §5) | Moyen — dépend de #70 |
| [#72](../../issues/72) | Reprendre la messagerie | Distinction émetteur, zone de saisie, sortir les 31 styles inline (§5) | Moyen — dépend de #68 et #70 |
| [#73](../../issues/73) | Tableau de bord d'accueil | Remplacer « Vous êtes connecté » par des entrées vers les domaines (§3.4) | Faible — dépend de #66 et #68 |
| [#74](../../issues/74) | Accessibilité tactile et contraste | Cibles ≥ 44 px, contraste AA vérifié sur les deux thèmes (§4.3, §4.6) | Faible |

**#65, #66 et #67 sont sans dépendance et peuvent partir immédiatement.**
**#70 est le point de bascule** : #71, #72 et #73 en dépendent. #74 clôt le
chantier.
