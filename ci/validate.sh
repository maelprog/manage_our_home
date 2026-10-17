#!/usr/bin/env bash
# Every overlay renders, and what it renders is valid Kubernetes for the
# cluster's version, unknown fields included (`-strict`): a misspelt key is
# an error here instead of a field the API server drops without a word.
# Needs `kustomize` and `kubeconform` on PATH (ci/install-tools.sh), and
# network access for kubeconform's schemas.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
# The Kubernetes version of the k3s image the k3d job runs (K3S_IMAGE in
# .github/workflows/deploy.yml), which is the version the clusters are
# installed with (#438).
k8s_version=${K8S_VERSION:-1.37.1}

# The custom kinds — Argo CD Applications, cert-manager ClusterIssuers, the
# Kyverno policy (#432), the CloudNativePG Cluster and ScheduledBackup
# (#433), Traefik's IngressRoutes and Middlewares, the Certificate and k3s's
# HelmChartConfig (#434) — are checked against the CRDs catalog
# (datreeio/CRDs-catalog), at a commit pinned here so a schema does not
# change under the CI; a kind it lacks fails rather than passing unchecked.
crds_commit=63669a570e231d4f1f8396d229a1de512bcf0a34
crds="https://raw.githubusercontent.com/datreeio/CRDs-catalog/$crds_commit/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json"

for overlay in "$root"/apps/manage-our-home/overlays/*/; do
  name=$(basename "$overlay")
  echo "== $name"
  kustomize build "$overlay" | kubeconform -strict -summary -kubernetes-version "$k8s_version" \
    -schema-location default -schema-location "$crds"
done

# Each environment's Postgres (#433), synced by an Application of its own.
for overlay in "$root"/apps/manage-our-home/overlays/*/postgres/; do
  name=$(basename "$(dirname "$overlay")")
  echo "== $name/postgres"
  kustomize build "$overlay" | kubeconform -strict -summary -kubernetes-version "$k8s_version" \
    -schema-location default -schema-location "$crds"
done

# The platform's render (#432), the same way.
for cluster in "$root"/platform/clusters/*/; do
  name=$(basename "$cluster")
  echo "== platform/$name"
  {
    kustomize build "$cluster"
    echo "---"
    cat "$cluster/root.yaml"
  } | kubeconform -strict -summary -kubernetes-version "$k8s_version" \
    -schema-location default -schema-location "$crds"
done
