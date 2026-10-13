#!/usr/bin/env bash
# Bootstraps the platform on the cluster of the current kube context — the
# k3d cluster of .github/workflows/deploy.yml, its node capped at 4 GB like
# the staging server — and lets Argo CD deploy staging, as on the real
# cluster (#432). Then checks that:
#   - every Application of the platform, and the application's, is Synced
#     and Healthy;
#   - the Secrets arrive sealed: sealed here with the cluster's own
#     controller, and only the SealedSecrets are applied;
#   - the migration Job ran as a PreSync hook and succeeded, api and web
#     are Available and answer /readyz with 200, as the non-root,
#     read-only containers the manifests describe (#431);
#   - Kyverno refuses an image of the application referenced by tag, and
#     one that is not signed, and admits a signed one;
#   - nothing restarts, nothing is OOM-killed, over SOAK_SECONDS (600 s);
# and prints what every pod uses (`kubectl top pods -A`).
#
#   ci/staging-smoke.sh <path to infra/postgres/init/01-roles.sh of main> <git revision>
#
# <git revision> is the commit Argo CD reads this branch at: the one under
# test, which must be on GitHub. Needs kustomize, kubectl, helm, kubeseal,
# yq, openssl and curl. Every credential is generated here, for this run
# only: none is in the branch.

set -euo pipefail

roles_script=$1
revision=$2
root=$(cd "$(dirname "$0")/.." && pwd)
ns=manage-our-home
app=manage-our-home-staging
work=$(mktemp -d)
pf_pids=()

