#!/usr/bin/env bash
# Deploys the staging overlay on the cluster of the current kube context —
# the k3d cluster of .github/workflows/deploy.yml — and checks that it
# runs: the migration Job succeeds, api and web become Available, both
# answer /readyz with 200, as the non-root, read-only containers the
# manifests describe. Then prints what they use (`kubectl top`).
#
#   ci/staging-smoke.sh <path to infra/postgres/init/01-roles.sh of main>
#
# Needs kustomize, kubectl, yq, openssl and curl. Every credential is
# generated here, for this run only: none is in the branch.

set -euo pipefail

roles_script=$1
root=$(cd "$(dirname "$0")/.." && pwd)
ns=manage-our-home
work=$(mktemp -d)
pf_pids=()

cleanup() {
  for p in "${pf_pids[@]}"; do kill "$p" 2> /dev/null || true; done
  rm -rf "$work"
}
dump() {
  echo "::group::diagnostics"
  kubectl get all,pdb,configmap -A -o wide || true
  kubectl get events -A --sort-by=.lastTimestamp | tail -n 60 || true
  kubectl -n "$ns" describe pods || true
  for p in $(kubectl -n "$ns" get pods -o name 2> /dev/null); do
    echo "--- logs $p"
    kubectl -n "$ns" logs "$p" --all-containers --tail=100 || true
    kubectl -n "$ns" logs "$p" --all-containers --previous --tail=50 2> /dev/null || true
  done
  kubectl -n ci-postgres logs deploy/postgres --tail=50 || true
  echo "::endgroup::"
}
on_exit() {
  local status=$?
  [ "$status" = 0 ] || dump
  cleanup
  exit "$status"
}
trap on_exit EXIT

pw() { openssl rand -hex 24; }
key() { openssl rand -base64 32; }

echo "== test Postgres"
pg_password=$(pw); app_pw=$(pw); migration_pw=$(pw); admin_pw=$(pw)
kubectl apply -f - <<< "$(yq e 'select(.kind == "Namespace")' "$root/ci/postgres.yaml")"
kubectl -n ci-postgres create secret generic postgres \
  --from-literal=POSTGRES_PASSWORD="$pg_password" \
  --from-literal=APP_ROLE_PASSWORD="$app_pw" \
  --from-literal=MIGRATION_ROLE_PASSWORD="$migration_pw" \
  --from-literal=ADMIN_ROLE_PASSWORD="$admin_pw"
kubectl -n ci-postgres create configmap postgres-init --from-file=01-roles.sh="$roles_script"
kubectl apply -f "$root/ci/postgres.yaml"
kubectl -n ci-postgres rollout status deploy/postgres --timeout=180s

echo "== render staging"
kustomize build "$root/apps/manage-our-home/overlays/staging" > "$work/staging.yaml"

echo "== namespace and secrets"
kubectl apply -f - <<< "$(yq e 'select(.kind == "Namespace")' "$work/staging.yaml")"
db=postgres.ci-postgres.svc.cluster.local:5432/manage_our_home
kubectl -n "$ns" create secret generic migrate-secrets \
  --from-literal=MIGRATION_DATABASE_URL="postgres://migration_role:${migration_pw}@${db}"
kubectl -n "$ns" create secret generic api-secrets \
  --from-literal=DATABASE_URL="postgres://app_role:${app_pw}@${db}" \
  --from-literal=ADMIN_DATABASE_URL="postgres://admin_role:${admin_pw}@${db}" \
  --from-literal=GOOGLE_CLIENT_ID=ci-client-id \
  --from-literal=GOOGLE_CLIENT_SECRET="$(pw)" \
  --from-literal=OAUTH_ENCRYPTION_KEY="$(key)" \
  --from-literal=MESSAGE_ENCRYPTION_KEY="$(key)" \
  --from-literal=CALENDAR_FEED_ENCRYPTION_KEY="$(key)" \
  --from-literal=LOGIN_THROTTLE_KEY="$(key)" \
  --from-literal=SMTP_USERNAME=ci \
  --from-literal=SMTP_PASSWORD="$(pw)" \
  --from-literal=MINIO_ACCESS_KEY=ci \
  --from-literal=MINIO_SECRET_KEY="$(pw)"

