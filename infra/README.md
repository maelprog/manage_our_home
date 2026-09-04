# infra

- `docker-compose.yml` — la stack complète (Postgres, MinIO, API, web, Caddy).
- `generate-env.sh` — génère le `.env` attendu par la compose.
- `docker-prune.sh` — purge périodique des ressources Docker.
- `mcp-playwright.sh` — serveur MCP Playwright, en conteneur (`.mcp.json`).

## Purge Docker (`docker-prune.sh`)

Sur WSL le disque virtuel de la distro ne fait que grossir, et Docker en est
l'essentiel du contenu. Le poids n'est pas dans les images — ce sont des bases
nommées, rarement dangling — mais dans les **volumes de cache de build** : un
seul `*-target` Rust orphelin atteignait 16 Go.

```sh
./infra/docker-prune.sh --install-hook  # purge après chaque merge  <- en place
./infra/docker-prune.sh --dry-run       # ce qui serait supprimé, sans rien toucher
./infra/docker-prune.sh                 # purge immédiate
./infra/docker-prune.sh --install       # variante : purge quotidienne (timer)
./infra/docker-prune.sh --uninstall     # retire hook et timer
```

### Déclenchement : le hook `post-merge`

La purge tourne après chaque merge entrant, c'est-à-dire au `git pull` qui
rapatrie une PR fusionnée — précisément le moment où les caches de build de
cette branche deviennent des déchets. Les deux chemins passent par la même unité
systemd utilisateur, lancée avec `--no-block` : `git pull` rend la main
immédiatement (mesuré à 0,05 s) et la purge se déroule derrière, journalisée.

```sh
journalctl --user -u docker-prune.service -n 50
```

Le hook est installé dans `.git/hooks/`, **pas** dans `.githooks/`. Activer
`core.hooksPath=.githooks` allumerait aussi `pre-commit`, qui lance
`cargo fmt/clippy/build` alors qu'il n'y a pas de toolchain Rust sur l'hôte
(tout passe par Docker) : chaque commit échouerait. L'installation vise
`--git-common-dir`, donc les worktrees du dépôt sont couverts eux aussi.

Un `post-merge` déjà présent et non écrit par le script n'est jamais écrasé :
`--install-hook` refuse et sort en erreur, `--uninstall` le laisse en place.
La reconnaissance se fait sur une signature en tête du fichier généré.

Deux limites à connaître :

