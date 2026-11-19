#!/usr/bin/env bash
# Banc d'attaque local (#915) : une pile jetable (Postgres, MinIO, apps/api,
# apps/web) sur un réseau Docker à elle, ports liés à 127.0.0.1 seulement, et
# deux foyers fictifs A et B dont chaque compte reçoit une session utilisable
# avec curl. Mode d'emploi et règle d'usage : docs/guides/banc-attaque.md.
#
#   infra/attack-bench/bench.sh up [--api-url URL] [--web-url URL] > bench.env
#   infra/attack-bench/bench.sh down
#   infra/attack-bench/bench.sh check-target URL
#
# `up` écrit sur stdout des affectations shell (`source bench.env`) et sa
# progression sur stderr. Cible locale uniquement : toute URL qui n'est pas
# http://127.0.0.1[:port] ou http://localhost[:port] est refusée (exit 2)
# avant le premier appel à docker ou à curl.
#
# Aucun secret réel : mots de passe des rôles Postgres, clés MinIO, clés de
# chiffrement et mot de passe des comptes sont tirés de /dev/urandom à chaque
# `up`, et disparaissent avec la pile au `down`.
set -euo pipefail

REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

PREFIX=${BENCH_PREFIX:-mhome-bench}
LABEL="mhome.attack-bench=$PREFIX"
CPUS=${BENCH_CPUS:-4}

# Mêmes images et mêmes pins que infra/docker-compose.yml et le job `e2e` de
# .github/workflows/ci.yml : le banc attaque ce que le déploiement fait
# tourner.
PG_IMAGE=postgres:16.15@sha256:a902560563d3a5661228585f53474c5065e7f273b21ffd77e555a8b5682c5690
MINIO_IMAGE=docker.io/bitnamilegacy/minio:2025.7.23-debian-12-r5@sha256:6dabb4a2088c9a79908de3bc05f4586c23ad2182c8908e7e3acbf61c1467fb20
MC_IMAGE=docker.io/bitnamilegacy/minio-client:2025.7.21-debian-12-r3@sha256:73bd39f7899a0cef12b8dd5df13aa93a3ed1aaa44236542442e9ac76819ac158
RUST_IMAGE=rust:1-slim-bookworm

NET="$PREFIX-net"
PG="$PREFIX-pg"
MINIO="$PREFIX-minio"
API="$PREFIX-api"
WEB="$PREFIX-web"
TARGET_VOLUME="$PREFIX-target"

log() { echo "bench: $*" >&2; }
die() {
	echo "bench: ERREUR : $*" >&2
	exit 1
}

usage() {
	cat >&2 <<-'EOF'
		usage: bench.sh up [--api-url URL] [--web-url URL]   démarre la pile et crée les comptes
		       bench.sh down                                 détruit la pile (volumes compris)
		       bench.sh check-target URL                     exit 0 si URL est une cible locale
	EOF
	exit 2
}

# --- Garde de cible -------------------------------------------------------
#
# Seules passent `http://127.0.0.1` et `http://localhost`, avec un port et un
# chemin facultatifs. Le motif est ancré des deux côtés et n'admet ni
# arobase (l'hôte réel serait après), ni `?`/`#` collés à l'hôte, ni blanc,
# ni saut de ligne : la chaîne entière doit être une URL de boucle locale.
# https est refusé aussi sur localhost : la pile du banc ne parle que http, et
# une URL https désigne une autre pile que la sienne.
LOCAL_TARGET_RE='^http://(127\.0\.0\.1|localhost)(:[0-9]{1,5})?(/[A-Za-z0-9._~/%-]*)?$'

