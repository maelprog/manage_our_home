#!/usr/bin/env bash
# Moves the application's images of one overlay to a pair of digests
# (#436): the edit a promotion PR makes, and nothing else — the two
# `digest:` lines change, each followed by the source of the pair as a
# comment; the rest of the file is left as it is. Running it again with
# the same pair changes nothing. Needs yq (mikefarah, v4) on PATH.
#
#   ci/set-images.sh <staging|prod> <source> <api digest> <web digest>
#
# <source> says where the pair comes from, e.g. `main 10aabde` or `v1.4.0`.

set -euo pipefail

if [ $# -ne 4 ]; then
  echo "usage: $0 <staging|prod> <source> <api digest> <web digest>" >&2
  exit 2
fi
overlay=$1 source=$2 api=$3 web=$4

root=$(cd "$(dirname "$0")/.." && pwd)
file="$root/apps/manage-our-home/overlays/$overlay/kustomization.yaml"

case "$overlay" in
  staging | prod) ;;
  *) echo "unknown overlay '$overlay' (staging or prod)" >&2; exit 2 ;;
esac
if [ -z "$source" ] || [[ "$source" == *$'\n'* ]]; then
  echo "the source must be one non-empty line" >&2
  exit 2
fi
for d in "$api" "$web"; do
  if ! [[ "$d" =~ ^sha256:[0-9a-f]{64}$ ]]; then
    echo "'$d' is not a digest (sha256: and 64 lowercase hex digits)" >&2
    exit 2
  fi
done

for pair in "ghcr.io/maelprog/manage-our-home-api=$api" "ghcr.io/maelprog/manage-our-home-web=$web"; do
  name=${pair%%=*}
  n=$(N="$name" yq '[.images[] | select(.name == env(N))] | length' "$file")
  if [ "$n" != 1 ]; then
    echo "$file: expected one image named $name, found $n" >&2
    exit 1
  fi
done

# By line, not through `yq -i`: yq drops the blank lines between the
# sections. The `digest:` line right under each `- name:` is rewritten;
# yq then checks the result says what was asked.
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
API="$api" WEB="$web" SOURCE="$source" awk '
  BEGIN {
    want["ghcr.io/maelprog/manage-our-home-api"] = ENVIRON["API"]
    want["ghcr.io/maelprog/manage-our-home-web"] = ENVIRON["WEB"]
  }
  pending != "" && match($0, /^[ ]+digest:/) {
    print substr($0, 1, RLENGTH) " " want[pending] " # " ENVIRON["SOURCE"]
    pending = ""
    next
  }
  { pending = "" }
  match($0, /^[ ]+- name: /) {
    name = substr($0, RLENGTH + 1)
    if (name in want) pending = name
  }
  { print }
' "$file" > "$tmp"

for pair in "ghcr.io/maelprog/manage-our-home-api=$api" "ghcr.io/maelprog/manage-our-home-web=$web"; do
  got=$(N="${pair%%=*}" yq '.images[] | select(.name == env(N)) | .digest' "$tmp")
  if [ "$got" != "${pair#*=}" ]; then
    echo "$file: could not set the digest of ${pair%%=*} (its digest: line must follow its name: line)" >&2
    exit 1
  fi
done
cat "$tmp" > "$file"

echo "$overlay: api $api, web $web ($source)"
