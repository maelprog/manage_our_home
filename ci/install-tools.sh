#!/usr/bin/env bash
# Installs the tools the CI of this branch runs, at pinned versions, each
# checked against a SHA-256 written here — a release asset replaced
# upstream fails the job instead of running. linux/amd64 only.
#
#   ci/install-tools.sh <directory> <tool>...
#   tools: kustomize kubeconform yq kubectl k3d helm kubeseal cosign promtool amtool
#
# To move a version: change it and its checksum together, the checksum
# taken from the project's own release checksums file.

set -euo pipefail

dest=$1
shift
mkdir -p "$dest"

KUSTOMIZE_VERSION=5.8.3
KUSTOMIZE_SHA256=cb9e31198d3f63b44848bd0afc4c8efc56e6cfef9c93a4a39a211940e6decd61
KUBECONFORM_VERSION=0.8.0
KUBECONFORM_SHA256=9bc2bffbf71f261128533edaf912153948b7ff238f9a531ae6d34466ec287883
YQ_VERSION=4.54.1
YQ_SHA256=8e34fc298390875de416e6a4afcb8cabeceb25d9aa8506c1a2f9353cf702ea5f
# Same version as the k3s image of the k3d job (K3S_IMAGE in
# .github/workflows/deploy.yml).
KUBECTL_VERSION=1.37.1
KUBECTL_SHA256=65691ff77eb6fa44c908b77a1082c9f092c3b9733b5cefabec0d1104890e21a8
K3D_VERSION=5.9.0
K3D_SHA256=06d8f25bc3a971c4eb29e0ff08429b180402db0f4dec838c9eac427e296800a0
# Argo CD's first install (platform/bootstrap.sh, #432); after it, Argo CD
# renders the charts itself.
HELM_VERSION=4.3.0
HELM_SHA256=86584a54def73570558f66f5111cc53dfed56689637ae32c1201205d494f54fb
# Same version as the controller of
# platform/components/sealed-secrets/application.yaml (chart 2.20.0).
KUBESEAL_VERSION=0.40.0
KUBESEAL_SHA256=9314c35916646e9d59c8f06b1314574b4e79c4d76f079433607ed7b697bb5eb7
# v3: the signatures of images.yml on main are in the Sigstore bundle
# format, which cosign v2 does not find.
COSIGN_VERSION=3.1.3
COSIGN_SHA256=4629c757b7618056f8ddd7e2625ae9fdd94c0372a65049520bc7d9df9efc7f71
# promtool, for the alerts' tests (ci/alerts-test.sh, #437): the version of
# the Prometheus that kube-prometheus-stack 92.9.0 runs
# (platform/components/observability/kube-prometheus-stack.yaml). The
# checksum is that of the release tarball, from its sha256sums.txt.
PROMETHEUS_VERSION=3.15.0
PROMETHEUS_SHA256=2a542df32eac02ee17b9d844fb2aa1de00dafa5476579ba8a3ba862e9d572ea0
# amtool, for Alertmanager's configuration (ci/alerts-test.sh): the
# version of the Alertmanager the same chart runs.
ALERTMANAGER_VERSION=0.34.1
ALERTMANAGER_SHA256=265b9d1e55ef0d5306a436018af6d2b686c2ce051f03d968f7464ecb1372a7e8

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

fetch() { # url sha256 file
  curl --fail --silent --show-error --location --retry 3 --output "$tmp/$3" "$1"
  echo "$2  $tmp/$3" | sha256sum --check --quiet
}