cleanup() {
  for p in "${pf_pids[@]}"; do kill "$p" 2> /dev/null || true; done
  rm -rf "$work"
}
dump() {
  echo "::group::diagnostics"
  kubectl get applications -n argocd -o wide || true
  for a in $(kubectl -n argocd get applications -o name 2> /dev/null); do
    echo "--- $a"
    kubectl -n argocd get "$a" -o yaml | yq e '.status | {"sync": .sync, "health": .health, "operationState": .operationState, "conditions": .conditions}' - || true
  done
  kubectl get pods -A -o wide || true
  kubectl get events -A --sort-by=.lastTimestamp | tail -n 80 || true
  kubectl -n "$ns" describe pods || true
  for p in $(kubectl -n "$ns" get pods -o name 2> /dev/null); do
    echo "--- logs $p"
    kubectl -n "$ns" logs "$p" --all-containers --tail=100 || true
    kubectl -n "$ns" logs "$p" --all-containers --previous --tail=50 2> /dev/null || true
  done
  kubectl -n kyverno logs deploy/kyverno-admission-controller --tail=80 || true
  kubectl -n argocd logs statefulset/argocd-application-controller --tail=80 || true
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

# For Kyverno's refusals: the tag images.yml gave the staging api image of
# the overlays when #431 pinned it (commit 10aabde of main), and the
# digest of a manifest of that repository nobody signed — one of the
# Sigstore bundles attached to that image (OCI referrer). Neither moves
# when the overlays' digests do.
API_TAG=sha-10aabdead035b61f4071cffc7e91413c22814c2d
API_UNSIGNED_DIGEST=sha256:706ed61cc9d7a1dce88285ddd52f52cd2aff0a104b926c8c7d5a0fc80e0ce169

pw() { openssl rand -hex 24; }
key() { openssl rand -base64 32; }

# Waits until Argo CD reports the Application Synced and Healthy.
wait_app() { # name timeout-seconds
  local state=""
  for _ in $(seq 1 $(($2 / 5))); do
    state=$(kubectl -n argocd get application "$1" -o jsonpath='{.status.sync.status} {.status.health.status}' 2> /dev/null || true)
    if [ "$state" = "Synced Healthy" ]; then
      echo "$1: $state"
      return 0
    fi
    sleep 5
  done
  echo "::error::Application $1 is '$state' after $2 s, expected 'Synced Healthy'"
  return 1
}

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

# The staging root Application, at the commit under test instead of the
# branch it names — the application's Application included, through a
# patch the root applies to its own render.
echo "== bootstrap (root Application at $revision)"
yq e "
  .spec.source.targetRevision = \"$revision\" |
  .spec.source.kustomize.patches = [{
    \"target\": {\"kind\": \"Application\", \"name\": \"$app\"},
    \"patch\": \"- op: replace\n  path: /spec/source/targetRevision\n  value: $revision\n\"
  }]
" "$root/platform/clusters/staging/root.yaml" > "$work/root.yaml"
"$root/platform/bootstrap.sh" staging "$work/root.yaml"

echo "== platform"
wait_app platform 900 &
root_wait=$!
for a in argocd sealed-secrets cert-manager cloudnative-pg kyverno; do
  wait_app "$a" 900
done

# Sealed with the certificate of this cluster's controller, as the user
# does for a real one (platform/README.md); the Secret in clear only goes
# through a pipe.
echo "== sealed Secrets"
kubectl wait --for=jsonpath='{.status.phase}'=Active "namespace/$ns" --timeout=300s
db=postgres.ci-postgres.svc.cluster.local:5432/manage_our_home
seal() { kubeseal --format yaml > "$work/$1.sealed.yaml"; }
kubectl -n "$ns" create secret generic migrate-secrets --dry-run=client -o yaml \
  --from-literal=MIGRATION_DATABASE_URL="postgres://migration_role:${migration_pw}@${db}" \
  | seal migrate-secrets
kubectl -n "$ns" create secret generic api-secrets --dry-run=client -o yaml \
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
  --from-literal=MINIO_SECRET_KEY="$(pw)" \
  | seal api-secrets
for s in migrate-secrets api-secrets; do
  kind=$(yq e '.kind' "$work/$s.sealed.yaml")
  [ "$kind" = SealedSecret ] || { echo "::error::$s: kubeseal gave a $kind"; exit 1; }
  kubectl apply -f "$work/$s.sealed.yaml"
done
for s in migrate-secrets api-secrets; do
  kubectl -n "$ns" wait --for=create "secret/$s" --timeout=120s
done

echo "== $app (Argo CD)"
wait_app "$app" 900
wait "$root_wait"
kubectl -n "$ns" get job/migrate -o jsonpath='{.metadata.annotations.argocd\.argoproj\.io/hook}{"\n"}'
kubectl -n "$ns" wait --for=condition=Complete job/migrate --timeout=0
kubectl -n "$ns" logs job/migrate
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

# A pod that passes Pod Security `restricted`, so that a refusal can only
# be Kyverno's; the signed image of the running api is admitted with the
# same spec, which proves it.
echo "== Kyverno"
api_image=$(kubectl -n "$ns" get deploy/api -o jsonpath='{.spec.template.spec.containers[0].image}')
api_repo=${api_image%@*}
try_pod() { # name image
  kubectl create -f - 2>&1 << EOF
apiVersion: v1
kind: Pod
metadata:
  name: $1
  namespace: $ns
spec:
  automountServiceAccountToken: false
  securityContext:
    runAsNonRoot: true
    runAsUser: 10001
    seccompProfile: {type: RuntimeDefault}
  containers:
    - name: probe
      image: $2
      command: ["sleep", "1"]
      securityContext:
        allowPrivilegeEscalation: false
        readOnlyRootFilesystem: true
        capabilities: {drop: ["ALL"]}
      resources:
        limits: {memory: 32Mi}
EOF
}
refused() { # name image what
  local out
  if out=$(try_pod "$1" "$2"); then
    echo "::error::Kyverno admitted $3 ($2)"
    kubectl -n "$ns" delete pod "$1" --wait=false || true
    return 1
  fi
  echo "$out"
  grep -q 'manage-our-home-signed-images' <<< "$out" \
    || { echo "::error::$3 refused, but not by the policy manage-our-home-signed-images"; return 1; }
  echo "refused, as expected: $3"
}
out=$(try_pod kyverno-signed "$api_image") || { echo "$out"; echo "::error::Kyverno refused the signed image $api_image"; exit 1; }
echo "admitted, as expected: the signed image by digest"
kubectl -n "$ns" delete pod kyverno-signed --wait=false
refused kyverno-tag "$api_repo:$API_TAG" "a signed image of the application, by tag"
refused kyverno-unsigned "$api_repo@$API_UNSIGNED_DIGEST" "an unsigned manifest of the application's repository"

# Restarts before and after: any container that restarts in the window,
# or was ever OOM-killed, fails the run.
restart_counts() {
  kubectl get pods -A -o yaml \
    | yq e '.items[] | .metadata.namespace + "/" + .metadata.name + " " + ([.status.containerStatuses[]?.restartCount] | .[] as $n ireduce (0; . + $n) | tostring)' - \
    | sort
}
oom_killed() {
  kubectl get pods -A -o yaml \
    | yq e '.items[] | .metadata.namespace + "/" + .metadata.name as $p | .status.containerStatuses[]? | select(.lastState.terminated.reason == "OOMKilled" or .state.terminated.reason == "OOMKilled") | $p + "/" + .name' -
}
soak=${SOAK_SECONDS:-600}
echo "== ${soak} s at rest, 4 GB node"
restart_counts > "$work/restarts.before"
sleep "$soak"
restart_counts > "$work/restarts.after"
if ! diff "$work/restarts.before" "$work/restarts.after"; then
  echo "::error::a container restarted during the ${soak} s"
  exit 1
fi
oom=$(oom_killed)
[ -z "$oom" ] || { echo "::error::OOM-killed: $oom"; exit 1; }
notready=$(kubectl get pods -A -o yaml \
  | yq e '.items[] | select(.status.phase != "Succeeded") | select([.status.containerStatuses[]?.ready] | all | not) | .metadata.namespace + "/" + .metadata.name' -)
[ -z "$notready" ] || { echo "::error::pods not Ready: $notready"; exit 1; }
for a in platform argocd sealed-secrets cert-manager cloudnative-pg kyverno "$app"; do
  wait_app "$a" 5
done
cat "$work/restarts.after"

echo "== memory and CPU at rest"
for _ in $(seq 1 30); do
  if kubectl top pods -A --sort-by=memory 2> /dev/null > "$work/top"; then
    break
  fi
  sleep 5
done
[ -s "$work/top" ] || { echo "::error::kubectl top returned no sample"; exit 1; }
cat "$work/top"
# Working set per namespace, then the platform's (everything but the
# application, the test Postgres and k3s's own kube-system pods).
awk 'NR > 1 {
    sub(/Mi$/, "", $4); total[$1] += $4; all += $4
    if ($1 ~ /^(argocd|cert-manager|cnpg-system|kyverno)$/ || $2 ~ /^sealed-secrets-controller-/) platform += $4
  }
  END {
    for (n in total) printf "%-20s %6d Mi\n", n, total[n]
    printf "%-20s %6d Mi (Argo CD, Sealed Secrets, cert-manager, CloudNativePG, Kyverno)\n", "platform", platform
    printf "%-20s %6d Mi\n", "all pods", all
  }' "$work/top"
kubectl top nodes

echo "platform and staging smoke: OK"
