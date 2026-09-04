#!/usr/bin/env bash
# Purge des ressources Docker inutilisées, pour empêcher le disque virtuel WSL
# (~/AppData/Local/wsl/{...}/ext4.vhdx côté Windows) de gonfler sans fin.
#
# Le poids ici n'est pas dans les images (~7 Go de bases nommées, aucune
# dangling) mais dans les volumes de cache de build : un seul `*-target` Rust
# orphelin pesait 16 Go. La purge est donc agressive sur tout ce qui n'est
# rattaché à aucun conteneur — images comprises — SAUF les volumes de données
# listés dans KEEP_VOLUMES, qui contiennent Postgres/MinIO/Caddy et se
# retrouvent "non référencés" dès que la stack infra/ est simplement à l'arrêt.
#
#   ./docker-prune.sh                purge
#   ./docker-prune.sh --dry-run      montre ce qui serait supprimé
#   ./docker-prune.sh --install-hook purge après chaque merge (hook git post-merge)
#   ./docker-prune.sh --install      purge quotidienne (timer systemd utilisateur)
#   ./docker-prune.sh --uninstall    retire hook et timer
#
# Réglages par variable d'environnement :
#   PRUNE_KEEP_VOLUMES  regex des volumes à ne jamais supprimer
#   PRUNE_KEEP_IMAGES   regex des images à ne jamais supprimer, p.ex.
#                       '^mcr\.microsoft\.com/playwright' (2,4 Go à re-pull)
set -euo pipefail

# Le contexte `desktop-linux` (Docker Desktop) coexiste avec le moteur installé
# dans la WSL. Sans ce pin, un DOCKER_CONTEXT hérité de l'environnement ferait
# purger le mauvais démon.
export DOCKER_CONTEXT="${DOCKER_CONTEXT:-default}"

KEEP_VOLUMES="${PRUNE_KEEP_VOLUMES:-(postgres_data|minio_data|caddy_data|ollama_data)$}"
# L'image du MCP Playwright (infra/mcp-playwright.sh) est un outil du dépôt,
# mais aucun conteneur ne la référence entre deux sessions : sans cette
# exception la purge la supprime et Claude Code re-télécharge 1 Go au
# démarrage suivant.
KEEP_IMAGES="${PRUNE_KEEP_IMAGES:-^mcr\.microsoft\.com/playwright/mcp}"

DRY_RUN=0
SERVICE_NAME="docker-prune"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
SCRIPT_PATH="$(readlink -f "${BASH_SOURCE[0]}")"

log() { printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*"; }

# Espace utilisé sur / en Ko : c'est la seule mesure qui reflète vraiment le
# vhdx, `docker system df` ne comptant que ce que le démon sait attribuer.
disk_used_kb() { df -k --output=used / | tail -1 | tr -d ' '; }

human() { numfmt --to=iec --suffix=B "$(( $1 * 1024 ))" 2>/dev/null || echo "${1}K"; }

run() {
	if (( DRY_RUN )); then
		printf '  [dry-run] %s\n' "$*"
	else
		"$@" >/dev/null 2>&1 || true
	fi
}

prune_containers() {
	local ids
	ids=$(docker ps -aq -f status=exited -f status=created -f status=dead)
	[[ -z $ids ]] && { log "conteneurs   : rien à faire"; return; }
	log "conteneurs   : $(wc -l <<<"$ids") arrêté(s)"
	run docker rm $ids
}

prune_images() {
	local in_use ref id removed=0
	# Toute image référencée par un conteneur, y compris arrêté — on tourne
	# après prune_containers, donc l'ensemble est déjà au plus juste.
	in_use=$(docker ps -aq | xargs -r docker inspect -f '{{.Image}}' 2>/dev/null | sort -u)

	while IFS=$'\t' read -r id ref; do
		[[ -z $id ]] && continue
		grep -qxF "$id" <<<"$in_use" && continue
		if [[ -n $KEEP_IMAGES && $ref =~ $KEEP_IMAGES ]]; then
			log "  gardée     : $ref (KEEP_IMAGES)"
			continue
		fi
		# Supprimer par référence et non par ID : une image portant plusieurs
		# tags refuse le `rmi <id>` et il faut détaguer tag par tag.
		if [[ $ref == *"<none>"* ]]; then
			log "  supprimée  : $id (sans tag)"
			run docker rmi "$id"
		else
			log "  supprimée  : $ref"
			run docker rmi "$ref"
		fi
		removed=$((removed + 1))
	done < <(docker image ls --no-trunc --format '{{.ID}}\t{{.Repository}}:{{.Tag}}')

	log "images       : $removed supprimée(s)"
	# Balaie les couches parentes devenues orphelines par les rmi ci-dessus.
	run docker image prune -f
}

prune_volumes() {
	local name removed=0
	while read -r name; do
		[[ -z $name ]] && continue
		if [[ $name =~ $KEEP_VOLUMES ]]; then
			log "  gardé      : $name (volume de données)"
			continue
		fi
		log "  supprimé   : $name"
		run docker volume rm "$name"
		removed=$((removed + 1))
	done < <(docker volume ls -q -f dangling=true)
	log "volumes      : $removed supprimé(s)"
}

prune_rest() {
	log "build cache  : purge"
	run docker builder prune -af
	log "réseaux      : purge"
	run docker network prune -f
}

do_prune() {
	local before after
	if ! docker info >/dev/null 2>&1; then
		log "démon Docker injoignable (contexte $DOCKER_CONTEXT) — abandon"
		exit 0
	fi

	before=$(disk_used_kb)
	log "=== purge Docker — $(human "$before") utilisés sur / ==="
	prune_containers
	prune_images
	prune_volumes
	prune_rest
	after=$(disk_used_kb)

	# / est monté avec `discard`, donc le TRIM part bien vers le disque virtuel,
	# mais cela ne suffit pas à faire maigrir le ext4.vhdx : WSL ne rend les
	# blocs à Windows que si le mode sparse est activé sur la distro, une fois
	# pour toutes, distro arrêtée (voir infra/README.md) :
	#     wsl --manage Ubuntu --set-sparse true
	# Sans cela cette purge empêche le vhdx de grossir davantage, mais ne
	# reprend pas l'espace déjà alloué côté Windows.
	log "=== terminé — $(human "$after") utilisés, $(human "$(( before - after ))") libérés ==="
}

# L'unité systemd est partagée par les deux déclenchements : le timer l'appelle
# quotidiennement, le hook git l'appelle après chaque merge. Passer par elle
# plutôt que par un `&` détaché donne le journal (journalctl) et la garantie
# qu'une seule purge tourne à la fois.
write_service_unit() {
	mkdir -p "$UNIT_DIR"
	cat >"$UNIT_DIR/$SERVICE_NAME.service" <<EOF
[Unit]
Description=Purge des ressources Docker inutilisées (manage_our_home)
Documentation=file://$SCRIPT_PATH

[Service]
Type=oneshot
ExecStart=$SCRIPT_PATH
Nice=10
IOSchedulingClass=idle
EOF
	systemctl --user daemon-reload
}

install_hook() {
	local hook_dir hook
	# --git-common-dir et non --git-dir : depuis un worktree le second pointe
	# sur .git/worktrees/<nom>, qui n'a pas de hooks. Les worktrees partagent
	# ceux du dépôt principal, donc une seule installation les couvre tous.
	hook_dir="$(git rev-parse --path-format=absolute --git-common-dir)/hooks"
	hook="$hook_dir/post-merge"
	mkdir -p "$hook_dir"

	write_service_unit

	cat >"$hook" <<EOF
#!/usr/bin/env bash
# Purge Docker après chaque merge entrant — typiquement le \`git pull\` qui
# rapatrie une PR fusionnée, moment où les caches de sa branche deviennent
# des déchets. Installé par $SCRIPT_PATH --install-hook
#
# Volontairement dans .git/hooks et non .githooks : activer core.hooksPath
# allumerait aussi le pre-commit, qui lance cargo sans toolchain sur l'hôte.

# --no-block rend la main tout de suite : le \`git pull\` ne doit pas attendre
# la purge. Les logs vont dans journalctl --user -u $SERVICE_NAME.service
if systemctl --user start --no-block $SERVICE_NAME.service 2>/dev/null; then
	exit 0
fi

# Repli si le systemd utilisateur est indisponible (session non interactive).
setsid "$SCRIPT_PATH" >/dev/null 2>&1 &
exit 0
EOF
	chmod +x "$hook"
	log "hook post-merge installé dans $hook"
}

install_timer() {
	write_service_unit

	cat >"$UNIT_DIR/$SERVICE_NAME.timer" <<EOF
[Unit]
Description=Purge Docker quotidienne (manage_our_home)

[Timer]
OnCalendar=daily
# La WSL est éteinte la plupart du temps : sans Persistent le déclenchement de
# 03:00 serait simplement manqué. Ici il rattrape à la première session du jour.
Persistent=true
RandomizedDelaySec=15m

[Install]
WantedBy=timers.target
EOF

	systemctl --user daemon-reload
	systemctl --user enable --now "$SERVICE_NAME.timer"
	log "timer installé dans $UNIT_DIR"
	systemctl --user list-timers "$SERVICE_NAME.timer" --no-pager
}

uninstall_all() {
	systemctl --user disable --now "$SERVICE_NAME.timer" 2>/dev/null || true
	rm -f "$UNIT_DIR/$SERVICE_NAME.service" "$UNIT_DIR/$SERVICE_NAME.timer"
	systemctl --user daemon-reload
	rm -f "$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null)/hooks/post-merge"
	log "hook et timer retirés"
}

case "${1:-}" in
	--dry-run)      DRY_RUN=1; do_prune ;;
	--install-hook) install_hook ;;
	--install)      install_timer ;;
	--uninstall)    uninstall_all ;;
	"")             do_prune ;;
	*) echo "usage: $0 [--dry-run|--install-hook|--install|--uninstall]" >&2; exit 2 ;;
esac
