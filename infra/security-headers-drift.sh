#!/usr/bin/env bash
# The six security headers of infra/Caddyfile against those of the ingress
# Middleware on the `deploy` branch (#955), compared as text.
#
#   infra/security-headers-drift.sh diff CADDYFILE MIDDLEWARES
#   infra/security-headers-drift.sh where MIDDLEWARES LABEL < diff
#   infra/security-headers-drift.sh comment MIDDLEWARES LABEL < diff
#   infra/security-headers-drift.sh extract-caddy CADDYFILE
#   infra/security-headers-drift.sh extract-ingress MIDDLEWARES
#
# `diff` prints a unified diff of the six headers, one `name: value` line
# each (- Caddyfile, + ingress), and exits 0 when they are the same, 1 when
# they differ, 2 when a file is unreadable or yields none of the six.
# `where` reads such a diff and prints, for each header in it, the line of
# MIDDLEWARES to change, as `LABEL:<line>: <Header>`. `comment` prints the
# pull request comment (Markdown) for such a diff: the diff, those lines
# and what to do. CI's `security-headers-drift` job runs `diff` and
# `comment` on a pull request that changes
# infra/Caddyfile and only warns: see the job's comment in
# .github/workflows/ci.yml.
#
# The six headers are those of the `SECURITY` pattern of ci/ingress-test.sh
# on `deploy`, which compares the same two sides on real responses when
# `deploy` is promoted. Names are compared in lower case; values once their
# quoting is removed (Caddyfile: "…" or `…`; YAML: "…" or '…'), then
# character for character.
#
# What it reads of the Caddyfile: the lines of every `header {` block
# without a matcher, each `Name value`, the last one winning — wherever the
# block sits: one inside a `handle /api/* { … }` or another route block is
# read as if it applied to every response. It is not the parser of
# apps/web/src/csp.rs (`caddy_header`), which follows Caddy's semantics —
# matchers, `>`/`?`/`-` prefixes, `defer`, one-line `header Name value` —
# to pin the CSP the browser receives; this one only has to read the block
# infra/Caddyfile actually writes, and the test of this script checks it
# still finds the six there. Of the YAML, the `customResponseHeaders:` maps,
# the keys indented under them.
set -euo pipefail

usage() {
  sed -n '5,10p' "${BASH_SOURCE[0]}" | sed 's/^# *//' >&2
  exit 2
}

# The first line of the pull request comment `comment` prints: CI finds the
# comment to update by it.
MARKER='<!-- security-headers-drift -->'

# The six names, lower case, in the order of ci/ingress-test.sh's pattern.
NAMES='content-security-policy reporting-endpoints strict-transport-security x-content-type-options x-frame-options referrer-policy'

# Lines `name: value` for the six headers found, in NAMES order.
print_in_order='
function emit(   i, n, order) {
  n = split(names, order, " ")
  for (i = 1; i <= n; i++) if (order[i] in v) print order[i] ": " v[order[i]]
}'

