#!/usr/bin/env bash
# Tests of ci/preload-images.sh (#557), offline, on fixtures: which images
# of a render it preloads, and its verdict on a namespace's events — the
# imported copy used, or the image pulled from its registry anyway; then
# key, pull, import and check against stand-ins for helm, docker, k3d and
# kubectl: a failed or empty render, a refused pull, a missing archive, an
# image missing from the node, several images judged. Needs yq (mikefarah,
# v4) on PATH.
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

# --- key, pull, import, check: against stand-in helm, docker, k3d, kubectl

OTHER=docker.io/library/busybox:1.37
stubs=$(mktemp -d)
trap 'rm -rf "$stubs"' EXIT
# helm prints $STUB_RENDER, or fails when STUB_HELM_FAIL is set.
cat > "$stubs/helm" <<'EOF'
#!/usr/bin/env bash
[ -z "${STUB_HELM_FAIL:-}" ] || { echo "Error: repo not reachable" >&2; exit 1; }
cat "$STUB_RENDER"
EOF
# docker: pull fails with $STUB_PULL_ERROR if set; exec (crictl inspecti
# in the node) fails for the image in $STUB_NODE_MISSING.
cat > "$stubs/docker" <<'EOF'
#!/usr/bin/env bash
echo "$*" >> "$STUB_LOG"
case "$1" in
  pull) [ -z "${STUB_PULL_ERROR:-}" ] || { echo "$STUB_PULL_ERROR" >&2; exit 1; } ;;
  save) touch "$3" ;;
  exec) [ "${*: -1}" != "${STUB_NODE_MISSING:-}" ] || { echo "no such image" >&2; exit 1; } ;;
esac
EOF
cat > "$stubs/k3d" <<'EOF'
#!/usr/bin/env bash
echo "k3d $*" >> "$STUB_LOG"
EOF
cat > "$stubs/kubectl" <<'EOF'
#!/usr/bin/env bash
cat "$STUB_EVENTS"
EOF
chmod +x "$stubs/helm" "$stubs/docker" "$stubs/k3d" "$stubs/kubectl"

render() { # image...
  local img
  for img in "$@"; do
    echo "---"
    echo "kind: Deployment"
    echo "spec: {template: {spec: {containers: [{image: $img}]}}}"
  done
}
render "$REDIS" > "$stubs/render-redis.yaml"
render "$REDIS" "$OTHER" quay.io/argoproj/argocd:v3.5.5 > "$stubs/render-two.yaml"
render quay.io/argoproj/argocd:v3.5.5 > "$stubs/render-none.yaml"

# Runs the script with the stand-ins; sets out, err and rc.
run() { # args...
  : > "$stubs/log"
  rc=0
  out=$(PATH="$stubs:$PATH" STUB_LOG="$stubs/log" STUB_RENDER=${STUB_RENDER:-$stubs/render-redis.yaml} \
    PRELOAD_RETRY_DELAY=0 "$script" "$@" 2> "$stubs/err") || rc=$?
  err=$(cat "$stubs/err")
}
expect() { # name expected-rc stderr-grep (empty: none)
  checks=$((checks + 1))
  [ "$rc" = "$2" ] || fail "$1: exited $rc, expected $2 [$err]"
  [ -z "$3" ] || grep -qF -- "$3" <<< "$err" || fail "$1: no [$3] in [$err]"
}

run key
expect "key" 0 ""
checks=$((checks + 1))
[ "$out" = "key=argocd-images-$REDIS" ] || fail "key: got [$out]"

STUB_RENDER=$stubs/render-two.yaml run key
expect "key, two images" 0 ""
checks=$((checks + 1))
[ "$out" = "key=argocd-images-$OTHER $REDIS" ] || fail "key, two images: got [$out]"

STUB_RENDER=$stubs/render-none.yaml run key
expect "key, nothing to preload" 1 "renders no image outside quay.io and ghcr.io"

STUB_HELM_FAIL=1 run key
expect "key, helm fails" 1 "helm template of Argo CD's chart failed"
checks=$((checks + 1))
! grep -q "renders no image" <<< "$err" || fail "key, helm fails: read as an empty render [$err]"
checks=$((checks + 1))
[ -z "$out" ] || fail "key, helm fails: printed [$out]"

