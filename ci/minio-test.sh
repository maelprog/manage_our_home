#!/usr/bin/env bash
# MinIO in the cluster (#435), on the k3d cluster ci/staging-smoke.sh has
# just brought up — StatefulSet `minio`, the bucket created by the Job
# `minio-bucket` during Argo CD's sync, api running on it:
#
#   1. The bucket Job ran in the sync and created the bucket.
#   2. An attachment sent through the API lands in the bucket — its object
#      is on MinIO's volume — and reads back, byte for byte, through the
#      short-lived URL the API hands out.
#   3. Replayed by a new sync of the application, the bucket Job succeeds
#      and does nothing: it says the bucket is there, and the attachment
#      is still there.
#   4. MinIO restarted, the attachment is still there: it is on the volume
#      of the StatefulSet's claim, not in the pod.
#
#   ci/minio-test.sh
#
# Needs kubectl, yq, curl, python3 and openssl, and the kube context of the
# k3d cluster.

set -euo pipefail

ns=manage-our-home
app=manage-our-home-staging
work=$(mktemp -d)
pf_pids=()

on_exit() {
  local status=$?
  if [ "$status" != 0 ]; then
    echo "::group::diagnostics"
    kubectl -n "$ns" get statefulsets,pods,pvc,jobs -o wide || true
    kubectl -n "$ns" logs statefulset/minio --tail=80 || true
    kubectl -n "$ns" logs job/minio-bucket --tail=40 || true
    kubectl -n "$ns" logs deploy/api --tail=80 || true
    kubectl -n argocd get application "$app" -o yaml | yq e '.status.operationState' - || true
    for f in "$work"/*.log; do [ -e "$f" ] && { echo "--- $f"; cat "$f"; }; done
    echo "::endgroup::"
  fi
  for p in "${pf_pids[@]}"; do kill "$p" 2> /dev/null || true; done
  rm -rf "$work"
  exit "$status"
}
trap on_exit EXIT

bucket=$(kubectl -n "$ns" get job/minio-bucket -o jsonpath='{.spec.template.spec.containers[0].env[?(@.name=="MINIO_BUCKET")].value}')
data=$(kubectl -n "$ns" get statefulset/minio -o jsonpath='{.spec.template.spec.containers[0].command[2]}')

echo "== the bucket Job of the sync"
kubectl -n "$ns" get job/minio-bucket -o jsonpath='{.metadata.annotations.argocd\.argoproj\.io/hook}{"\n"}'
kubectl -n "$ns" wait --for=condition=Complete job/minio-bucket --timeout=0
kubectl -n "$ns" logs job/minio-bucket | tee "$work/bucket-1.log"
grep -q "Bucket created successfully \`local/$bucket\`" "$work/bucket-1.log" \
  || { echo "::error::the first run of the bucket Job did not create the bucket $bucket"; exit 1; }

# Through the ingress, as a browser goes: the api's session cookie is
# `Secure`. The member is made as ci/ingress-test.sh does.
host=$(kubectl -n "$ns" get certificates.cert-manager.io manage-our-home-tls -o jsonpath='{.spec.dnsNames[0]}')
node=$(kubectl get nodes -o jsonpath='{.items[0].metadata.name}')
address=$(kubectl get node "$node" -o jsonpath='{.status.addresses[?(@.type=="InternalIP")].address}')
ingress() {
  local path=$1
  shift
  curl --silent --show-error --insecure --resolve "$host:443:$address" "https://$host$path" "$@"
}
json() { python3 -c "import json, sys; print(json.load(sys.stdin)$1)"; }

echo "== a member, a family, an event"
email="ci-minio-$(openssl rand -hex 4)@example.test"
password=$(openssl rand -hex 16)
jar="$work/cookies"
code=$(ingress /api/auth/register --output "$work/register.log" --write-out '%{http_code}' \
  -H 'Content-Type: application/json' \
  -d "{\"email\":\"$email\",\"password\":\"$password\",\"display_name\":\"CI MinIO\",\"declares_minimum_age\":true,\"accepts_terms\":true}")
[ "$code" = 201 ] || { echo "::error::register: $code"; exit 1; }
primary=$(kubectl -n "$ns" get clusters.postgresql.cnpg.io postgres -o jsonpath='{.status.currentPrimary}')
v=$(kubectl -n "$ns" exec "$primary" -c postgres -- psql -X -At -d manage_our_home \
  -c "UPDATE users SET email_verified = true WHERE email = '$email'")
[ "$v" = "UPDATE 1" ] || { echo "::error::verifying $email: $v"; exit 1; }
code=$(ingress /api/auth/login --cookie-jar "$jar" --output "$work/login.log" --write-out '%{http_code}' \
  -H 'Content-Type: application/json' -d "{\"email\":\"$email\",\"password\":\"$password\"}")
[ "$code" = 200 ] || { echo "::error::login: $code"; exit 1; }
group=$(ingress /api/groups --cookie "$jar" -H 'Content-Type: application/json' -d '{"name":"CI MinIO"}' | json '["id"]')
start=$(date -u -d '+1 day' +%Y-%m-%dT10:00:00Z); end=$(date -u -d '+1 day' +%Y-%m-%dT11:00:00Z)
event=$(ingress "/api/groups/$group/events" --cookie "$jar" -H 'Content-Type: application/json' \
  -d "{\"title\":\"CI MinIO\",\"starts_at\":\"$start\",\"ends_at\":\"$end\",\"all_day\":false,\"is_task\":false}" \
  | json '["id"]')
echo "group $group, event $event"

echo "== an attachment, through the API"
# A PNG by its signature, which is what the API sniffs; 1 MiB of noise
# after it, so that nothing could pass for it by chance.
{ printf '\x89PNG\r\n\x1a\n'; head -c 1048576 /dev/urandom; } > "$work/sent.png"
code=$(ingress "/api/groups/$group/events/$event/attachments" --cookie "$jar" \
  --form "file=@$work/sent.png;type=image/png" --output "$work/upload.json" --write-out '%{http_code}')
[ "$code" = 201 ] || { echo "::error::upload: $code $(cat "$work/upload.json")"; exit 1; }
attachment=$(json '["id"]' < "$work/upload.json")
key=$(kubectl -n "$ns" exec "$primary" -c postgres -- psql -X -At -d manage_our_home \
  -c "SELECT storage_key FROM event_attachments WHERE id = '$attachment'")
echo "attachment $attachment, object $bucket/$key"

# MinIO's port, for the URL the API signs: it names http://minio:9000,
# the Service inside the cluster, and the signature covers that host.
kubectl -n "$ns" port-forward svc/minio 19000:9000 > "$work/pf-minio.log" 2>&1 &
pf_pids+=($!)
read_back() { # label
  local url code
  url=$(ingress "/api/groups/$group/events/$event/attachments/$attachment/download" --cookie "$jar" | json '["url"]')
  [[ "$url" == "http://minio:9000/$bucket/$key?"* ]] || { echo "::error::$1: the API hands out '$url'"; return 1; }
  for _ in $(seq 1 30); do
    code=$(curl --silent --connect-to minio:9000:127.0.0.1:19000 --output "$work/read.png" --write-out '%{http_code}' "$url" || true)
    [ "$code" = 200 ] && break
    sleep 1
  done
  [ "$code" = 200 ] || { echo "::error::$1: the signed URL answers $code"; return 1; }
  cmp "$work/sent.png" "$work/read.png" || { echo "::error::$1: the object read back differs from the one sent"; return 1; }
  # On MinIO's volume: one directory per object, its metadata inside.
  kubectl -n "$ns" exec minio-0 -- test -f "$data/$bucket/$key/xl.meta" \
    || { echo "::error::$1: $bucket/$key is not on MinIO's volume ($data)"; return 1; }
  echo "$1: $bucket/$key read back, $(wc -c < "$work/read.png") bytes, identical; on the volume"
}
read_back "after the upload"

# A sync of the application, as any merge brings: its hooks run again —
# the migration Job, then the bucket Job, on a bucket that is there.
echo "== the bucket Job, replayed by a sync"
old_uid=$(kubectl -n "$ns" get job/minio-bucket -o jsonpath='{.metadata.uid}')
revision=$(kubectl -n argocd get application "$app" -o jsonpath='{.spec.source.targetRevision}')
kubectl -n argocd patch application "$app" --type merge \
  -p "{\"operation\":{\"initiatedBy\":{\"username\":\"ci\"},\"sync\":{\"revision\":\"$revision\"}}}"
for i in $(seq 1 120); do
  uid=$(kubectl -n "$ns" get job/minio-bucket -o jsonpath='{.metadata.uid}' 2> /dev/null || true)
  phase=$(kubectl -n argocd get application "$app" -o jsonpath='{.status.operationState.phase}')
  [ -n "$uid" ] && [ "$uid" != "$old_uid" ] && [ "$phase" = Succeeded ] && break
  [ "$i" = 120 ] && { echo "::error::no new run of the bucket Job, or the sync is '$phase', after 600 s"; exit 1; }
  sleep 5
done
kubectl -n "$ns" wait --for=condition=Complete job/minio-bucket --timeout=0
kubectl -n "$ns" logs job/minio-bucket | tee "$work/bucket-2.log"
grep -qx "bucket $bucket: already there, nothing to do" "$work/bucket-2.log" \
  || { echo "::error::the replayed bucket Job did not find the bucket there"; exit 1; }
! grep -q 'created' "$work/bucket-2.log" || { echo "::error::the replayed bucket Job created something"; exit 1; }
read_back "after the replay"

echo "== MinIO restarted"
kubectl -n "$ns" delete pod minio-0 --wait=true
kubectl -n "$ns" wait --for=create pod/minio-0 --timeout=120s
kubectl -n "$ns" wait --for=condition=Ready pod/minio-0 --timeout=180s
kill "${pf_pids[@]}" 2> /dev/null || true
kubectl -n "$ns" port-forward svc/minio 19000:9000 > "$work/pf-minio-2.log" 2>&1 &
pf_pids+=($!)
read_back "after a restart"

echo "MinIO: OK"
