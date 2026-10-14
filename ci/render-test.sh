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

# A memory quantity in Mi, for the units the manifests use.
to_mib() {
  case "$1" in
    *Mi) echo "${1%Mi}" ;;
    *Gi) echo $((${1%Gi} * 1024)) ;;
    *) return 1 ;;
  esac
}

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

    # --- The limit is not lowered below its floor: for api and web, the
    # worst case main bounds (README.md, "Mémoire"), which a lower limit
    # would OOM-kill under a load main allows; for migrate, which has no
    # bounded worst case, the limit main's migrations were observed to
    # run within (migrate-job.yaml).
    check
    case "$name" in
      api) floor=256; why="the worst case main bounds" ;;
      web) floor=448; why="the worst case main bounds" ;;
      migrate) floor=128; why="the limit main's migrations were observed to run within" ;;
      *) floor=""; why="" ;;
    esac
    if [ -z "$floor" ]; then
      fail "$c: no memory floor known for container '$name'"
    elif ! mib=$(to_mib "$lim"); then
      fail "$c: memory limit '$lim' is not in Mi or Gi"
    elif [ "$mib" -lt "$floor" ]; then
      fail "$c: memory limit $lim is below its ${floor}Mi floor, $why"
    fi

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
  # The previous run deleted when the next one is created, not when it
  # succeeds or fails: its logs stay readable until the next sync.
  check
  v=$(y 'select(.kind == "Job" and .metadata.name == "migrate") | .metadata.annotations["argocd.argoproj.io/hook-delete-policy"] // ""')
  [ "$v" = BeforeHookCreation ] || fail "Job migrate: hook-delete-policy is '$v', expected 'BeforeHookCreation'"
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
    # A single envFrom source, its ConfigMap, without a prefix: a second
    # one could redefine any key of the first (the last source wins) past
    # the checks below, which read the first; a prefix would rename every
    # key, TRUSTED_PROXY_CIDRS included, so that none of them arrives.
    check
    v=$(y "$ctr | (.envFrom // []) | [length, (.[0].configMapRef.name // \"none\"), (.[0].prefix // \"\")] | join(\" \")")
    [[ "$v" =~ ^1\ [^\ ]+\ $ ]] && [ "$v" != "1 none " ] || fail "$d: envFrom is '$v', expected exactly one source, a ConfigMap, without prefix"
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

  # --- Postgres (#433): the overlay's `postgres/` folder, which an
  # Application of its own syncs before the application.
  pg="$work/$overlay-postgres.yaml"
  check
  if ! kustomize build "$app/overlays/$overlay/postgres" > "$pg" 2> "$work/err"; then
    fail "kustomize build postgres: $(cat "$work/err")"
    continue
  fi
  p() { yq e -N "$1" "$pg" | sed "/^$/d"; }
  cl='select(.kind == "Cluster" and .metadata.name == "postgres") | .spec'
  check
  v=$(p 'select(.kind == "Cluster") | .metadata.name' | tr '\n' ' ')
  [ "$v" = "postgres " ] || fail "postgres: Clusters are '$v', expected 'postgres'"
  check
  n=$(yq ea '[select(.kind == "Secret")] | length' "$pg")
  [ "$n" = 0 ] || fail "postgres: $n Secret(s) in the render"
  # Argo CD never deletes it: that would delete its volume.
  check
  v=$(p 'select(.kind == "Cluster") | .metadata.annotations["argocd.argoproj.io/sync-options"] // ""')
  [[ "$v" == *Delete=false* && "$v" == *Prune=false* ]] || fail "postgres: sync-options '$v', expected Delete=false and Prune=false"

  # Instances: one to start with, on staging as in production.
  check
  v=$(p "$cl | .instances")
  [ "$v" = 1 ] || fail "postgres: $v instance(s), expected 1"
  check
  v=$(p "$cl | [.affinity.enablePodAntiAffinity, .affinity.topologyKey] | join(\" \")")
  [ "$v" = "true kubernetes.io/hostname" ] || fail "postgres: anti-affinity is '$v', expected 'true kubernetes.io/hostname'"
  check
  v=$(p "$cl | .imageName")
  [[ "$v" =~ ^ghcr\.io/cloudnative-pg/postgresql:16\.[0-9]+-system-[a-z]+@sha256:[0-9a-f]{64}$ ]] \
    || fail "postgres: image '$v' is not Postgres 16 by tag and digest, with barman-cloud (system)"
  check
  v=$(p "$cl | .enableSuperuserAccess")
  [ "$v" = false ] || fail "postgres: enableSuperuserAccess is '$v', expected false"

  # Memory: requested and capped; on the 4 GB server, within the 256 to
  # 384 Mi the arbitrage of 2026-10-12 gives it. shared_buffers within a
  # quarter of the limit, the rest being the connections' and barman's.
  check
  req=$(p "$cl | .resources.requests.memory // \"\"")
  lim=$(p "$cl | .resources.limits.memory // \"\"")
  sb=$(p "$cl | .postgresql.parameters.shared_buffers // \"\"")
  if [ -z "$req" ] || [ -z "$lim" ]; then
    fail "postgres: memory request '$req' / limit '$lim' missing"
  elif ! lim_mib=$(to_mib "$lim"); then
    fail "postgres: memory limit '$lim' is not in Mi or Gi"
  else
    if [ "$overlay" = staging ]; then
      check
      [ "$lim_mib" -ge 256 ] && [ "$lim_mib" -le 384 ] || fail "postgres: memory limit $lim, expected 256 to 384 Mi on staging"
    fi
    check
    case "$sb" in
      *MB) sb_mib=${sb%MB} ;;
      *GB) sb_mib=$((${sb%GB} * 1024)) ;;
      *) sb_mib="" ;;
    esac
    if [ -z "$sb_mib" ]; then
      fail "postgres: shared_buffers '$sb' is not set in MB or GB"
    elif [ $((sb_mib * 4)) -gt "$lim_mib" ]; then
      fail "postgres: shared_buffers $sb is more than a quarter of the limit $lim"
    fi
  fi
  for k in effective_cache_size work_mem maintenance_work_mem max_connections; do
    check
    v=$(p "$cl | .postgresql.parameters.$k // \"\"")
    [ -n "$v" ] || fail "postgres: parameter $k not set"
  done

  # Connections: what the api pods may open at once fits in
  # max_connections, beside the superuser slots. Per pod, the budget of
  # apps/api/README.md on main: the handlers' 20 and the admin pool's 5
  # (apps/api/src/main.rs), the three limits' pools (their ConfigMap value,
  # or 4, 2 and 2 when unset), one per background job (4, apps/api/src/
  # jobs). As many pods as replicas plus maxSurge; then the migration Job's
  # 1 (apps/api/src/migrations.rs) and reconcile_attachments' 2.
  check
  acm=$(y 'select(.kind == "Deployment" and .metadata.name == "api") | .spec.template.spec.containers[0].envFrom[] | select(.configMapRef) | .configMapRef.name' | head -n 1)
  pool() { y "select(.kind == \"ConfigMap\" and .metadata.name == \"$acm\") | .data.$1 // \"$2\""; }
  per_pod=$((20 + 5 + $(pool LOGIN_THROTTLE_POOL_SIZE 4) + $(pool CSP_REPORT_POOL_SIZE 2) + $(pool RATE_LIMIT_POOL_SIZE 2) + 4))
  pods=$(($(y 'select(.kind == "Deployment" and .metadata.name == "api") | .spec.replicas') + $(y 'select(.kind == "Deployment" and .metadata.name == "api") | .spec.strategy.rollingUpdate.maxSurge // 1')))
  need=$((pods * per_pod + 1 + 2))
  maxc=$(p "$cl | .postgresql.parameters.max_connections // \"100\"")
  reserved=$(p "$cl | .postgresql.parameters.superuser_reserved_connections // \"3\"")
  [ $((maxc - reserved)) -ge "$need" ] \
    || fail "postgres: max_connections $maxc - $reserved superuser slots < $need ($pods api pods x $per_pod, + 3)"

  # The roles of infra/postgres/init/01-roles.sh (main): their attributes,
  # each password from a Secret; the database's owner never logs in. The
  # k3d job compares the result with that script's (ci/postgres-test.sh).
  check
  v=$(p "$cl | .managed.roles[] | [.name, (.ensure // \"present\"), (.login // false), (.superuser // false), (.bypassrls // false), (.passwordSecret.name // \"-\")] | join(\" \")" | sort | tr '\n' ';')
  want="admin_role present true false true postgres-admin-role;app_role present true false false postgres-app-role;mhome present false false false -;migration_role present true false true postgres-migration-role;"
  [ "$v" = "$want" ] || fail "postgres: managed roles are '$v', expected '$want'"
  check
  v=$(p "$cl | .bootstrap.initdb | [.database, .owner] | join(\" \")")
  [ "$v" = "manage_our_home mhome" ] || fail "postgres: initdb database/owner '$v', expected 'manage_our_home mhome'"
  # The statements of 01-roles.sh, exactly and in order: a widened grant
  # or an added one fails here, not only in the k3d job.
  check
  v=$(p "$cl | .bootstrap.initdb.postInitApplicationSQL[]")
  want="CREATE EXTENSION IF NOT EXISTS pgcrypto
