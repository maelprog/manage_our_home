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
# Every image the prod overlay renders is checked, a container's
# (`containers`, `initContainers`, `ephemeralContainers`) or a volume's
# (`volumes[].image.reference`): the application's must be the pair's,
# MinIO's and its client's are the third parties', and any other fails.
# Postgres's image is not read: the CNPG Cluster's `imageName` is rendered
# by overlays/prod/postgres, a kustomization of its own.
# A third party's image, tag and digest, is held to the same rule as the
# pair, container by container: unchanged from what prod renders at the
# tip of <base>, or what staging renders there, or what prod rendered at
# an earlier commit of <base> (a revert). A patch of the prod overlay
# alone that moves MinIO to another digest of its repository fails, and
# so does one PR moving staging's and prod's together.
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
app_dir=apps/manage-our-home
prod=$app_dir/overlays/prod/kustomization.yaml
staging=$app_dir/overlays/staging/kustomization.yaml
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

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
# Every image of a render, whatever its kind (Pod, Deployment, Job,
# CronJob…), one per line: each container of the `containers`,
# `initContainers` and `ephemeralContainers` lists of each document, as
# `<kind>/<name>/<container> <image>`, and each volume of an image, as
# `<kind>/<name>/volume:<volume> <image>`.
images_of() {
  yq -N '
    .kind as $k | .metadata.name as $n
    | .. | select(tag == "!!map")
    | ((((.containers // []) + (.initContainers // []) + (.ephemeralContainers // []))[]
        | $k + "/" + $n + "/" + .name + " " + .image),
      ((.volumes // [])[] | select(has("image"))
        | $k + "/" + $n + "/volume:" + .name + " " + (.image.reference // "")))'
}
# images_of the render of <overlay> as <base> holds it at <commit>, kept
# in $work; fails if it does not render.
render_at() { # commit overlay
  local out="$work/render-$1-$2"
  if [ ! -e "$out" ]; then
    rm -rf "$work/tree"
    mkdir "$work/tree"
    git -C "$base" archive "$1" "$app_dir" | tar -x -C "$work/tree" \
      && kustomize build "$work/tree/$app_dir/overlays/$2" | images_of > "$out.tmp" \
      || return 1
    mv "$out.tmp" "$out"
  fi
  cat "$out"
}
image_in() { # id, in images_of on stdin
  awk -v id="$1" '$1 == id { print $2 }'
}
# The application's containers, with the image each must run; the third
# parties' (MinIO and its client, pinned by tag and digest by
# render-test.sh), with the repository theirs must come from, the image
# itself compared with staging's below. Any other image fails.
declare -A app=(
  [Deployment/api/api]=$api_ref
  [Job/migrate/migrate]=$api_ref
  [Deployment/web/web]=$web_ref
)
declare -A third=(
  [StatefulSet/minio/minio]='^docker\.io/bitnamilegacy/minio:'
  [Job/minio-bucket/bucket]='^docker\.io/bitnamilegacy/minio-client:'
)
if ! rendered=$(kustomize build "$(dirname "$root/$prod")" | images_of); then
  echo "FAIL: kustomize build of the prod overlay" >&2
  exit 1
fi
bad=0
declare -A seen=() head_third=()
while read -r id image; do
  [ -n "$id" ] || continue
  # Two containers of one name in one document (a container and an
  # initContainer, say): one image per id, or the third parties' check
  # below would see only the last.
  if [ -n "${seen[$id]+x}" ]; then
    echo "FAIL: prod renders $id more than once" >&2
    bad=1
  fi
  seen[$id]=1
  if [ -n "${app[$id]+x}" ]; then
    if [ "$image" != "${app[$id]}" ]; then
      echo "FAIL: prod renders $id with '$image', not '${app[$id]}' of its images:" >&2
      bad=1
    fi
  elif [ -n "${third[$id]+x}" ]; then
    head_third[$id]=$image
    if ! [[ "$image" =~ ${third[$id]} ]]; then
      echo "FAIL: prod renders $id with '$image', not from ${third[$id]}" >&2
      bad=1
    fi
  else
    echo "FAIL: prod renders an image this check does not know: $id ($image)" >&2
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

# The third parties, container by container: what prod renders at the
# tip of <base>, or what staging renders there, or what prod rendered at
# an earlier commit of <base>.
if ! base_prod=$(render_at HEAD prod) || ! base_staging_images=$(render_at HEAD staging); then
  echo "FAIL: kustomize build of the base branch's overlays" >&2
  exit 1
fi
for id in "${!head_third[@]}"; do
  image=${head_third[$id]}
  [ "$image" != "$(image_in "$id" <<< "$base_prod")" ] || continue
  staging_image=$(image_in "$id" <<< "$base_staging_images")
  if [ "$image" = "$staging_image" ]; then
    echo "prod: $id promoted to the image staging runs ($image)"
    continue
  fi
  reverted=""
  for c in $(git -C "$base" log --format=%H -- "$app_dir"); do
    if [ "$( (render_at "$c" prod 2> /dev/null || true) | image_in "$id")" = "$image" ]; then
      reverted=$c
      break
    fi
  done
  if [ -n "$reverted" ]; then
    echo "prod: $id back to the image it ran at $(git -C "$base" log -1 --format='%h %s' "$reverted")"
    continue
  fi
  {
    echo "FAIL: prod would run $id on an image that is neither the one staging runs nor one prod ran before."
    echo "  proposed for prod:  $image"
    echo "  staging runs:       ${staging_image:-(no such container)}"
    echo "Only what ran in staging is promoted: move staging to this image first."
  } >&2
  bad=1
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
