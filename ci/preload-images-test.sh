#!/usr/bin/env bash
# Tests of ci/preload-images.sh (#557), offline, on fixtures: which images
# of a render it preloads, and its verdict on a namespace's events — the
# imported copy used, or the image pulled from its registry anyway. Needs
# yq (mikefarah, v4) on PATH.
#
#   ci/preload-images-test.sh
#
# Exits non-zero if any check fails, after running all of them.

set -euo pipefail

script="$(cd "$(dirname "$0")" && pwd)/preload-images.sh"
REDIS=ecr-public.aws.com/docker/library/redis:8.6.4-alpine

failures=0
checks=0
fail() { echo "FAIL $*" >&2; failures=$((failures + 1)); }

expect_select() { # name expected < render
  local got
  checks=$((checks + 1))
  got=$("$script" select) || { fail "$1: select exited non-zero"; return; }
  [ "$got" = "$2" ] || fail "$1: got [$got], expected [$2]"
}

expect_verdict() { # name 0|1 < events
  local rc=0
  checks=$((checks + 1))
  "$script" verdict "$REDIS" > /dev/null 2>&1 || rc=1
  [ "$rc" = "$2" ] || fail "$1: verdict exited $rc, expected $2"
}

# --- select ---------------------------------------------------------------

expect_select "images outside quay.io and ghcr.io, sorted, once each" \
  "docker.io/library/busybox:1.37
$REDIS" <<EOF
apiVersion: v1
kind: ConfigMap
metadata: {name: argocd-cm}
data: {image: not-an-image.example/x:1}
---
apiVersion: apps/v1
kind: Deployment
metadata: {name: argocd-redis}
spec:
  template:
    spec:
      initContainers:
        - {name: init, image: docker.io/library/busybox:1.37}
      containers:
        - {name: redis, image: $REDIS}
---
apiVersion: apps/v1
kind: StatefulSet
metadata: {name: argocd-application-controller}
spec:
  template:
    spec:
      containers:
        - {name: c, image: quay.io/argoproj/argocd:v3.5.5}
        - {name: d, image: ghcr.io/example/sidecar:1}
---
apiVersion: batch/v1
kind: Job
metadata: {name: argocd-redis-secret-init}
spec:
  template:
    spec:
      containers:
        - {name: secret-init, image: $REDIS}
EOF

expect_select "nothing outside quay.io and ghcr.io" "" <<'EOF'
apiVersion: apps/v1
kind: Deployment
metadata: {name: argocd-repo-server}
spec:
  template:
    spec:
      containers:
        - {name: c, image: quay.io/argoproj/argocd:v3.5.5}
EOF

# --- verdict --------------------------------------------------------------

event() { # pod reason message
  P=$1 R=$2 M=$3 yq -n -o json -I0 e '.involvedObject.name = strenv(P) | .reason = strenv(R) | .message = strenv(M)'
}
events() { echo "{\"items\":[$(IFS=,; echo "$*")]}"; }

present="Container image \"$REDIS\" already present on machine and can be accessed by the pod"

expect_verdict "found on the node" 0 < <(events \
  "$(event argocd-redis-1 Scheduled 'Successfully assigned argocd/argocd-redis-1')" \
  "$(event argocd-redis-1 Pulled "$present")" \
  "$(event argocd-redis-1 Started 'Started container redis')")

expect_verdict "found on the node twice, other images pulled" 0 < <(events \
  "$(event argocd-redis-1 Pulled "$present")" \
  "$(event argocd-redis-2 Pulled "$present")" \
  "$(event argocd-server-1 Pulling 'Pulling image "quay.io/argoproj/argocd:v3.5.5"')" \
  "$(event argocd-server-1 Pulled 'Successfully pulled image "quay.io/argoproj/argocd:v3.5.5" in 3s')")

expect_verdict "a longer image of the same prefix pulled" 0 < <(events \
  "$(event argocd-redis-1 Pulled "$present")" \
  "$(event other-1 Pulling "Pulling image \"$REDIS-debug\"")")

expect_verdict "pulled from its registry" 1 < <(events \
  "$(event argocd-redis-1 Pulling "Pulling image \"$REDIS\"")" \
  "$(event argocd-redis-1 Pulled "Successfully pulled image \"$REDIS\" in 4.1s (4.1s including waiting)")")

expect_verdict "found once, pulled by a later pod" 1 < <(events \
  "$(event argocd-redis-1 Pulled "$present")" \
  "$(event argocd-redis-2 Pulling "Pulling image \"$REDIS\"")")

expect_verdict "the pull refused (429)" 1 < <(events \
  "$(event argocd-redis-1 Pulling "Pulling image \"$REDIS\"")" \
  "$(event argocd-redis-1 Failed "Failed to pull image \"$REDIS\": 429 Too Many Requests")")

expect_verdict "no event for the image" 1 < <(events \
  "$(event argocd-server-1 Pulled 'Container image "quay.io/argoproj/argocd:v3.5.5" already present on machine')")

expect_verdict "no event at all" 1 < <(events)

# --- pull, against a stand-in helm and docker -----------------------------

stubs=$(mktemp -d)
trap 'rm -rf "$stubs"' EXIT
cat > "$stubs/helm" <<EOF
#!/usr/bin/env bash
printf 'kind: Deployment\nspec: {template: {spec: {containers: [{image: $REDIS}]}}}\n'
EOF
cat > "$stubs/docker" <<'EOF'
#!/usr/bin/env bash
echo "$*" >> "$STUB_LOG"
case "$1" in
  pull) [ -z "$STUB_PULL_ERROR" ] || { echo "$STUB_PULL_ERROR" >&2; exit 1; } ;;
  save) touch "$3" ;;
esac
EOF
chmod +x "$stubs/helm" "$stubs/docker"

expect_pull() { # name pull-error 0|1 stderr-grep pulls
  local rc=0 err
  checks=$((checks + 1))
  : > "$stubs/log"
  err=$(PATH="$stubs:$PATH" STUB_LOG="$stubs/log" STUB_PULL_ERROR=$2 PRELOAD_RETRY_DELAY=0 \
    "$script" pull "$stubs/out.tar" 2>&1 > /dev/null) || rc=1
  [ "$rc" = "$3" ] || fail "$1: pull exited $rc, expected $3"
  [ -z "$4" ] || grep -qF -- "$4" <<< "$err" || fail "$1: no [$4] in [$err]"
  [ "$(grep -c '^pull ' "$stubs/log")" = "$5" ] || fail "$1: $(grep -c '^pull ' "$stubs/log") pulls, expected $5"
}

expect_pull "pulled and saved" "" 0 "" 1
checks=$((checks + 1))
grep -qxF "save --output $stubs/out.tar $REDIS" "$stubs/log" || fail "pulled and saved: no docker save of $REDIS"
expect_pull "refused with 429, said so" \
  "Error response from daemon: toomanyrequests: Rate exceeded" 1 \
  "::error::cache miss, and the pull of $REDIS from ecr-public.aws.com was refused: 429 Too Many Requests, 3 times" 3
expect_pull "another failure, the registry named" \
  "Error response from daemon: manifest unknown" 1 \
  "::error::cache miss, and the pull of $REDIS from ecr-public.aws.com failed 3 times" 3

echo "$checks checks, $failures failed"
[ "$failures" -eq 0 ]
