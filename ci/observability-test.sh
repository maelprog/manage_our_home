#!/usr/bin/env bash
# The observability (#437) on the cluster of the current kube context —
# the k3d cluster of the `observability` job of
# .github/workflows/deploy.yml, its node capped at 4 GB. Bootstraps Argo
# CD as platform/bootstrap.sh does, its root Application rendering
# ci/observability (Argo CD, cert-manager and the observability brick of
# production) at the commit under test, then checks that:
#   - every Application is Synced and Healthy;
#   - Prometheus scrapes cert-manager, kube-state-metrics, node-exporter
#     and the kubelet, and has the rules of alerts.yaml;
#   - the alerts fire on what ci/observability-fixtures.yaml sets up: a
#     certificate expiring within the hour, a failed `migrate` Job, a
#     container whose readiness probe fails;
#   - they reach Alertmanager, Watchdog too (Prometheus → Alertmanager);
#     Alertmanager runs the configuration of alertmanager-config.sh, and
#     mails the alerts labelled `notify: mail`, Watchdog not;
#   - Loki has the marker line the not-ready pod logged, once, with its
#     namespace, pod and container, and keeps 14 days of logs;
#   - Grafana answers through `kubectl port-forward`, with the generated
#     admin password, and reaches Loki; nothing of `monitoring` is exposed
#     outside the cluster;
#   - over SOAK_SECONDS (300 s), nothing restarts, the Applications stay
#     Synced and Grafana keeps its password;
# and prints what every pod uses (`kubectl top pods -A`).
#
#   ci/observability-test.sh <git revision>
#
# <git revision> is the commit Argo CD reads this branch at, which must
# be on GitHub. Needs kubectl, helm, yq and curl.

set -euo pipefail

revision=$1
root=$(cd "$(dirname "$0")/.." && pwd)
mon=monitoring
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
    kubectl -n argocd get "$a" -o yaml | yq e '.status | {"sync": .sync, "health": .health, "outOfSync": [.resources[]? | select(.status == "OutOfSync") | .kind + "/" + .name], "operationState": .operationState, "conditions": .conditions}' - || true
  done
  kubectl get pods -A -o wide || true
  kubectl get events -A --sort-by=.lastTimestamp | tail -n 80 || true
  for p in $(kubectl -n "$mon" get pods -o name 2> /dev/null); do
    echo "--- logs $p"
    kubectl -n "$mon" logs "$p" --all-containers --tail=60 || true
  done
  kubectl -n ci-mail logs deploy/mailpit --tail=30 || true
  echo "::endgroup::"
}
on_exit() {
  local status=$?
  [ "$status" = 0 ] || dump
  cleanup
  exit "$status"
}
trap on_exit EXIT

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
forward() { # namespace service port local-port
  kubectl -n "$1" port-forward "svc/$2" "$4:$3" > "$work/pf-$2.log" 2>&1 &
  pf_pids+=($!)
  for _ in $(seq 1 30); do
    curl --silent --output /dev/null "http://127.0.0.1:$4/" && return 0
    sleep 1
  done
  echo "::error::port-forward to $1/$2 does not answer"
  return 1
}
# Retries a check until it passes, or fails after <tries> × 10 s.
eventually() { # tries description command...
  local tries=$1 what=$2
  shift 2
  for _ in $(seq 1 "$tries"); do
    if "$@" > "$work/last" 2>&1; then
      echo "ok: $what"
      return 0
    fi
    sleep 10
  done
  cat "$work/last"
  echo "::error::$what: not after $((tries * 10)) s"
  return 1
}

echo "== fixtures: mail catcher"
kubectl apply -f - <<< "$(yq e 'select(.metadata.namespace == "ci-mail" or .metadata.name == "ci-mail")' "$root/ci/observability-fixtures.yaml")"
kubectl -n ci-mail rollout status deploy/mailpit --timeout=180s

echo "== bootstrap (root Application at $revision, ci/observability)"
REVISION=$revision yq e '
  .spec.source.targetRevision = strenv(REVISION) |
  .spec.source.path = "ci/observability"
' "$root/platform/clusters/staging/root.yaml" > "$work/root.yaml"
"$root/platform/bootstrap.sh" prod "$work/root.yaml"

echo "== Applications"
for a in argocd cert-manager kube-prometheus-stack loki alloy; do
  wait_app "$a" 900
done
wait_app platform 600

