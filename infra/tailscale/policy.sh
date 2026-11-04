#!/usr/bin/env bash
# Valide ou applique au tailnet du staging la politique versionnée (#684).
#
#   policy.sh render     écrit sur la sortie la politique complète (groupes remplis)
#   policy.sh validate   la fait valider par le coordinateur, bloc `tests` compris
#   policy.sh apply      la valide, puis remplace la politique du tailnet
#
# Le dépôt est public : `policy.json` n'écrit aucun identifiant de connexion.
# Ses groupes y sont vides ; leurs membres viennent de TS_ACL_GROUPS, un objet
# JSON qui nomme exactement les mêmes groupes :
#   {"group:famille": ["…@gmail.com"], "group:admin": ["…@gmail.com"]}
#
# Authentification, au choix (docs/staging-tailscale.md, « Qui applique ») :
#   TS_OAUTH_CLIENT_ID et TS_OAUTH_CLIENT_SECRET   client OAuth, portée policy_file
#   TS_API_KEY                                     clé API d'un administrateur
# TS_TAILNET : nom du tailnet ; `-` (défaut) désigne celui de la clé.
#
# Rien n'est écrit sur disque : la politique rendue ne passe que par des tubes.
set -euo pipefail

API=https://api.tailscale.com/api/v2
TAILNET="${TS_TAILNET:--}"
POLICY="$(dirname "$0")/policy.json"

die() {
	echo "policy.sh: $*" >&2
	exit 1
}

command -v jq >/dev/null || die "jq introuvable"
command -v curl >/dev/null || die "curl introuvable"

render() {
	[ -n "${TS_ACL_GROUPS:-}" ] || die "TS_ACL_GROUPS absent : les groupes ne sont pas dans le dépôt"
	jq -e --argjson groups "$TS_ACL_GROUPS" '
		($groups | type) == "object"
		and (.groups | keys) == ($groups | keys)
		and ([$groups[][] | type == "string" and test("^[^@\\s]+@[^@\\s]+$")] | all)
	' "$POLICY" >/dev/null ||
		die "TS_ACL_GROUPS doit nommer exactement les groupes de policy.json, chacun par une liste d'adresses"
	jq --argjson groups "$TS_ACL_GROUPS" '.groups = $groups' "$POLICY"
}

# En-tête d'autorisation. curl lit l'en-tête et le secret OAuth dans des
# descripteurs (`@<(…)`) : ni l'un ni l'autre n'apparaît dans la liste des
# processus.
auth_header() {
	if [ -n "${TS_OAUTH_CLIENT_ID:-}" ] && [ -n "${TS_OAUTH_CLIENT_SECRET:-}" ]; then
		local token
		token=$(curl -fsS "$API/oauth/token" \
			--data-urlencode "client_id=$TS_OAUTH_CLIENT_ID" \
			--data-urlencode client_secret@<(printf '%s' "$TS_OAUTH_CLIENT_SECRET") |
			jq -er .access_token) || die "échange du client OAuth refusé"
		printf 'Authorization: Bearer %s\n' "$token"
	elif [ -n "${TS_API_KEY:-}" ]; then
		printf 'Authorization: Basic %s\n' "$(printf '%s:' "$TS_API_KEY" | base64 -w0)"
	else
		die "ni TS_OAUTH_CLIENT_ID/TS_OAUTH_CLIENT_SECRET ni TS_API_KEY"
	fi
}

validate() {
	local policy header body
	policy=$(render)
	header=$(auth_header)
	body=$(printf '%s' "$policy" | curl -sS -X POST "$API/tailnet/$TAILNET/acl/validate" \
		-H @<(printf '%s' "$header") -H 'Content-Type: application/json' \
		--data-binary @- -w '\n%{http_code}')
	local code=${body##*$'\n'}
	body=${body%$'\n'*}
	# Réponse vide (`{}`) : la politique est valide et ses tests passent ;
	# sinon `message` et `data` (une entrée par test en échec).
	if [ "$code" = 200 ] && jq -e '(.message // "") == "" and ((.data // []) | length) == 0' <<<"$body" >/dev/null; then
		echo "policy.sh: politique valide, tests du coordinateur verts"
	else
		echo "$body" | jq . >&2 2>/dev/null || echo "$body" >&2
		die "validation refusée (HTTP $code)"
	fi
}

apply() {
	validate
	local policy header etag code
	policy=$(render)
	header=$(auth_header)
	# If-Match : la politique n'est remplacée que si elle n'a pas changé
	# depuis cette lecture.
	etag=$(curl -fsS -D - -o /dev/null "$API/tailnet/$TAILNET/acl" \
		-H @<(printf '%s' "$header") -H 'Accept: application/json' |
		tr -d '\r' | sed -n 's/^[Ee][Tt][Aa][Gg]: *//p')
	[ -n "$etag" ] || die "pas d'ETag sur la politique en place"
	code=$(printf '%s' "$policy" | curl -sS -o /dev/null -w '%{http_code}' -X POST "$API/tailnet/$TAILNET/acl" \
		-H @<(printf '%s' "$header") -H 'Content-Type: application/json' \
		-H "If-Match: $etag" --data-binary @-)
	[ "$code" = 200 ] || die "application refusée (HTTP $code)"
	echo "policy.sh: politique appliquée au tailnet"
}

case "${1:-}" in
render) render ;;
validate) validate ;;
apply) apply ;;
*) die "usage : policy.sh render|validate|apply" ;;
esac
