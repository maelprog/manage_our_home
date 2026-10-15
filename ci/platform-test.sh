#!/usr/bin/env bash
# Tests on what platform/clusters/<env> renders (#432) — the root
# Application's view of each cluster — and on the charts its Applications
# install, rendered by Helm with their values. Needs kustomize, helm and
# yq (mikefarah, v4) on PATH, and network access for the charts; the CI
# installs the tools with ci/install-tools.sh.
#
#   ci/platform-test.sh
#
# EXPECT_STAGING_REVISION, when set, is the branch every Application of
# staging must read: the CI sets it to the branch a PR goes into, so the
# promotion PR to `deploy` fails while staging still follows `k3s`.
#
# Exits non-zero if any check fails, after running all of them.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
platform="$root/platform"
REPO=https://github.com/maelprog/manage_our_home.git
# The Kubernetes version of the clusters (ci/validate.sh).
k8s_version=${K8S_VERSION:-1.37.1}

failures=0
checks=0
ctx=""
fail() { echo "FAIL [$ctx] $*" >&2; failures=$((failures + 1)); }
check() { checks=$((checks + 1)); }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for env in staging prod; do
  ctx=$env
  out="$work/$env.yaml"
  rootapp="$platform/clusters/$env/root.yaml"
  check
  if ! kustomize build "$platform/clusters/$env" > "$out" 2> "$work/err"; then
    fail "kustomize build: $(cat "$work/err")"
    continue
  fi
  y() { yq e -N "$1" "$out" | sed "/^$/d"; }
  ya() { yq ea "$1" "$out"; }
  app="manage-our-home-$env"
  appsel="select(.kind == \"Application\" and .metadata.name == \"$app\")"
  pgapp="postgres-$env"
  pgsel="select(.kind == \"Application\" and .metadata.name == \"$pgapp\")"

  # --- The bricks, the application's Application and its Postgres's (#433).
  check
  v=$(y 'select(.kind == "Application") | .metadata.name' | sort | tr '\n' ' ')
  want="argocd cert-manager cloudnative-pg kyverno $app $pgapp sealed-secrets "
  [ "$v" = "$want" ] || fail "Applications: expected '$want', got '$v'"

  # --- No Secret in clear; whatever the secrets directory holds is sealed.
  check
  n=$(ya '[select(.kind == "Secret")] | length')
  [ "$n" = 0 ] || fail "$n Secret(s) in the render: only SealedSecrets are committed"
  check
  v=$(kustomize build "$platform/clusters/$env/secrets" | yq e -N 'select(. != null) | .kind' - | sed '/^$/d' | sort -u | tr '\n' ' ')
  [ -z "$v" ] || [ "$v" = "SealedSecret " ] || fail "secrets/ renders '$v', expected SealedSecret only"

  # --- Synchronisation: production is never synced without the user —
  # neither the application, its Postgres nor the root (arbitrage of
  # 2026-10-10); staging is, all three.
  check
  auto=$(y "$appsel | .spec.syncPolicy.automated // \"none\"" | head -n 1)
  pauto=$(y "$pgsel | .spec.syncPolicy.automated // \"none\"" | head -n 1)
  rauto=$(yq e '.spec.syncPolicy.automated // "none"' "$rootapp" | head -n 1)
  case "$env" in
    prod)
      [ "$auto" = none ] || fail "$app has syncPolicy.automated: production syncs only on the user's Sync"
      check
      [ "$pauto" = none ] || fail "$pgapp has syncPolicy.automated: production syncs only on the user's Sync"
      check
      [ "$rauto" = none ] || fail "root Application has syncPolicy.automated: production syncs only on the user's Sync"
      ;;
    *)
      [ "$auto" != none ] || fail "$app has no syncPolicy.automated"
      check
      [ "$pauto" != none ] || fail "$pgapp has no syncPolicy.automated"
      check
      [ "$rauto" != none ] || fail "root Application has no syncPolicy.automated"
      ;;
  esac

  # --- Which commit each cluster reads: one branch for the root, the
  # application and its Postgres, `deploy` in production.
  check
  revs=$( (y "select(.kind == \"Application\" and .spec.source.repoURL == \"$REPO\") | .spec.source.targetRevision"
    yq e '.spec.source.targetRevision' "$rootapp") | sort -u | tr '\n' ' ')
  n=$( (y "select(.kind == \"Application\" and .spec.source.repoURL == \"$REPO\") | .metadata.name"
    yq e '.metadata.name' "$rootapp") | wc -l)
  [ "$n" = 3 ] || fail "$n Application(s) read $REPO, expected 3 (root, $app and $pgapp)"
  case "$env" in
    prod) want=deploy ;;
    *) want=${EXPECT_STAGING_REVISION:-${revs%% *}} ;;
  esac
  [ "$revs" = "$want " ] || fail "the Applications reading $REPO follow '$revs', expected '$want'"
  check
  v=$(yq e '[.spec.source.repoURL, .spec.source.path] | join(" ")' "$rootapp")
  [ "$v" = "$REPO platform/clusters/$env" ] || fail "root Application renders '$v'"
  check
  v=$(y "$appsel | [.spec.source.path, .spec.destination.namespace] | join(\" \")")
  [ "$v" = "apps/manage-our-home/overlays/$env manage-our-home" ] || fail "$app renders '$v'"
  check
  v=$(y "$pgsel | [.spec.source.path, .spec.destination.namespace] | join(\" \")")
  [ "$v" = "apps/manage-our-home/overlays/$env/postgres manage-our-home" ] || fail "$pgapp renders '$v'"

  # --- The namespace, its Pod Security level, and the order of the first
  # sync: the namespace and the sealed Secrets exist before the
  # application's PreSync migration Job runs; the platform before both;
  # Postgres after the Secrets (its roles' passwords) and before the
  # application (whose PreSync Job migrates it).
  check
  v=$(y 'select(.kind == "Namespace" and .metadata.name == "manage-our-home") | .metadata.labels["pod-security.kubernetes.io/enforce"]')
  [ "$v" = restricted ] || fail "namespace manage-our-home: Pod Security enforce is '$v', expected 'restricted'"
  check
  v=$(kustomize build "$root/apps/manage-our-home/overlays/$env" | yq e -N 'select(.kind == "Namespace") | .metadata.name' - | sed '/^$/d')
  [ -z "$v" ] || fail "the $env overlay renders namespace '$v': it is the platform's"
  wave() { y "$1 | .metadata.annotations[\"argocd.argoproj.io/sync-wave\"] // \"0\"" | head -n 1; }
  w_bricks=$(y 'select(.kind == "Application" and .spec.source.chart != null) | .metadata.annotations["argocd.argoproj.io/sync-wave"] // "0"' | sort -n | tail -n 1)
  w_ns=$(wave 'select(.kind == "Namespace" and .metadata.name == "manage-our-home")')
  w_policy=$(wave 'select(.kind == "ClusterPolicy")')
  w_secrets=$(yq e '.commonAnnotations["argocd.argoproj.io/sync-wave"]' "$platform/clusters/$env/secrets/kustomization.yaml")
  w_pg=$(wave "$pgsel")
  w_app=$(wave "$appsel")
  check
  [ "$w_bricks" -lt "$w_ns" ] && [ "$w_bricks" -lt "$w_policy" ] && [ "$w_ns" -lt "$w_secrets" ] && [ "$w_policy" -lt "$w_app" ] && [ "$w_secrets" -lt "$w_pg" ] && [ "$w_pg" -lt "$w_app" ] \
    || fail "sync waves: bricks $w_bricks < namespace $w_ns, policy $w_policy < secrets $w_secrets < postgres $w_pg < application $w_app does not hold"

  # --- Charts pinned to a version, never a range or a branch.
  for a in $(y 'select(.kind == "Application" and .spec.source.chart != null) | .metadata.name'); do
    check
    v=$(y "select(.kind == \"Application\" and .metadata.name == \"$a\") | .spec.source.targetRevision")
    [[ "$v" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "$a: chart version '$v' is not pinned"
  done

  # --- The Kyverno policy (#432): enforced, on the application's
  # namespace, for its images, by digest, signed by images.yml of main.
  pol='select(.kind == "ClusterPolicy" and .metadata.name == "manage-our-home-signed-images")'
  vi="$pol | .spec.rules[0].verifyImages[0]"
  check
  v=$(y "$pol | [.spec.failurePolicy, .spec.background, (.spec.rules | length), (.spec.rules[0].match.any[0].resources.namespaces | join(\",\")), (.spec.rules[0].match.any[0].resources.kinds | join(\",\"))] | join(\" \")")
  [ "$v" = "Fail false 1 manage-our-home Pod" ] || fail "ClusterPolicy: '$v', expected 'Fail false 1 manage-our-home Pod'"
  # Nothing narrows the rule: no exclude, precondition or selector, which
  # would let some pods of the namespace through unchecked.
  check
  v=$(y "$pol | .spec.rules[0] | [(keys | sort | join(\",\")), (.match | keys | sort | join(\",\")), (.match.any | length), (.match.any[0] | keys | sort | join(\",\")), (.match.any[0].resources | keys | sort | join(\",\"))] | join(\" \")")
  [ "$v" = "match,name,verifyImages any 1 resources kinds,namespaces" ] \
    || fail "ClusterPolicy rule is narrowed: '$v', expected 'match,name,verifyImages any 1 resources kinds,namespaces'"
  check
  v=$(y "$vi | [.type, .failureAction, .required, .verifyDigest, .mutateDigest, (.imageReferences | join(\",\"))] | join(\" \")")
  [ "$v" = "SigstoreBundle Enforce true true false ghcr.io/maelprog/manage-our-home-*" ] \
    || fail "ClusterPolicy verifyImages: '$v'"
  check
  v=$(y "$vi | .attestors[0].entries[0].keyless | [.issuer, .subjectRegExp, .rekor.url] | join(\" \")")
  [ "$v" = 'https://token.actions.githubusercontent.com ^https://github\.com/maelprog/manage_our_home/\.github/workflows/image-build\.yml@refs/(heads/main|tags/v[^/]+)$ https://rekor.sigstore.dev' ] \
    || fail "ClusterPolicy keyless attestor: '$v'"
  # Every image the overlay runs is one the policy checks: a rename of the
  # images or of the owner would leave them unchecked without a word.
  for image in $(kustomize build "$root/apps/manage-our-home/overlays/$env" | yq e -N '.. | select(tag == "!!map" and has("image")) | .image' - | sed '/^$/d' | sort -u); do
    check
    [[ "$image" == ghcr.io/maelprog/manage-our-home-* ]] || fail "$image is outside the policy's imageReferences"
  done

  # --- Traefik, k3s's own (#434): its settings that no IngressRoute
  # reaches. The client's address kept (`Local`), the Caddyfile's
  # connection bounds on both entry points, memory capped; before the
  # application. ci/ingress-test.sh checks that Traefik runs with them.
  hcc='select(.kind == "HelmChartConfig" and .metadata.name == "traefik" and .metadata.namespace == "kube-system")'
  check
  y "$hcc | .spec.valuesContent" > "$work/traefik-values.yaml"
  t() { yq e -N "$1" "$work/traefik-values.yaml"; }
  v=$(t '[.service.spec.externalTrafficPolicy, (.ports.web.transport.respondingTimeouts | [.readTimeout, .idleTimeout] | join(",")), (.ports.websecure.transport.respondingTimeouts | [.readTimeout, .idleTimeout] | join(",")), .resources.limits.memory // "none"] | join(" ")')
  [[ "$v" =~ ^Local\ 16m,5m\ 16m,5m\ [0-9]+Mi$ ]] || fail "Traefik's HelmChartConfig: '$v', expected 'Local 16m,5m 16m,5m <limit>Mi'"
  check
  w_traefik=$(wave "$hcc")
  [ -n "$w_traefik" ] && [ "$w_traefik" -lt "$w_app" ] || fail "Traefik's HelmChartConfig (wave '$w_traefik') is not before the application ($w_app)"

  # --- What the charts install, rendered by Helm with their values: a
  # memory limit on every container, one replica of everything (the
  # 4 GB staging server, arbitrage of 2026-10-12), and nothing of Argo
  # CD's Dex, notifications or ApplicationSet controller.
  for a in $(y 'select(.kind == "Application" and .spec.source.chart != null) | .metadata.name'); do
    ctx="$env/$a"
    sel="select(.kind == \"Application\" and .metadata.name == \"$a\") | .spec"
    chart=$(y "$sel | .source.chart"); repo=$(y "$sel | .source.repoURL"); version=$(y "$sel | .source.targetRevision")
    release=$(y "$sel | .source.helm.releaseName"); ns=$(y "$sel | .destination.namespace")
    y "$sel | .source.helm.valuesObject" > "$work/values.yaml"
    rendered="$work/$env-$a.yaml"
    check
    if ! helm template "$release" "$chart" --repo "$repo" --version "$version" --namespace "$ns" \
      --kube-version "$k8s_version" --values "$work/values.yaml" > "$rendered" 2> "$work/err"; then
      fail "helm template: $(cat "$work/err")"
      continue
    fi
    r() { yq e -N "$1" "$rendered" | sed "/^$/d"; }
    # shellcheck disable=SC2016 # $w is yq's, not the shell's
    for c in $(r 'select(.kind == "Deployment" or .kind == "StatefulSet" or .kind == "DaemonSet" or .kind == "Job") | .kind + "/" + .metadata.name as $w | .spec.template.spec | (.containers + (.initContainers // []))[] | $w + "/" + .name + "=" + (.resources.limits.memory // "none")'); do
      check
      [ "${c#*=}" != none ] || fail "${c%=*}: no memory limit"
    done
    for d in $(r 'select(.kind == "Deployment" or .kind == "StatefulSet") | .kind + "/" + .metadata.name + "=" + ((.spec.replicas // 1) | tostring)'); do
      check
      case "${d%=*}" in
        Deployment/argocd-applicationset-controller) want=0 ;;
        *) want=1 ;;
      esac
      [ "${d#*=}" = "$want" ] || fail "${d%=*}: ${d#*=} replica(s), expected $want"
    done
    if [ "$a" = argocd ]; then
      check
      v=$(r 'select(.kind == "Deployment" or .kind == "StatefulSet") | .metadata.name' | grep -E 'dex|notifications' || true)
      [ -z "$v" ] || fail "Argo CD runs '$v'"
    fi
    if [ "$a" = kyverno ]; then
      check
      v=$(r 'select(.kind == "Deployment") | .metadata.name' | sort | tr '\n' ' ')
      [ "$v" = "kyverno-admission-controller " ] || fail "Kyverno runs '$v', expected the admission controller alone"
    fi
  done
  ctx=$env
done

# --- The Argo CD that platform/bootstrap.sh installs is the one its
# Application then manages: same chart, version and values, read from the
# one file.
ctx=bootstrap
check
grep -q 'components/argocd/application.yaml' "$platform/bootstrap.sh" \
  || fail "bootstrap.sh does not read components/argocd/application.yaml"

echo "platform-test: $checks checks, $failures failure(s)"
[ "$failures" = 0 ]
