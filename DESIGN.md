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
2. **Le CSS est inliné dans chaque réponse** (`shell()` dans `apps/web/src/app.rs`)
   — **et c'est le seul point de cette liste qui soit un arbitrage plutôt
   qu'une règle : il tient tant que le budget tient.** Ce qu'on y gagne :
   aucun aller-retour bloquant au premier rendu, et surtout l'impossibilité
   structurelle qu'un HTML neuf soit servi avec un CSS périmé — `include_str!`
   scelle la feuille dans le binaire. Ce qu'on y perd : la feuille est
   incachable par construction, donc refacturée à chaque page vue,
   commentaires compris. Conséquence pratique inchangée — chaque règle se paie
   sur chaque page : pas de framework utilitaire, pas de redondance. Le seuil
   chiffré, son garde-fou et la porte de sortie sont dans
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

La feuille voyage à l'intérieur de chaque document. C'est un pari, pas une
propriété du monde : on paie une copie par page vue pour épargner un
aller-retour bloquant au premier rendu. Le pari n'est gagnant que tant que le
document **et** la feuille tiennent ensemble dans la première fenêtre de
congestion. Il a donc une taille au-delà de laquelle il devient faux — et
c'est cette taille qui manquait ici.

### Ce que l'inlining apporte

1. **Zéro aller-retour bloquant au premier rendu.** Une feuille externe est
   render-blocking : le navigateur parse le HTML, découvre le `<link>`, ouvre
   une requête, attend.
2. **Impossibilité structurelle du décalage CSS/markup.** `include_str!` scelle
   la feuille dans le binaire : il n'existe aucun état où du HTML neuf est
   servi avec du CSS périmé — pas de nom haché, pas d'invalidation, pas de
   fenêtre de déploiement où les deux divergent. C'est une propriété de
   **correction**, pas de performance, et c'est le meilleur argument du lot.
3. **Aucun pipeline d'assets**, ce qui est la contrainte n°3 vue de l'autre
   côté : rien à installer, rien à builder.
4. **Une pression permanente vers la sobriété.** Le gaspillage est visible, ce
   qui interdit de fait un framework utilitaire. C'est ce bénéfice qui a rendu
   #66 et #68 nécessaires.

### Ce qu'il coûte

1. **Incachable par construction.** Le HTML d'une application de données dépend
   de la session et des données, donc n'est pas cacheable ; ce qu'on inline
   dedans hérite de cette non-cacheabilité. Or c'est une application de foyer,
   consultée plusieurs fois par jour, avec beaucoup de navigations par
   session : le profil de trafic où le cache rapporterait le plus est
   précisément celui où on y renonce.
2. **Le coût suit le nombre de pages vues, pas la taille de la feuille.**
   Chaque règle ajoutée est multipliée par le volume de navigation.
3. **Une taxe sur la documentation.** Les commentaires sont 58 % de la feuille
   brute — et **encore 68 % de la feuille compressée** : gzip ne les rend pas
   gratuits. L'inlining les facture à l'utilisateur à chaque page vue, ce qui
   crée une incitation perverse à moins commenter. Le projet a choisi
   l'inverse, et il a bien choisi ; c'est la manière de livrer qui doit céder,
   pas la prose. Voir le garde-fou ci-dessous, qui est construit pour rendre
   cet arbitrage impossible à trancher en douce.
4. **Conflit avec une CSP stricte.** Il n'y en a pas aujourd'hui. Le jour où on
   en veut une, un `<style>` inline impose `unsafe-inline`, ou un nonce/hash à
   générer par réponse. Une feuille externe est le cas trivial.

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
octets **inconditionnellement**, et la page rend jusqu'à 50 messages (100 avec
`?limit=`) de 4 000 caractères chacun. Le document seul y pèse 8 416 o
compressés, contre 750 à 2 754 sur les sept autres. Mesurée à vide elle est à
11 188 o : ce n'est donc pas un cas extrême construit pour l'occasion, c'est
une conversation de famille ordinaire qui l'y amène.

Ce que ça veut dire, écrit sans le contourner : **le seuil de 14 KiB n'est pas
tenable route par route en bornant la feuille**, parce que la moitié document
est fonction des données et non du CSS. Sur `/messagerie` le déclencheur de la
porte de sortie a donc **déjà été franchi** — sortir la feuille de l'inlining
y ramènerait la page autour de 8,4 Ko. Ce n'est pas corrigé ici (#72 tient
cette page, et l'issue #83 exclut explicitement la bascule) : c'est constaté,
daté, et c'est le premier argument que reprendra la PR qui fera la bascule.

