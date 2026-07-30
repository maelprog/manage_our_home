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
2. **Le CSS est inliné dans chaque réponse** (`shell()` dans `apps/web/src/app.rs`).
   Chaque règle se paie sur chaque page : pas de framework utilitaire, pas de
   redondance.
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

Le pétrole remplace le `#2952cc` actuel : il évite le bleu SaaS générique et,
surtout, il ne rentre pas en collision avec le rouge d'erreur ni avec le vert
de succès.

### Sémantiques

| Jeton | Clair | Sombre |
|---|---|---|
| `--success` / `--success-soft` | `#3F7A34` / `#E6F0E2` | `#7CB86C` / `#22301E` |
| `--warning` / `--warning-soft` | `#9A6B10` / `#F7EEDA` | `#D9A441` / `#332813` |
| `--error` / `--error-soft` | `#A8342A` / `#F7E5E2` | `#E88075` / `#3A211E` |

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

### Plancher typographique sur téléphone

`--t-xs` (0.8125rem / 13 px) est trop petit pour un écran consulté à bout de
bras dans une cuisine. **Sous 861 px**, remonter le plancher :
`--t-xs` → 0.875rem et `--t-sm` → 0.9375rem. Le corps reste à 1rem, les titres
d'événement passent à 1.0625rem.

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
| `.cal`, `.cal-cell`, `.cal-week`, `.cal-col`, `.cal-day` | la grille du mois et la bande de semaine de l'agenda — posées en CSS pour que #71 les reprenne là et non dans le Rust |
| `.composer`, `.live-status` | la zone de saisie de la messagerie et sa ligne d'état ; #72 les reprend |

Le résiduel de `style="…"` assumé après #68 : une couleur de membre calculée
(`.avatar`), la largeur d'un champ de prix, l'alignement d'une colonne
numérique, la largeur minimale du champ d'édition d'un message.

---

## Interaction et motion

- **Approche :** minimal-fonctionnel. Aucune animation d'entrée : l'application
  fait un rechargement complet à chaque navigation, une animation au chargement
  deviendrait pénible dès la troisième page.
- **Durée :** 150 ms (`--dur`), courbe `cubic-bezier(0.2, 0, 0.2, 1)`.
- **Transitions autorisées :** `background`, `border-color`, `box-shadow`
  au survol et au focus. Rien d'autre.
- **`:hover`** sur tout élément interactif — boutons, liens, lignes de liste,
  pastilles. C'est le premier facteur du ressenti « rudimentaire » sur PC, et
  il est totalement absent aujourd'hui.
- **`:focus-visible`** : `outline: 2px solid var(--accent); outline-offset: 2px`
  sur les éléments cliquables, `box-shadow: 0 0 0 3px var(--accent-soft)` sur
  les champs. Jamais `outline: none` sans remplacement.
- **`aria-current="page"`** sur le lien de navigation de la page courante,
  avec un traitement visuel distinct.
- **`prefers-reduced-motion: reduce`** neutralise toutes les transitions.

---

## Journal des décisions

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
