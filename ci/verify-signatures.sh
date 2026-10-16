#!/usr/bin/env bash
# Each application image an overlay names carries the signature Kyverno
# will ask for (#436): checked here, before a promotion PR is merged,
# rather than by an admission refusal once Argo CD syncs it. The issuer
# and the identity are read from the Kyverno policy itself
# (platform/components/kyverno/signed-images.yaml), so the two cannot
# drift apart.
#
#   ci/verify-signatures.sh
#
# Needs yq (mikefarah, v4) and cosign v3 on PATH (ci/install-tools.sh), and
# network access to GHCR and Rekor.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
policy="$root/platform/components/kyverno/signed-images.yaml"
keyless='.spec.rules[0].verifyImages[0].attestors[0].entries[0].keyless'
issuer=$(yq "$keyless.issuer" "$policy")
subject=$(yq "$keyless.subjectRegExp" "$policy")
if [ -z "$issuer" ] || [ "$issuer" = null ] || [ -z "$subject" ] || [ "$subject" = null ]; then
  echo "FAIL: no keyless issuer and subject in $policy" >&2
  exit 1
fi

refs=$(for overlay in "$root"/apps/manage-our-home/overlays/*/kustomization.yaml; do
  yq '.images[] | select(.name | test("^ghcr\.io/maelprog/manage-our-home-")) | .name + "@" + .digest' "$overlay"
done | sort -u)

failures=0
for ref in $refs; do
  if cosign verify "$ref" \
    --certificate-oidc-issuer "$issuer" \
    --certificate-identity-regexp "$subject" > /dev/null; then
    echo "signed: $ref"
  else
    echo "FAIL: $ref has no signature from $subject" >&2
    failures=$((failures + 1))
  fi
done
[ "$failures" -eq 0 ]
