# Outillage IA pour le front — état du marché et ce qui colle à `apps/web`

Veille au 2026-08-28. Ce document est un **constat + une recommandation
d'outillage**, pas un plan d'implémentation et pas une décision visuelle : la
cible visuelle reste [`DESIGN.md`](../DESIGN.md), l'état des lieux qui l'a
motivée est dans [`design-audit.md`](design-audit.md).

Question de départ : « quelles technos IA existent pour améliorer le front, et
lesquelles sont adaptées à la stack de ce repo ». La deuxième moitié de la
question est celle qui tranche — la stack élimine la majorité du marché avant
même qu'on ait à comparer les outils entre eux.

---

## 1. Les contraintes qui filtrent

Ce ne sont pas des préférences, ce sont les règles du jeu déjà écrites
ailleurs et vérifiées par des tests :

- **Rendu serveur pur.** `apps/web` rend du HTML via
  `leptos::ssr::render_to_string` dans des handlers axum. Pas d'hydratation,
  pas de bundle WASM, pas de `fetch()`. Les formulaires sont des
  `<form method=post>` en PRG. L'app fonctionne JS désactivé.
- **Une seule feuille, écrite à la main.** `apps/web/src/style.css` (544
  lignes), scellée dans le binaire par `include_str!`, servie sous
  `/assets/style-<sha256>.css` depuis #89.