- `git pull --rebase` ne déclenche pas `post-merge` (c'est `post-rewrite`). La
  config actuelle du dépôt fait bien un merge, mais un `pull.rebase=true` posé
  plus tard désactiverait silencieusement la purge.
- Une PR fusionnée sur GitHub sans `git pull` derrière ne déclenche rien. Et
  fusionner ne libère pas à soi seul le cache d'une stack de worktree : tant que
  ses conteneurs tournent, son volume `*-target` reste attaché et protégé. Il
  faut arrêter la stack pour que la purge suivante le reprenne.

La variante `--install` (timer quotidien, `Persistent=true` pour rattraper les
jours WSL éteinte) reste disponible si un filet de sécurité temporel devient
souhaitable ; les deux peuvent cohabiter.

### Ce qui est supprimé

Tout ce qui n'est rattaché à aucun conteneur : conteneurs arrêtés, images
inutilisées, volumes non référencés, build cache, réseaux orphelins.

### Ce qui est protégé

Les volumes de **données** — `postgres_data`, `minio_data`, `caddy_data`,
`ollama_data`. Docker considère un volume nommé comme « non référencé » dès que
la stack est simplement à l'arrêt : sans cette liste blanche, un
`docker volume prune -a` de routine effacerait la base de données entre deux
`docker compose down`. Les deux listes sont ajustables :

```sh
PRUNE_KEEP_IMAGES='^(mcr\.microsoft\.com/playwright|rust:)' ./infra/docker-prune.sh
PRUNE_KEEP_VOLUMES='(postgres_data|minio_data|mon_volume)$' ./infra/docker-prune.sh
```

`PRUNE_KEEP_IMAGES` vaut `^mcr\.microsoft\.com/playwright` par défaut, ce qui
couvre les **deux** images Playwright du dépôt : celle du serveur MCP (1 Go) et
celle du runner e2e, `:*-noble` (2,4 Go). Aucun conteneur ne les retient entre
deux sessions ; sans cette exception, chaque purge les reprendrait et il
faudrait les re-télécharger à la session ou à la suite e2e suivante. Y donner
une regex plus étroite les réexpose.

Le script épingle `DOCKER_CONTEXT=default` — le contexte `desktop-linux` de
Docker Desktop coexiste avec le moteur installé dans la WSL, et un
`DOCKER_CONTEXT` hérité de l'environnement ferait purger le mauvais démon.

### Rendre l'espace à Windows

La purge empêche le `ext4.vhdx` de grossir, mais ne le fait pas maigrir. `/`
est monté avec `discard`, donc le TRIM part bien vers le disque virtuel, mais
WSL ne rend les blocs à Windows que si le mode **sparse** est activé. À faire
une fois, depuis PowerShell, distro arrêtée :

```powershell
wsl --shutdown
wsl --manage Ubuntu --set-sparse true
```

Ensuite le vhdx suit l'usage réel après chaque purge.

## Playwright MCP (`mcp-playwright.sh`)

Le serveur MCP qui donne des yeux à l'agent : il ouvre une vraie page dans un
vrai Chromium, en rend l'arbre d'accessibilité et les captures, et permet donc
de **voir** un rendu cassé au lieu de raisonner à l'aveugle sur du CSS. Le
pourquoi et la place de cet outil dans l'ensemble sont dans
[`docs/outillage-ia-front.md`](../docs/outillage-ia-front.md) (§ Couche 1).

`.mcp.json` le lance via ce script, qui lance lui-même l'image officielle :

```sh
./infra/mcp-playwright.sh --version   # test à froid (tire l'image si absente)
```

Le chemin est écrit `${CLAUDE_PROJECT_DIR:-.}/infra/mcp-playwright.sh`, et le
repli `:-.` n'est pas décoratif. Claude Code pose bien `CLAUDE_PROJECT_DIR`
dans l'environnement du serveur qu'il lance, mais pas dans le sien : au moment
où il développe `${...}` du champ `command`, la variable n'existe pas encore.
Sans repli, c'est la chaîne littérale qui part à l'exécution et le serveur
meurt sur

```
playwright (ENOENT): posix_spawn '${CLAUDE_PROJECT_DIR}/infra/mcp-playwright.sh'
```

Le `.` retombe sur la racine du projet, d'où les chemins relatifs d'un
`.mcp.json` de dépôt sont résolus.

Il déclarait auparavant `npx -y @playwright/mcp@latest` et mourait au
démarrage — il n'y a pas de Node sur l'hôte, tout passe par des conteneurs.
Installer Node n'aurait réglé que la moitié du problème : `playwright install
--with-deps chromium` réclame apt et root. L'image officielle embarque Node,
Chromium et ses dépendances.

Quatre points non évidents, tous justifiés en commentaire dans le script :

- **`--network host`.** La compose ne publie que le `:80` de Caddy, et
  `PUBLIC_BASE_URL` vaut `http://localhost` : les redirections et les liens
  absolus de l'app pointent vers `localhost`. Sur le réseau `infra_default`,
  ce `localhost` désignerait le conteneur du navigateur. L'app est donc à
  `http://localhost` pour le MCP comme pour un navigateur de l'hôte.
- **Le montage en miroir.** Le serveur écrit snapshots, captures et journaux
  de console dans `--output-dir` et n'en renvoie que le chemin, *relatif à son
  répertoire courant*. Monté ailleurs, il rend des `../../output/page-….yml`
  que l'agent cherche depuis la racine du dépôt et ne trouve pas. Même chemin
  absolu des deux côtés + `-w` sur la racine ⇒ le chemin rendu est
  `.playwright-mcp/page-….yml`, ouvrable tel quel. Ce dossier est gitignoré.
- **La version est épinglée** dans le script, pas `:latest` : sans ça elle est
  figée au premier `docker pull` sous une étiquette qui ment. Pour la monter,
  lire `curl -s https://mcr.microsoft.com/v2/playwright/mcp/tags/list`, changer
  la ligne, relancer Claude Code.
- **L'image est protégée de la purge** (`KEEP_IMAGES` dans
  `docker-prune.sh`) : aucun conteneur ne la référence entre deux sessions, et
  sans exception la purge reprendrait 1 Go à re-télécharger à chaque fois. La
  regex couvre tout le préfixe `mcr.microsoft.com/playwright`, donc aussi
  l'image `:*-noble` du runner e2e (2,4 Go), soumise au même oubli.

Après toute modification de `.mcp.json` ou du script, **relancer Claude Code** :
les serveurs MCP ne sont démarrés qu'à l'ouverture de la session.

## Interroger la base

Il n'y a plus de serveur MCP postgres. Celui qui était déclaré était mort trois
fois — `npx` absent, Postgres non publié sur un port de l'hôte, mot de passe
`mhome` codé en dur alors qu'il vient de `.env` — et son paquet
(`@modelcontextprotocol/server-postgres`) est archivé depuis mai 2025, non
maintenu, avec une injection SQL connue. Le remplacer aurait voulu dire confier
les identifiants de la base à une image tierce non auditée pour un service que
`psql` rend déjà :

```sh
docker exec -i infra-postgres-1 psql -U mhome -d manage_our_home -c '\dt'
```

C'est le même chemin d'accès que les tests e2e (`e2e/lib/db.ts`) et que les
tests d'intégration Rust.