# What Argo CD does with a PreSync hook: the Job first, alone, and nothing
# else unless it succeeds.
echo "== migration Job (PreSync)"
kubectl apply -f - <<< "$(yq e 'select(.kind == "Job")' "$work/staging.yaml")"
for _ in $(seq 1 120); do
  if kubectl -n "$ns" wait --for=condition=Complete job/migrate --timeout=0 > /dev/null 2>&1; then break; fi
  if kubectl -n "$ns" wait --for=condition=Failed job/migrate --timeout=0 > /dev/null 2>&1; then
    echo "::error::the migration Job failed"
    exit 1
  fi
  sleep 3
done
kubectl -n "$ns" wait --for=condition=Complete job/migrate --timeout=0
kubectl -n "$ns" logs job/migrate

echo "== api and web"
kubectl apply -f - <<< "$(yq e 'select(.kind != "Job" and .kind != "Namespace")' "$work/staging.yaml")"
kubectl -n "$ns" rollout status deploy/api --timeout=300s
kubectl -n "$ns" rollout status deploy/web --timeout=300s
kubectl -n "$ns" wait --for=condition=Available deploy/api deploy/web --timeout=60s
kubectl -n "$ns" get pods -o wide

echo "== /readyz"
probe() { # service port local-port
  kubectl -n "$ns" port-forward "svc/$1" "$3:$2" > "$work/pf-$1.log" 2>&1 &
  pf_pids+=($!)
  local code=000
  for _ in $(seq 1 30); do
    code=$(curl --silent --output /dev/null --write-out '%{http_code}' "http://127.0.0.1:$3/readyz" || true)
    [ "$code" = 200 ] && break
    sleep 1
  done
  echo "$1 /readyz: $code"
  [ "$code" = 200 ]
}
probe api 8080 18080
probe web 3000 13000

echo "== pods as described"
pods=$(kubectl -n "$ns" get pods -l 'app.kubernetes.io/name in (api,web)' -o yaml)
# Running read-only, as a non-root UID, on the pod network the API
# trusts (TRUSTED_PROXY_CIDRS=10.42.0.0/16).
ro=$(yq e '[.items[].spec.containers[].securityContext.readOnlyRootFilesystem] | all' <<< "$pods")
[ "$ro" = true ] || { echo "::error::a container is not read-only"; exit 1; }
nonroot=$(yq e '[.items[].spec.securityContext.runAsUser != 0] | all' <<< "$pods")
[ "$nonroot" = true ] || { echo "::error::a pod runs as root"; exit 1; }
for ip in $(yq e '.items[].status.podIP' <<< "$pods"); do
  case "$ip" in
    10.42.*) ;;
    *) echo "::error::pod IP $ip is outside 10.42.0.0/16, which TRUSTED_PROXY_CIDRS trusts"; exit 1 ;;
  esac
done
# No restart: a container that died once (OOM, a crash on a read-only
# filesystem) and came back would still be Available.
# shellcheck disable=SC2016 # $n is yq's, not the shell's
restarts=$(yq e '[.items[].status.containerStatuses[].restartCount] | .[] as $n ireduce (0; . + $n)' <<< "$pods")
[ "$restarts" = 0 ] || { echo "::error::$restarts container restart(s)"; exit 1; }

echo "== memory and CPU at rest"
# metrics-server ships with k3s; its first sample takes a minute or so.
sleep "${REST_SECONDS:-60}"
for _ in $(seq 1 30); do
  if kubectl -n "$ns" top pods --containers 2> /dev/null > "$work/top"; then
    cat "$work/top"
    break
  fi
  sleep 5
done
[ -s "$work/top" ] || echo "::warning::kubectl top returned no sample"

echo "staging smoke: OK"
