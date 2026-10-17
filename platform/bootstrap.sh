#!/usr/bin/env bash
# Bootstraps a cluster (#432): installs Argo CD once, with Helm, at the
# chart version and with the values of components/argocd/application.yaml,
# then applies the cluster's root Application. From there Argo CD installs
# the rest of platform/clusters/<env> — itself included: its own
# Application takes over the Helm release this script made.
#
#   platform/bootstrap.sh <staging|prod> [<root Application file>]
#
# The environment chooses the root Application, so which bricks the
# cluster runs and which branch it follows. The second argument replaces
# clusters/<env>/root.yaml; the CI uses it to point the cluster at the
# commit under test.
#
# Run once per cluster, against the current kube context, by the user:
# Argo CD's credentials are never given to the CI (arbitrage of
# 2026-10-10). Needs helm, kubectl and yq (mikefarah, v4); the CI installs
# them with ci/install-tools.sh. Running it again is harmless.

set -euo pipefail

usage() { echo "usage: $0 <staging|prod> [<root Application file>]" >&2; exit 2; }
[ $# -ge 1 ] && [ $# -le 2 ] || usage
env=$1
here=$(cd "$(dirname "$0")" && pwd)
case "$env" in
  staging | prod) ;;
  *) usage ;;
esac
root_app=${2:-$here/clusters/$env/root.yaml}

argocd="$here/components/argocd/application.yaml"
repo=$(yq e '.spec.source.repoURL' "$argocd")
chart=$(yq e '.spec.source.chart' "$argocd")
version=$(yq e '.spec.source.targetRevision' "$argocd")
release=$(yq e '.spec.source.helm.releaseName' "$argocd")
namespace=$(yq e '.spec.destination.namespace' "$argocd")

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
yq e '.spec.source.helm.valuesObject' "$argocd" > "$work/values.yaml"

echo "== Argo CD: chart $chart $version from $repo"
helm upgrade --install "$release" "$chart" \
  --repo "$repo" --version "$version" \
  --namespace "$namespace" --create-namespace \
  --values "$work/values.yaml" \
  --wait --timeout 10m

echo "== root Application ($root_app)"
kubectl apply -f "$root_app"