# Alertmanager's configuration, as a cluster's is filled
# (platform/README.md, "Observabilité"), with the mail catcher for relay:
# plain SMTP on 1025, no authentication offered, so none attempted. Not
# sealed here: ci/staging-smoke.sh tests the sealing.
echo "== alertmanager-config"
SMTP_HOST=mailpit.ci-mail.svc.cluster.local SMTP_PORT=1025 SMTP_REQUIRE_TLS=false \
  SMTP_FROM=alertmanager@ci.invalid SMTP_USERNAME=ci SMTP_PASSWORD=ci ALERT_TO=ops@ci.invalid \
  "$root/platform/components/observability/alertmanager-config.sh" \
  | kubectl -n "$mon" create secret generic alertmanager-config --from-file=alertmanager.yaml=/dev/stdin

echo "== fixtures: what the alerts fire on"
kubectl apply -f "$root/ci/ci-issuer.yaml"
kubectl apply -f - <<< "$(yq e 'select(.metadata.namespace != "ci-mail" and .metadata.name != "ci-mail")' "$root/ci/observability-fixtures.yaml")"
kubectl -n ci-obs wait --for=condition=Ready certificate/ci-short --timeout=180s
kubectl -n manage-our-home wait --for=condition=Failed job/migrate --timeout=180s
kubectl -n ci-obs wait --for=jsonpath='{.status.phase}'=Running pod/ci-not-ready --timeout=180s

forward "$mon" kube-prometheus-stack-prometheus 9090 19090
forward "$mon" kube-prometheus-stack-alertmanager 9093 19093
forward "$mon" loki 3100 13100
forward "$mon" kube-prometheus-stack-grafana 80 13000
forward ci-mail mailpit 8025 18025
prom() { curl --silent --fail --get "http://127.0.0.1:19090/api/v1/$1" "${@:2}"; }

echo "== Prometheus: targets and rules"
target_up() { # job
  [ "$(prom query --data-urlencode "query=up{job=\"$1\"}" | yq e -p json '[.data.result[] | select(.value[1] == "1")] | length' -)" -gt 0 ]
}
for job in cert-manager kube-state-metrics node-exporter kubelet; do
  eventually 30 "target $job up" target_up "$job"
done
v=$(prom rules | yq e -p json '[.data.groups[].name | select(test("^manage-our-home\\."))] | sort | join(" ")' -)
want=$(yq e '[.spec.groups[].name] | sort | join(" ")' "$root/platform/components/observability/alerts.yaml")
[ "$v" = "$want" ] || { echo "::error::Prometheus has the groups '$v', expected '$want'"; exit 1; }
echo "rules: $v"

echo "== the alerts fire"
alert_state() { # alertname label value states-regex
  prom alerts | NAME=$1 KEY=$2 VALUE=$3 yq e -p json '.data.alerts[] | select(.labels.alertname == strenv(NAME) and .labels[strenv(KEY)] == strenv(VALUE)) | .state' - | grep -Eqx "$4"
}
eventually 30 "CertificateExpiringSoon firing for ci-obs/ci-short" alert_state CertificateExpiringSoon name ci-short firing
eventually 30 "MigrationJobFailed firing" alert_state MigrationJobFailed job_name migrate firing
eventually 30 "ContainerNotReady pending or firing for ci-not-ready" alert_state ContainerNotReady pod ci-not-ready 'pending|firing'

echo "== Alertmanager"
am_has() { # alertname
  curl --silent --fail "http://127.0.0.1:19093/api/v2/alerts" | NAME=$1 yq e -p json '[.[] | select(.labels.alertname == strenv(NAME))] | length' - | grep -qv '^0$'
}
for a in Watchdog CertificateExpiringSoon MigrationJobFailed; do
  eventually 30 "$a in Alertmanager" am_has "$a"
done
am_config() {
  curl --silent --fail "http://127.0.0.1:19093/api/v2/status" | yq e -p json '.config.original' - | grep -q 'mailpit.ci-mail.svc.cluster.local:1025'
}
eventually 30 "Alertmanager runs alertmanager-config" am_config

echo "== mail"
mailed() { # alertname
  curl --silent --fail "http://127.0.0.1:18025/api/v1/messages" | NAME=$1 yq e -p json '[.messages[] | select(.Subject | contains(strenv(NAME))) | select(.To[0].Address == "ops@ci.invalid")] | length' - | grep -qv '^0$'
}
for a in CertificateExpiringSoon MigrationJobFailed; do
  eventually 30 "$a mailed" mailed "$a"
done
if mailed Watchdog; then echo "::error::Watchdog was mailed"; exit 1; fi
echo "Watchdog not mailed"
curl --silent "http://127.0.0.1:18025/api/v1/messages" | yq e -p json '.messages[] | .Subject' -

echo "== Loki"
marker() {
  curl --silent --fail --get "http://127.0.0.1:13100/loki/api/v1/query_range" \
    --data-urlencode 'query={namespace="ci-obs", pod="ci-not-ready", container="probe"} |= "ci-loki-marker"' \
    --data-urlencode "start=$(($(date +%s) - 3600))000000000" > "$work/loki.json"
  n=$(yq e -p json '[.data.result[].values[]] | length' "$work/loki.json")
  echo "marker lines: $n"
  [ "$n" -gt 0 ]
}
eventually 30 "the marker line in Loki" marker
n=$(yq e -p json '[.data.result[].values[]] | length' "$work/loki.json")
[ "$n" = 1 ] || { echo "::error::the marker line is $n times in Loki, expected once"; exit 1; }
curl --silent --fail "http://127.0.0.1:13100/config" > "$work/loki-config.yaml"
v=$(yq e '[.limits_config.retention_period, .compactor.retention_enabled] | join(" ")' "$work/loki-config.yaml")
case "$v" in
  "2w true" | "336h true") echo "Loki retention: $v" ;;
  *) echo "::error::Loki runs with retention '$v', expected 14 days"; exit 1 ;;
