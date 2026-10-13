#!/usr/bin/env bash
# Every overlay renders, and what it renders is valid Kubernetes for the
# cluster's version, unknown fields included (`-strict`): a misspelt key is
# an error here instead of a field the API server drops without a word.
# Needs `kustomize` and `kubeconform` on PATH (ci/install-tools.sh), and
# network access for kubeconform's schemas.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
# The Kubernetes version of the k3s image ci/install-tools.sh pins for the
# k3d job, which is the version the clusters are installed with (#438).
k8s_version=${K8S_VERSION:-1.37.1}

for overlay in "$root"/apps/manage-our-home/overlays/*/; do
  name=$(basename "$overlay")
  echo "== $name"
  kustomize build "$overlay" | kubeconform -strict -summary -kubernetes-version "$k8s_version"
done

# The platform's render (#432): Argo CD Applications, cert-manager
# ClusterIssuers, the Kyverno policy, beside core kinds. Their schemas come
# from the CRDs catalog (datreeio/CRDs-catalog), at a commit pinned here
# so a schema does not change under the CI; a kind it lacks fails rather
# than passing unchecked.
crds_commit=63669a570e231d4f1f8396d229a1de512bcf0a34
crds="https://raw.githubusercontent.com/datreeio/CRDs-catalog/$crds_commit/{{.Group}}/{{.ResourceKind}}_{{.ResourceAPIVersion}}.json"
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