check_target() {
	local url=$1 port
	if [[ "$url" =~ $LOCAL_TARGET_RE ]]; then
		port=${BASH_REMATCH[2]#:}
		if [[ -z "$port" ]] || ((10#$port >= 1 && 10#$port <= 65535)); then
			return 0
		fi
	fi
	{
		printf 'bench: cible refusée : %s\n' "$url"
		echo "bench: cible locale uniquement — le banc ne vise que http://127.0.0.1[:port] ou http://localhost[:port]"
		echo "bench: (jamais le staging, ni un hôte distant : docs/guides/banc-attaque.md)"
	} >&2
	exit 2
}

# Port d'une URL déjà gardée ; 80 sans port explicite.
port_of() {
	[[ "$1" =~ $LOCAL_TARGET_RE ]]
	local port=${BASH_REMATCH[2]#:}
	echo "$((10#${port:-80}))"
}

# --- Outils ---------------------------------------------------------------

random_hex() { od -An -tx1 -N"$1" /dev/urandom | tr -d ' \n'; }
random_b64() { head -c "$1" /dev/urandom | base64 | tr -d '\n'; }

# psql en superutilisateur dans le conteneur Postgres, requête sur stdin.
sql() {
	docker exec -i "$PG" psql -X -q -t -A -v ON_ERROR_STOP=1 -U bench -d mhome
}

# Requête HTTP vers apps/api. Imprime le code de statut ; corps dans $BODY_FILE.
# `--noproxy '*'` : un http_proxy de l'environnement ne doit pas emmener la
# requête ailleurs que sur la boucle locale.
http() {
	local method=$1 path=$2 jar=${3:-} data=${4:-}
	local args=(-s -o "$BODY_FILE" -w '%{http_code}' --noproxy '*' -X "$method")
	[[ -n "$jar" ]] && args+=(-b "$jar" -c "$jar")
	[[ -n "$data" ]] && args+=(-H 'Content-Type: application/json' --data "$data")
	curl "${args[@]}" "$API_URL$path"
}

expect() {
	local want=$1 got=$2 what=$3
	[[ "$got" == "$want" ]] || die "$what : HTTP $got (attendu $want) — $(head -c 300 "$BODY_FILE")"
}

json_field() { sed -n "s/.*\"$1\":\"\\([^\"]*\\)\".*/\\1/p" "$BODY_FILE"; }

cookie_of() { awk '$6 == "session_id" { print $6 "=" $7 }' "$1"; }

wait_for() {
	local what=$1 tries=$2
	shift 2
	for ((i = 1; i <= tries; i++)); do
		"$@" >/dev/null 2>&1 && return 0
		sleep 1
	done
	die "$what ne répond pas après ${tries} s"
}

# --- up -------------------------------------------------------------------

start_stack() {
	if [[ -n "$(docker ps -aq --filter "label=$LABEL")" ]]; then
		die "une pile $PREFIX existe déjà : bench.sh down d'abord"
	fi

	"$REPO/scripts/dev/ensure-docker-cache.sh" >&2

	log "réseau $NET"
	docker network create --label "$LABEL" "$NET" >/dev/null

	export POSTGRES_PASSWORD APP_ROLE_PASSWORD MIGRATION_ROLE_PASSWORD ADMIN_ROLE_PASSWORD
	POSTGRES_PASSWORD=$(random_hex 24)
	APP_ROLE_PASSWORD=$(random_hex 24)
	MIGRATION_ROLE_PASSWORD=$(random_hex 24)
	ADMIN_ROLE_PASSWORD=$(random_hex 24)

	# Rôles du déploiement (infra/postgres/init/01-roles.sh) : apps/api sert
	# les requêtes en app_role, NOSUPERUSER NOBYPASSRLS, comme en production.
	log "Postgres"
	docker run -d --name "$PG" --label "$LABEL" --network "$NET" \
		-e POSTGRES_USER=bench -e POSTGRES_DB=mhome -e POSTGRES_PASSWORD \
		-e APP_ROLE_PASSWORD -e MIGRATION_ROLE_PASSWORD -e ADMIN_ROLE_PASSWORD \
		-v "$REPO/infra/postgres/init:/docker-entrypoint-initdb.d:ro" \
		"$PG_IMAGE" >/dev/null

	export MINIO_ROOT_USER MINIO_ROOT_PASSWORD
	MINIO_ROOT_USER=bench-$(random_hex 6)
	MINIO_ROOT_PASSWORD=$(random_hex 24)
	log "MinIO"
	docker run -d --name "$MINIO" --label "$LABEL" --network "$NET" \
		-e MINIO_ROOT_USER -e MINIO_ROOT_PASSWORD "$MINIO_IMAGE" >/dev/null

	log "compilation d'apps/api et apps/web (volume $TARGET_VOLUME)"
	docker volume create --label "$LABEL" "$TARGET_VOLUME" >/dev/null
	docker run --rm --name "$PREFIX-build" --label "$LABEL" --cpus "$CPUS" \
		-v "$REPO:/workspace:ro" -w /workspace \
		-v "$TARGET_VOLUME:/target" -e CARGO_TARGET_DIR=/target \
		-v mhome-cargo-registry:/usr/local/cargo/registry \
		-v mhome-sccache:/sccache -v mhome-sccache-bin:/opt/sccache:ro \
		-e RUSTC_WRAPPER=/opt/sccache/sccache -e SCCACHE_DIR=/sccache \
		-e CARGO_INCREMENTAL=0 -e CARGO_BUILD_JOBS="$CPUS" -e SQLX_OFFLINE=true \
		"$RUST_IMAGE" sh -c 'scripts/dev/require-sccache.sh && cargo build -q -p manage_our_home -p manage_our_home_web' >&2

	# Postgres : la sonde TCP, pas le socket (le premier démarrage passe par
	# un serveur temporaire qui n'écoute que sur le socket, cf. compose).
	wait_for Postgres 60 docker exec "$PG" pg_isready -h 127.0.0.1 -U bench -d mhome

	# Bitnami démarre une instance de réglage puis la finale (#298) : on
	# attend la ligne qui clôt le réglage avant de créer le bucket.
	wait_for MinIO 60 sh -c "docker logs '$MINIO' 2>&1 | grep -q 'MinIO setup finished'"
	docker run --rm --label "$LABEL" --network "$NET" \
		-e MINIO_ROOT_USER -e MINIO_ROOT_PASSWORD --entrypoint /bin/sh "$MC_IMAGE" -c \
		"for i in \$(seq 1 30); do mc alias set local http://$MINIO:9000 \"\$MINIO_ROOT_USER\" \"\$MINIO_ROOT_PASSWORD\" >/dev/null 2>&1 && break; sleep 1; done; mc mb -p local/manage-our-home" >&2

	log "migrations"
	docker run --rm --label "$LABEL" --network "$NET" \
		-v "$TARGET_VOLUME:/target:ro" \
		-e MIGRATION_DATABASE_URL="postgres://migration_role:$MIGRATION_ROLE_PASSWORD@$PG/mhome" \
		"$RUST_IMAGE" /target/debug/migrate >&2

	export OAUTH_ENCRYPTION_KEY MESSAGE_ENCRYPTION_KEY LOGIN_THROTTLE_KEY CALENDAR_FEED_ENCRYPTION_KEY
	OAUTH_ENCRYPTION_KEY=$(random_b64 32)
	MESSAGE_ENCRYPTION_KEY=$(random_b64 32)
	LOGIN_THROTTLE_KEY=$(random_b64 32)
	CALENDAR_FEED_ENCRYPTION_KEY=$(random_b64 32)

	# SMTP pointe sur un hôte qui n'existe pas : aucun courriel ne sort ;
	# les jetons de vérification se lisent en base (seed_accounts).
	log "apps/api sur $API_URL"
	docker run -d --name "$API" --label "$LABEL" --network "$NET" \
		-p "127.0.0.1:$API_PORT:8080" \
		-v "$TARGET_VOLUME:/target:ro" -v "$REPO:/workspace:ro" -w /workspace \
		-e DATABASE_URL="postgres://app_role:$APP_ROLE_PASSWORD@$PG/mhome" \
		-e ADMIN_DATABASE_URL="postgres://admin_role:$ADMIN_ROLE_PASSWORD@$PG/mhome" \
		-e PUBLIC_BASE_URL="$API_URL" -e FRONTEND_BASE_URL="$WEB_URL" \
		-e GOOGLE_CLIENT_ID=bench-dummy -e GOOGLE_CLIENT_SECRET=bench-dummy \
		-e OAUTH_ENCRYPTION_KEY -e MESSAGE_ENCRYPTION_KEY -e LOGIN_THROTTLE_KEY \
		-e CALENDAR_FEED_ENCRYPTION_KEY \
		-e SECURE_COOKIES=false \
		-e SMTP_HOST=smtp.invalid -e SMTP_USERNAME=bench-dummy -e SMTP_PASSWORD=bench-dummy \
		-e 'SMTP_FROM=Banc <no-reply@example.test>' \
		-e MINIO_ENDPOINT="http://$MINIO:9000" -e MINIO_ACCESS_KEY="$MINIO_ROOT_USER" \
		-e MINIO_SECRET_KEY="$MINIO_ROOT_PASSWORD" -e MINIO_BUCKET=manage-our-home \
		"$RUST_IMAGE" /target/debug/manage_our_home >/dev/null

	log "apps/web sur $WEB_URL"
	docker run -d --name "$WEB" --label "$LABEL" --network "$NET" \
		-p "127.0.0.1:$WEB_PORT:3000" \
		-v "$TARGET_VOLUME:/target:ro" -v "$REPO:/workspace:ro" -w /workspace \
		-e API_INTERNAL_BASE_URL="http://$API:8080" -e API_PUBLIC_BASE_URL="$API_URL" \
		-e WEB_BIND_ADDR=0.0.0.0:3000 \
		"$RUST_IMAGE" /target/debug/manage_our_home_web >/dev/null

	wait_for apps/api 60 curl -sf --noproxy '*' "$API_URL/readyz"
	wait_for apps/web 60 curl -sf --noproxy '*' -o /dev/null "$WEB_URL/login"
}

# Inscrit un compte par l'API. Le jeton de vérification n'est gardé en base
# que par son empreinte (#335) : comme e2e/lib/db.ts, on pose sur la ligne
# créée l'empreinte d'un jeton neuf, puis on suit le vrai lien.
register() {
	local email=$1 name=$2
	expect 201 "$(http POST /auth/register '' \
		"{\"email\":\"$email\",\"password\":\"$PASSWORD\",\"display_name\":\"$name\",\"declares_minimum_age\":true,\"accepts_terms\":true}")" \
		"inscription de $email"
}

verify() {
	local email=$1 token
	token=$(sql <<-SQL
		WITH t AS (SELECT gen_random_bytes(32) AS b)
		UPDATE email_verification_tokens SET token_hash = digest((SELECT b FROM t), 'sha256')
		WHERE token_hash = (
		    SELECT v.token_hash FROM email_verification_tokens v JOIN users u ON u.id = v.user_id
		    WHERE u.email = '$email' ORDER BY v.created_at DESC LIMIT 1)
		RETURNING rtrim(translate(encode((SELECT b FROM t), 'base64'), '+/', '-_'), '=');
	SQL
	)
	[[ -n "$token" ]] || die "aucun jeton de vérification pour $email"
	expect 200 "$(http GET "/auth/verify-email?token=$token")" "vérification de $email"
}

login() {
	local email=$1 jar=$2
	http POST /auth/login "$jar" "{\"email\":\"$email\",\"password\":\"$PASSWORD\"}"
}

user_id() { sql <<<"SELECT id FROM users WHERE email = '$1';"; }

# Imprime les affectations d'un compte sur stdout.
emit_account() {
	local var=$1 email=$2 jar=$3 note=$4
	printf '%s_EMAIL=%s\n%s_ID=%s\n' "$var" "$email" "$var" "$(user_id "$email")"
	printf "%s_COOKIE='%s'\n" "$var" "$(cookie_of "$jar")"
	SUMMARY+=$(printf '\n  %-22s %-34s %s' "$var" "$email" "$note")
}

seed_household() {
	local h=$1 lc jars group token
	lc=$(tr '[:upper:]' '[:lower:]' <<<"$h")
	jars="$WORK/$lc"
	mkdir -p "$jars"
	for role in owner member former deactivated superadmin unverified; do : >"$jars/$role"; done
	for role in owner member former deactivated superadmin unverified; do
		register "$lc-$role@bench.example.test" "$h ${role^}"
	done
	for role in owner member former deactivated superadmin; do
		verify "$lc-$role@bench.example.test"
		expect 200 "$(login "$lc-$role@bench.example.test" "$jars/$role")" "connexion de $lc-$role"
	done

	expect 201 "$(http POST /groups "$jars/owner" "{\"name\":\"Foyer $h\"}")" "création du foyer $h"
	group=$(json_field id)
	[[ -n "$group" ]] || die "identifiant du foyer $h introuvable"

	for role in member former deactivated superadmin; do
		expect 201 "$(http POST "/groups/$group/invitations" "$jars/owner" '{"invited_email":null}')" \
			"invitation pour $lc-$role"
		token=$(json_field token)
		expect 200 "$(http POST "/groups/invitations/$token/accept" "$jars/$role")" \
			"acceptation de l'invitation par $lc-$role"
	done

	# Ancien membre : quitte le foyer, garde sa session.
	expect 200 "$(http POST "/groups/$group/leave" "$jars/former" '{}')" "départ de $lc-former"

	# Superadmin technique (aucune route ne le donne : SQL, comme
	# e2e/lib/db.ts makeSuperadmin), membre simple de son foyer.
	sql <<<"UPDATE users SET is_superadmin = true WHERE email = '$lc-superadmin@bench.example.test';" >/dev/null

	# Désactivé par la vraie action de support, qui révoque ses sessions ;
	# la reconnexion ouvre la session restreinte (#289).
	expect 204 "$(http POST "/admin/users/$(user_id "$lc-deactivated@bench.example.test")/deactivate" "$jars/superadmin")" \
		"désactivation de $lc-deactivated"
	: >"$jars/deactivated"
	expect 200 "$(login "$lc-deactivated@bench.example.test" "$jars/deactivated")" "reconnexion de $lc-deactivated"

	# Non vérifié : la connexion est refusée, aucune session.
	local unverified_status
	unverified_status=$(login "$lc-unverified@bench.example.test" "$jars/unverified")

	printf '%s_GROUP_ID=%s\n' "$h" "$group"
	emit_account "${h}_OWNER" "$lc-owner@bench.example.test" "$jars/owner" "propriétaire"
	emit_account "${h}_MEMBER" "$lc-member@bench.example.test" "$jars/member" "membre simple"
	emit_account "${h}_FORMER" "$lc-former@bench.example.test" "$jars/former" "ancien membre (a quitté)"
	emit_account "${h}_UNVERIFIED" "$lc-unverified@bench.example.test" "$jars/unverified" \
		"non vérifié — connexion HTTP $unverified_status, aucune session"
	emit_account "${h}_DEACTIVATED" "$lc-deactivated@bench.example.test" "$jars/deactivated" \
		"désactivé — session restreinte (#289)"
	emit_account "${h}_SUPERADMIN" "$lc-superadmin@bench.example.test" "$jars/superadmin" \
		"superadmin, membre simple du foyer"
	eval "${h}_GROUP=$group"
}

# Contrôle intégré : A-propriétaire lit son foyer, pas celui de B.
sanity_check() {
	local own other jar="$WORK/a/owner"
	own=$(http GET "/groups/$A_GROUP" "$jar")
	other=$(http GET "/groups/$B_GROUP" "$jar")
	[[ "$own" == 200 ]] || die "contrôle : A-propriétaire sur son foyer → HTTP $own (attendu 200)"
	[[ "$other" == 403 || "$other" == 404 ]] ||
		die "contrôle : A-propriétaire sur le foyer B → HTTP $other (attendu 403 ou 404) — ISOLATION ROMPUE"
	log "contrôle : A-propriétaire → foyer A HTTP $own, foyer B HTTP $other"
}

cmd_up() {
	API_URL=${BENCH_API_URL:-http://127.0.0.1:${BENCH_API_PORT:-18080}}
	WEB_URL=${BENCH_WEB_URL:-http://127.0.0.1:${BENCH_WEB_PORT:-13000}}
	while (($#)); do
		case $1 in
		--api-url)
			(($# >= 2)) || usage
			API_URL=$2
			shift 2
			;;
		--web-url)
			(($# >= 2)) || usage
			WEB_URL=$2
			shift 2
			;;
		*) usage ;;
		esac
	done
	# La garde passe avant tout appel à docker ou à curl.
	check_target "$API_URL"
	check_target "$WEB_URL"
	API_URL=${API_URL%/}
	WEB_URL=${WEB_URL%/}
	API_PORT=$(port_of "$API_URL")
	WEB_PORT=$(port_of "$WEB_URL")
	[[ "$PREFIX" =~ ^[a-z0-9][a-z0-9_.-]*$ ]] || die "BENCH_PREFIX invalide : $PREFIX"

	WORK=$(mktemp -d)
	BODY_FILE="$WORK/body"
	trap 'rm -rf "$WORK"' EXIT

	start_stack

	PASSWORD=$(random_hex 16)
	SUMMARY=""
	{
		printf 'BENCH_API_URL=%s\nBENCH_WEB_URL=%s\nBENCH_PASSWORD=%s\n' "$API_URL" "$WEB_URL" "$PASSWORD"
		seed_household A
		seed_household B
	} >"$WORK/env"
	# shellcheck source=/dev/null
	source <(grep -E '^[AB]_GROUP_ID=' "$WORK/env")
	A_GROUP=$A_GROUP_ID B_GROUP=$B_GROUP_ID
	sanity_check
	cat "$WORK/env"
	log "comptes (mot de passe commun : $PASSWORD)$SUMMARY"
	log "prêt. Exemple : curl -H \"Cookie: \$A_OWNER_COOKIE\" \"\$BENCH_API_URL/groups/\$B_GROUP_ID\""
}

# --- down -----------------------------------------------------------------

cmd_down() {
	(($# == 0)) || usage
	local ids left
	ids=$(docker ps -aq --filter "label=$LABEL")
	# shellcheck disable=SC2086 # une liste d'identifiants, découpée exprès
	[[ -z "$ids" ]] || docker rm -f -v $ids >/dev/null
	ids=$(docker volume ls -q --filter "label=$LABEL")
	# shellcheck disable=SC2086
	[[ -z "$ids" ]] || docker volume rm $ids >/dev/null
	ids=$(docker network ls -q --filter "label=$LABEL")
	# shellcheck disable=SC2086
	[[ -z "$ids" ]] || docker network rm $ids >/dev/null
	left=$(
		docker ps -aq --filter "label=$LABEL"
		docker volume ls -q --filter "label=$LABEL"
		docker network ls -q --filter "label=$LABEL"
	)
	[[ -z "$left" ]] || die "il reste des ressources $LABEL : $left"
	log "pile $PREFIX détruite : aucun conteneur, volume ni réseau restant"
}

case ${1:-} in
up)
	shift
	cmd_up "$@"
	;;
down)
	shift
	cmd_down "$@"
	;;
check-target)
	(($# == 2)) || usage
	check_target "$2"
	;;
*) usage ;;
esac
