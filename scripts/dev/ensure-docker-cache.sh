#!/usr/bin/env bash
# Répare le cache de compilation Docker partagé par les gates Rust (#639).
#
# Pas de toolchain Rust sur l'hôte : les gates tournent dans rust:1-slim-bookworm
# avec trois volumes nommés communs à toutes les sessions. `docker run -v nom:…`
# recrée un volume absent vide et sans erreur : un prune les efface sans bruit et
# les builds repartent sans cache. Ce script, idempotent :
#   - crée les volumes manquants ;
#   - installe sccache (version et sha256 épinglés ci-dessous) dans le volume
#     binaire s'il est absent ou d'une autre version, après vérification du sha256 ;
#   - ne touche à rien si tout est en place (une ligne d'état, code 0) ;
#   - échoue (code ≠ 0) si le téléchargement ou la vérification échoue, sans
#     jamais installer un binaire non vérifié.
#
# Noms de volumes surchargeables (tests sur des volumes jetables) :
#   MHOME_CARGO_REGISTRY_VOLUME, MHOME_SCCACHE_VOLUME, MHOME_SCCACHE_BIN_VOLUME.
set -euo pipefail

SCCACHE_VERSION=0.18.0
SCCACHE_TARBALL="sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl"
# sha256 de l'archive, identique au .sha256 publié avec la release.
SCCACHE_SHA256=45f1447fbe231e3037bde351ef70677dd212216c8d62ae7ca409fecc4d6acc89
SCCACHE_URL="https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/${SCCACHE_TARBALL}.tar.gz"
# Image outil : busybox (wget, sha256sum, tar) ; le binaire musl y tourne.
HELPER_IMAGE=alpine:3.22

REGISTRY_VOLUME="${MHOME_CARGO_REGISTRY_VOLUME:-mhome-cargo-registry}"
SCCACHE_VOLUME="${MHOME_SCCACHE_VOLUME:-mhome-sccache}"
BIN_VOLUME="${MHOME_SCCACHE_BIN_VOLUME:-mhome-sccache-bin}"

die() {
	echo "ensure-docker-cache: ERREUR : $*" >&2
	exit 1
}

command -v docker >/dev/null || die "docker introuvable"

created=()
for volume in "$REGISTRY_VOLUME" "$SCCACHE_VOLUME" "$BIN_VOLUME"; do
	if ! docker volume inspect "$volume" >/dev/null 2>&1; then
		docker volume create "$volume" >/dev/null || die "création du volume $volume impossible"
		created+=("$volume")
	fi
done

installed_version() {
	docker run --rm -v "$BIN_VOLUME:/opt/sccache:ro" "$HELPER_IMAGE" \
		sh -c '[ -x /opt/sccache/sccache ] && /opt/sccache/sccache --version' 2>/dev/null || true
}

current="$(installed_version)"
if [ "$current" = "sccache $SCCACHE_VERSION" ]; then
	if [ "${#created[@]}" -eq 0 ]; then
		echo "ensure-docker-cache: OK (sccache $SCCACHE_VERSION, volumes $REGISTRY_VOLUME $SCCACHE_VOLUME $BIN_VOLUME présents)"
	else
		echo "ensure-docker-cache: OK (sccache $SCCACHE_VERSION ; volumes créés : ${created[*]})"
	fi
	exit 0
fi

echo "ensure-docker-cache: $BIN_VOLUME porte « ${current:-aucun sccache} », installation de sccache $SCCACHE_VERSION" >&2

# Téléchargement et vérification hors du volume (/tmp du conteneur) ; le binaire
# n'est copié qu'après un sha256 conforme, puis renommé : le volume reste
# inchangé sur tout échec.
docker run --rm -v "$BIN_VOLUME:/opt/sccache" \
	-e URL="$SCCACHE_URL" -e SHA256="$SCCACHE_SHA256" -e DIR="$SCCACHE_TARBALL" \
	"$HELPER_IMAGE" sh -euc '
		cd /tmp
		wget -q -O sccache.tar.gz "$URL" || { echo "téléchargement échoué : $URL" >&2; exit 1; }
		echo "$SHA256  sccache.tar.gz" | sha256sum -c -s \
			|| { echo "sha256 non conforme pour $URL (attendu $SHA256)" >&2; exit 1; }
		tar -xzf sccache.tar.gz "$DIR/sccache"
		cp "$DIR/sccache" /opt/sccache/.sccache.new
		chmod 0755 /opt/sccache/.sccache.new
		mv -f /opt/sccache/.sccache.new /opt/sccache/sccache
	' || die "installation de sccache $SCCACHE_VERSION échouée, $BIN_VOLUME non modifié"

current="$(installed_version)"
[ "$current" = "sccache $SCCACHE_VERSION" ] \
	|| die "après installation, $BIN_VOLUME rapporte « ${current:-rien} » au lieu de sccache $SCCACHE_VERSION"
echo "ensure-docker-cache: OK (sccache $SCCACHE_VERSION installé dans $BIN_VOLUME${created[*]:+ ; volumes créés : ${created[*]}})"