CREATE ROLE migration_role NOSUPERUSER BYPASSRLS
GRANT USAGE, CREATE ON SCHEMA public TO migration_role
CREATE ROLE admin_role NOSUPERUSER BYPASSRLS
ALTER DEFAULT PRIVILEGES FOR ROLE migration_role IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO admin_role
ALTER DEFAULT PRIVILEGES FOR ROLE migration_role IN SCHEMA public GRANT USAGE, SELECT ON SEQUENCES TO admin_role
CREATE ROLE app_role NOSUPERUSER NOBYPASSRLS
ALTER DEFAULT PRIVILEGES FOR ROLE migration_role IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO app_role
ALTER DEFAULT PRIVILEGES FOR ROLE migration_role IN SCHEMA public GRANT USAGE, SELECT ON SEQUENCES TO app_role"
  [ "$v" = "$want" ] || fail "postgres: postInitApplicationSQL is not the 9 statements of 01-roles.sh: $(diff <(echo "$want") <(echo "$v") | tr '\n' ' ')"

  # Continuous backup off the server, with a retention.
  bo="$cl | .backup"
  check
  v=$(p "$bo | .retentionPolicy // \"\"")
  [[ "$v" =~ ^[0-9]+[dwm]$ ]] || fail "postgres: retentionPolicy '$v' is not declared"
  check
  v=$(p "$bo | .barmanObjectStore | [.endpointURL, .destinationPath] | join(\" \")")
  [[ "$v" =~ ^https://s3\.fr-par\.scw\.cloud\ s3://[a-z0-9-]*-$overlay/$ ]] \
    || fail "postgres: backups go to '$v', expected Scaleway fr-par and a bucket of $overlay's own"
  check
  v=$(p "$bo | .barmanObjectStore.s3Credentials | [.accessKeyId.name, .secretAccessKey.name] | join(\" \")")
  [ "$v" = "postgres-backup postgres-backup" ] || fail "postgres: backup credentials from '$v', expected the Secret postgres-backup"
  # Scaleway refuses the per-request SSE header (cluster.yaml): the bucket
  # encrypts by default instead.
  check
  v=$(p "$bo | .barmanObjectStore | [.wal.encryption // \"\", .data.encryption // \"\"] | join(\"\")")
  [ -z "$v" ] || fail "postgres: per-request encryption '$v', which Scaleway refuses"
  check
  v=$(p 'select(.kind == "ScheduledBackup") | [.spec.cluster.name, .spec.immediate] | join(" ")')
  [ "$v" = "postgres true" ] || fail "postgres: ScheduledBackup is '$v', expected 'postgres true'"
done

echo "render-test: $checks checks, $failures failure(s)"
[ "$failures" = 0 ]
