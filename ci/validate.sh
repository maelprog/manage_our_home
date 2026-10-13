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