Deux routes hors nav, mesurées au passage : `/account` sort à 8 501 o et
`/privacy-policy` à 10 432 o. Toutes deux dans le budget, mais la seconde est
publique et de taille fixe — c'est du texte réglementaire, il ne fera que
s'allonger. À surveiller au même titre que les huit.

Trois seuils, du plus englobant au plus fin :

| Seuil | Valeur | Aujourd'hui | Ce qu'on fait au dépassement |
|---|---|---|---|
| Réponse complète compressée, routes principales | **≤ 14 KiB** (14 336 o) | 11 936 o au pire sur 7 routes, **16 761 o sur `/messagerie`** | passer la feuille sur `/assets` |
| Feuille compressée | **≤ 10 KiB** (10 240 o) | 8 633 o | idem — **jamais** dégraisser les commentaires |
| Déclarations compressées | **≤ 3 KiB** (3 072 o) | 2 670 o | supprimer une règle redondante |

La colonne « aujourd'hui » est **remesurée le 2026-07-31, après #69**, avec le
corpus reproductible de `npm run seed` (#85) et non le jeu de données ad hoc
de la campagne #83 — les deux ne sont pas comparables entre eux. Ce que #69 a
coûté, mesuré des deux côtés sur ce même corpus : la feuille passe de 7 192 à
8 633 o compressés, les déclarations de 2 299 à 2 670 o, et chaque route gagne
environ 1 440 o (`/agenda`, la plus lourde des sept, de 10 495 à 11 936 o).
Aucun des trois plafonds n'est franchi par ce changement ; `/messagerie` était
déjà au-dessus du premier avant lui.

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
plafonds impose un plancher : les déclarations font 30,9 % de la feuille
compressée (2 670 sur 8 633 après #69), donc le plafond des déclarations n'est
atteint le premier que si celui de la feuille reste **au-dessus de ~9 942 o**.
Descendre sous ce
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
c'est le constat mineur laissé par #83, corrigé ici. Après #69 :
**13 % de marge sur les déclarations, 16 % sur la feuille** — le plafond des
déclarations reste bien le plus mordant des deux.

Les deux derniers seuils sont tenus par des tests dans `apps/web/src/app.rs`
(`the_compressed_stylesheet_fits_its_share_of_the_first_round_trip` et
`the_compressed_declarations_stay_inside_the_design_system_budget`). Le
premier ne l'est pas, et ne peut pas l'être : il dépend du volume de données
d'un foyer. Il se vérifie en mesurant une stack qui tourne **avec des données
dedans** — une stack vide sous-estime la moitié document d'un facteur deux à
trois, et c'est comme ça que `/messagerie` a failli n'être jamais mesurée.

Le plafond des déclarations peut être relevé, avec un motif écrit dans la PR
— c'est la même convention que le plafond de styles inline. **Le plafond de la
feuille, lui, ne se relève pas :** le dépasser signifie que le pari est perdu,
et la réponse est la porte de sortie ci-dessous, pas un nombre plus grand.

### Compression

`infra/Caddyfile` fait `encode zstd gzip` sur le bloc `:80`, ce qui couvre le
HTML — donc le CSS inliné — et le JSON de `apps/api`. C'est ce qui rend la
prémisse de l'inlining à nouveau vraie : sans compression, aucune des huit
routes principales ne tenait dans le premier aller-retour ; avec, sept y
tiennent, la plus lourde des sept (`/agenda`, 11 936 o après #69) gardant 17 %
de marge. La huitième, `/messagerie`, n'y tient pas — voir plus haut.

`apps/web` ne compresse **pas** de son côté. Caddy laisse intacte une réponse
qui porte déjà un `Content-Encoding` (vérifié), donc un `CompressionLayer`
dans `apps/web` remplacerait le choix de Caddy par le sien sur le chemin
déployé, pour le seul bénéfice des accès directs à `web:3000` (dev, tests e2e)
où personne ne compte les octets. Le budget est tenu par un test unitaire, pas
par le transport.

### Porte de sortie : servir la feuille depuis `/assets`

Le pipeline existe déjà — #67 sert les polices depuis `apps/web` avec
`Cache-Control: immutable` — donc la bascule est courte : un fichier de plus
dans `assets/`, un `<link>` dans `shell()` à la place du `<style>`.

**Ce qu'elle ferait perdre**, et que le futur implémenteur oubliera si
personne ne l'écrit : l'avantage n°2 ci-dessus. Une feuille externe rouvre la
fenêtre où du HTML neuf est servi avec du CSS périmé — cache navigateur,
déploiement progressif. Il faut donc la servir sous un **nom haché par son
contenu**, et le hachage doit venir du binaire lui-même (le contenu est déjà
là via `include_str!`), pas d'une étape de build : la contrainte n°3 reste.

**Déclencheurs**, l'un ou l'autre suffit :

- dépassement d'un des deux plafonds compressés ci-dessus ;
- exposition publique de l'application — le commentaire en tête de
  `infra/Caddyfile` la repousse après la v1. Une feuille cachable a beaucoup
  plus de valeur dès que les visiteurs ne sont plus quelques personnes sur un
  LAN, et une CSP stricte devient à ce moment-là un sujet.

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
| 2026-07-30 | CSS inliné — décision datée, plus une propriété héritée (#83) | L'inlining n'avait jamais été argumenté : la contrainte n°2 décrivait ce que `shell()` fait. Il est conservé pour l'impossibilité structurelle du décalage CSS/markup (`include_str!`), pas pour la performance, et requalifié en arbitrage valable **sous condition de budget** |
| 2026-07-30 | Budget de livraison : 14 KiB compressés par réponse, 10 KiB pour la feuille, 3 KiB pour les déclarations (#83) | 14 KiB est la fenêtre de congestion initiale, seul chiffre non arbitraire ; les deux autres s'en déduisent. Deux plafonds plutôt qu'un parce que « taille de la feuille » a deux réponses et deux remèdes : le brut compressé se corrige en sortant de l'inlining, les déclarations en supprimant une règle. Le second est calibré pour être atteint le premier, donc la pression ne tombe jamais sur les commentaires |
| 2026-07-30 | `encode zstd gzip` dans `infra/Caddyfile` (#83) | Le seul écart réellement hors-norme n'était pas l'inlining mais l'absence totale de compression : 20 à 81 Ko de texte brut par navigation, aucune route ne tenant dans le premier aller-retour. Après, 7,9 à 10,1 Ko sur sept des huit routes de la nav, qui y tiennent |
| 2026-07-30 | `/messagerie` constatée hors du budget de 14 KiB (#83) | 15 739 o compressés avec 50 messages ordinaires, 11 188 o à vide : un `<script>` inline de 7 274 o émis inconditionnellement plus 50 messages rendus. Le déclencheur de la porte de sortie est donc déjà franchi sur cette route. Non corrigé par #83 (bascule hors périmètre, page tenue par #72) — constaté et daté |
| 2026-07-30 | Pas de `CompressionLayer` dans `apps/web` (#83) | Caddy laisse intacte une réponse déjà encodée (mesuré) : compresser dans `apps/web` imposerait son gzip au chemin déployé à la place du choix de Caddy, au bénéfice des seuls accès directs à `web:3000` (dev, e2e) où aucun octet n'est compté — et ajouterait une dépendance à un graphe qui porte déjà deux tower-http |
| 2026-07-30 | BREACH : aucune route exclue de `encode` (#83) | Le seul secret rendu dans un corps compressible est le jeton de réinitialisation (`reset_password.rs:42`). L'attaque demande une seconde chaîne choisie par l'attaquant dans la même réponse ; cette page n'en a aucune (sa seule variable est le jeton, qui doit parser en UUID). Usage unique et péremption 24 h vérifiés dans `apps/api/src/auth/mod.rs` |
| 2026-07-31 | Jetons `--accent-hover` / `--error-hover` (#69) | Un aplat plein survolé ne peut pas se contenter d'une teinte : `--accent-fg` y passerait sous AA. La moitié creusée (clair) ou éclaircie (sombre) de chaque aplat tient 7,8:1 au pire dans les deux thèmes |
| 2026-07-31 | Le lien de nav courant se décide sur le premier segment de chemin (#69) | Un test par préfixe allume deux onglets à la fois : `/` préfixe toute l'application. Le segment garde aussi une section allumée sur ses sous-pages (`/agenda/new`) et le lien Admin — qui pointe vers l'un des deux écrans — allumé sur l'autre |
| 2026-07-31 | Les champs neutralisent l'anneau du navigateur avec `outline: 2px solid transparent`, jamais `none` (#69) | En mode contrastes forcés, `box-shadow` est ignoré et les contours sont repeints par le système : `outline: none` y laisserait un champ focalisé sans aucun indicateur |
| 2026-07-31 | `prefers-reduced-motion` neutralise en remettant `--dur` à 0s (#69) | Une déclaration au lieu d'un `* { transition: none !important }` : toutes les transitions étant minutées par le jeton, il n'y a aucune liste à tenir à jour et aucune règle à sur-spécifier |
| 2026-07-31 | Marges du budget : une seule formule, `(plafond − actuel) / plafond` (#69) | Deux marges étaient comparées dans la même phrase avec deux formules différentes (25 % rapporté au plafond contre 42 % rapporté à la valeur courante) — constat mineur laissé par #83, corrigé au passage |