extract_caddy() {
  awk -v names="$NAMES" "$print_in_order"'
    BEGIN { want = " " names " " }
    {
      line = $0
      sub(/^[ \t]+/, "", line)
      sub(/[ \t\r]+$/, "", line)
    }
    inblock && line ~ /^}/ { inblock = 0; next }
    inblock {
      if (line == "" || line ~ /^#/) next
      name = line
      sub(/[ \t].*/, "", name)
      val = substr(line, length(name) + 1)
      sub(/^[ \t]+/, "", val)
      name = tolower(name)
      if (!index(want, " " name " ")) next
      if (val ~ /^".*"$/) {
        val = substr(val, 2, length(val) - 2)
        gsub(/\\"/, "\"", val)
      } else if (val ~ /^`.*`$/) {
        val = substr(val, 2, length(val) - 2)
      }
      v[name] = val
      next
    }
    line ~ /^header[ \t]*\{$/ { inblock = 1 }
    END { emit() }
  ' "$1"
}

# With `-v lines=1`: `<line>\t<name>` instead, plus `<line>\tCRH` for each
# `customResponseHeaders:` line.
extract_ingress() {
  awk -v names="$NAMES" -v lines="${2:-0}" "$print_in_order"'
    BEGIN { want = " " names " " }
    {
      raw = $0
      sub(/\r$/, "", raw)
      match(raw, /^ */)
      indent = RLENGTH
    }
    inmap && raw !~ /^[ \t]*(#.*)?$/ && indent <= mapindent { inmap = 0 }
    inmap {
      line = raw
      sub(/^[ \t]+/, "", line)
      if (line == "" || line ~ /^#/ || !index(line, ":")) next
      name = substr(line, 1, index(line, ":") - 1)
      val = substr(line, index(line, ":") + 1)
      if (name ~ /^".*"$/ || name ~ /^\047.*\047$/) name = substr(name, 2, length(name) - 2)
      name = tolower(name)
      if (!index(want, " " name " ")) next
      sub(/^[ \t]+/, "", val)
      sub(/[ \t]+$/, "", val)
      if (val ~ /^".*"$/) {
        val = substr(val, 2, length(val) - 2)
        gsub(/\\"/, "\"", val)
        gsub(/\\\\/, "\\", val)
      } else if (val ~ /^\047.*\047$/) {
        val = substr(val, 2, length(val) - 2)
        gsub(/\047\047/, "\047", val)
      } else {
        sub(/[ \t]+#.*$/, "", val)
      }
      v[name] = val
      at[name] = NR
      next
    }
    raw ~ /^[ \t]*customResponseHeaders:[ \t]*$/ {
      inmap = 1
      mapindent = indent
      crh = NR
    }
    END {
      if (!lines) { emit(); exit }
      if (crh) print crh "\tCRH"
      for (name in at) print at[name] "\t" name
    }
  ' "$1"
}

# One line `LABEL:<line>: <Header>` per header named in the diff on stdin:
# the line of MIDDLEWARES that carries it or, when it is absent there, the
# `customResponseHeaders:` line it goes under.
locate() {
  local found crh name canonical line
  found=$(extract_ingress "$1" 1)
  crh=$(awk -F '\t' '$2 == "CRH" { print $1; exit }' <<< "$found")
  # Each header named on a removed or added line, once, in diff order.
  { grep -E '^[-+][a-z]' || true; } | cut -c2- | cut -d: -f1 | awk '!seen[$0]++' \
    | while read -r name; do
        canonical=$(sed 's/^\(.\)/\U\1/; s/-\(.\)/-\U\1/g' <<< "$name")
        line=$(awk -F '\t' -v n="$name" '$2 == n { print $1 }' <<< "$found")
        if [ -n "$line" ]; then
          echo "$2:$line: $canonical"
        else
          echo "$2:${crh:-?}: $canonical (absent: add it under customResponseHeaders)"
        fi
      done
}

readable() {
  [ -f "$1" ] && [ -r "$1" ] || { echo "security-headers-drift: cannot read $1" >&2; exit 2; }
}

cmd=${1:-}
case "$cmd" in
  extract-caddy)
    [ $# = 2 ] || usage
    readable "$2"
    extract_caddy "$2"
    ;;
  extract-ingress)
    [ $# = 2 ] || usage
    readable "$2"
    extract_ingress "$2"
    ;;
  diff)
    [ $# = 3 ] || usage
    readable "$2"
    readable "$3"
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    extract_caddy "$2" > "$work/caddy"
    extract_ingress "$3" > "$work/ingress"
    for side in caddy ingress; do
      if [ ! -s "$work/$side" ]; then
        echo "security-headers-drift: none of the six security headers found in the $side file" >&2
        exit 2
      fi
    done
    # diff exits 1 on a difference, the status this command gives too.
    diff -u --label 'Caddyfile' --label 'ingress (deploy)' "$work/caddy" "$work/ingress"
    ;;
  where)
    [ $# = 3 ] || usage
    readable "$2"
    locate "$2" "$3"
    ;;
  comment)
    [ $# = 3 ] || usage
    readable "$2"
    drift=$(cat)
    locations=$(locate "$2" "$3" <<< "$drift" | sed 's/^\([^:]*:[0-9?]*\): \(.*\)$/- `\1` : \2/')
    cat <<EOF
$MARKER
### En-têtes de sécurité : écart avec l'ingress de \`deploy\`

Cette PR modifie \`infra/Caddyfile\`, et ses en-têtes de sécurité ne sont plus ceux de la Middleware \`security-headers\` de la branche \`deploy\` (\`-\` Caddyfile de la PR, \`+\` ingress de \`deploy\`) :

\`\`\`diff
$drift
\`\`\`

À changer sur \`deploy\` :

$locations

**Ouvrir la PR jumelle vers \`deploy\` et la merger juste après celle-ci.** Sans elle, la promotion suivante vers \`deploy\` échoue : \`ci/ingress-test.sh\` (job \`k3d\` de \`deploy.yml\`) compare les en-têtes de l'ingress à ceux du Caddyfile de \`main\`. Ce job n'échoue pas : il avertit seulement (#955).
EOF
    ;;
  *)
    usage
    ;;
esac