esac

echo "== Grafana"
grafana_pw() { kubectl -n "$mon" get secret kube-prometheus-stack-grafana -o jsonpath='{.data.admin-password}' | base64 -d; }
pw=$(grafana_pw)
gf() { curl --silent --fail --user "admin:$pw" "http://127.0.0.1:13000/api/$1"; }
gf health > /dev/null
v=$(gf datasources | yq e -p json '[.[].type] | sort | join(" ")' -)
[ "$v" = "alertmanager loki prometheus" ] || [ "$v" = "loki prometheus" ] || { echo "::error::Grafana's datasources: '$v'"; exit 1; }
v=$(gf datasources/uid/loki/health | yq e -p json '.status' -)
[ "$v" = OK ] || { echo "::error::Grafana does not reach Loki: '$v'"; exit 1; }
echo "Grafana: admin login, datasources '$(gf datasources | yq e -p json '[.[].name] | join(", ")' -)', Loki reachable"
v=$( (kubectl -n "$mon" get ingress -o name 2> /dev/null
  kubectl -n "$mon" get services -o yaml | yq e '.items[] | select(.spec.type != "ClusterIP") | "service/" + .metadata.name' -) | tr '\n' ' ')
[ -z "$v" ] || { echo "::error::exposed outside the cluster: $v"; exit 1; }
echo "nothing of $mon exposed outside the cluster"

restart_counts() {
  # shellcheck disable=SC2016 # $n is yq's, not the shell's
  kubectl get pods -A -o yaml \
    | yq e '.items[] | .metadata.namespace + "/" + .metadata.name + " " + ([.status.containerStatuses[]?.restartCount] | .[] as $n ireduce (0; . + $n) | tostring)' - \
    | LC_ALL=C sort
}
soak=${SOAK_SECONDS:-300}
echo "== ${soak} s at rest, 4 GB node"
grafana_uid=$(kubectl -n "$mon" get pods -l app.kubernetes.io/name=grafana -o jsonpath='{.items[0].metadata.uid}')
restart_counts > "$work/restarts.before"
sleep "$soak"
restart_counts > "$work/restarts.after"
restarted=$(LC_ALL=C join "$work/restarts.before" "$work/restarts.after" | awk '$2 != $3')
[ -z "$restarted" ] || { echo "$restarted"; echo "::error::a container restarted during the ${soak} s"; exit 1; }
for a in platform argocd cert-manager kube-prometheus-stack loki alloy; do
  wait_app "$a" 5
done
[ "$(grafana_pw)" = "$pw" ] || { echo "::error::Grafana's admin password changed"; exit 1; }
v=$(kubectl -n "$mon" get pods -l app.kubernetes.io/name=grafana -o jsonpath='{.items[0].metadata.uid}')
[ "$v" = "$grafana_uid" ] || { echo "::error::Grafana's pod was replaced"; exit 1; }
echo "no restart; Applications Synced; Grafana's pod and password unchanged"

echo "== memory and CPU at rest"
for _ in $(seq 1 30); do
  kubectl top pods -A --sort-by=memory 2> /dev/null > "$work/top" && break
  sleep 5
done
[ -s "$work/top" ] || { echo "::error::kubectl top returned no sample"; exit 1; }
cat "$work/top"
awk 'NR > 1 { sub(/Mi$/, "", $4); total[$1] += $4; all += $4 }
  END { for (n in total) printf "%-20s %6d Mi\n", n, total[n]; printf "%-20s %6d Mi\n", "all pods", all }' "$work/top"
kubectl top nodes

echo "observability: OK"
