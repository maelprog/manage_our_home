#!/usr/bin/env bash
# Tests on what the overlays render (#431), not on the files: a patch that
# silently stops applying, or a base change an overlay does not expect,
# shows here. Needs `kustomize` and `yq` (mikefarah, v4) on PATH; the CI
# installs both with ci/install-tools.sh.
#
#   ci/render-test.sh            # every overlay
#   ci/render-test.sh staging    # one of them
#
# Exits non-zero if any check fails, after running all of them.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
app="$root/apps/manage-our-home"
overlays=("$@")
[ ${#overlays[@]} -gt 0 ] || overlays=(staging prod)

# What the pods are told in clear (ConfigMaps, `env[].value`) must never be
# one of these: they come from Secrets, whose values are not in this branch.
SECRET_NAMES=(
  DATABASE_URL ADMIN_DATABASE_URL MIGRATION_DATABASE_URL
  GOOGLE_CLIENT_ID GOOGLE_CLIENT_SECRET
  OAUTH_ENCRYPTION_KEY MESSAGE_ENCRYPTION_KEY CALENDAR_FEED_ENCRYPTION_KEY
  LOGIN_THROTTLE_KEY VAPID_PRIVATE_KEY
  SMTP_USERNAME SMTP_PASSWORD
  MINIO_ACCESS_KEY MINIO_SECRET_KEY
)

failures=0
checks=0
overlay=""
fail() { echo "FAIL [$overlay] $*" >&2; failures=$((failures + 1)); }
check() { checks=$((checks + 1)); }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Every container of a workload, one line each: `<kind>/<workload>/<container>`.
WORKLOADS='select(.kind == "Deployment" or .kind == "Job")'

for overlay in "${overlays[@]}"; do
  out="$work/$overlay.yaml"
  check
  if ! kustomize build "$app/overlays/$overlay" > "$out" 2> "$work/err"; then
    fail "kustomize build: $(cat "$work/err")"
    continue
  fi
  y() { yq e -N "$1" "$out" | sed "/^$/d"; }
  ya() { yq ea "$1" "$out"; }

  # --- The workloads are there.
  check
  deployments=$(y 'select(.kind == "Deployment") | .metadata.name' | sort | tr '\n' ' ')
  [ "$deployments" = "api web " ] || fail "Deployments: expected 'api web', got '$deployments'"
  check
  jobs=$(y 'select(.kind == "Job") | .metadata.name' | tr '\n' ' ')
  [ "$jobs" = "migrate " ] || fail "Jobs: expected 'migrate', got '$jobs'"

  # --- No Secret rendered: their values are sealed elsewhere (#432).
  check
  n=$(ya '[select(.kind == "Secret")] | length')
  [ "$n" = 0 ] || fail "$n Secret(s) in the render: secret values never live in this branch"

  # shellcheck disable=SC2016 # $w is yq's, not the shell's
  containers=$(y "$WORKLOADS"' | .kind + "/" + .metadata.name as $w | .spec.template.spec | (.containers + (.initContainers // []))[] | $w + "/" + .name')
  for c in $containers; do
    kind=${c%%/*}; rest=${c#*/}; w=${rest%%/*}; name=${rest#*/}
    sel="select(.kind == \"$kind\" and .metadata.name == \"$w\") | .spec.template.spec | (.containers + (.initContainers // []))[] | select(.name == \"$name\")"

    # --- Images by digest, never by tag alone.
    check
    image=$(y "$sel | .image")
    [[ "$image" =~ ^ghcr\.io/[a-z0-9-]+/manage-our-home-(api|web)@sha256:[0-9a-f]{64}$ ]] \
      || fail "$c: image '$image' is not referenced by digest"

    # --- Memory: requested and capped on every container.
    check
    req=$(y "$sel | .resources.requests.memory // \"\"")
    lim=$(y "$sel | .resources.limits.memory // \"\"")
    [ -n "$req" ] && [ -n "$lim" ] || fail "$c: memory request '$req' / limit '$lim' missing"

    # --- Unprivileged, read-only, no capability.
    check
    sc=$(y "$sel | [.securityContext.allowPrivilegeEscalation, .securityContext.readOnlyRootFilesystem, (.securityContext.capabilities.drop // [] | join(\",\"))] | join(\" \")")
    [ "$sc" = "false true ALL" ] || fail "$c: securityContext is '$sc', expected 'false true ALL'"

    # --- Nothing secret in clear.
    for s in "${SECRET_NAMES[@]}"; do
      check
      v=$(y "$sel | .env // [] | .[] | select(.name == \"$s\") | select(.valueFrom.secretKeyRef == null) | .name")
      [ -z "$v" ] || fail "$c: $s is set without a secretKeyRef"
    done
  done

  for w in $(y "$WORKLOADS"' | .kind + "/" + .metadata.name'); do
    kind=${w%%/*}; name=${w#*/}
    pod="select(.kind == \"$kind\" and .metadata.name == \"$name\") | .spec.template.spec"
    check
    psc=$(y "$pod | [.securityContext.runAsNonRoot, .securityContext.seccompProfile.type, .automountServiceAccountToken] | join(\" \")")
    [ "$psc" = "true RuntimeDefault false" ] || fail "$w: pod security is '$psc', expected 'true RuntimeDefault false'"
  done

  for s in "${SECRET_NAMES[@]}"; do
    check
    v=$(y "select(.kind == \"ConfigMap\") | .data // {} | keys | .[] | select(. == \"$s\")")
    [ -z "$v" ] || fail "ConfigMap key $s: secret values never go in a ConfigMap"
  done

  # --- Only the migration Job holds the migration role (#425).
  check
  v=$(y 'select(.kind == "Deployment") | .spec.template.spec.containers[] | .env // [] | .[] | select(.name == "MIGRATION_DATABASE_URL") | .name')
  [ -z "$v" ] || fail "a Deployment receives MIGRATION_DATABASE_URL"

  # --- The migration Job: the `migrate` command, as an Argo CD PreSync hook.
  check
  v=$(y 'select(.kind == "Job" and .metadata.name == "migrate") | [.metadata.annotations["argocd.argoproj.io/hook"], (.spec.template.spec.containers[0].command | join(" ")), .spec.template.spec.restartPolicy] | join(" ")')
  [ "$v" = "PreSync migrate Never" ] || fail "Job migrate: hook/command/restartPolicy is '$v', expected 'PreSync migrate Never'"
  check
  v=$(y 'select(.kind == "Job" and .metadata.name == "migrate") | .spec.template.spec.containers[0].env[] | select(.name == "MIGRATION_DATABASE_URL") | .valueFrom.secretKeyRef.name // ""')
  [ -n "$v" ] || fail "Job migrate: MIGRATION_DATABASE_URL does not come from a Secret"

  for d in api web; do
    dep="select(.kind == \"Deployment\" and .metadata.name == \"$d\")"
    ctr="$dep | .spec.template.spec.containers[0]"

    # --- Probes (#424). Readiness waits longer than the route's own 2 s
    # timeout on its dependency, or a slow dependency reads as a probe
    # timeout instead of the route's 503.
    check
    v=$(y "$ctr | [.readinessProbe.httpGet.path, .livenessProbe.httpGet.path, .startupProbe.httpGet.path] | join(\" \")")
    [ "$v" = "/readyz /healthz /healthz" ] || fail "$d: probes are '$v', expected '/readyz /healthz /healthz'"
    check
    v=$(y "$ctr | .readinessProbe.timeoutSeconds // 1")
    [ "$v" -gt 2 ] || fail "$d: readinessProbe.timeoutSeconds is $v, must be above 2"

    # --- Stopping (#424): the preStop sleep lets the endpoint removal
    # reach every proxy while the process still serves, then the process
    # drains for SHUTDOWN_GRACE_SECONDS; the kubelet must wait for both.
    check
    sleep_s=$(y "$ctr | .lifecycle.preStop.sleep.seconds // 0")
    [ "$sleep_s" -ge 1 ] || fail "$d: no preStop sleep"
    cm=$(y "$ctr | .envFrom // [] | .[] | select(.configMapRef) | .configMapRef.name" | head -n 1)
    drain=$(y "select(.kind == \"ConfigMap\" and .metadata.name == \"$cm\") | .data.SHUTDOWN_GRACE_SECONDS // \"\"")
    tgps=$(y "$dep | .spec.template.spec.terminationGracePeriodSeconds // 30")

    # --- What the ConfigMap says is what the process gets: an `env[]`
    # entry of the same name wins over `envFrom`, so a TRUSTED_PROXY_CIDRS
    # or SHUTDOWN_GRACE_SECONDS set there would void the checks on the
    # ConfigMap below without a word.
    for k in $(y "select(.kind == \"ConfigMap\" and .metadata.name == \"$cm\") | .data // {} | keys | .[]"); do
      check
      v=$(y "$ctr | .env // [] | .[] | select(.name == \"$k\") | .name")
      [ -z "$v" ] || fail "$d: env[] redefines $k, which its ConfigMap '$cm' sets"
    done

    check
    if [ -z "$drain" ]; then
      fail "$d: SHUTDOWN_GRACE_SECONDS not set in its ConfigMap '$cm'"
    elif [ "$tgps" -lt $((sleep_s + drain)) ]; then
      fail "$d: terminationGracePeriodSeconds $tgps < preStop $sleep_s + SHUTDOWN_GRACE_SECONDS $drain"
    fi

    # --- A PodDisruptionBudget that never blocks a drain on its own.
    check
    sel=$(y "$dep | .spec.selector.matchLabels | to_entries | map(.key + \"=\" + .value) | join(\",\")")
    pdb=$(y "select(.kind == \"PodDisruptionBudget\") | select((.spec.selector.matchLabels | to_entries | map(.key + \"=\" + .value) | join(\",\")) == \"$sel\") | [.spec.maxUnavailable, (.spec.minAvailable // \"none\")] | join(\" \")")
    [ "$pdb" = "1 none" ] || fail "$d: PodDisruptionBudget is '$pdb', expected maxUnavailable 1 and no minAvailable"

    # --- Spread by node.
    check
    v=$(y "$dep | .spec.template.spec.topologySpreadConstraints // [] | .[] | select(.topologyKey == \"kubernetes.io/hostname\") | .whenUnsatisfiable")
    case "$overlay" in
      staging) want=ScheduleAnyway; replicas=1 ;;
      *) want=DoNotSchedule; replicas=2 ;;
    esac
    [ "$v" = "$want" ] || fail "$d: spread by node is '$v', expected '$want'"

    # --- Replicas: one on the 4 GB staging server, two elsewhere.
    check
    v=$(y "$dep | .spec.replicas")
    [ "$v" = "$replicas" ] || fail "$d: $v replica(s), expected $replicas"
  done

  # --- Whose X-Forwarded-For the API believes (#178): the k3s pod network,
  # where web and the ingress controller sit.
  check
  cm=$(y 'select(.kind == "Deployment" and .metadata.name == "api") | .spec.template.spec.containers[0].envFrom[] | select(.configMapRef) | .configMapRef.name' | head -n 1)
  v=$(y "select(.kind == \"ConfigMap\" and .metadata.name == \"$cm\") | .data.TRUSTED_PROXY_CIDRS // \"\"")
  [ "$v" = "10.42.0.0/16" ] || fail "api: TRUSTED_PROXY_CIDRS is '$v', expected '10.42.0.0/16'"
done

echo "render-test: $checks checks, $failures failure(s)"
[ "$failures" = 0 ]