for tool in "$@"; do
  case "$tool" in
    kustomize)
      fetch "https://github.com/kubernetes-sigs/kustomize/releases/download/kustomize%2Fv${KUSTOMIZE_VERSION}/kustomize_v${KUSTOMIZE_VERSION}_linux_amd64.tar.gz" \
        "$KUSTOMIZE_SHA256" kustomize.tar.gz
      tar -xzf "$tmp/kustomize.tar.gz" -C "$tmp" kustomize
      install -m 0755 "$tmp/kustomize" "$dest/kustomize"
      ;;
    kubeconform)
      fetch "https://github.com/yannh/kubeconform/releases/download/v${KUBECONFORM_VERSION}/kubeconform-linux-amd64.tar.gz" \
        "$KUBECONFORM_SHA256" kubeconform.tar.gz
      tar -xzf "$tmp/kubeconform.tar.gz" -C "$tmp" kubeconform
      install -m 0755 "$tmp/kubeconform" "$dest/kubeconform"
      ;;
    yq)
      fetch "https://github.com/mikefarah/yq/releases/download/v${YQ_VERSION}/yq_linux_amd64" "$YQ_SHA256" yq
      install -m 0755 "$tmp/yq" "$dest/yq"
      ;;
    kubectl)
      fetch "https://dl.k8s.io/release/v${KUBECTL_VERSION}/bin/linux/amd64/kubectl" "$KUBECTL_SHA256" kubectl
      install -m 0755 "$tmp/kubectl" "$dest/kubectl"
      ;;
    k3d)
      fetch "https://github.com/k3d-io/k3d/releases/download/v${K3D_VERSION}/k3d-linux-amd64" "$K3D_SHA256" k3d
      install -m 0755 "$tmp/k3d" "$dest/k3d"
      ;;
    helm)
      fetch "https://get.helm.sh/helm-v${HELM_VERSION}-linux-amd64.tar.gz" "$HELM_SHA256" helm.tar.gz
      tar -xzf "$tmp/helm.tar.gz" -C "$tmp" linux-amd64/helm
      install -m 0755 "$tmp/linux-amd64/helm" "$dest/helm"
      ;;
    kubeseal)
      fetch "https://github.com/bitnami-labs/sealed-secrets/releases/download/v${KUBESEAL_VERSION}/kubeseal-${KUBESEAL_VERSION}-linux-amd64.tar.gz" \
        "$KUBESEAL_SHA256" kubeseal.tar.gz
      tar -xzf "$tmp/kubeseal.tar.gz" -C "$tmp" kubeseal
      install -m 0755 "$tmp/kubeseal" "$dest/kubeseal"
      ;;
    cosign)
      fetch "https://github.com/sigstore/cosign/releases/download/v${COSIGN_VERSION}/cosign-linux-amd64" "$COSIGN_SHA256" cosign
      install -m 0755 "$tmp/cosign" "$dest/cosign"
      ;;
    promtool)
      fetch "https://github.com/prometheus/prometheus/releases/download/v${PROMETHEUS_VERSION}/prometheus-${PROMETHEUS_VERSION}.linux-amd64.tar.gz" \
        "$PROMETHEUS_SHA256" prometheus.tar.gz
      tar -xzf "$tmp/prometheus.tar.gz" -C "$tmp" "prometheus-${PROMETHEUS_VERSION}.linux-amd64/promtool"
      install -m 0755 "$tmp/prometheus-${PROMETHEUS_VERSION}.linux-amd64/promtool" "$dest/promtool"
      ;;
    amtool)
      fetch "https://github.com/prometheus/alertmanager/releases/download/v${ALERTMANAGER_VERSION}/alertmanager-${ALERTMANAGER_VERSION}.linux-amd64.tar.gz" \
        "$ALERTMANAGER_SHA256" alertmanager.tar.gz
      tar -xzf "$tmp/alertmanager.tar.gz" -C "$tmp" "alertmanager-${ALERTMANAGER_VERSION}.linux-amd64/amtool"
      install -m 0755 "$tmp/alertmanager-${ALERTMANAGER_VERSION}.linux-amd64/amtool" "$dest/amtool"
      ;;
    *)
      echo "unknown tool: $tool" >&2
      exit 1
      ;;
  esac
  echo "installed $tool"
done
