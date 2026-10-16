#!/usr/bin/env bash
# Only what ran in staging goes to prod (#436). Compares the application's
# images of prod in this tree (the PR) with the branch the PR goes into,
# checked out with its history at <base>:
#
#   - prod's pair unchanged: nothing to check;
#   - prod moved to the pair staging runs at the tip of <base>: a
#     promotion, passes;
#   - prod moved back to a pair it ran at an earlier commit of <base>: the
#     `git revert` of a promotion (README, « Revenir en arrière »), passes;
#   - anything else fails, a pair staging ran once but no longer runs
#     included: it is not what is running there now.
#
# The pair is compared whole, api and web together: they were built from
# one commit, and staging ran them together.
#
#   ci/promotion-check.sh <base>
#
# Needs git and yq (mikefarah, v4) on PATH.

set -euo pipefail

if [ $# -ne 1 ]; then
  echo "usage: $0 <checkout of the base branch, with its history>" >&2
  exit 2
fi
base=$1
root=$(cd "$(dirname "$0")/.." && pwd)
prod=apps/manage-our-home/overlays/prod/kustomization.yaml
staging=apps/manage-our-home/overlays/staging/kustomization.yaml

# `<name>@<digest>` of each application image, sorted, on one line.
pair() {
  yq '[.images[] | select(.name | test("^ghcr\.io/maelprog/manage-our-home-")) | .name + "@" + .digest] | sort | join(" ")' "$1"
}
pair_at() { # commit path
  git -C "$base" show "$1:$2" | pair -
}

if ! git -C "$base" cat-file -e "HEAD:$prod" 2> /dev/null; then
  echo "the base branch has no prod overlay yet: nothing to compare with"
  exit 0
fi

head_prod=$(pair "$root/$prod")
if [ -z "$head_prod" ]; then
  echo "FAIL: no application image in $prod" >&2
  exit 1
fi

if [ "$head_prod" = "$(pair_at HEAD "$prod")" ]; then
  echo "prod: images unchanged"
  exit 0
fi

base_staging=$(pair_at HEAD "$staging")
if [ "$head_prod" = "$base_staging" ]; then
  echo "prod: promotion of the pair staging runs ($head_prod)"
  exit 0
fi

for c in $(git -C "$base" log --format=%H -- "$prod"); do
  if [ "$(pair_at "$c" "$prod")" = "$head_prod" ]; then
    echo "prod: back to the pair it ran at $(git -C "$base" log -1 --format='%h %s' "$c")"
    exit 0
  fi
done

{
  echo "FAIL: prod would run a pair that is neither the one staging runs nor one prod ran before."
  echo "  proposed for prod:  $head_prod"
  echo "  staging runs:       $base_staging"
  echo "Only what ran in staging is promoted: move staging to this pair first, or"
  echo "promote staging's pair (ci/set-images.sh prod <source> <api digest> <web digest>)."
} >&2
exit 1
