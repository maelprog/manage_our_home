#!/usr/bin/env bash
# Serveur MCP Playwright — lancé en conteneur, comme tout le reste ici.
#
# Pourquoi pas `npx -y @playwright/mcp@latest`, la configuration d'origine :
# il n'y a pas de Node sur l'hôte (cf. e2e/README.md, apps/*/Dockerfile — tout
# passe par des conteneurs), donc le serveur mourait au démarrage sur
# « Executable not found in $PATH: npx » et Claude Code démarrait sans yeux.
# Installer Node réglerait `npx` mais pas la suite : `playwright install
# --with-deps chromium` réclame apt et root sur l'hôte. L'image officielle
# embarque déjà Node, Chromium et ses dépendances système.
#
# Le protocole MCP parle JSON-RPC sur stdin/stdout : rien ne doit être écrit
# sur stdout en dehors du serveur. Les diagnostics vont sur stderr.
set -euo pipefail

# Docker Desktop installe un contexte `desktop-linux` qui coexiste avec le
# moteur de la WSL ; sans ce pin un DOCKER_CONTEXT hérité viserait le mauvais
# démon (même raison que dans docker-prune.sh).
export DOCKER_CONTEXT="${DOCKER_CONTEXT:-default}"

# Épinglé plutôt que `:latest` : la version doit être visible dans git, sinon
# elle est figée au premier `docker pull` sous une étiquette qui ment. Pour la
# monter : lire les tags publiés, changer la ligne, relancer Claude Code.
#   curl -s https://mcr.microsoft.com/v2/playwright/mcp/tags/list
IMAGE="${PLAYWRIGHT_MCP_IMAGE:-mcr.microsoft.com/playwright/mcp:v0.0.80}"

REPO_ROOT="$(cd "$(dirname "$(readlink -f "${BASH_SOURCE[0]}")")/.." && pwd)"
OUTPUT_DIR="${PLAYWRIGHT_MCP_OUTPUT_DIR:-$REPO_ROOT/.playwright-mcp}"
mkdir -p "$OUTPUT_DIR"

# `docker run` tire l'image manquante tout seul, mais 1 Go de téléchargement
# pendant la poignée de main MCP dépasse le délai de démarrage : on tire
# d'abord, en clair, sur stderr. Ne se produit qu'une fois — docker-prune.sh
# garde cette image (PRUNE_KEEP_IMAGES).
if ! docker image inspect "$IMAGE" >/dev/null 2>&1; then
	echo "mcp-playwright: image absente, téléchargement de $IMAGE…" >&2
	docker pull "$IMAGE" >&2
fi

# --network host : la stack infra/ n'expose que le port 80 de Caddy sur
# l'hôte, et PUBLIC_BASE_URL vaut http://localhost — les redirections et les
# liens absolus de l'app pointent donc vers `localhost`. Branché sur le réseau
# `infra_default`, ce `localhost` désignerait le conteneur du navigateur.
#
# Le montage : le serveur écrit ses snapshots d'accessibilité, ses captures et
# ses journaux de console dans --output-dir et ne renvoie que le chemin — et il
# le renvoie *relatif à son répertoire courant*. Monté n'importe où, ça donne
# des « ../../output/page-….yml » que l'agent, lui, cherche depuis la racine du
# dépôt et ne trouve pas. D'où le miroir : même chemin absolu des deux côtés,
# et -w sur la racine du dépôt, pour que le chemin rendu soit exactement
# « .playwright-mcp/page-….yml ». L'image tourne en uid 1000 (`node`), comme
# l'utilisateur de la WSL : pas de fichier root déposé dans le dépôt.
exec docker run -i --rm --init \
	--network host \
	-v "$OUTPUT_DIR:$REPO_ROOT/.playwright-mcp" \
	-w "$REPO_ROOT" \
	"$IMAGE" \
	--output-dir "$REPO_ROOT/.playwright-mcp" \
	--isolated \
	--viewport-size 1280x720 \
	"$@"
