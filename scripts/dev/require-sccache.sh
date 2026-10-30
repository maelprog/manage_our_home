#!/bin/sh
# Garde de gate, à lancer DANS le conteneur Rust avant cargo (#639) :
#   docker run … rust:1-slim-bookworm sh -c 'scripts/dev/require-sccache.sh && cargo …'
# Arrête le gate si le wrapper sccache manque ou n'est pas la version épinglée,
# au lieu de compiler sans cache sans que personne le voie.
set -eu

script_dir=$(dirname "$0")
expected=$(sed -n 's/^SCCACHE_VERSION=//p' "$script_dir/ensure-docker-cache.sh")
wrapper=${RUSTC_WRAPPER:-}

fail() {
	echo "require-sccache: $1" >&2
	echo "require-sccache: lancer scripts/dev/ensure-docker-cache.sh sur l'hôte, puis relancer le gate." >&2
	exit 1
}

[ -n "$wrapper" ] || fail "RUSTC_WRAPPER non défini (cache sccache non monté)."
[ -x "$wrapper" ] || fail "$wrapper absent : volume du binaire sccache vide ou non monté."
actual=$("$wrapper" --version 2>/dev/null) || fail "$wrapper ne s'exécute pas."
[ "$actual" = "sccache $expected" ] || fail "$wrapper rapporte « $actual », attendu sccache $expected."
echo "require-sccache: $actual"