echo "kind: [Deployment" > "$stubs/render-broken.yaml"
STUB_RENDER=$stubs/render-broken.yaml run key
expect "key, render yq cannot read" 1 ""
checks=$((checks + 1))
! grep -q "renders no image" <<< "$err" || fail "key, render yq cannot read: read as an empty render [$err]"

STUB_HELM_FAIL=1 run pull "$stubs/out.tar"
expect "pull, helm fails" 1 "helm template of Argo CD's chart failed"
checks=$((checks + 1))
! grep -q '^pull ' "$stubs/log" || fail "pull, helm fails: pulled anyway"

run import "$stubs/missing.tar" c
expect "import, no archive" 1 "is missing or empty"
checks=$((checks + 1))
! grep -q '^k3d ' "$stubs/log" || fail "import, no archive: k3d ran"
: > "$stubs/images.tar"
run import "$stubs/images.tar" c
expect "import, empty archive" 1 "is missing or empty"

echo x > "$stubs/images.tar"
run import "$stubs/images.tar" c
expect "import" 0 ""
checks=$((checks + 1))
grep -qxF "exec k3d-c-server-0 crictl inspecti $REDIS" "$stubs/log" || fail "import: image not looked for in the node"
STUB_NODE_MISSING=$REDIS run import "$stubs/images.tar" c
expect "import, image not in the node" 1 "$REDIS is not in k3d-c-server-0 after the import"
STUB_RENDER=$stubs/render-two.yaml STUB_NODE_MISSING=$REDIS run import "$stubs/images.tar" c
expect "import, the second of two not in the node" 1 "$REDIS is not in k3d-c-server-0 after the import"
STUB_HELM_FAIL=1 run import "$stubs/images.tar" c
expect "import, helm fails" 1 "helm template of Argo CD's chart failed"

present_of() { echo "Container image \"$1\" already present on machine"; }
pulling_of() { echo "Pulling image \"$1\""; }
events "$(event r Pulled "$(present_of "$REDIS")")" "$(event o Pulled "$(present_of "$OTHER")")" > "$stubs/ev-ok.json"
events "$(event r Pulling "$(pulling_of "$REDIS")")" "$(event o Pulled "$(present_of "$OTHER")")" > "$stubs/ev-redis.json"
events "$(event r Pulled "$(present_of "$REDIS")")" "$(event o Pulling "$(pulling_of "$OTHER")")" > "$stubs/ev-other.json"
events "$(event r Pulling "$(pulling_of "$REDIS")")" "$(event o Pulling "$(pulling_of "$OTHER")")" > "$stubs/ev-both.json"

STUB_EVENTS=$stubs/ev-ok.json STUB_RENDER=$stubs/render-two.yaml run check
expect "check, two images found on the node" 0 ""
STUB_EVENTS=$stubs/ev-redis.json STUB_RENDER=$stubs/render-two.yaml run check
expect "check, the second of two pulled" 1 "$REDIS was pulled"
STUB_EVENTS=$stubs/ev-other.json STUB_RENDER=$stubs/render-two.yaml run check
expect "check, the first of two pulled" 1 "$OTHER was pulled"
# Both judged, not only the first.
STUB_EVENTS=$stubs/ev-both.json STUB_RENDER=$stubs/render-two.yaml run check
expect "check, both pulled: the first said" 1 "$OTHER was pulled"
expect "check, both pulled: the second said" 1 "$REDIS was pulled"
STUB_EVENTS=$stubs/ev-ok.json STUB_HELM_FAIL=1 run check
expect "check, helm fails" 1 "helm template of Argo CD's chart failed"

# pull, against the same stand-ins.

expect_pull() { # name pull-error 0|1 stderr-grep pulls
  local rc=0 err
  checks=$((checks + 1))
  : > "$stubs/log"
  err=$(PATH="$stubs:$PATH" STUB_LOG="$stubs/log" STUB_PULL_ERROR=$2 PRELOAD_RETRY_DELAY=0 STUB_RENDER="$stubs/render-redis.yaml" \
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