- **Un budget en octets compressés** (#83), testé en CI. C'est lui qui rend le
  gaspillage visible.
- **Aucune dépendance CSS externe, aucun pipeline d'assets**, polices
  auto-hébergées pour raison RGPD (un CDN exposerait l'IP des visiteurs).
- **`DESIGN.md` est normatif** : « aucune texture, aucun dégradé, aucune ombre
  décorative », une seule couleur saturée dans toute l'interface.

---

## 2. Ce qui ne colle pas — et pourquoi c'est écrit ici quand même

**v0** (Vercel), **Lovable**, **Figma Make**, **Superdesign**, **Locofy**,
**Anima** produisent tous du React + Tailwind + shadcn/ui, ou de la soupe de
`<div>` issue d'un fichier Figma. Aucune de ces sorties ne peut atterrir dans
un `view!`. Ce n'est pas un défaut des outils : c'est qu'ils sont construits
pour l'écosystème inverse de celui-ci.

Le piège est plus subtil que l'incompatibilité de langage, et c'est pour ça
que la liste est conservée : **leur esthétique par défaut — cartes flottantes,
ombres portées, dégradés, sans géométrique arrondi — est mot pour mot la liste
de ce que `DESIGN.md` refuse** (§ Direction esthétique, « Ce qu'on refuse »).
Un rendu de v0 est donc doublement inutilisable : le code ne rentre pas, et
l'image tire dans la direction opposée à celle qui a été choisie.

Ils restent utilisables comme **source d'images**, jamais comme source de
code, et seulement avec ce filtre en tête.

**Corollaire, à refuser explicitement :** adopter Tailwind ou shadcn « pour
débloquer les outils IA ». Ça retourne le bénéfice n°4 de la section
« Livraison du CSS » de `DESIGN.md` — le gaspillage visible qui « interdit de
fait un framework utilitaire » — et fait sauter le budget compressé. Le jour
où quelqu'un le proposera, c'est ce paragraphe qu'il faudra réfuter.

---

## 3. Ce qui colle — trois couches

### Couche 1 — donner des yeux à l'agent

**C'est le levier n°1, et il est en place depuis la réparation du MCP.**

`.mcp.json` déclare **Playwright MCP**, et depuis la réparation décrite dans
[`infra/README.md`](../infra/README.md) il démarre. Il ne démarrait pas
jusque-là : la commande était `npx -y @playwright/mcp@latest` et `npx` est
introuvable dans le `PATH`, ce qui est cohérent avec le fait que Node ne vit
ici que dans les conteneurs (cf. la recette e2e via Docker). Le serveur était
configuré mais mort — configuré et mort étant, du point de vue de l'agent, la
même chose que pas configuré du tout, à la ligne d'erreur près.

Le correctif suit la règle de la maison plutôt que de la contourner : au lieu
d'installer Node sur l'hôte, `infra/mcp-playwright.sh` lance l'image officielle
`mcr.microsoft.com/playwright/mcp`, qui embarque Node, Chromium et ses
dépendances système. Installer Node aurait de toute façon réglé `npx` sans
régler la suite — `playwright install --with-deps chromium` réclame apt et
root.

**Chrome DevTools MCP** est le complément, avec une division du travail nette :
Playwright pilote (parcours, formulaires, multi-navigateurs, snapshots d'arbre
d'accessibilité), Chrome DevTools MCP audite (réseau, cycle de vie de la page,
mesures type Lighthouse, captures à la demande plutôt que systématiques —
moins coûteux en contexte). Le second serait l'outil pour chiffrer ce qu'a
réellement coûté l'aller-retour render-blocking assumé en #89.

Ce que ça change concrètement : l'agent ouvre `/agenda` en 375 px, en thème
sombre, regarde le rendu, et **voit** que c'est cassé. Aujourd'hui il raisonne
à l'aveugle sur du CSS. Le précédent est documenté : c'est le motif
`var(--x, #hex)` qui « a cassé le thème sombre de l'agenda sans que personne
le voie » — exactement la classe de bug qu'un œil, humain ou non, attrape en
une seconde et qu'une lecture de diff n'attrape pas.

### Couche 2 — idéation, en amont du code

Sortie attendue : **une référence visuelle**, jamais du code à coller. Le
portage vers `view!` + `style.css` reste manuel, et `DESIGN.md` gagne
systématiquement en cas de désaccord.

- **Google Stitch** (Gemini 3, gratuit) — sort des maquettes et du HTML/CSS
  générique, *pas* du React. C'est le format de sortie le plus proche de ce
  repo. Reste un aller-retour navigateur, hors IDE.
- **Le skill `design` de Claude Code** (déjà installé) — canvas multi-artboards
  éditable, sans quitter le terminal.
- **gstack** (déjà installé) — `/gstack-design-shotgun` (N variantes + board de
  comparaison), `/gstack-design-consultation`, `/gstack-design-review` (revue
  « œil de designer » sur le rendu réel, qui se combine avec la couche 1).

### Couche 3 — garde-fous, pour que l'itération IA ne dérive pas

La suite e2e existe déjà (13 specs Playwright, dont `interaction.spec.ts` et
`fonts.spec.ts`). Deux ajouts la transforment en filet pour du travail visuel
piloté par une IA :

- **Régression visuelle** — `toHaveScreenshot()` par route × (clair/sombre) ×
  (375 px / 1280 px). Sans ça, une itération IA sur le CSS est non vérifiable.
- **Accessibilité** — `@axe-core/playwright` : contraste, rôles, focus
  visible. Mesurable, donc pilotable par un agent.

Le **budget CSS compressé** (#83) est déjà, de fait, le meilleur garde-fou
anti-verbosité du repo : il rend immédiatement rouge tout ce qu'un LLM ajoute
« au cas où ».

---

## 4. Ordre proposé

1. ~~Réparer Playwright MCP.~~ Fait — `infra/mcp-playwright.sh`.
2. Ajouter les snapshots visuels sur les routes principales.
3. Passer `/gstack-design-review` sur une ou deux routes, boucle navigateur
   active.
4. Stitch / canvas `design` **seulement** s'il faut réinventer la direction
   esthétique — or `DESIGN.md` l'a déjà tranchée. À ce stade ce serait rouvrir
   une décision fermée, pas améliorer le front.

---

## 5. La question laissée ouverte

« Améliorer l'aspect front » recouvre deux chantiers très différents, et le
choix n'est pas fait :

- **Mieux styler l'existant** → les couches 1 et 3 suffisent, l'architecture ne
  bouge pas.
- **Ajouter de l'interactivité** → impliquerait le mode **islands** de Leptos
  (hydratation partielle, WASM sous ~50 Ko pour des pages majoritairement
  statiques, disponible depuis 0.8 avec le routage client). Ça casse le
  « fonctionne sans JS » revendiqué par toutes les specs front F1–F11, et
  impose un toolchain `wasm32`/trunk que l'environnement de dev n'a pas —
  c'est précisément le compromis acté à la création de `apps/web`. À traiter
  comme une décision d'architecture, pas comme une amélioration de front.

---

## Sources

- [Superdesign — AI design stack 2026](https://superdesign.dev/blog/2026-ai-design-stack)
- [Superdesign — alternatives à Google Stitch](https://superdesign.dev/blog/google-stitch-alternative)
- [Banani — alternatives à Figma Make (v0, Lovable, Dyad)](https://www.banani.co/blog/best-figma-make-ai-alternatives)
- [MCP.Directory — Chrome DevTools MCP vs Playwright MCP (2026)](https://mcp.directory/blog/chrome-devtools-mcp-vs-playwright-mcp-2026)
- [Browser verification for coding agents](https://www.huuhka.net/browser-verification-for-coding-agents-chrome-devtools-mcp-vs-agent-browser/)
- [Puck — outils de génération d'UI sortant du HTML/CSS](https://puckeditor.com/blog/top-5-ai-tools-for-ui-generation)
- [Leptos — guide Islands](https://book.leptos.dev/islands.html)
