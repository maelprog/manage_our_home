# Design System — Manage our home

Source de vérité pour toute décision visuelle dans `apps/web`.
L'état des lieux qui a motivé ce système est dans
[`docs/design-audit.md`](docs/design-audit.md).

---

## Contexte produit

- **Ce que c'est :** une application de gestion familiale — agenda partagé,
  stocks, recettes, liste de courses, budget, messagerie. Auto-hébergée
  (docker-compose + Caddy), interface en français.
- **Pour qui :** les membres d'un foyer, tous âges, non techniques. Deux usages
  distincts : consultation rapide sur téléphone (« qu'est-ce qu'il y a
  aujourd'hui », « qu'est-ce qu'il manque »), et planification sur PC (budget,
  menus, calendrier).
- **Type :** application web de données, rendue côté serveur.
- **Ce qu'on veut qu'il en reste :** l'impression d'un outil de maison — calme,
  fiable, lisible en un coup d'œil — et non d'un panneau d'administration.

### Contraintes non négociables

1. **Rendu serveur pur, aucun JS de framework.** Pas d'hydratation, pas de WASM.
   Toute solution est CSS-first.
2. **Le CSS est servi sous une URL qui porte l'empreinte de son contenu**
   (`/assets/style-<empreinte>.css`, `apps/web/src/assets.rs`) — il était
   inliné dans chaque réponse jusqu'à #89, **et c'est le seul point de cette
   liste qui soit un arbitrage plutôt qu'une règle : il a tenu tant que le
   budget a tenu.** Ce qu'on garde de l'inlining : l'impossibilité
   structurelle qu'un HTML neuf soit servi avec un CSS périmé. L'empreinte,
   l'URL, les octets servis et le `<link>` de chaque page sortent tous de la
   **même constante `include_str!` du binaire** — jamais d'un fichier sur
   disque, jamais d'une étape de build — donc aucun déploiement ne peut les
   désaccorder. Ce qu'on regagne : la feuille est cachable (`immutable`, un
   an), payée une fois par visiteur et par déploiement au lieu d'une fois par
   page vue. Ce qu'on paie : un aller-retour bloquant au premier rendu sur un
   cache froid. Conséquence pratique, qui change : une règle n'est plus
   refacturée à chaque page, mais la feuille reste bornée par un budget — pas
   de framework utilitaire, pas de redondance. Le seuil chiffré et ses
   garde-fous sont dans
   [Livraison du CSS](#livraison-du-css--budget-et-porte-de-sortie).
3. **Aucune dépendance CSS externe.** Un fichier, écrit à la main.
4. **Les polices sont auto-hébergées, jamais servies par un CDN tiers.**
   Le projet est RGPD-compliant et documente ses traitements
   (`docs/registre-traitements.md`) : appeler Google Fonts ou Bunny Fonts
   exposerait l'IP de chaque visiteur à un tiers, ce qui créerait un traitement
   à déclarer. Les fichiers `.woff2` sont servis par `apps/web` lui-même.
5. **Les rôles ARIA, les `name` de formulaire et les libellés visibles ne
   changent pas** — la suite Playwright (`e2e/tests/*.spec.ts`) s'appuie dessus.

---

## Direction esthétique

- **Direction :** utilitaire chaleureux.
- **Niveau de décoration :** minimal — la typographie et l'espacement font tout
  le travail. Aucune texture, aucun dégradé, aucune ombre décorative.
- **Ambiance :** du papier posé sur un plan de travail. Neutres chauds plutôt
  que blanc clinique ou gris-bleu SaaS. Une seule couleur saturée dans toute
  l'interface, donc tout ce qui est teinté porte du sens.
- **Ce qu'on refuse :** les primaires saturées et le codage couleur bariolé de
  la catégorie (Cozi, FamilyWall), les sans géométriques arrondis qui font
  « application pour enfants », les ombres portées et les cartes flottantes.

---

## Typographie

- **Titres (`h1`, `h2`, `h3`) :** **Fraunces** (variable, OFL) —
  un serif chaud et légèrement excentrique. C'est le choix qui empêche
  l'application de ressembler à un back-office : toute la catégorie utilise des
  sans arrondis. Réservé aux titres, jamais au corps.
- **Corps, UI, libellés, données :** **Source Sans 3** (variable, OFL) —
  humaniste, dessinée pour la lisibilité en petit corps, couverture complète des
  diacritiques françaises (é è à ç ù â ê î ô û ë ï ü œ), et **chiffres
  tabulaires intégrés**. Une seule famille couvre le corps, l'UI et les
  montants : pas de troisième fichier à charger.
- **Données chiffrées :** Source Sans 3 avec `font-variant-numeric: tabular-nums`
  sur les montants du budget, les quantités de stock et les heures de l'agenda.
- **Code / identifiants techniques :** `ui-monospace, SFMono-Regular, Menlo,
  monospace` (pile système — usage marginal, ne justifie pas un fichier).

**Chargement.** Deux `.woff2` variables, sous-ensemble latin + latin-ext,
servis par `apps/web` via un `ServeDir` avec `Cache-Control: immutable`,
et `font-display: swap`. `apps/web` ne sert aucun fichier statique aujourd'hui :
ajouter `tower-http` et la route est un prérequis, pas un détail.

**Échelle** — base 16 px :

| Jeton | Valeur | Usage |
|---|---|---|
| `--t-2xl` | 1.75rem | titre de page (`h1`) |
| `--t-xl` | 1.375rem | titre de section (`h2`) |
| `--t-lg` | 1.125rem | sous-titre (`h3`), montants mis en avant |
| `--t-base` | 1rem | corps — **base, jamais en dessous pour du texte lu** |
| `--t-sm` | 0.875rem | libellés de champ, actions, tableaux |
| `--t-xs` | 0.8125rem | métadonnées, badges, en-têtes de colonne |

Les deux dernières valeurs sont celles d'un écran large : sous 861 px,
`--t-sm` monte à 0.9375rem et `--t-xs` à 0.875rem
(voir [Layout](#layout) → plancher typographique).

Interlignage : 1.5 pour le corps, 1.25 pour les titres.
`letter-spacing: -0.01em` sur les titres, `0.04em` sur les micro-libellés
capitalisés.

---

## Couleur

**Approche : restreinte.** Un accent, des neutres chauds, un jeu sémantique
complet, plus une rampe catégorielle réservée à l'identification des membres.

### Neutres et accent

| Jeton | Clair | Sombre | Usage |
|---|---|---|---|
| `--bg` | `#FBFAF7` | `#1A1917` | fond de page |
| `--surface` | `#FFFFFF` | `#242320` | cartes, sidebar, champs — **une seule élévation** |
| `--fg` | `#1C1A17` | `#EDEAE4` | texte principal |
| `--muted` | `#6B655C` | `#A19A8F` | texte secondaire — contraste AA vérifié sur `--bg` **et** `--surface` |
| `--border` | `#E3DFD7` | `#38352F` | bordures, séparateurs |
| `--hover` | `#F2EFE9` | `#2E2C28` | fond au survol |
| `--accent` | `#14706E` | `#4FB3AF` | actions primaires, liens, état actif |
| `--accent-fg` | `#FFFFFF` | `#10201F` | texte sur `--accent` |
| `--accent-soft` | `#E1F0EF` | `#1E3A39` | fonds teintés (jour courant, nav active) |
| `--accent-hover` | `#105855` | `#6EC7C3` | aplat plein survolé (#69) |

Le pétrole remplace le `#2952cc` actuel : il évite le bleu SaaS générique et,
surtout, il ne rentre pas en collision avec le rouge d'erreur ni avec le vert
de succès.

### Sémantiques

| Jeton | Clair | Sombre |
|---|---|---|
| `--success` / `--success-soft` | `#3F7A34` / `#E6F0E2` | `#7CB86C` / `#22301E` |
| `--warning` / `--warning-soft` | `#9A6B10` / `#F7EEDA` | `#D9A441` / `#332813` |
| `--error` / `--error-soft` | `#A8342A` / `#F7E5E2` | `#E88075` / `#3A211E` |
| `--error-hover` | `#8C2A22` | `#F09A90` |

Les deux moitiés `-hover` sont la couleur creusée (clair) ou éclaircie
(sombre) de l'aplat qu'elles survolent, réglées pour que `--accent-fg` y reste
au-dessus de AA : 8,2:1 et 8,5:1 en clair, 8,5:1 et 7,8:1 en sombre.

Succès et avertissement **n'existent pas dans la feuille actuelle** :
`.notice.success` emprunte aujourd'hui le bleu d'accent, ce qui rend une
confirmation visuellement identique à une information neutre.

### Couleurs par membre

Rampe de 8 teintes sourdes (`--m1` … `--m8`), attribuée par hachage stable du
`user_id` — **aucune migration de base de données**, le modèle `GroupMember`
n'a pas de champ couleur et n'en a pas besoin.

| | Clair | Sombre | | Clair | Sombre |
|---|---|---|---|---|---|
| `--m1` | `#7A5EA8` | `#A48BD0` | `--m5` | `#A8555C` | `#D08A90` |
| `--m2` | `#2F7A9E` | `#64A8C8` | `--m6` | `#55707E` | `#8AA3B0` |
| `--m3` | `#4B8B4A` | `#7DB77B` | `--m7` | `#8A6D3B` | `#B99B68` |
| `--m4` | `#B07A2E` | `#D2A059` | `--m8` | `#6A6AA8` | `#9494CE` |

**La couleur n'est jamais le seul porteur d'information** (WCAG 1.4.1) :
elle accompagne toujours une initiale ou un nom.

### Règles

- **Aucune valeur codée en dur dans les routes.** Toute couleur passe par un
  jeton. Les replis `var(--x, #hex)` sont interdits : ils masquent les jetons
  manquants et c'est exactement ce qui a cassé le thème sombre de l'agenda
  (`--accent-bg` et `--chip-bg`, utilisés mais jamais définis).
  Les deux moitiés de cette règle sont tenues par des tests dans
  `apps/web/src/app.rs` : tout jeton référencé doit exister dans `:root`,
  tout jeton peint derrière `--fg` doit être redéfini dans le bloc sombre,
  et aucun `var()` ne peut porter de repli.
- **Contraste AA minimum** (4.5:1 texte normal, 3:1 texte large et bordures
  porteuses de sens), vérifié dans **les deux thèmes**.

---

## Espacement

- **Unité de base :** 4 px.
- **Densité :** confortable sur PC, tactile sur téléphone.
- **Échelle :** `--s2:2` `--s1:4` `--s2x:8` `--s3:12` `--s4:16` `--s6:24`
  `--s8:32` `--s12:48` `--s16:64`.
- **Cibles tactiles ≥ 44 px** sur tout élément interactif — boutons, liens de
  navigation, champs, bascule d'affichage du mot de passe, flèches du
  calendrier. Le `.pw-toggle` actuel fait ~28 px.

---

## Layout

- **Approche :** grid-disciplined. C'est une application de données, pas un
  objet éditorial : alignement prévisible, colonnes strictes.
- **Structure :** navigation latérale persistante à partir de 861 px
  (`--w-sidebar: 15rem`), qui devient une barre d'onglets basse en dessous.
- **Trois largeurs de contenu, choisies par la page** — `shell()` prend la
  largeur en paramètre au lieu d'imposer `.container` à tout le monde :

| Jeton | Valeur | Pages |
|---|---|---|
| `--w-form` | 28rem | authentification, création/édition, confirmations |
| `--w-read` | 65rem | détail d'événement, recettes, compte, politique de confidentialité |
| *(pleine)* | 100% | agenda, tables d'admin, messagerie, listes |

- **Rayons :** `--r-sm: 4px` (badges, pastilles) · `--r-md: 8px` (boutons,
  champs) · `--r-lg: 12px` (cartes). Pas de rayon uniforme partout, pas de
  `border-radius: 9999px` décoratif.
- **Élévation :** une seule surface (`--surface`) délimitée par `--border`.
  **Aucune `box-shadow` décorative** — l'ombre est réservée à l'anneau de focus.

Livré par #70, et voici ce que la mise en œuvre a fixé que ce tableau ne
disait pas.

- **Un seul markup pour les deux dispositions.** Les mêmes liens sont la
  colonne latérale au-dessus de 861 px et la barre basse en dessous ; c'est
  le CSS qui les déplace. Deux navigations rendues côte à côte donneraient
  deux `aria-current="page"` sur la même page, ce qui est pire qu'aucun.
- **La feuille est écrite mobile first**, la barre d'onglets dans le corps du
  document et la sidebar dans un `@media (min-width: 861px)`. Une paire
  `min-width` / `max-width` laisserait un trou entre 860 et 861 px CSS, où
  atterrit un viewport zoomé.
- **`shell()` porte la navigation**, qui devient ainsi le frère de `<main>` et
  non son premier enfant : une colonne de grille ne peut pas se placer à côté
  d'un contenu qui la contient. C'est aussi ce qui sort `<header>` de
  `<main>`, où il n'avait jamais eu sa place.
- **La barre d'onglets défile latéralement** plutôt que de comprimer ses
  libellés : neuf onglets ne tiennent pas sur 390 px. Les remplacer par des
  icônes n'est pas gratuit non plus — une icône dans le lien change son nom
  accessible, et c'est par ce nom que la suite Playwright sait sur quelle
  page elle est. Les onglets font 48 px de haut (`--s12`), au-dessus des
  44 px que demande [Espacement](#espacement).
- **Une page sans session ne rend aucune navigation** (authentification,
  politique de confidentialité vue déconnecté) : pas de grille, pas de
  colonne latérale de 15 rem tenant un seul lien.

### L'agenda sur téléphone : vue jour

La barre d'onglets ne suffit pas. Une grille de 7 colonnes sur un écran de
390 px donne ~50 px par jour, ce qui reste illisible quoi qu'on fasse au CSS.
**Sous 861 px, l'agenda bascule par défaut sur une vue jour**, la grille
mensuelle restant accessible par un bouton « Mois ». C'est un changement de
contenu, pas de mise en forme.

L'agenda expose déjà `?view=week|month&date=YYYY-MM-DD` avec prev/next et
« Aujourd'hui » (`apps/web/src/routes/agenda/calendar.rs:68,189`). **`view=day`
s'insère comme troisième valeur**, sans JS : chaque jour est un lien, chaque
navigation est un rechargement de page — cohérent avec le reste de
l'application.

**Bandeau de semaine.** Sept cellules en `grid-template-columns: repeat(7, 1fr)`,
chacune un `<a href="/agenda?view=day&date=…">` :

- lettre du jour (0.75rem, `--muted`, capitales),
- numéro du jour (**1.125rem, semi-gras, tabulaire**),
- jusqu'à 3 points de 5 px colorés par membre concerné — densité d'un coup d'œil,
- hauteur minimale 62 px, donc cible tactile confortable,
- jour courant cerclé en `--accent`, jour sélectionné en aplat `--accent`.

**Liste du jour.** Une ligne par événement, `min-height: 60px` :

| Élément | Traitement |
|---|---|
| Barre latérale 4 px | couleur du membre assigné |
| Heure | **0.9375rem, gras, tabulaire**, colonne fixe de 3.4rem ; durée en dessous en 0.8125rem `--muted` ; « Journée » pour un événement sur la journée entière |
| Titre | **1.0625rem, semi-gras** — délibérément plus gros que le corps du reste de l'application |
| Métadonnées | initiale colorée du membre (`.avatar` 20 px) + nom, pièces jointes, rappel |

#### Ce que #71 a livré, et ce qu'il concède

Cette spécification décrit **deux rendus** : une grille pour le PC, des lignes
construites pour le téléphone. Le coût de les rendre tous les deux ne tombe
pas sur la feuille, il tombe sur le **document** — donc payé à chaque page
vue, proportionnel aux données du foyer, sur `/agenda`, déjà la plus lourde
des sept routes qui tiennent dans le budget de réponse. L'arbitrage rendu
avant l'implémentation est donc : **un seul rendu, relu par le CSS.**

Le serveur émet la grille, une fois. Sous 861 px, un bloc
`@media not all and (min-width: 861px)` relit ces mêmes cellules en liste des
jours qui portent quelque chose (les vides tombent par `:has()`, sans que le
serveur les marque). Ce que ça concède, écrit ici pour que personne ne le
redécouvre :

- **la liste est une projection de cellules de grille**, pas des lignes
  dessinées pour un téléphone : un jour s'affiche par son numéro, pas
  « lundi 15 » — le jour de la semaine vit dans l'en-tête de table, que la
  liste masque, et le remettre dans chaque cellule coûterait du document ;
- **il n'y a pas de `view=day`**, donc pas de bouton « Mois » : il n'aurait
  rien à ramener. La bascule mois/semaine continue de faire ce travail sur
  les deux tailles d'écran ;
- **pas de barre ni de pastille colorée par membre** : WCAG 1.4.1 interdit
  que la teinte porte seule l'information, il lui faut donc le *nom* du
  membre à côté — un second appel API et un coût par pastille sur le
  document, c'est-à-dire exactement ce que l'arbitrage évite. À reprendre
  quand le budget de réponse le permettra ;
- **le titre d'événement atterrit sur `--t-base` (1rem)** et non sur les
  1.0625rem ci-dessus : aucun jeton ne porte cette valeur et en ajouter un
  coûte plus qu'il ne rapporte.

Ce qui est livré côté PC, en revanche, l'est en entier : la vue mois utilise
la largeur (`Width::Full` depuis #70) et sa hauteur de cellule est un minimum
porté par le `<tr>` — déclaré dans la requête 861 px, parce que sur une ligne
de tableau `height` est un plancher et sur le bloc que la liste téléphone en
fait, un plafond ; la vue semaine est **la même table sur une seule ligne**,
donc sept vraies colonnes au lieu du `flex: 1; min-width: 8rem` qui réclamait
56 rem dans un conteneur de 28 et retombait en liste verticale ; et une
journée qui déborde s'arrête à trois lignes, la dernière comptant le reste
(« +N autres ») et menant à la vue semaine, qui ne plafonne rien.

Une conséquence de cette table unique, qui n'allait pas de soi : `outside` —
le gris des jours affichés seulement parce que la grille du mois est faite de
semaines entières — **n'existe pas en vue semaine**. Les sept jours y sont
ceux qu'on a demandés, y compris quand la semaine chevauche deux mois ; les
griser dirait d'eux quelque chose de faux.

### Plancher typographique sur téléphone

`--t-xs` (0.8125rem / 13 px) est trop petit pour un écran consulté à bout de
bras dans une cuisine. **Sous 861 px**, remonter le plancher :
`--t-xs` → 0.875rem et `--t-sm` → 0.9375rem. Le corps reste à 1rem, les titres
d'événement passent à 1.0625rem.

Livré par **#70**, et par l'autre bout : `:root` porte les valeurs téléphone
et c'est le `@media (min-width: 861px)` qui rend à l'échelle ses propres
chiffres — la feuille est écrite mobile first. #71 n'y touche pas ; le titre
d'événement, lui, s'arrête à `--t-base` (voir ci-dessus).

---

## Composants

Classes à définir dans `style.css`, en remplacement des 173 attributs
`style="..."` recensés dans les routes :

| Classe | Remplace |
|---|---|
| `.page-header` | le motif titre + actions, dupliqué dans 6 fichiers |
| `.list-row` | la ligne de liste, dupliquée dans 9 endroits |
| `.card` | l'encadré `padding:1rem;border:1px solid…`, dupliqué 3 fois |
| `.badge` / `.badge.warn` | les pastilles, dupliquées 4 fois |
| `.avatar` | l'initiale colorée d'un membre (nouveau) |
| `.chip` | la pastille d'événement du calendrier |
| `.field` | le bloc `<label>` + champ, avec `input`, `select` **et `textarea`** |
| `.btn` (+ `.secondary`, `.danger`, `.sm`) | `button, .button` et ses variantes |
| `.notice` (+ `.success`, `.warning`, `.error`) | la classe actuelle, complétée |
| `.navlink` | les liens de navigation, avec état `aria-current="page"` |

**Le bouton primaire ne porte pas de bordure grise.** Aujourd'hui
`button, .button` cumule `background: var(--accent)` et
`border: 1px solid var(--border)`, ce qui cercle chaque bouton bleu d'un gris
sans signification.

### Classes de soutien

L'extraction (#68) a eu besoin de quelques classes que le tableau ci-dessus ne
nommait pas — chacune remplace un motif recopié dans plusieurs routes, et
aucune n'introduit de valeur hors des échelles :

| Classe | Rôle |
|---|---|
| `.split` | la primitive de `.page-header` et `.list-row` : une ligne dont les extrémités s'écartent, portée aussi par un `.notice` qui contient une action |
| `.actions` | un groupe de contrôles côte à côte (boutons, formulaire de filtre, cellule d'actions) |
| `.list` | un `<ul>` de `.list-row` : les séparateurs viennent des lignes, donc ni puces ni retrait |
| `.list-row.stacked` | une ligne dont le contenu s'empile (un message, une recette et son résumé) |
| `.card.inline` | la même carte disposée en une seule ligne de champs (formulaires « ajouter un article ») |
| `.field.inline` | l'étiquette d'une case à cocher, à côté de son texte |
| `.done` | ce qui est déjà traité : tâche complétée, article coché |
| `.multiline` | un texte dont les retours à la ligne sont ceux de l'auteur (message, méthode d'une recette) |
| `.current` | la cellule d'aujourd'hui, l'occurrence sur laquelle une fiche est ouverte |
| `.cal`, `.cal-cell`, `.cal-day` | l'agenda : une seule grille, que la vue mois affiche sur six semaines et la vue semaine sur une. `.cal-week`/`.cal-col` ont disparu avec #71 — la bande de semaine était un `flex` séparé, elle est aujourd'hui la même table |
| `.composer`, `.live-status` | la zone de saisie de la messagerie et sa ligne d'état. Repris par #72 : `.content > .composer` colle au bas du viewport au-dessus de 861 px, et le sélecteur descendant *direct* est le point — la même classe habille le formulaire d'édition d'une ligne, qui ne doit pas coller |
| `.list-row.mine` | un message que vous avez écrit (#72) : `--surface` plus la barre de 4 px en `--accent` que [Layout](#layout) décrit. Fondé sur l'écriture, jamais sur la permission |
| `summary` | le déclencheur d'une disclosure (#72) — élément, pas classe. Il portait `btn secondary sm` : une disclosure déguisée en bouton, dont la boîte masquait la pastille native qui est son affordance |
| `.actions > details[open]` | une disclosure ouverte prend la largeur de sa ligne (#72), ce que le `min-width: 16rem` inline du champ d'édition demandait à sa place |
| `.app`, `.content`, `.w-form`, `.w-read`, `.tabs` | la coque responsive de #70 : la grille sidebar + contenu, la colonne de contenu et ses deux largeurs bornées, la barre d'onglets qui devient la sidebar |

Le résiduel de `style="…"` assumé après #68 : une couleur de membre calculée
(`.avatar`), la largeur d'un champ de prix, l'alignement d'une colonne
numérique, la largeur minimale du champ d'édition d'un message.

**Après #72 il en reste cinq**, pour un plafond de six
(`inline_styles_are_bounded_to_a_justified_residue`) : la largeur minimale du
champ d'édition disparaît, les deux teintes de membre de la messagerie n'en
font qu'une — les deux formes de ligne partagent désormais une seule fonction
d'identité (`message_meta`) — et il s'en ajoute donc *une*, pas deux. Le
compte exact : deux couleurs de membre calculées (`groups/members.rs`,
`messagerie/thread.rs`), la largeur du champ de prix, et deux alignements de
colonne numérique dans `admin/groups.rs`.

---

## Interaction et motion

Livré par #69. Ce qui suit décrit la feuille telle qu'elle est, pas telle
qu'on l'espérait : avant #69 elle ne contenait **aucun** `:hover`, `:focus-visible`
ni `transition`, et `aria-current` n'apparaissait nulle part.

- **Approche :** minimal-fonctionnel. Aucune animation d'entrée : l'application
  fait un rechargement complet à chaque navigation, une animation au chargement
  deviendrait pénible dès la troisième page.
- **Durée :** `--dur: 150ms`, courbe `--ease: cubic-bezier(0.2, 0, 0.2, 1)`.
  Les deux moitiés sont des jetons parce que la neutralisation sous
  `prefers-reduced-motion` passe par eux (voir plus bas) : une durée écrite en
  dur continuerait de tourner pour qui a demandé l'inverse.
- **Transitions autorisées :** `background`, `border-color`, `box-shadow`
  au survol et au focus. Rien d'autre — donc rien ne bouge ni ne change de
  taille. La liste est tenue par un test
  (`only_the_three_allowed_properties_are_transitioned`).
- **`:hover` sur tout ce qui est interactif** — `button`/`.btn`, `.list-row`,
  `.chip`, `.navlink`, et le soulignement d'un lien de texte, qui s'épaissit
  plutôt que de se peindre un fond au milieu d'un paragraphe.
  Les aplats pleins prennent la moitié creusée de leur propre couleur
  (`--accent-hover`, `--error-hover`) : une simple teinte ferait passer
  `--accent-fg` sous AA sur le bouton survolé.
- **`:focus-visible`** : `outline: 2px solid var(--accent); outline-offset: 2px`
  sur les éléments cliquables, `box-shadow: 0 0 0 3px var(--accent-soft)` sur
  les champs. **Jamais `outline: none`.** Les champs neutralisent bien
  l'anneau du navigateur, mais avec un `outline: 2px solid transparent` et non
  `none` : en mode contrastes forcés, `box-shadow` est ignoré et les contours
  sont repeints par le système — c'est la seule écriture qui y survit.
- **`aria-current="page"`** sur le lien de navigation de la page courante.
  L'appartenance se décide sur le **premier segment de chemin**
  (`app::nav_link_is_current`), pas sur un préfixe : `/` préfixe toute
  l'application et allumerait « Accueil » partout, et le segment garde le lien
  Admin — qui pointe vers l'un des deux écrans — allumé sur l'autre.
  Le traitement visuel est le fond `--accent-soft` (4,99:1 en clair, 4,88:1 en
  sombre) **plus** la graisse 600, pour que la couleur ne soit pas le seul
  porteur d'information (WCAG 1.4.1).
- **La navigation n'est pas du contenu secondaire.** Elle est sortie de
  `.muted` : taille de base et `--fg`, pas 0,875 rem en gris.
- **`prefers-reduced-motion: reduce`** neutralise toutes les transitions, en
  une déclaration : `:root { --dur: 0s; }`. Toutes les transitions étant
  minutées par ce jeton, il n'y a ni balayage `* { transition: none !important }`
  ni liste à tenir à jour.

**Vérification.** L'issue demandait un parcours clavier complet, focus visible
à chaque étape, dans les deux thèmes. C'est
`e2e/tests/interaction.spec.ts` : Tab sur quatre pages × deux thèmes en
vérifiant l'indicateur sur chaque arrêt, l'unicité de l'onglet actif sur les
huit routes de la nav, la réaction au pointeur, et `transition-duration: 0s`
sous `prefers-reduced-motion`.

---

## Livraison du CSS — budget et porte de sortie

**État au 2026-08-03 (#89) : la feuille ne voyage plus dans le document.**
Elle est servie par `apps/web` sous `/assets/style-<empreinte>.css`, avec
`Cache-Control: public, max-age=31536000, immutable`. La porte de sortie
décrite plus bas a été prise ; ce qui suit garde le raisonnement complet,
parce que c'est lui qui fixe les seuils qui restent et qui explique pourquoi
la bascule devait se faire *ainsi* et pas autrement.

**Conséquence à dire explicitement, parce qu'elle change la façon d'écrire
dans `style.css` : un commentaire ne se paie plus à chaque page vue.** Il est
téléchargé une fois par visiteur et par déploiement, comme le reste de la
feuille. La « taxe sur la documentation » listée plus bas comme coût n°3 de
l'inlining n'existe plus. L'en-tête de `style.css` le dit désormais au seul
lecteur que ça concerne : celui qui s'apprête à écrire une règle.

**Et donc le dispositif à deux plafonds n'a plus de prose à protéger** — ce
qui rétrécit l'ordre des deux garde-fous sans l'annuler. Il est tentant, en
lisant le reste de cette section, de croire que cet ordre garde toute sa
valeur *et* que la prose est devenue gratuite : les deux ne peuvent pas être
vrais ensemble. Ce qui reste à l'ordre est écrit une seule fois, là où il est
calibré (« Seuil 3 » plus bas) : il ne décide plus *qui* paie la prose, il
décide seulement **quelle question un test rouge pose en premier**. Ce qui
reste borné, c'est le volume total de la feuille, une fois, dans un
aller-retour.

La feuille voyageait à l'intérieur de chaque document. C'était un pari, pas
une propriété du monde : on payait une copie par page vue pour épargner un
aller-retour bloquant au premier rendu. Le pari n'était gagnant que tant que
le document **et** la feuille tenaient ensemble dans la première fenêtre de
congestion. Il avait donc une taille au-delà de laquelle il devenait faux —
c'est cette taille qui manquait, #83 l'a écrite, et #89 est le moment où elle
a été atteinte.

### Ce que l'inlining apportait

1. **Zéro aller-retour bloquant au premier rendu.** Une feuille externe est
   render-blocking : le navigateur parse le HTML, découvre le `<link>`, ouvre
   une requête, attend. **C'est le seul des quatre que #89 abandonne** —
   chiffré plus bas.
2. **Impossibilité structurelle du décalage CSS/markup.** `include_str!` scelle
   la feuille dans le binaire : il n'existe aucun état où du HTML neuf est
   servi avec du CSS périmé — pas de nom haché, pas d'invalidation, pas de
   fenêtre de déploiement où les deux divergent. C'est une propriété de
   **correction**, pas de performance, et c'est le meilleur argument du lot.
   **Conservé par #89**, et c'est ce qui a dicté la forme de la bascule : le
   nom du fichier est l'empreinte SHA-256 de cette même constante, calculée
   dans le binaire au démarrage. Deux contenus différents ne peuvent pas
   partager une URL, et une URL ne peut pas désigner autre chose que ce que
   ce binaire-là sert. Servir la feuille depuis un fichier de `assets/` via le
   `ServeDir` existant aurait rouvert la fenêtre exactement là où on la
   fermait : l'image Docker copie `apps/web/assets` à la construction, le
   binaire et le fichier peuvent être à un déploiement l'un de l'autre.
   *Conservé, mais pas gratuitement* : la fenêtre se ferme par un 404, donc
   par une page sans style plutôt que par une page mal stylée — voir le coût
   n°2 plus bas, qui est le prix de cette conservation.
3. **Aucun pipeline d'assets**, ce qui est la contrainte n°3 vue de l'autre
   côté : rien à installer, rien à builder. **Conservé par #89** : l'empreinte
   se calcule à l'exécution sur une constante compilée, il n'y a ni étape de
   build, ni fichier généré, ni manifeste.
4. **Une pression permanente vers la sobriété.** Le gaspillage est visible, ce
   qui interdit de fait un framework utilitaire. C'est ce bénéfice qui a rendu
   #66 et #68 nécessaires. **Conservé, atténué** : la feuille reste bornée par
   un plafond compressé, mais ce plafond ne se paie plus par page vue.

### Ce qu'il coûtait

Les quatre sont réglés par #89. Ils sont gardés parce que ce sont eux qui ont
motivé la bascule, et parce que le jour où quelqu'un voudra revenir à
l'inlining, c'est cette liste qu'il faudra réfuter.

1. **Incachable par construction.** Le HTML d'une application de données dépend
   de la session et des données, donc n'est pas cacheable ; ce qu'on inline
   dedans hérite de cette non-cacheabilité. Or c'est une application de foyer,
   consultée plusieurs fois par jour, avec beaucoup de navigations par
   session : le profil de trafic où le cache rapporterait le plus est
   précisément celui où on y renonçait.
2. **Le coût suivait le nombre de pages vues, pas la taille de la feuille.**
   Chaque règle ajoutée était multipliée par le volume de navigation. Il suit
   désormais le nombre de déploiements.
3. **Une taxe sur la documentation.** Les commentaires sont 58 % de la feuille
   brute — et **encore 68 % de la feuille compressée** : gzip ne les rend pas
   gratuits. L'inlining les facturait à l'utilisateur à chaque page vue, ce qui
   créait une incitation perverse à moins commenter. Le projet a choisi
   l'inverse, et il a bien choisi ; c'est la manière de livrer qui devait
   céder, pas la prose — et c'est elle qui a cédé. La prose est aujourd'hui
   payée une fois par déploiement, et le garde-fou construit pour rendre cet
   arbitrage impossible à trancher en douce n'a plus rien à empêcher.
4. **Conflit avec une CSP stricte.** Il n'y en a pas aujourd'hui. Le jour où on
   en veut une, un `<style>` inline imposait `unsafe-inline`, ou un nonce/hash
   à générer par réponse. Une feuille externe est le cas trivial — c'est
   désormais le nôtre. (Le `<script>` inline de `messagerie/thread.rs` reste,
   lui, un obstacle. #72 l'a laissé en place et a corrigé le constat qui
   l'accompagnait : il n'est **pas** émis inconditionnellement — voir
   [Le budget](#le-budget) ci-dessous. Le sortir vers `/assets` sous son
   empreinte est la bascule de #89 rejouée sur un second actif, donc une
   issue à part.)

### Le budget

Mesures du 2026-07-30 (commit `e5785ac`) sur une stack docker complète, les
huit routes que nomme la nav (`apps/web/src/app.rs`), **avec un mois ordinaire
de données de foyer** — 40 événements, 40 articles de stock, 30 courses, 30
dépenses, 25 recettes, 50 messages :

| | Brut | gzip | zstd |
|---|---|---|---|
| Feuille seule † | 18 855 o | 7 199 o | — |
| Déclarations seules, commentaires strippés † | 7 856 o | 2 298 o | — |
| Réponse complète, 7 des 8 routes ‡ | 20 082 – 30 016 o | 7 949 – 9 953 o | 8 159 – 10 142 o |
| Réponse complète, `/messagerie` ‡ | 80 933 o | 15 615 o | 15 739 o |

† compressé par le test lui-même (flate2, niveau 6) — ‡ compressé par Caddy,
octets réellement reçus par le client (`curl -w '%{size_download}'`). Les deux
niveaux ne sont pas les mêmes : Caddy gzippe au niveau 5, ce qui donne 7 211 o
pour la même feuille, et son zstd — réglé pour la vitesse — sort ~2,6 % plus
gros que son propre gzip.

**`/messagerie` est déjà hors budget**, et c'est le constat le plus important
de cette section. Elle sort à 15 739 o compressés là où le budget est de
14 336. Deux causes cumulées : `thread.rs` émet un `<script>` inline de 7 274
octets ~~inconditionnellement~~, et la page rend jusqu'à 50 messages (100 avec
`?limit=`) de 4 000 caractères chacun.

> **Corrigé par #72 :** « inconditionnellement » est faux et l'est depuis
> l'origine. `page()` ne rend le script que sur la vue live (`if live`), donc
> jamais sur une fenêtre d'historique (`?before_created_at`+`?before_id`).
> Vérifié empiriquement. Ce qui reste vrai, c'est qu'il part sur toute vue
> live, y compris pour un visiteur dont le navigateur n'a pas `WebSocket` —
> le script teste la disponibilité une fois arrivé.

Le document seul y pèse 8 416 o compressés, contre 750 à 2 754 sur les sept
autres. Mesurée à vide elle est à 11 188 o : ce n'est donc pas un cas extrême
construit pour l'occasion, c'est une conversation de famille ordinaire qui
l'y amène.

Ce que ça veut dire, écrit sans le contourner : **le seuil de 14 KiB n'est pas
tenable route par route en bornant la feuille**, parce que la moitié document
est fonction des données et non du CSS. Sur `/messagerie` le déclencheur de la
porte de sortie a donc **déjà été franchi** — sortir la feuille de l'inlining
y ramènerait la page autour de 8,4 Ko. Ce n'est pas corrigé ici (#72 tient
cette page, et l'issue #83 exclut explicitement la bascule) : c'est constaté,
daté, et c'est le premier argument que reprendra la PR qui fera la bascule.
*C'est ce qui s'est passé : #89 a fait la bascule pour ce motif, et la page
est ressortie à 8 126 o — l'estimation était juste (voir ci-dessous).*

Deux routes hors nav, mesurées au passage : `/account` sort à 8 501 o et
`/privacy-policy` à 10 432 o. Toutes deux dans le budget, mais la seconde est
publique et de taille fixe — c'est du texte réglementaire, il ne fera que
s'allonger. À surveiller au même titre que les huit.

#### Ce que la bascule a rendu (#89)

Mesuré le 2026-08-03 avec `npm run seed` puis `npm run measure` (#85), sur la
même stack et la même base semée avant et après, gzip calculé localement des
deux côtés — donc comparables entre eux :

| Route | Avant (gzip) | Après (gzip) | Gagné |
|---|---|---|---|
| `/` | 10 543 o | **684 o** | −9 859 |
| `/agenda` | 13 288 o | **3 274 o** | −10 014 |
| `/stocks` | 12 726 o | **2 696 o** | −10 030 |
| `/recipes` | 12 628 o | **2 676 o** | −9 952 |
| `/grocery-list` | 12 887 o | **2 902 o** | −9 985 |
| `/budget` | 12 475 o | **2 489 o** | −9 986 |
| `/messagerie` | **18 093 o** | **8 126 o** | −9 967 |
| `/groups` | 10 750 o | **900 o** | −9 850 |

Le bruit de mesure sur ces chiffres est de l'ordre de quelques octets : deux
semis successifs du même corpus ne produisent pas exactement le même texte, et
une relecture indépendante est tombée à 14 o (0,08 %) de cette campagne.

**`/messagerie` rentre dans le budget** pour la première fois depuis qu'il
existe : 8 126 o contre 14 336, soit 6 210 o de marge, sans qu'une ligne de
cette page ait changé. Les 7 274 o de `<script>` inline y sont toujours et
restent le sujet de #72 ; ils ne la mettent simplement plus dehors.

La feuille, elle, est désormais une réponse à part : **27 213 o bruts**, et en
compressé trois chiffres qu'il faut garder distincts parce qu'ils servent à
des choses différentes — **10 131 o** avec flate2 niveau 6 (l'encodeur du
garde-fou d'`app.rs`, donc le chiffre auquel le plafond se compare), 10 093 o
avec le zlib de `npm run measure`, et **10 398 o gzip / 10 631 o zstd
réellement reçus à travers Caddy**, qui compresse bien le `text/css` et laisse
passer le `Cache-Control: immutable` (vérifié). Payés **une fois par visiteur
et par déploiement**, l'URL portant l'empreinte du contenu.

#### Ce qu'on abandonne : deux coûts, pas un

**1. Un aller-retour bloquant au premier rendu.** C'est le bénéfice n°1
ci-dessus, et il est réel : sur un cache froid, le navigateur parse le
document, découvre le `<link>`, demande la feuille et attend avant de peindre.
**Coût : un aller-retour, une seule fois.**

En octets, la bascule est déjà rentable à la **première** page vue : sur
`/agenda`, avant, 13 288 o ; après, 3 274 + 10 093 = 13 367 o — à 79 octets
près, la même chose. À partir de la deuxième page de la session, le visiteur
économise ~10 000 o à chaque navigation. Le coût n'est donc pas un volume,
c'est une **sérialisation** : deux allers-retours avant le premier pixel au
lieu d'un, et seulement au tout premier chargement.

Ordre de grandeur : ~30 ms de plus sur une liaison grand public à 30 ms de RTT,
sous la milliseconde sur le LAN ou le VPN où cette application tourne
aujourd'hui (`infra/Caddyfile` repousse l'exposition publique après la v1).
Face à ~10 000 o épargnés par page vue ensuite — ~16 ms à 5 Mb/s —
l'aller-retour est remboursé par la deuxième navigation.

**2. Une page peut sortir sans style pendant un déploiement.** C'est la
contrepartie exacte de la manière dont l'avantage n°2 est préservé : la
fenêtre CSS/markup est fermée **par un 404**. Un HTML émis par l'ancien
binaire, encore en vol quand le nouveau prend la main, résout son `<link>`
sur un nom qui n'existe plus — et le navigateur rend la page avec les polices
de repli et aucune règle. L'inlining rendait ça impossible : la feuille était
déjà dans la réponse.

Ce que ça vaut en pratique, borné plutôt qu'agité : la fenêtre est celle des
requêtes **en vol**, pas celle des caches. Le HTML de cette application ne
porte ni `Cache-Control`, ni `ETag`, ni `Last-Modified` (vérifié) : aucun
navigateur ne conserve un vieux document pour le rejouer plus tard. Il faut
donc qu'un document parte de l'ancien binaire et que sa requête de feuille
arrive après la bascule — quelques centaines de millisecondes, une fois par
déploiement, sur une application de foyer. Et l'échec est *visible et
transitoire* : un rechargement le corrige, là où une feuille périmée servie
sous un nom stable serait invisible et durable. C'est le troc, il est assumé
dans ce sens-là.

**Les polices, et pourquoi rien n'a été posé.** `@font-face` vit dans la
feuille, donc la chaîne passe de `document → police` à
`document → feuille → police` : un aller-retour de plus avant que les deux
`.woff2` ne partent. Un `<link rel="preload" as="font" crossorigin>` par
famille le supprimerait. Il n'y en a pas, pour trois raisons :

1. **`font-display: swap` fait que la police ne bloque jamais le texte** — la
   pile de repli (`ui-sans-serif, system-ui` et `Georgia`) est déjà déclarée
   et rendue. L'aller-retour supplémentaire rallonge un FOUT, il ne retarde
   ni le premier rendu ni la lisibilité.
2. **Un preload se paie sur chaque page vue** (~2 × 95 o bruts dans chaque
   `<head>`), pour raccourcir un FOUT qui n'arrive **qu'une fois par visiteur
   et par an** : les polices sont servies `immutable` depuis #67. C'est
   exactement le troc que cette issue défait — payer à chaque page vue un
   bénéfice de premier chargement.
3. Un preload est une **obligation** de télécharger, pas une indication : les
   deux familles partiraient sur chaque page, y compris là où l'une n'est pas
   utilisée.

Si l'exposition publique change la donne (RTT plus longs, visiteurs de
passage qui ne reviennent pas), c'est la ligne à rouvrir en premier — et la
mesure à refaire, parce que le coût du preload, lui, se mesure avec
`npm run measure`.

#### Les trois seuils, statués après #89

Chacun est repris explicitement, parce que la bascule change le motif de
chacun et qu'un seuil dont le motif a disparu est pire qu'un seuil absent.

| Seuil | Valeur | Aujourd'hui | Ce qu'on fait au dépassement |
|---|---|---|---|
| Réponse complète compressée, routes principales | **≤ 14 KiB** (14 336 o) | 8 204 o au pire (`/messagerie`), 686 – 3 272 sur les sept autres | alléger le **document** — la feuille n'y est plus |
| Feuille compressée | **≤ 13 KiB** (13 312 o), *relevé par #72, était 11 KiB* | 10 926 o | découper la feuille ; il n'y a plus de `/assets` où la déplacer |
| Déclarations compressées | **≤ 3 KiB + 64 o** (3 136 o), *relevé par #72, était 3 072* | 3 071 o | supprimer une règle redondante |

La colonne « Aujourd'hui » est celle de #72 (2026-08-06), mesurée sur la
campagne de ce lot. Elle n'est **pas** comparable octet à octet avec celle de
#89 (8 126 / 10 131 / 2 995) : ce sont deux semis et deux stacks, et l'écart
entre les deux campagnes est du même ordre que les deltas qu'on y lit — c'est
pourquoi les deltas avant/après de #72 sont mesurés des deux côtés de la
*même* base semée, et rapportés séparément.

**Seuil 1 — la réponse : garde son sens, change de remède.** Le document reste
render-blocking et reste ce que la première fenêtre de congestion doit porter ;
14 KiB reste le seul chiffre de ce document qui ne soit pas de notre fait. Ce
qui change, c'est la réponse au dépassement : « passer la feuille sur
`/assets` » a été consommé, il ne reste que réduire le document lui-même
(pagination, `<script>` inline de #72). Ce seuil n'est tenu par aucun test et
ne peut pas l'être — il dépend du volume de données d'un foyer.

**Seuil 2 — la feuille : sa dérivation tombe, le garde-fou reste, son chiffre
passe de 10 à 11 KiB.** C'est la seule vraie concession de #89, et elle est
écrite ici plutôt que subie.

Ce qui tombe. 10 KiB n'était pas un jugement sur les feuilles de style,
c'était une soustraction : 14 336 − le document le plus lourd (3 248 o sur
`/agenda`) = 11 088, arrondi à la baisse. Cette soustraction n'a plus d'objet
quand le document voyage sans la feuille. Et l'ancienne consigne — « ce
plafond ne se relève pas, le dépassement veut dire qu'on prend la porte de
sortie » — est *épuisée*, pas contredite : la porte a été prise, il n'y a pas
de troisième endroit où mettre la feuille.

Ce qui le remplace. Le plafond est désormais **encadré par deux contraintes**,
pas une — c'est ce que la première rédaction de cette section ratait, en
présentant le choix comme binaire (supprimer le garde-fou, ou le porter à
14 336). Les deux chiffres ci-dessous sont mesurés avec **flate2 niveau 6**,
l'encodeur du garde-fou lui-même :

- **Borne haute — un aller-retour.** La feuille est bloquante au premier
  rendu, elle doit donc arriver en un seul : IW10 ⇒ 14 336 o. La borne est
  conservatrice deux fois. D'abord parce que la feuille pèse 10 131 o
  (10 398 à travers Caddy). Ensuite parce qu'elle **ne reçoit même pas une
  IW10 fraîche** : elle est demandée sur la connexion qui vient de porter le
  document, un RTT plus tard, avec une `cwnd` déjà crue par le slow start —
  la fenêtre réellement disponible est plus large que celle qu'on lui compte.
- **Borne basse — l'ordre des deux garde-fous.** Les déclarations font 29,6 %
  de la feuille compressée (2 995 sur 10 131), donc le plafond des
  déclarations n'est atteint le premier que si celui de la feuille reste
  au-dessus de `3 072 / 0,296` = **10 391,5 o**. C'est cette borne, et non la
  précédente, qui rend 10 240 intenable : le plancher est passé *au-dessus*
  du plafond.

**11 264 tient dans cet intervalle** avec **1 133 o** de croissance de feuille
disponible et **872 o** de marge d'inversion — raisonnement et chiffres de #89.
**#72 a porté ce plafond à 13 312**, en refaisant la dérivation depuis la borne
physique une fois les en-têtes de réponse comptés ; voir la mise à jour plus
bas et l'entrée de journal du 2026-08-06. 14 336 aurait aussi été
défendable sur la seule borne haute — c'est même le nombre le plus permissif
qui le soit, et c'est pour ça qu'il n'est pas retenu : un plafond à 4 200 o
au-dessus de la feuille actuelle ne sonnerait avant plusieurs lots, ce qui
n'est pas un garde-fou. Le test
`the_compressed_stylesheet_fits_its_share_of_the_first_round_trip` est renommé
`the_compressed_stylesheet_still_arrives_in_one_round_trip` — renommé, pas
supprimé.

**Seuil 3 — les déclarations : motif rétréci par #89, valeur relevée par #72.**
Ce que #89 en disait — même chiffre, même remède, même test — a tenu jusqu'à
#72, qui a porté le plafond de 3 072 à **3 136** ; le remède et le test, eux,
n'ont pas bougé. Ce qui suit est le raisonnement de #89 sur la *calibration*,
qui reste valable et que #72 n'a fait qu'appliquer. Sa calibration — se
déclencher avant le plafond de feuille — ne sert plus ce qu'elle servait, et
c'est le point où il serait facile de tricher :

- Ce qu'elle protégeait a disparu. Elle existait pour que la pression ne tombe
  jamais sur les commentaires, parce que l'inlining facturait chaque
  commentaire à chaque page vue. La feuille est cachée ; un commentaire coûte
  un téléchargement par déploiement. **Il n'y a plus de taxe sur la prose dont
  protéger qui que ce soit.**
- Ce qui reste est plus étroit, et réel : l'ordre décide **quelle question le
  premier test rouge pose**. Le plafond des déclarations demande « une règle
  en répète-t-elle une autre ? » — local, répondable, corrigeable dans le lot
  courant. Le plafond de feuille demande « la stratégie de livraison
  tient-elle encore ? », dont la seule réponse restante est de découper la
  feuille, c'est-à-dire une issue. Garder la question bon marché en premier
  vaut d'être gardé. Et supprimer des commentaires reste le raccourci tentant
  devant un test de taille rouge, même s'il ne rapporte presque plus rien :
  l'ordre le tient hors du premier chemin.

C'est donc **la borne basse qui justifie le relèvement**, avec ce motif-là et
pas l'ancien. La marge d'inversion passe de ~0 à 872 o, et #89 ne la répare en
dégraissant rien : il la répare en donnant à la feuille une fenêtre à elle.

Un mot sur le « 0,34 octet » cité plus haut, parce que ce n'est pas une
propriété du monde. Après #71, avec flate2, le plancher valait 10 239,66 pour
un plafond de 10 240 : 0,34 o de marge. Une relecture indépendante au zlib
système mesure la même paire à 9 978 / 2 992, soit un plancher de 10 244,8 —
la paire y est **déjà inversée de ~4,8 o**. Le signe dépend de
l'implémentation de gzip ; ce sur quoi les deux lectures s'accordent, c'est
que la marge tenait dans 5 octets, donc qu'elle n'existait plus.

À la sortie de #89 il restait **77 octets de déclarations** à se partager pour
#72–#74 : ce lot n'ajoute pas une règle. **#72 les a dépensés et a relevé le
plafond ; l'état à jour est juste en dessous.**

#### Où en est le budget après #72

Les chiffres ci-dessus sont ceux de #89 et ne décrivent plus l'état du dépôt.
Mesuré sur la branche de #72 avec flate2 niveau 6, l'encodeur des garde-fous :

| | après #89 | après #72 | plafond | reste |
|---|---|---|---|---|
| Déclarations | 2 995 | **3 071** | **3 136** *(relevé par #72)* | **65 o** |
| Feuille | 10 131 | **10 926** | **13 312** *(relevé par #72)* | **2 386 o** |

**Les deux plafonds ont été relevés par #72, sur deux arbitrages distincts et
pour deux raisons sans rapport.** Les dérivations complètes sont dans les
doc-comments de `SHEET_CEILING` et `DECLARATIONS_CEILING`, et au journal.

**Les déclarations, 3 072 → 3 136 :** à 3 071 contre 3 072, appendre un bloc de
commentaire ordinaire de trois lignes mesurait **3 072**, pile le plafond, et
le suivant cassait le build — `css_without_comments` retire le texte du
commentaire mais garde son saut de ligne et son indentation. Un garde-fou dont
le remède est « supprimer une règle redondante » qui se déclenche sur un
paragraphe dit au prochain auteur exactement ce que #89 existe pour empêcher.

**La feuille, 11 264 → 13 312 :** l'écart entre 11 264 et la borne physique
était de la pression disciplinaire, pas de la performance. La dérivation refaite
depuis IW10 ne retombe pas sur 14 336, et **ce n'est pas qu'un terme manquait à
celle de #89** : #89 partait déjà de 14 600 « less headers and TLS framing »
pour arriver à 14 336, et l'écrivait après la bascule vers `/assets`, donc en
sachant que la feuille est une réponse à elle seule. Ce qu'elle ne faisait pas,
c'est **chiffrer** ces deux termes : 14 600 − 14 336 leur laissait **264 o** au
total, une allocation implicite portée par l'arrondi. Ce que #72 change, c'est
cette allocation, **264 → 346 o** — 14 600 − 280 o d'en-têtes (170 mesurés sur
`apps/web` lors du relèvement, non remesurés depuis, plus
`server`/`content-encoding`/`vary` à travers Caddy) − 66 o de cadrage TLS =
**14 254 o** de corps disponible. Les 82 o qui séparent 14 336 de 14 254 sont
exactement ce dont l'allocation a grossi. 14 KiB ne tient plus ; **13 KiB est le
plus grand palier de KiB qui tienne**, à **942 o** sous la borne. Le relèvement
**renforce** l'ordre des deux gardes au lieu de l'affaiblir — la contrainte
d'ordre est une borne *basse* sur ce plafond — et la marge d'inversion passe de
107 à **2 154,7 o**.

### Ce que coûte vraiment un paragraphe

Trois mesures antérieures de ce document étaient fausses : deux sur la
méthode, la troisième sur l'échantillon. La méthode est toute l'histoire.
**Appendre le *même* bloc en boucle** donne ~135 o pour le premier et 5 à
20 o pour chaque répétition — mais c'est gzip qui retrouve un texte déjà vu,
pas le coût d'écrire quelque chose. C'est le piège que le briefing du projet
signale et que ce document dénonce déjà, plus bas, pour un chiffre
antérieur : *mesurer sur des données peu variées fausse le verdict*.

Le chiffre qui veut dire quelque chose se mesure **en supprimant de vrais
blocs de commentaire de cette feuille, un par un, et en pesant ce qui
revient**, pas en ajoutant du remplissage.

**Ce document ne recopie plus le résultat.** Il l'a fait, sous la forme d'une
plage lue sur un échantillon de dix blocs, et cette plage a été prise pour
celle de la feuille : l'étalement réel des blocs est assez large pour que la
borne haute d'un petit échantillon **sous-estime largement le pire cas** — ce
qui est exactement l'erreur que commet un lecteur budgétant « au pire tant
d'octets le paragraphe ». Un chiffre qu'on ne recopie pas ne peut pas
périmer (#95).

**Comment l'obtenir sur la feuille du jour.** Retirer un bloc de commentaire
de `apps/web/src/style.css` avec ses lignes entières, repeser, prendre la
différence, recommencer bloc par bloc. Repeser avec **l'encodeur du garde-fou
et lui seul** : flate2 niveau 6, `Compression::default()`, celui de `gzipped`
dans `apps/web/src/app.rs` — c'est lui qui décide si le test passe. Le zlib de
Node (`npm run measure`) répond quelques octets à côté sans que personne se
trompe ; c'est un autre encodeur, et **dire lequel on lit fait partie du
chiffre**. Sur de la prose variée la feuille croît du même ordre : 10 926 →
11 076 (1 bloc) → 11 212 (2) → 11 450 (4) → 11 911 (8) → 13 139 (20).

**2 386 o, c'est donc une vingtaine de paragraphes de plus** — c'est le chiffre
sur lequel #73 et #74 peuvent tabler.

**Et les déclarations ne plafonnent pas.** Une rédaction antérieure l'affirmait,
sur le même corpus dégénéré. Sur de la prose variée elles montent lentement :
3 071 → 3 072 (1 à 4 blocs) → 3 074 (8) → 3 074 (20) → **3 075 (50)**. Les sauts
de ligne et l'indentation que `css_without_comments` conserve s'accumulent ; il
n'y a pas de palier, seulement une pente très faible. La conclusion pratique
tient — à ~0,08 o par bloc, aller de 3 071 à 3 136 demanderait quelque **800
paragraphes**, quand `SHEET_CEILING` sonne vers la vingtaine — mais elle tient
**parce que le garde-fou de feuille sonne d'abord**, pas parce que celui-ci
serait immunisé. Ne pas écrire « jamais ».

Restent ~65 o de déclarations pour des règles neuves. `SHEET_CEILING`, lui,
n'est **pas relevé une seconde fois** dans ce lot — mais 13 312 n'est pas une
clôture, et l'écrire serait faux. **13 312 n'est pas la borne physique** :
c'est le palier de KiB immédiatement sous elle, et il reste **942 o** entre les
deux (14 254 − 13 312, la borne étant celle dérivée plus haut). Ces 942 o ne
sont ni une marge calculée ni une réserve dimensionnée : c'est le prix de
l'arrondi. Le dire coûte moins cher que de laisser un lecteur de #73/#74 se
croire devant une issue de découpage alors qu'il reste de la place sous la
borne que ce lot vient de dériver.

Ce document a déjà écrit deux fois qu'un plafond de feuille ne se relevait pas,
et les deux ont été démenties : à 10 KiB (« on prend la porte de sortie » — #89
a pris la porte) puis à 11 KiB (« pas relevable ici » — relevé par le commit
suivant de cette PR même). Une troisième ne vaudrait pas mieux. Ce qui tient
sans réserve, et c'est aussi tout ce qu'affirme `app.rs`, c'est le **remède** :
au-delà du plafond il n'y a plus d'échappatoire architecturale, il faut
découper la feuille, ce qui est une issue.

Aucun garde-fou n'est supprimé. Les deux tests de `apps/web/src/app.rs`
existent toujours, l'un renommé et redérivé, l'autre intact.

<details>
<summary>La dérivation d'origine des 10 KiB, gardée pour mémoire</summary>

La colonne « aujourd'hui » est **remesurée le 2026-08-03, après #71**, avec le
corpus reproductible de `npm run seed` (#85) et non le jeu de données ad hoc
de la campagne #83 — les deux ne sont pas comparables entre eux. Ce que #69
avait coûté, sur ce même corpus : la feuille de 7 192 à 8 633 o compressés,
les déclarations de 2 299 à 2 670 o, `/agenda` de 10 495 à 11 936 o.

Ce que **#70** coûte, mesuré des deux côtés sur ce corpus et sur la même base
semée : la feuille passe de 8 633 à **9 570 o** compressés, les déclarations
de 2 670 à **2 921 o**, et chaque route gagne **925 à 944 o** (`/agenda`, la
plus lourde des sept, de 11 932 à 12 876 o). La **moitié document ne bouge
pas** (±6 o sur les huit routes) : sortir la navigation du corps de chaque
page et la remettre dans `shell()` n'ajoute pas un octet au document, tout le
coût est la douzaine de règles de la coque responsive. Aucun des trois
plafonds n'est franchi ; `/messagerie` était au-dessus du premier avant #69 et
son dépassement passe de +2 411 à +3 338 o (c'est #72 qui tient cette page).

Ce que **#71** coûte, mesuré des deux côtés le 2026-08-03 sur ce même corpus
et sur la même base semée : la feuille passe de 9 570 à **9 983 o** compressés
et les déclarations de 2 921 à **2 995 o**, pour la table unique de l'agenda,
son plafond de trois lignes et la relecture en liste sous 861 px. Chaque route
gagne **409 à 421 o** — `/agenda`, la plus lourde des sept, de 12 871 à
**13 292 o**, soit 1 044 o encore libres sous le budget de réponse. Et **la
moitié document ne bouge pas d'un octet** (3 241 o avant comme après sur
`/agenda`, 10 555 o bruts de part et d'autre) : c'est la vérification de
l'arbitrage rendu au début du lot — un seul rendu relu par le CSS, plutôt
qu'une grille et une liste dont une moitié serait masquée. Le second aurait
mis son coût là, dans la part qui est payée à chaque page vue et qui grossit
avec les données du foyer. Aucun des trois plafonds n'est franchi et #71 n'en
relève aucun.

**Mais la marge d'inversion, elle, est épuisée.** Le plancher décrit plus bas
(le plafond de feuille sous lequel les deux garde-fous échangent leur ordre)
vaut `3 072 / (2 995 / 9 983)` = **10 239,66 o**, pour un plafond de feuille à
10 240 : le plafond des déclarations reste le premier à mordre, mais d'un
octet, contre 175 après #70. Écrit autrement : **la feuille ne peut plus
recevoir un seul octet de commentaire sans que la pression bascule sur la
prose**, et un octet de déclaration ne rachète que ~2,3 o de commentaire. #72,
#73 et #74 se partagent 77 o de déclarations et 257 o de feuille — mais toute
dépense de prose non compensée par des règles inverse le dispositif. C'est un
constat, pas une demande de relèvement : c'est l'utilisateur qui arbitre.

**14 KiB** est le seul chiffre qui ne soit pas de notre fait : c'est ce que la
fenêtre de congestion initiale (IW10 — dix segments d'un MSS de 1 460 octets,
soit ≈ 14 600 octets, moins les en-têtes de réponse et le cadrage TLS) fait
tenir dans le premier aller-retour.

**10 KiB** est la part de la feuille, et voici la dérivation en entier, y
compris ce qu'elle concède. Sur les sept routes où le budget est atteignable,
le document pèse au plus 2 754 o compressés (`/agenda`, données réelles) :
14 336 − 2 754 = 11 582 o disponibles pour la feuille. On arrondit **à la
baisse** à 10 240, ce qui laisse 1 342 o de marge réelle sur la route la plus
lourde des sept — et cette marge est là parce que la moitié document grossit
avec les données, ce que `/messagerie` démontre en passant de 3 666 à 8 416 o
entre une messagerie vide et une page de conversation ordinaire.

**La marge est donc de 1 342 octets, pas d'un facteur.** Une version
antérieure de ce document annonçait « trois fois et demie » : c'était mesuré
sur des pages vides, et c'était faux.

Remesurée le 2026-07-31 avec le corpus de `npm run seed`, la moitié document
d'`/agenda` sort à 3 248 o compressés au lieu des 2 754 ci-dessus : la
dérivation devient 14 336 − 3 248 = 11 088, et la marge réelle sous le
plafond de 10 240 tombe à **848 octets**. Le raisonnement ne bouge pas, le
chiffre si — et il dépend du jeu de données, ce qui est précisément pourquoi
ce seuil-là n'est tenu par aucun test.

Pourquoi ne pas resserrer davantage, puisque la moitié document s'est révélée
deux à trois fois plus lourde que prévu ? Parce que la structure à deux
plafonds impose un plancher : les déclarations font 30,0 % de la feuille
compressée (2 995 sur 9 983 après #71 ; c'était 2 921 sur 9 570 après #70),
donc le plafond des déclarations n'est atteint le premier que si celui de la
feuille reste **au-dessus de 10 240 o** — ~10 065 après #70, ~10 239,66
aujourd'hui, c'est-à-dire à un octet du plafond lui-même. Descendre sous ce
plancher inverserait l'ordre des deux garde-fous et ferait retomber la
pression sur les commentaires — exactement ce que le dispositif existe pour
empêcher. 10 KiB est la première valeur ronde au-dessus de ce plancher ; c'est
une contrainte, pas un confort.

**3 KiB** est la part du CSS seul, calibrée pour être atteinte *avant* les
10 KiB si la feuille continue de croître au rythme actuel de commentaires — de
sorte que la pression tombe toujours sur les règles avant de pouvoir tomber
sur la prose. C'est ce plafond-là qui porte le mordant du dispositif.

**Une seule formule pour toutes les marges de ce document :
`(plafond − actuel) / plafond`** — la place qui reste, en part du plafond.
Les deux chiffres comparés ici l'étaient auparavant avec deux formules
différentes (25 % rapporté au plafond contre 42 % rapporté à la valeur
courante), ce qui les rendait incomparables alors que la phrase les opposait ;
c'est le constat mineur laissé par #83, corrigé ici. Après #69 : 13 % de marge
sur les déclarations, 16 % sur la feuille. Après #70 : 4,9 % sur les
déclarations, 6,5 % sur la feuille — le plafond des déclarations restait le
plus mordant des deux, mais de 175 octets seulement. **Après #71 : 2,5 % sur
les déclarations, 2,5 % sur la feuille** — les deux marges se rejoignent,
et c'est exactement ce que dit le plancher passé de ~10 065 à ~10 239,66 o
pour un plafond de feuille à 10 240. Ce qu'il reste à se partager pour
#72–#74 est **77 octets de déclarations** et 257 de feuille, avec cette
contrainte de plus : une dépense de prose non compensée par des règles
inverse l'ordre des deux garde-fous. Ni #70 ni #71 n'a relevé de plafond ; le
lot qui en aura besoin doit le demander dans son corps de PR, pas l'écrire au
passage.

Le plafond des déclarations peut être relevé, avec un motif écrit dans la PR
— c'est la même convention que le plafond de styles inline. **Le plafond de la
feuille, lui, ne se relève pas :** le dépasser signifie que le pari est perdu,
et la réponse est la porte de sortie ci-dessous, pas un nombre plus grand.

</details>

Les deux derniers seuils sont tenus par des tests dans `apps/web/src/app.rs`
(`the_compressed_stylesheet_still_arrives_in_one_round_trip` et
`the_compressed_declarations_stay_inside_the_design_system_budget`). Le
premier ne l'est pas, et ne peut pas l'être : il dépend du volume de données
d'un foyer. Il se vérifie en mesurant une stack qui tourne **avec des données
dedans** — une stack vide sous-estime le document d'un facteur deux à trois,
et c'est comme ça que `/messagerie` a failli n'être jamais mesurée.

### Compression

`infra/Caddyfile` fait `encode zstd gzip` sur le bloc `:80`, ce qui couvre le
HTML, le `text/css` de la feuille (vérifié après #89 : `Content-Encoding:
gzip`, et le `Cache-Control: immutable` passe intact) et le JSON de
`apps/api`. Sans compression, aucune des huit routes principales ne tenait
dans le premier aller-retour ; c'est `encode` qui rendait la prémisse de
l'inlining vraie, et c'est encore lui qui fait tenir la feuille de 27 213 o
bruts dans 10 398 o sur le fil.

`apps/web` ne compresse **pas** de son côté. Caddy laisse intacte une réponse
qui porte déjà un `Content-Encoding` (vérifié), donc un `CompressionLayer`
dans `apps/web` remplacerait le choix de Caddy par le sien sur le chemin
déployé, pour le seul bénéfice des accès directs à `web:3000` (dev, tests e2e)
où personne ne compte les octets. Le budget est tenu par un test unitaire, pas
par le transport.

### Porte de sortie : servir la feuille depuis `/assets` (prise le 2026-08-03, #89)

Le déclencheur était le premier des deux ci-dessous : `/messagerie` à
18 093 o compressés contre un budget de 14 336. Voici ce que la bascule est,
en une page, pour qui la relit dans six mois.

- **`apps/web/src/assets.rs` porte la constante** `STYLESHEET =
  include_str!("style.css")`. C'est la seule copie de la feuille à
  l'exécution.
- **Le nom est son empreinte** : les 16 premiers caractères hexadécimaux de
  SHA-256 sur cette constante, calculés une fois au démarrage
  (`/assets/style-<empreinte>.css`). 64 bits suffisent — personne ne choisit
  les octets, ils sont compilés — et l'URL est écrite dans chaque page, donc
  chaque caractère de plus se paie.
- **La route sert la constante**, pas un fichier. Le `ServeDir` de #67 reste
  pour les polices ; la feuille passe devant lui sur ce seul chemin. Servir la
  feuille depuis `assets/` aurait rouvert la fenêtre qu'on ferme : l'image
  Docker copie ce répertoire à la construction, le binaire et le fichier
  peuvent diverger.
- **`shell()` émet un `<link rel="stylesheet">`** dont l'URL vient de la même
  fonction. Empreinte, URL, octets servis et `<link>` sortent donc tous de la
  même constante : ils ne peuvent pas se désaccorder.
- **`Cache-Control: public, max-age=31536000, immutable`**, réutilisé tel quel
  du `map_response` de #67 — qui ne le pose que sur ce qui a été servi, jamais
  sur un 404.
- **Aucune étape de build, aucun fichier généré, aucun manifeste** : la
  contrainte n°3 tient.

**Ce qu'elle fait perdre** est l'avantage n°1, chiffré plus haut ; ce qu'elle
préserve est l'avantage n°2, et c'est ce qui a dicté sa forme.

Les deux déclencheurs qui étaient écrits ici, pour mémoire — le premier a
suffi :

- dépassement d'un des deux plafonds compressés ci-dessus ;
- exposition publique de l'application — le commentaire en tête de
  `infra/Caddyfile` la repousse après la v1. Une feuille cachable a beaucoup
  plus de valeur dès que les visiteurs ne sont plus quelques personnes sur un
  LAN, et une CSP stricte devient à ce moment-là un sujet.

---

## Journal des décisions

**Convention d'écriture, posée par #72 parce qu'elle était suivie sans être
dite.** Une entrée datée n'est jamais réécrite : ce qu'elle affirmait au jour
où elle a été prise reste lisible tel quel, et une décision qui la renverse
s'ajoute à la fin. Ce qui *peut* être ajouté à une entrée existante est un
**renvoi** vers l'entrée qui la dépasse — il ne modifie aucune affirmation et
évite au lecteur d'avoir à parcourir toute la table pour savoir si ce qu'il
lit tient encore. C'est la différence entre l'entrée du 2026-07-30 sur
`/messagerie` (fausse sur un point, laissée intacte, corrigée par l'entrée du
2026-08-05) et celle du 2026-08-04 sur le plafond des déclarations (exacte à
sa date, augmentée d'un renvoi vers le relèvement de #72).

| Date | Décision | Motif |
|---|---|---|
| 2026-07-29 | Système de design initial | Créé par `/design-consultation` à partir de l'audit `docs/design-audit.md` |
| 2026-07-29 | Sidebar persistante desktop, onglets bas mobile | Le `max-width: 28rem` unique rendait l'agenda et les tables d'admin inutilisables sur PC |
| 2026-07-29 | Fraunces en titres (pari assumé) | Différencie d'une catégorie uniformément en sans arrondis ; coût : un second fichier de police |
| 2026-07-29 | Neutres chauds plutôt que blanc/gris-bleu | Écran consulté plusieurs fois par jour ; coût : les pièces jointes photo ressortent légèrement jaunies |
| 2026-07-29 | Couleurs membres dérivées du `user_id` | Évite une migration ; `GroupMember` reste inchangé |
| 2026-07-29 | Polices auto-hébergées, pas de CDN | Un CDN de polices exposerait l'IP des visiteurs à un tiers — traitement à déclarer au registre RGPD |
| 2026-07-29 | Replis `var(--x, #hex)` interdits | Ce motif a cassé le thème sombre de l'agenda sans que personne le voie |
| 2026-07-29 | Vue jour + bandeau de semaine sur téléphone, `view=day` | Une grille de 7 colonnes est illisible sous 861 px quelle que soit la mise en forme ; le bandeau garde l'orientation sans sacrifier la taille du texte |
| 2026-07-29 | Plancher typographique remonté sous 861 px | 13 px est trop petit sur un téléphone consulté à bout de bras |
| 2026-07-30 | Classes de soutien ajoutées au tableau des composants (#68) | Le tableau nommait les motifs, pas les primitives dont ils sont faits ; l'extraction des 181 styles inline en a réclamé neuf de plus, chacune remplaçant un motif recopié dans plusieurs routes |
| 2026-07-30 | `--accent-bg` et `--chip-bg` retirés (#68) | Le premier était l'aplat provisoire du jour courant, remplacé par `--accent-soft` comme #66 l'annonçait ; le second n'existait que pour la pastille du calendrier, qui prend `--hover`. Plus aucun jeton hors de ce document |
| 2026-07-30 | `.badge.warn` reste un `--error` plein (#68) | La paire `--warning` de ce document mesure 4,06:1 en clair, sous AA ; `--error` + `--accent-fg` tient 6,2:1 dans les deux thèmes. Le réglage des paires sémantiques appartient à #74 |
| 2026-07-30 | CSS inliné — décision datée, plus une propriété héritée (#83) | L'inlining n'avait jamais été argumenté : la contrainte n°2 décrivait ce que `shell()` fait. Il est conservé pour l'impossibilité structurelle du décalage CSS/markup (`include_str!`), pas pour la performance, et requalifié en arbitrage valable **sous condition de budget** |
| 2026-07-30 | Budget de livraison : 14 KiB compressés par réponse, 10 KiB pour la feuille, 3 KiB pour les déclarations (#83) | 14 KiB est la fenêtre de congestion initiale, seul chiffre non arbitraire ; les deux autres s'en déduisent. Deux plafonds plutôt qu'un parce que « taille de la feuille » a deux réponses et deux remèdes : le brut compressé se corrige en sortant de l'inlining, les déclarations en supprimant une règle. Le second est calibré pour être atteint le premier, donc la pression ne tombe jamais sur les commentaires |
| 2026-07-30 | `encode zstd gzip` dans `infra/Caddyfile` (#83) | Le seul écart réellement hors-norme n'était pas l'inlining mais l'absence totale de compression : 20 à 81 Ko de texte brut par navigation, aucune route ne tenant dans le premier aller-retour. Après, 7,9 à 10,1 Ko sur sept des huit routes de la nav, qui y tiennent |
| 2026-07-30 | `/messagerie` constatée hors du budget de 14 KiB (#83) | 15 739 o compressés avec 50 messages ordinaires, 11 188 o à vide : un `<script>` inline de 7 274 o émis inconditionnellement plus 50 messages rendus. Le déclencheur de la porte de sortie est donc déjà franchi sur cette route. Non corrigé par #83 (bascule hors périmètre, page tenue par #72) — constaté et daté. **« Inconditionnellement » est faux : voir l'entrée du 2026-08-05.** (Renvoi ajouté par #72 — c'est l'entrée fausse qui en a le plus besoin.) |
| 2026-07-30 | Pas de `CompressionLayer` dans `apps/web` (#83) | Caddy laisse intacte une réponse déjà encodée (mesuré) : compresser dans `apps/web` imposerait son gzip au chemin déployé à la place du choix de Caddy, au bénéfice des seuls accès directs à `web:3000` (dev, e2e) où aucun octet n'est compté — et ajouterait une dépendance à un graphe qui porte déjà deux tower-http |
| 2026-07-30 | BREACH : aucune route exclue de `encode` (#83) | Le seul secret rendu dans un corps compressible est le jeton de réinitialisation (`reset_password.rs:42`). L'attaque demande une seconde chaîne choisie par l'attaquant dans la même réponse ; cette page n'en a aucune (sa seule variable est le jeton, qui doit parser en UUID). Usage unique et péremption 24 h vérifiés dans `apps/api/src/auth/mod.rs` |
| 2026-07-31 | Jetons `--accent-hover` / `--error-hover` (#69) | Un aplat plein survolé ne peut pas se contenter d'une teinte : `--accent-fg` y passerait sous AA. La moitié creusée (clair) ou éclaircie (sombre) de chaque aplat tient 7,8:1 au pire dans les deux thèmes |
| 2026-07-31 | Le lien de nav courant se décide sur le premier segment de chemin (#69) | Un test par préfixe allume deux onglets à la fois : `/` préfixe toute l'application. Le segment garde aussi une section allumée sur ses sous-pages (`/agenda/new`) et le lien Admin — qui pointe vers l'un des deux écrans — allumé sur l'autre |
| 2026-07-31 | Les champs neutralisent l'anneau du navigateur avec `outline: 2px solid transparent`, jamais `none` (#69) | En mode contrastes forcés, `box-shadow` est ignoré et les contours sont repeints par le système : `outline: none` y laisserait un champ focalisé sans aucun indicateur |
| 2026-07-31 | `prefers-reduced-motion` neutralise en remettant `--dur` à 0s (#69) | Une déclaration au lieu d'un `* { transition: none !important }` : toutes les transitions étant minutées par le jeton, il n'y a aucune liste à tenir à jour et aucune règle à sur-spécifier |
| 2026-07-31 | Marges du budget : une seule formule, `(plafond − actuel) / plafond` (#69) | Deux marges étaient comparées dans la même phrase avec deux formules différentes (25 % rapporté au plafond contre 42 % rapporté à la valeur courante) — constat mineur laissé par #83, corrigé au passage |
| 2026-07-31 | `shell()` prend la largeur **et** la navigation (#70) | La sidebar est une colonne de grille : elle ne peut pas être le premier enfant du `<main>` qu'elle borde. Les 73 routes passaient `app_header` en tête de leur propre corps, ce qui plaçait aussi `<header>` dans `<main>`. La largeur est un paramètre plutôt qu'une table chemin → largeur : une route sait ce qu'elle rend, une table centrale serait une chose de plus à tenir en phase avec le routeur |
| 2026-07-31 | Un seul markup de navigation pour les deux dispositions (#70) | Rendre une sidebar *et* une barre d'onglets mettrait deux `aria-current="page"` sur la même page. Le CSS déplace les mêmes liens ; la barre basse est le cas de base, la sidebar l'override à partir de 861 px — une paire `min-width`/`max-width` laisserait un trou entre 860 et 861 px CSS |
| 2026-07-31 | Barre d'onglets à défilement latéral, sans icônes (#70) | Neuf onglets ne tiennent pas sur 390 px. L'icône que demandait l'issue n'est pas gratuite : dans le lien elle change le nom accessible et le `textContent` sur lesquels s'appuie `e2e/tests/interaction.spec.ts` (`toHaveText("Accueil")`, `getByRole("link", { name: "Admin" })`), y compris en `::before` CSS que Chromium intègre au nom accessible. Reporté plutôt que payé par un ajustement de la suite |
| 2026-07-31 | Plancher typographique écrit à l'envers (#70) | `:root` porte les valeurs *téléphone* de `--t-xs`/`--t-sm` et le `@media (min-width: 861px)` y rétablit celles de l'échelle. Mobile first, une media query de moins, et le trou entre 860 et 861 px disparaît |
| 2026-08-03 | **Fin de l'inlining : la feuille passe sur `/assets` (#89)** | Le déclencheur écrit par #83 était atteint — `/messagerie` à 18 093 o compressés contre 14 336. Mesuré avant/après sur la même base semée : les huit routes de la nav perdent 9 850 à 10 030 o gzip chacune, `/messagerie` rentre dans le budget (8 126 o, 6 210 de marge) sans qu'une ligne de cette page change |
| 2026-08-04 | Une page peut sortir sans style pendant un déploiement (#89) | Coût nommé plutôt que découvert : la fenêtre CSS/markup se ferme par un **404**, donc un HTML de l'ancien binaire encore en vol rend une page sans règles. Borné aux requêtes en vol — le HTML ne porte ni `Cache-Control`, ni `ETag`, ni `Last-Modified`, donc rien n'est rejoué depuis un cache — et l'échec est visible et corrigé par un rechargement, là où une feuille périmée sous nom stable serait invisible et durable |
| 2026-08-03 | L'URL de la feuille est l'empreinte SHA-256 de la constante du binaire (#89) | C'est ce qui préserve l'avantage n°2 de l'inlining, sa seule propriété de *correction* : empreinte, URL, octets servis et `<link>` sortent de la même constante `include_str!`, donc aucun déploiement ne peut servir du HTML neuf avec du CSS périmé. Servir un fichier de `assets/` via le `ServeDir` de #67 aurait rouvert cette fenêtre (l'image Docker copie le répertoire, le binaire et le fichier peuvent diverger). Calculée à l'exécution : pas d'étape de build, la contrainte n°3 tient |
| 2026-08-04 | Plafond de feuille : **10 KiB → 11 KiB** (11 264 o), dérivation refaite (#89) | La seule concession du lot, déclarée. Les 10 KiB étaient une soustraction (14 336 − le document le plus lourd) qui n'a plus d'objet quand la feuille ne voyage plus dans le document, et la consigne « ce plafond ne se relève pas, on prend la porte de sortie » est épuisée : la porte a été prise. Le plafond est **encadré par deux bornes** et non plus par une : un aller-retour au-dessus (IW10 = 14 336, borne conservatrice — la feuille pèse 10 131 o flate2 et ne reçoit même pas une IW10 fraîche, elle arrive un RTT après le document sur la même connexion), et le plancher d'inversion des deux garde-fous en dessous (`3 072 / (2 995 / 10 131)` = 10 391,5 o — c'est *lui* qui rend 10 240 intenable). 11 264 laisse 1 133 o de croissance et 872 o de marge d'inversion. Arbitrage utilisateur : 14 336 était défendable sur la seule borne haute mais c'est le nombre le plus permissif qui le soit, et un plafond qui ne sonne pas avant plusieurs lots n'est pas un garde-fou. Test renommé `the_compressed_stylesheet_still_arrives_in_one_round_trip` |
| 2026-08-04 | Plafond des déclarations : **inchangé à 3 KiB**, mais son motif rétrécit (#89) | Même valeur, même remède, même test. Ce que sa calibration protégeait — que la pression ne tombe jamais sur les commentaires — **n'existe plus** : la feuille étant cachée, un commentaire coûte un téléchargement par déploiement et non un par page vue. Ce qui reste à l'ordre des deux gardes est plus étroit et réel : il décide quelle question le premier test rouge pose (« une règle en répète-t-elle une autre ? », locale et corrigeable, plutôt que « la livraison tient-elle ? », qui est une issue). C'est cet ordre-là, et non l'ancien motif, qui justifie le relèvement du plafond de feuille. #89 n'ajoute aucune déclaration : les 77 octets restants pour #72–#74 sont intacts *(le plafond a été relevé depuis, par #72 — voir l'entrée du 2026-08-05)* |
| 2026-08-04 | Le « 0,34 octet » de marge d'inversion est une mesure, pas une propriété (#89) | Chiffre obtenu avec flate2 niveau 6, l'encodeur du garde-fou. Une relecture indépendante au zlib système mesure la même paire à 9 978 / 2 992, soit un plancher de 10 244,8 : **déjà inversée de ~4,8 o**. Le signe dépend de l'implémentation de gzip ; les deux lectures s'accordent sur le seul point qui compte, la marge tenait dans 5 octets |
| 2026-08-03 | Pas de `rel=preload` sur les polices (#89) | La chaîne passe bien de `document → police` à `document → feuille → police`, mais `font-display: swap` fait que la police ne bloque jamais le texte : l'aller-retour de plus rallonge un FOUT. Un preload se paierait sur **chaque** page vue (~2 × 95 o) pour raccourcir un FOUT qui n'arrive qu'une fois par visiteur et par an (polices `immutable` depuis #67) — exactement le troc que #89 défait. À rouvrir si l'application est exposée publiquement |
| 2026-08-03 | Seuil d'absurdité du mesureur : 2 048 → 1 024 o (#89) | Il ne mesurait plus rien : la feuille inlinée pesait à elle seule ~27 000 o, donc toute réponse passait. La réponse étant désormais le document, la plus légère des huit routes tombe à 1 430 o bruts et le seuil redevient ce qu'il prétend être — attraper une page d'erreur ou une coque vide |
| 2026-08-05 | `.list-row.mine` se décide sur l'écriture, jamais sur `can_modify` (#72) | Un owner peut modifier le message d'un autre membre : marquer cette ligne comme sienne serait un mensonge sur qui a parlé, sur la seule page où « qui a dit ça » est l'information principale. `message_row` prend donc `mine` **et** `can_edit`, deux booléens distincts, là où un seul aurait suffi à faire compiler |
| 2026-08-05 | Le message propre prend `--surface` + une barre `--accent`, pas `--accent-soft` (#72) | `--accent-soft` aurait crié plus fort, mais met `--muted` — la ligne d'identité de chaque message — à **4,382:1** en thème sombre, sous AA. `--muted` n'est garanti que sur `--bg` et `--surface` (voir [Couleur](#couleur)), et retoucher la paire appartient à #74. La barre de 4 px est celle que [Layout](#layout) décrit déjà pour identifier un membre sur une ligne ; elle porte ici `--accent` parce qu'elle dit *vous* et non *qui* — *qui* est l'initiale colorée à côté du nom (WCAG 1.4.1) |
| 2026-08-05 | La zone de saisie ne colle qu'au-dessus de 861 px (#72) | Sous le point de bascule, la barre d'onglets fixe occupe déjà le bas du viewport : deux éléments collés l'un sur l'autre y mangeraient la moitié d'un écran de téléphone. Et le sélecteur est `.content > .composer`, descendant **direct** : la même classe habille le formulaire d'édition à l'intérieur d'une ligne de message, qui ne doit surtout pas se coller au viewport |
| 2026-08-05 | Plafond des déclarations : **3 072 → 3 136** (#72) | Premier relèvement, sur arbitrage utilisateur, et il corrige une pathologie et non une gêne. À 3 071 contre 3 072, appendre un bloc de commentaire ordinaire de trois lignes à `style.css` mesure **3 072** — pile le plafond — et le suivant casse le build : `css_without_comments` retire le texte entre `/*` et `*/` mais garde le saut de ligne et l'indentation. Un garde-fou dont le remède est « supprimer une règle redondante » qui se déclenche sur un paragraphe contredit frontalement le message du test, son propre doc-comment (« There is no per-page-view prose tax left to protect anyone from ») et l'en-tête de `style.css` depuis #89 (« Write the comment »). Valeur dérivée, pas choisie : la paire s'inverse à `SHEET_CEILING × déclarations / feuille` = 11 264 × 3 071 / 10 926 = **3 166,0**, et 3 136 est le palier de 64 o en dessous — les 30 o abandonnés paient la bande dont le ratio a besoin (l'écart entre flate2 et le zlib système, remesuré sur la feuille d'aujourd'hui et non repris de #89, vaut 35 o sur la feuille et 13 o sur les déclarations ; les 30 o couvrent les 13 observés, et la borne recalculée au zlib — 3 189,6 — reste au-dessus de 3 136). Reste **107 o** de marge d'inversion contre 334 : relever ce plafond dépense de la marge d'inversion, et c'est le troc déclaré. `SHEET_CEILING` n'était pas touché *à cette date* — il l'a été le lendemain, voir l'entrée du 2026-08-06, ce qui remonte la marge d'inversion à 2 154,7 o. **Deux chiffres de cette entrée ont été corrigés depuis** (entrée du 2026-08-06 sur le tarif de la prose) : un bloc de commentaire coûte 73 à 125 o et non « ~135 puis 5 à 20 », et les déclarations ne plafonnent pas — ce plafond-ci ne sonne pas sur un commentaire parce que celui de la feuille sonne d'abord, pas par immunité. *(Renvoi : « 73 à 125 » est la plage de l'échantillon de dix, pas celle de la feuille — voir l'entrée du 2026-08-29.)* |
| 2026-08-05 | Le `<script>` inline de la messagerie n'est **pas** émis inconditionnellement (#72) | Constat écrit par #83 dans **ce document** — `docs/design-audit.md` ne mentionne le `<script>` nulle part — et faux depuis l'origine : `page()` conditionne le script à `if live`, donc une fenêtre d'historique n'en reçoit aucun. Vérifié empiriquement. Corrigé ici plutôt que propagé une quatrième fois. Le script reste en place : `/messagerie` est à 8 204 o gzip contre 14 336 depuis #89, il n'est plus ce qui met la page dehors, et le sortir vers `/assets` sous son empreinte est la bascule de #89 rejouée sur un second actif — une issue, pas un passager d'une passe de design |
| 2026-08-06 | Plafond de feuille : **11 KiB → 13 KiB** (13 312 o) (#72) | Arbitrage utilisateur, second relèvement du lot et sans rapport avec le premier. L'écart entre 11 264 et la borne physique était de la **pression disciplinaire, pas de la performance** — le doc-comment de #89 le concédait déjà sans en tirer la conséquence (« 14 336 would also have been defensible on the upper bound alone », et cette borne y est elle-même dite « conservative twice over »). La dérivation refaite depuis le côté physique **ne retombe pas sur 14 336**, et **pas parce qu'un terme manquait** à celle de #89 : #89 partait déjà de 14 600 o « less headers and TLS framing » pour arriver à 14 336, et l'écrivait après la bascule vers `/assets`, donc en sachant que la feuille était une réponse à elle seule. Ce qu'elle ne faisait pas, c'est chiffrer ces deux termes : 14 600 − 14 336 leur laissait **264 o** d'allocation implicite. La nouveauté de #72 est cette allocation portée de **264 à 346 o** — IW10 14 600 o − 280 o d'en-têtes (**170 mesurés** sur `apps/web`, plus `server`/`content-encoding`/`vary` à travers Caddy) − 66 o de cadrage TLS 1.3 = **14 254 o** de corps disponible — et les 82 o qui séparent 14 336 de 14 254 sont exactement ce dont elle a grossi. 14 KiB ne tient plus ; 13 KiB est le plus grand palier de KiB qui tienne, et arrondir à la baisse sur un palier de KiB est l'habitude de ce document — ce qui laisse **942 o** entre le plafond et la borne, prix de l'arrondi et non réserve dimensionnée : 13 312 est *sous* sa borne physique, il n'en est pas la valeur. **Ce que ça abandonne**, dit plutôt qu'escamoté : la « pression permanente vers la sobriété » que ce document crédite d'avoir fait advenir #66 et #68 — affirmation jamais confrontée à l'hypothèse concurrente, qu'un audit recensant 173 styles inline dupliqués aurait produit ces lots de toute façon. Ni rien, ni établi. Ce qui est certain, c'est qu'à l'arrivée de #72 cette pression se dépensait en paragraphes plutôt qu'en règles. **Ce que ça garde** : le garde-fou, son remède et son sens — la feuille reste bloquante au premier rendu et doit tenir dans un aller-retour. **Et ça renforce la paire** : la contrainte d'ordre est une borne *basse* sur ce plafond, donc le relever éloigne de l'inversion — marge 107 → **2 154,7 o**. `DECLARATIONS_CEILING` reste à 3 136. Reste 2 386 o de feuille pour #73–#74 |
| 2026-08-06 | Le tarif d'un paragraphe se mesure en supprimant de vrais commentaires, pas en ajoutant du remplissage (#72) | Deux rédactions successives de ce document ont chiffré le coût d'un bloc de commentaire en **appendant le même bloc en boucle**, et ont lu ~135 o pour le premier puis 5 à 20 o par répétition. C'est gzip qui retrouve un texte déjà vu, pas le coût d'écrire. C'est exactement le piège que ce document dénonce déjà pour un chiffre antérieur — mesurer sur des données peu variées fausse le verdict — reproduit deux fois, dont une après avoir reçu la mesure correcte. La méthode qui vaut : **retirer un par un de vrais blocs de `style.css` et peser ce qui revient**, n = 10 → **73 à 125 o, moyenne 100,5**. Corollaire corrigé du même coup : les déclarations **ne plafonnent pas** (3 071 → 3 074 à 8 blocs → 3 075 à 50 sur de la prose variée) ; `DECLARATIONS_CEILING` ne sonne pas sur un commentaire parce que `SHEET_CEILING` sonne vers vingt paragraphes, pas parce qu'il serait immunisé. *(Renvoi : la plage « 73 à 125 » vaut pour l'échantillon de dix blocs que cette entrée relève, et pas pour les blocs de la feuille — voir l'entrée du 2026-08-29.)* |
| 2026-08-06 | #89 n'avait rien omis : ce que #72 relève, c'est une allocation (#72) | Correction du motif écrit à l'entrée du relèvement ci-dessus, qui attribuait à #89 une omission qui n'a pas eu lieu (« ce que l'ancienne soustraction n'avait jamais à compter »). Le texte de #89 — intact dans le même doc-comment de `SHEET_CEILING`, 36 lignes plus haut — part de 14 600 o « less headers and TLS framing » pour arriver à 14 336, et il a été écrit **après** la bascule vers `/assets` : il n'ignorait ni les en-têtes, ni le fait que la feuille soit une réponse à elle seule. Ce qu'il ne faisait pas, c'est chiffrer ces deux termes, à qui 14 600 − 14 336 laissait **264 o** d'allocation implicite. La nouveauté de #72 est cette allocation portée de **264 à 346 o** (280 + 66), soit les 82 o qui séparent 14 336 de 14 254. Le chiffre survivait, le récit non — et il se contredisait dans le même fichier. Aucune valeur ne change : 14 254, 13 312 et 3 136 sont ceux du relèvement |
| 2026-08-06 | 13 312 est un arrondi **sous** sa borne physique, pas cette borne (#72) | Ce document avait écrit « `SHEET_CEILING` n'est plus relevable : 13 312 est dérivé de sa borne physique ». La borne dérivée quarante lignes plus haut vaut **14 254 o** ; 13 312 est le palier de KiB en dessous, et les **942 o** d'écart n'étaient nommés nulle part — ni marge calculée, ni réserve dimensionnée, seulement le prix de l'arrondi. `apps/web/src/app.rs` n'affirmait, lui, que le remède : c'est la source de vérité qui sur-affirmait par rapport au code, au risque d'envoyer un lecteur de #73/#74 vers une issue de découpage alors qu'il reste de la place sous la borne. Troisième fois que ce document écrivait qu'un plafond de feuille ne se relève pas, après deux démentis (10 KiB par #89, 11 KiB par le commit suivant de cette PR même). Ce qui tient sans réserve est le **remède**, pas l'irrelevabilité |
| 2026-08-29 | Le tarif d'un paragraphe sort de la prose : « 73 à 125 o » était la plage d'un échantillon (#72) | Remesuré ici par la méthode que cette même entrée prescrit — retirer un vrai bloc de commentaire avec ses lignes entières, repeser — mais sur **les 44 blocs de 3 à 8 lignes que `style.css` contient réellement** et non sur dix : **68 à 238 o, moyenne 109,0**, flate2 niveau 6 (`Compression::default()`, l'encodeur de `gzipped`). « 73 à 125 » recouvre exactement les **36 blocs les moins chers sur 44** ; les 8 autres coûtent 135, 143, 146, 151, 164, 203, 218 et 238 o. La moyenne annoncée restait plausible pour un échantillon de dix, la plage non — et elle se trompe dans le sens **optimiste** : un lecteur de #73/#74 qui budgète « au pire 125 o le paragraphe » sous-estime son pire cas d'un facteur 2. **La correction n'est pas une meilleure plage, c'est le retrait de la valeur** (arbitrage du 2026-08-28, #95) : la section budget dit maintenant la méthode et **l'encodeur**, pas le résultat. Aucune valeur du garde-fou ne bouge — `style.css` n'est pas touchée, la feuille pèse toujours 10 926 o et les déclarations 3 071 |
