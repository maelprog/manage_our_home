#!/usr/bin/env bash
# The images of Argo CD's chart that are on neither quay.io nor ghcr.io,
# put into a throwaway k3d cluster's node before the bootstrap (#557).
# Today one: argocd-redis's, from ECR public, which answers a fresh
# cluster's anonymous pull from a shared GitHub runner address with
# 429 Too Many Requests often enough to make the k3d jobs red. The jobs
# keep the images in the Actions cache: ECR is only asked on a miss.
#
#   ci/preload-images.sh key                  cache key, as key=<...> for $GITHUB_OUTPUT
#   ci/preload-images.sh pull <tar>           docker pull, then docker save into <tar>
#   ci/preload-images.sh import <tar> <cluster>  into the node, then checked there
#   ci/preload-images.sh check                after the bootstrap: the kubelet found
#                                             them on the node, never pulled them
#   ci/preload-images.sh select < render      the images of a render (tests)
#   ci/preload-images.sh verdict <image> < events.json  (tests)
#
# The images are never written here: they come from the chart rendered at
# the version and with the values of platform/components/argocd/
# application.yaml, as platform/bootstrap.sh installs it. A bump that
# changes them changes the cache key. pull tries 3 times, waiting
# PRELOAD_RETRY_DELAY (20 s) then twice that, and fails naming the
# registry. Needs helm and yq (mikefarah, v4); pull needs docker, import
# k3d and docker, check kubectl.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
argocd="$root/platform/components/argocd/application.yaml"

usage() { sed -n '9,16p' "$0" >&2; exit 2; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The images of the workloads in a render, outside quay.io and ghcr.io,
# sorted and without repeats.
select_images() {
  local all
  all=$(yq e '
    select(.spec.template.spec != null) |
    (.spec.template.spec.initContainers[]?, .spec.template.spec.containers[]?) |
    .image
  ' -) || return 1
  # grep finding nothing is an answer: no image to preload.
  grep -v -e '^---$' -e '^quay\.io/' -e '^ghcr\.io/' <<< "$all" | sort -u || true
}

images() {
  local list
  # Each step checked: callers run this in a command substitution, and an
  # empty render must not read as "no image".
  yq e '.spec.source.helm.valuesObject' "$argocd" > "$work/values.yaml" || return 1
  helm template "$(yq e '.spec.source.helm.releaseName' "$argocd")" \
    "$(yq e '.spec.source.chart' "$argocd")" \
    --repo "$(yq e '.spec.source.repoURL' "$argocd")" \
    --version "$(yq e '.spec.source.targetRevision' "$argocd")" \
    --namespace "$(yq e '.spec.destination.namespace' "$argocd")" \
    --values "$work/values.yaml" > "$work/render.yaml" || {
    echo "::error::helm template of Argo CD's chart failed" >&2
    return 1
  }
  list=$(select_images < "$work/render.yaml") || return 1
  if [ -z "$list" ]; then
    echo "::error::Argo CD's chart renders no image outside quay.io and ghcr.io any more: drop ci/preload-images.sh and its steps in deploy.yml" >&2
    return 1
  fi
  printf '%s\n' "$list"
}

pull() { # tar
  local img list tries=3 n err
  list=$(images)
  mapfile -t imgs <<< "$list"
  for img in "${imgs[@]}"; do
    for n in $(seq 1 "$tries"); do
      echo "== pull $img (attempt $n of $tries)"
      if err=$(docker pull --quiet "$img" 2>&1); then
        echo "$err"
        break
      fi
      echo "$err" >&2
      if [ "$n" -eq "$tries" ]; then
        if grep -qi -e '429' -e 'toomanyrequests' <<< "$err"; then
          echo "::error::cache miss, and the pull of $img from ${img%%/*} was refused: 429 Too Many Requests, $tries times — the job failed on that pull, not on Argo CD" >&2
        else
          echo "::error::cache miss, and the pull of $img from ${img%%/*} failed $tries times — the job failed on that pull, not on Argo CD" >&2
        fi
        return 1
      fi
      sleep $((n * ${PRELOAD_RETRY_DELAY:-20}))
    done
  done
  docker save --output "$1" "${imgs[@]}"
  echo "== saved ${imgs[*]} into $1 ($(du -h "$1" | cut -f1))"
}

import() { # tar cluster
  local img list node="k3d-$2-server-0"
  [ -s "$1" ] || { echo "::error::$1 is missing or empty" >&2; return 1; }
  list=$(images)
  mapfile -t imgs <<< "$list"
  k3d image import "$1" --cluster "$2"
  for img in "${imgs[@]}"; do
    if ! docker exec "$node" crictl inspecti "$img" > /dev/null; then
      echo "::error::$img is not in $node after the import" >&2
      return 1
    fi
    echo "== $img is in $node"
  done
}

# From the namespace's events, as `kubectl get events -o json` prints
# them: ok if the kubelet found <image> already on the node at least
# once, and never pulled it. Prints the events it judged on.
verdict() { # image
  local events present pulled
  events=$(IMG="\"$1\"" yq -p json -o json e '
    [.items[] | select(.message | contains(strenv(IMG))) |
      {"pod": .involvedObject.name, "reason": .reason, "message": .message}]
  ' -)
  yq -p json e '.[] | .pod + ": " + .reason + " — " + .message' <<< "$events"
  present=$(yq -p json e '[.[] | select(.reason == "Pulled" and (.message | contains("already present")))] | length' <<< "$events")
  pulled=$(yq -p json e '[.[] | select(.reason == "Pulling" or (.reason == "Pulled" and (.message | contains("already present") | not)))] | length' <<< "$events")
  if [ "$pulled" -ne 0 ]; then
    echo "::error::$1 was pulled from its registry in the cluster ($pulled events): the imported copy was not used" >&2
    return 1
  fi
  if [ "$present" -eq 0 ]; then
    echo "::error::no event shows $1 found on the node: no container started from it" >&2
    return 1
  fi
  echo "== $1: found on the node by $present start(s), pulled by none"
}

check() {
  local img list namespace events rc=0
  namespace=$(yq e '.spec.destination.namespace' "$argocd")
  events=$(kubectl get events --namespace "$namespace" -o json)
  list=$(images)
  mapfile -t imgs <<< "$list"
  for img in "${imgs[@]}"; do
    verdict "$img" <<< "$events" || rc=1
  done
  return "$rc"
}

[ $# -ge 1 ] || usage
cmd=$1
shift
case "$cmd" in
  key)
    [ $# -eq 0 ] || usage
    list=$(images)
    echo "== images to preload:" >&2
    echo "$list" >&2
    echo "key=argocd-images-$(tr '\n' ' ' <<< "$list" | sed 's/ $//')"
    ;;
  pull) [ $# -eq 1 ] || usage; pull "$1" ;;
  import) [ $# -eq 2 ] || usage; import "$1" "$2" ;;
  check) [ $# -eq 0 ] || usage; check ;;
  select) [ $# -eq 0 ] || usage; select_images ;;
  verdict) [ $# -eq 1 ] || usage; verdict "$1" ;;
  *) usage ;;
esac
