#!/usr/bin/env bash
# Test of infra/security-headers-drift.sh (#955), run by CI's `shellcheck`
# job: `infra/security-headers-drift.test.sh`, from any directory, no
# argument. Each case writes a Caddyfile and a Middleware file to a
# temporary directory and checks the script's exit status and output.
set -euo pipefail

script="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/security-headers-drift.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
failures=0

fail() {
  echo "FAIL: $*" >&2
  failures=$((failures + 1))
}

CSP="default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; object-src 'none'"
CSP_OLD="default-src 'self'; script-src 'self'; object-src 'none'"

# The `header` block as infra/Caddyfile writes it, around other directives
# and comments that must not be read: a scoped `header @m …`, a scoped
# block `header /legacy/* { … }`, a `header_up`, a commented-out line
# inside the block.
caddyfile() { # $1 = CSP, $2 = name of the X-Frame-Options line
  cat <<EOF
{
	servers {
		timeouts {
			read_header 10s
		}
	}
}

:80 {
	header @has_body Connection close
	# Security headers (#141).
	header {
		Content-Security-Policy "$1"
		Reporting-Endpoints \`csp="/api/csp-report"\`
		Strict-Transport-Security "max-age=31536000"
		X-Content-Type-Options nosniff
		# Referrer-Policy origin
		$2 DENY
		Referrer-Policy no-referrer
	}
	header /legacy/* {
		X-Frame-Options SAMEORIGIN
	}
	reverse_proxy web:3000 {
		header_up X-Frame-Options SAMEORIGIN
	}
}
EOF
}

# The Middleware as deploy writes it: YAML quoting of every kind, and a
# second Middleware whose headers are not the security ones.
middleware() { # $1 = CSP line (whole), $2 = extra line or empty
  cat <<EOF
# Security headers (#141).
apiVersion: traefik.io/v1alpha1
kind: Middleware
metadata:
  name: security-headers
spec:
  headers:
    customResponseHeaders:
$1
      Reporting-Endpoints: 'csp="/api/csp-report"'
      Strict-Transport-Security: max-age=31536000
      # X-Frame-Options: SAMEORIGIN
      X-Content-Type-Options: nosniff
      X-Frame-Options: DENY
$2
      Referrer-Policy: "no-referrer"
---
apiVersion: traefik.io/v1alpha1
kind: Middleware
metadata:
  name: no-sniff
spec:
  headers:
    customRequestHeaders:
      X-Frame-Options: SAMEORIGIN
EOF
}

run() { # $1 = case name; runs `diff` on $work/$1.caddy and $work/$1.yaml
  set +e
  out=$("$script" diff "$work/$1.caddy" "$work/$1.yaml" 2> "$work/$1.err")
  status=$?
  set -e
}

# 1. Aligned: same values, quoted differently on each side (double quotes,
#    backticks, single quotes, bare) — nothing to say.
caddyfile "$CSP" X-Frame-Options > "$work/aligned.caddy"
middleware "      Content-Security-Policy: \"$CSP\"" "" > "$work/aligned.yaml"
run aligned
[ "$status" = 0 ] || fail "aligned: exit $status, expected 0 ($(cat "$work/aligned.err"))"
[ -z "$out" ] || fail "aligned: printed something: $out"
where=$("$script" where "$work/aligned.yaml" m.yaml <<< "$out") || fail "where aligned: exit $?"
[ -z "$where" ] || fail "where aligned: printed something: $where"

# 2. The state before #954: the Caddyfile has 'wasm-unsafe-eval', deploy does
#    not. The CSP and only the CSP differs.
caddyfile "$CSP" X-Frame-Options > "$work/csp.caddy"
middleware "      Content-Security-Policy: \"$CSP_OLD\"" "" > "$work/csp.yaml"
run csp
[ "$status" = 1 ] || fail "csp: exit $status, expected 1"
minus=$(grep -c '^-[^-]' <<< "$out" || true)
plus=$(grep -c '^+[^+]' <<< "$out" || true)
[ "$minus" = 1 ] && [ "$plus" = 1 ] || fail "csp: $minus removed and $plus added lines, expected 1 and 1: $out"
grep -qxF -- "-content-security-policy: $CSP" <<< "$out" || fail "csp: Caddy's CSP missing from the diff: $out"
grep -qxF -- "+content-security-policy: $CSP_OLD" <<< "$out" || fail "csp: the ingress's CSP missing from the diff: $out"

# 2b. `where` names the line of deploy's file to change, and only that one.
where=$("$script" where "$work/csp.yaml" apps/x/middlewares.yaml <<< "$out")
[ "$where" = "apps/x/middlewares.yaml:9: Content-Security-Policy" ] \
  || fail "where csp: got '$where'"

# 2c. `comment`: the marker CI finds it by first, the diff, the line to
#     change and the instruction.
comment=$("$script" comment "$work/csp.yaml" apps/x/middlewares.yaml <<< "$out")
[ "$(head -n1 <<< "$comment")" = '<!-- security-headers-drift -->' ] || fail "comment: marker not first: $comment"
grep -qxF -- "+content-security-policy: $CSP_OLD" <<< "$comment" || fail "comment: diff missing: $comment"
grep -qxF -- '- `apps/x/middlewares.yaml:9` : Content-Security-Policy' <<< "$comment" || fail "comment: line missing: $comment"
grep -qF 'merger juste après celle-ci' <<< "$comment" || fail "comment: instruction missing: $comment"

# 3. A header absent from the ingress: it shows as removed, and `where`
#    points at the `customResponseHeaders:` line, where it goes.
caddyfile "$CSP" X-Frame-Options > "$work/absent.caddy"
middleware "      Content-Security-Policy: \"$CSP\"" "" | grep -v '^      Referrer-Policy' > "$work/absent.yaml"
run absent
[ "$status" = 1 ] || fail "absent: exit $status, expected 1"
grep -qxF -- "-referrer-policy: no-referrer" <<< "$out" || fail "absent: Referrer-Policy not reported: $out"
[ "$(grep -c '^[-+][^-+]' <<< "$out" || true)" = 1 ] || fail "absent: more than one line differs: $out"
where=$("$script" where "$work/absent.yaml" m.yaml <<< "$out")
[ "$where" = "m.yaml:8: Referrer-Policy (absent: add it under customResponseHeaders)" ] \
  || fail "where absent: got '$where'"

# 3b. Absent from the Caddyfile, present on the ingress: shown as added.
caddyfile "$CSP" X-Frame-Options | grep -v 'X-Content-Type-Options' > "$work/absent2.caddy"
middleware "      Content-Security-Policy: \"$CSP\"" "" > "$work/absent2.yaml"
run absent2
[ "$status" = 1 ] || fail "absent2: exit $status, expected 1"
grep -qxF -- "+x-content-type-options: nosniff" <<< "$out" || fail "absent2: X-Content-Type-Options not reported: $out"

# 4. Names in another case on each side: the same headers.
caddyfile "$CSP" x-frame-OPTIONS > "$work/case.caddy"
middleware "      content-security-policy: \"$CSP\"" "" > "$work/case.yaml"
run case
[ "$status" = 0 ] || fail "case: exit $status, expected 0: $out"

# 5. A value differing only in case is a difference: values are compared
#    character for character once unquoted.
caddyfile "$CSP" X-Frame-Options | sed 's/ DENY$/ deny/' > "$work/vcase.caddy"
middleware "      Content-Security-Policy: \"$CSP\"" "" > "$work/vcase.yaml"
run vcase
[ "$status" = 1 ] || fail "vcase: exit $status, expected 1"
grep -qxF -- "-x-frame-options: deny" <<< "$out" || fail "vcase: X-Frame-Options not reported: $out"

# 6. A file that yields none of the six headers: exit 2, not a diff.
echo 'kind: Middleware' > "$work/empty.yaml"
cp "$work/aligned.caddy" "$work/empty.caddy"
run empty
[ "$status" = 2 ] || fail "empty ingress: exit $status, expected 2"
run missing-file
[ "$status" = 2 ] || fail "missing files: exit $status, expected 2"

# 7. The real files of this repository against themselves parse to six
#    headers each (a guard against the extraction silently finding fewer).
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
n=$("$script" extract-caddy "$root/infra/Caddyfile" | wc -l)
[ "$n" = 6 ] || fail "infra/Caddyfile: $n security headers extracted, expected 6"

if [ "$failures" != 0 ]; then
  echo "$failures failure(s)" >&2
  exit 1
fi
echo "security-headers-drift: all cases pass"
