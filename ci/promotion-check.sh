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
# one commit, and staging ran them together. Before any of that, the pair
# of the file must be what prod renders: no newName, newTag or patch that
# moves an application image behind it.
#
#   ci/promotion-check.sh <base>
#
# Needs git, kustomize and yq (mikefarah, v4) on PATH.

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

# The pair of the file is what prod runs only if nothing else moves an
# application image: no key but name and digest on its entries (a newName
# or a newTag would replace them), and, container by container, what
# kustomize renders is what that container must run — a patch of an
# image shows there, even onto an image the render holds elsewhere.
extra=$(yq '[.images[] | select(.name | test("^ghcr\.io/maelprog/manage-our-home-")) | keys | .[] | select(. != "name" and . != "digest")] | unique | join(" ")' "$root/$prod")
if [ -n "$extra" ]; then
  echo "FAIL: $prod: the application's images may carry name and digest only, not: $extra" >&2
  exit 1
fi
ref_of() { # image name -> name@digest in the prod overlay
  N="$1" yq '.images[] | select(.name == env(N)) | .name + "@" + .digest' "$root/$prod"
}
api_ref=$(ref_of ghcr.io/maelprog/manage-our-home-api)
web_ref=$(ref_of ghcr.io/maelprog/manage-our-home-web)
# Every container of the render, `<kind>/<workload>/<container>`: the
# application's, with the image each must run; the third parties' (MinIO
# and its client, pinned by tag and digest by render-test.sh), with the
# repository theirs must come from. Any other container fails.
declare -A app=(
  [Deployment/api/api]=$api_ref
  [Job/migrate/migrate]=$api_ref
  [Deployment/web/web]=$web_ref
)
declare -A third=(
  [StatefulSet/minio/minio]='^docker\.io/bitnamilegacy/minio:'
  [Job/minio-bucket/bucket]='^docker\.io/bitnamilegacy/minio-client:'
)
if ! rendered=$(kustomize build "$(dirname "$root/$prod")" | yq -N '
    (.spec.template.spec // .spec.jobTemplate.spec.template.spec) as $pod
    | select($pod != null)
    | .kind as $k | .metadata.name as $n
    | (($pod.containers // []) + ($pod.initContainers // []))[]
    | $k + "/" + $n + "/" + .name + " " + .image'); then
  echo "FAIL: kustomize build of the prod overlay" >&2
  exit 1
fi
bad=0
declare -A seen=()
while read -r id image; do
  [ -n "$id" ] || continue
  seen[$id]=1
  if [ -n "${app[$id]+x}" ]; then
    if [ "$image" != "${app[$id]}" ]; then
      echo "FAIL: prod renders $id with '$image', not '${app[$id]}' of its images:" >&2
      bad=1
    fi
  elif [ -n "${third[$id]+x}" ]; then
    if ! [[ "$image" =~ ${third[$id]} ]]; then
      echo "FAIL: prod renders $id with '$image', not from ${third[$id]}" >&2
      bad=1
    fi
  else
    echo "FAIL: prod renders a container this check does not know: $id ($image)" >&2
    bad=1
  fi
done <<< "$rendered"
for id in "${!app[@]}"; do
  if [ -z "${seen[$id]+x}" ]; then
    echo "FAIL: prod no longer renders $id" >&2
    bad=1
  fi
done
[ "$bad" -eq 0 ] || exit 1

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
