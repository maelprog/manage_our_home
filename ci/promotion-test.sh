#!/usr/bin/env bash
# Tests of the promotion scripts (#436), on throwaway copies of this
# branch: ci/set-images.sh, which the bot runs to move an overlay's
# digests; ci/promotion-check.sh, which makes a PR red when it moves prod
# to a pair that is not the one running in staging; and
# ci/verify-signatures.sh, against a stand-in cosign; and the ruleset of
# `deploy`, .github/rulesets/deploy.json. Offline. Needs git, kustomize
# and yq (mikefarah, v4) on PATH.
#
#   ci/promotion-test.sh
#
# Exits non-zero if any check fails, after running all of them.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
overlays=apps/manage-our-home/overlays
API=ghcr.io/maelprog/manage-our-home-api
WEB=ghcr.io/maelprog/manage-our-home-web
# MinIO and its client, at the tag of the base: another digest of the same
# repository and tag is what a patch of one overlay would move them to.
MINIO=docker.io/bitnamilegacy/minio:2025.7.23-debian-12-r5
MC=docker.io/bitnamilegacy/minio-client:2025.7.21-debian-12-r3

failures=0
checks=0
ctx=""
fail() { echo "FAIL [$ctx] $*" >&2; failures=$((failures + 1)); }
check() { checks=$((checks + 1)); }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# A digest of its own for each name: P1-api, P1-web, ...
digest() { echo "sha256:$(printf '%s' "$1" | sha256sum | cut -d' ' -f1)"; }

# A copy of what the scripts read, as a git repository of its own.
fixture() { # dir
  mkdir -p "$1/platform/components"
  cp -r "$root/apps" "$root/ci" "$1/"
  cp -r "$root/platform/components/kyverno" "$1/platform/components/"
  git -C "$1" init --quiet
  git -C "$1" -c user.name=t -c user.email=t@t add -A
  git -C "$1" -c user.name=t -c user.email=t@t commit --quiet -m init
}
commit() { # dir message
  git -C "$1" -c user.name=t -c user.email=t@t commit --quiet --allow-empty -am "$2"
}
set_pair() { # dir overlay pair-name
  "$1/ci/set-images.sh" "$2" "$3" "$(digest "$3-api")" "$(digest "$3-web")"
}
image_digest() { # dir overlay image
  N="$3" yq '.images[] | select(.name == env(N)) | .digest' "$1/$overlays/$2/kustomization.yaml"
}
# The first container of <kind>/<name> to <image>, by a patch of one
# kustomization.
patch_image() { # kustomization kind name image
  K="$2" W="$3" P="- op: replace
  path: /spec/template/spec/containers/0/image
  value: $4" yq -i '.patches += [{"target": {"kind": strenv(K), "name": strenv(W)}, "patch": strenv(P)}]' "$1"
}

# --- ci/set-images.sh ----------------------------------------------------
ctx=set-images
d="$work/set"
fixture "$d"

check
if set_pair "$d" staging P1 > "$work/out" 2>&1; then
  [ "$(image_digest "$d" staging "$API")" = "$(digest P1-api)" ] \
    || fail "staging api digest not moved"
  [ "$(image_digest "$d" staging "$WEB")" = "$(digest P1-web)" ] \
    || fail "staging web digest not moved"
  # Only the two digest lines change: comments, the rest of the overlay
  # and the other overlay as they were.
  changed=$(git -C "$d" diff --numstat | awk '{ print $1 "+" $2 " " $3 }')
  [ "$changed" = "2+2 $overlays/staging/kustomization.yaml" ] \
    || fail "expected 2 lines changed in staging only, got: $changed"
  kustomize build "$d/$overlays/staging" > "$work/render" 2>&1 \
    || fail "staging no longer renders: $(cat "$work/render")"
  grep -q "image: $API@$(digest P1-api)" "$work/render" \
    || fail "the render does not run $API@$(digest P1-api)"
  # The source is written next to each digest, for whoever reads a revert.
  grep -q "digest: $(digest P1-api) # P1$" "$d/$overlays/staging/kustomization.yaml" \
    || fail "no '# P1' comment on the api digest"
else
  fail "set-images.sh staging failed: $(cat "$work/out")"
fi

check
commit "$d" P1
set_pair "$d" staging P1 > /dev/null 2>&1 || fail "second identical run failed"
[ -z "$(git -C "$d" status --porcelain)" ] \
  || fail "an identical second run changed the tree (an open PR updated with nothing would get a commit)"

for bad in latest "sha256:abc" "sha256:$(digest x | cut -d: -f2 | tr a-f A-F)" "$(digest x)@x"; do
  check
  if "$d/ci/set-images.sh" prod P9 "$bad" "$(digest P9-web)" > /dev/null 2>&1; then
    fail "accepted '$bad' as a digest"
  fi
  [ -z "$(git -C "$d" status --porcelain)" ] || fail "'$bad' refused, but the tree changed"
  git -C "$d" checkout --quiet -- .
done

# An overlay naming the api twice: refused, and said so, rather than
# both entries rewritten.
check
N="$API" yq -i '.images += [.images[] | select(.name == env(N))]' "$d/$overlays/prod/kustomization.yaml"
git -C "$d" -c user.name=t -c user.email=t@t commit --quiet -am "api twice"
if "$d/ci/set-images.sh" prod P9 "$(digest P9-api)" "$(digest P9-web)" > "$work/out" 2>&1; then
  fail "accepted an overlay that names the api twice"
fi
grep -q "expected one image named $API, found 2" "$work/out" \
  || fail "the api named twice, not said so: $(cat "$work/out")"
[ -z "$(git -C "$d" status --porcelain)" ] || fail "the api named twice, yet the tree changed"
git -C "$d" reset --quiet --hard HEAD~1

check
if "$d/ci/set-images.sh" dev P9 "$(digest P9-api)" "$(digest P9-web)" > /dev/null 2>&1; then
  fail "accepted an overlay that does not exist"
fi

check
if "$d/ci/set-images.sh" prod "" "$(digest P9-api)" "$(digest P9-web)" > /dev/null 2>&1; then
  fail "accepted an empty source"
fi

# --- ci/promotion-check.sh -----------------------------------------------
# The base branch: staging moved to P1, promoted; staging to P2, then P4,
# promoted; one commit moving both to P5; staging to P6. At its tip,
# staging runs P6 and prod P5; prod has run P0 (the pair of this branch,
# as committed), P1, P4 and P5 — P4 at a commit that touched prod alone,
# and no commit touching staging saw it in prod. Then MinIO, by a patch
# of each overlay: staging to M1, promoted; staging to M2. At the tip,
# staging's MinIO is M2 and prod's M1; prod's ran M0 (the base's) first.
ctx=promotion-check
base="$work/base"
fixture "$base"
set_pair "$base" staging P1 > /dev/null && commit "$base" "staging P1"
set_pair "$base" prod P1 > /dev/null && commit "$base" "prod P1"
set_pair "$base" staging P2 > /dev/null && commit "$base" "staging P2"
set_pair "$base" staging P4 > /dev/null && commit "$base" "staging P4"
set_pair "$base" prod P4 > /dev/null && commit "$base" "prod P4"
set_pair "$base" staging P5 > /dev/null && set_pair "$base" prod P5 > /dev/null \
  && commit "$base" "staging and prod P5"
set_pair "$base" staging P6 > /dev/null && commit "$base" "staging P6"
patch_image "$base/$overlays/staging/kustomization.yaml" StatefulSet minio "$MINIO@$(digest M1)" \
  && commit "$base" "staging MinIO M1"
patch_image "$base/$overlays/prod/kustomization.yaml" StatefulSet minio "$MINIO@$(digest M1)" \
  && commit "$base" "prod MinIO M1"
patch_image "$base/$overlays/staging/kustomization.yaml" StatefulSet minio "$MINIO@$(digest M2)" \
  && commit "$base" "staging MinIO M2"

# expect <pass|fail> <description> <command on the head copy $h>...
expect() {
  local want=$1 what=$2
  shift 2
  check
  h="$work/head-$checks"
  git clone --quiet "$base" "$h"
  if ! "$@" > "$work/out" 2>&1; then
    fail "$what: could not set the head up — $(cat "$work/out")"
    return
  fi
  if "$h/ci/promotion-check.sh" "$base" > "$work/out" 2>&1; then got=pass; else got=fail; fi
  [ "$got" = "$want" ] || fail "$what: expected $want, got $got — $(cat "$work/out")"
}
# Set one overlay of the head to a pair: `$h` is read when it runs, not
# when `expect` is called.
head_pair() { set_pair "$h" "$1" "$2"; }
noop() { :; }
mixed() { # prod api from P6, web from P5
  "$h/ci/set-images.sh" prod mixed "$(digest P6-api)" "$(digest P5-web)"
}
both_new() { set_pair "$h" staging P3 && set_pair "$h" prod P3; }
initial_prod() { git -C "$base" show "$(git -C "$base" rev-list --max-parents=0 HEAD):$overlays/prod/kustomization.yaml" > "$h/$overlays/prod/kustomization.yaml"; }

expect pass "nothing moved" noop
expect pass "staging moved, prod not" head_pair staging P3
expect pass "prod moved to the pair staging runs" head_pair prod P6
expect pass "prod reverted to the pair it ran first (P0)" initial_prod
expect pass "prod reverted to a pair it ran at a commit of its own (P4)" head_pair prod P4
expect fail "prod moved to a pair that ran nowhere" head_pair prod P3
expect fail "prod moved to a pair staging ran but no longer runs" head_pair prod P2
expect fail "prod moved to staging's api and an older web" mixed
expect fail "staging and prod moved together to a new pair" both_new

# What the overlay runs is what kustomize renders, not only the `digest:`
# lines: anything else that moves an application image is refused, the
# pair of the file left as it is.
prod_file() { echo "$h/$overlays/prod/kustomization.yaml"; }
new_name() {
  N="$API" yq -i '(.images[] | select(.name == env(N))).newName = "ghcr.io/evil/manage-our-home-api"' "$(prod_file)"
}
new_tag() {
  N="$API" yq -i '(.images[] | select(.name == env(N))).newTag = "latest"' "$(prod_file)"
}
image_patch() { # the api container to an image of elsewhere, by a patch
  P="- op: replace
  path: /spec/template/spec/containers/0/image
  value: ghcr.io/evil/manage-our-home-api@$(digest P3-api)" yq -i '.patches += [{"target": {"kind": "Deployment", "name": "api"}, "patch": strenv(P)}]' "$(prod_file)"
}
image_patch_of() { patch_image "$(prod_file)" "$@"; } # <kind> <name> <image>
api_image_patch() { image_patch_of Deployment api "$1"; }
bare_pod() { # a Pod of its own in the prod overlay, on an image of elsewhere
  cat > "$h/$overlays/prod/pod.yaml" << EOF
apiVersion: v1
kind: Pod
metadata:
  name: extra
spec:
  containers:
    - name: thing
      image: ghcr.io/evil/thing@$(digest evil)
EOF
  yq -i '.resources += ["pod.yaml"]' "$(prod_file)"
}
minio_as_api() { # an application image run by MinIO's container
  image_patch_of StatefulSet minio "$API@$(digest P3-api)"
}
api_as_minio() { # an image the render holds elsewhere, for MinIO
  api_image_patch "docker.io/bitnamilegacy/minio:2025.7.23-debian-12-r5@$(digest evil)"
}
api_as_web() { # the web image of the pair, on the api container
  api_image_patch "$WEB@$(image_digest "$h" prod "$WEB")"
}
reordered() { # staging's pair, web listed before api
  head_pair prod P6 && yq -i '.images |= reverse' "$(prod_file)"
}
# And for the reason given: the check says why, not merely fails.
said() { grep -q "$1" "$work/out" || fail "$2: not refused for '$1' — $(cat "$work/out")"; }
expect fail "an api image renamed by newName" new_name
said "name and digest only, not: newName" newName
expect fail "an api image given a newTag" new_tag
said "name and digest only, not: newTag" newTag
expect fail "the api container's image moved by a patch" image_patch
said "prod renders Deployment/api/api" patch
expect fail "the api container on a MinIO image, by a patch" api_as_minio
said "Deployment/api/api" "api on MinIO"
expect fail "the api container on the web image of the pair, by a patch" api_as_web
said "Deployment/api/api" "api on web"
expect fail "MinIO's container on an application image, by a patch" minio_as_api
said "StatefulSet/minio/minio" "MinIO on api"
expect fail "a bare Pod added to prod, on an image of elsewhere" bare_pod
said "Pod/extra/thing" "bare Pod"
expect pass "prod moved to the pair staging runs, web listed first" reordered

# Every image the render holds, a container's or a volume's, and
# ephemeralContainers as much as the others.
api_pod_patch() { # a JSON patch of the api Deployment
  P="$1" yq -i '.patches += [{"target": {"kind": "Deployment", "name": "api"}, "patch": strenv(P)}]' "$(prod_file)"
}
ephemeral_unknown() {
  api_pod_patch '[{"op": "add", "path": "/spec/template/spec/ephemeralContainers", "value": [{"name": "debug", "image": "ghcr.io/evil/thing@'"$(digest evil)"'"}]}]'
}
ephemeral_web() { # an ephemeral container named as the api's, on the web image
  api_pod_patch '[{"op": "add", "path": "/spec/template/spec/ephemeralContainers", "value": [{"name": "api", "image": "'"$WEB@$(image_digest "$h" prod "$WEB")"'"}]}]'
}
volume_unknown() {
  api_pod_patch '[{"op": "add", "path": "/spec/template/spec/volumes", "value": [{"name": "tools", "image": {"reference": "ghcr.io/evil/thing@'"$(digest evil)"'"}}]}]'
}
expect fail "an ephemeral container on an image of elsewhere" ephemeral_unknown
said "image this check does not know: Deployment/api/debug" "ephemeral unknown"
expect fail "an ephemeral container on the image of another application" ephemeral_web
said "prod renders Deployment/api/api with '$WEB@" "ephemeral web"
expect fail "a volume on an image of elsewhere" volume_unknown
said "image this check does not know: Deployment/api/volume:tools" "volume unknown"

# MinIO and its client: prod runs the image staging runs, container by
# container, tag and digest, as for the application's pair.
prod_minio() { patch_image "$(prod_file)" StatefulSet minio "$MINIO@$(digest "$1")"; }
staging_minio() { patch_image "$h/$overlays/staging/kustomization.yaml" StatefulSet minio "$MINIO@$(digest "$1")"; }
both_minio() { staging_minio "$1" && prod_minio "$1"; }
prod_bucket() { patch_image "$(prod_file)" Job minio-bucket "$MC@$(digest "$1")"; }
prod_minio_first() { yq -i 'del(.patches)' "$(prod_file)"; } # the revert of M1's promotion
mc_base=$(yq '.spec.template.spec.containers[0].image' "$root/apps/manage-our-home/base/minio/bucket-job.yaml")
expect pass "prod's MinIO moved to the one staging runs" prod_minio M2
expect pass "prod's MinIO back to the one it ran first (M0)" prod_minio_first
expect fail "prod's MinIO moved alone to another digest of its repository" prod_minio M3
said "StatefulSet/minio/minio" "MinIO elsewhere"
said "$MINIO@$(digest M3)" "MinIO elsewhere, proposed"
said "$MINIO@$(digest M2)" "MinIO elsewhere, staging's"
expect fail "prod's MinIO client moved alone to another digest of its repository" prod_bucket M3
said "Job/minio-bucket/bucket" "client elsewhere"
said "$MC@$(digest M3)" "client elsewhere, proposed"
said "$mc_base" "client elsewhere, staging's"
expect fail "staging's and prod's MinIO moved together to a new digest" both_minio M3
said "StatefulSet/minio/minio" "MinIO moved together"
# Two containers of one name, MinIO's moved, an initContainer beside it
# on the image prod runs: each is checked, neither hides the other.
minio_twice() {
  prod_minio M3 && P='[{"op": "add", "path": "/spec/template/spec/initContainers", "value": [{"name": "minio", "image": "'"$MINIO@$(digest M1)"'"}]}]' \
    yq -i '.patches += [{"target": {"kind": "StatefulSet", "name": "minio"}, "patch": strenv(P)}]' "$(prod_file)"
}
expect fail "prod's MinIO moved, an initContainer of the same name on the image prod runs" minio_twice
said "prod renders StatefulSet/minio/minio more than once" "MinIO twice"

# A base with no overlay yet — `deploy` before `k3s` is merged into it.
check
empty="$work/empty"
mkdir -p "$empty"
git -C "$empty" init --quiet
git -C "$empty" -c user.name=t -c user.email=t@t commit --quiet --allow-empty -m root
h="$work/head-empty"
git clone --quiet "$base" "$h"
"$h/ci/promotion-check.sh" "$empty" > "$work/out" 2>&1 \
  || fail "a base without a prod overlay: expected pass — $(cat "$work/out")"

# --- ci/verify-signatures.sh ---------------------------------------------
ctx=verify-signatures
v="$work/verify"
fixture "$v"
set_pair "$v" staging P1 > /dev/null 2>&1 || fail "could not set the fixture up"
policy="$v/platform/components/kyverno/signed-images.yaml"
issuer=$(yq '.spec.rules[0].verifyImages[0].attestors[0].entries[0].keyless.issuer' "$policy")
subject=$(yq '.spec.rules[0].verifyImages[0].attestors[0].entries[0].keyless.subjectRegExp' "$policy")
mkdir -p "$work/fakebin"
# The stand-in cosign logs its arguments, one call per line, and refuses
# the reference named by REFUSE.
cat > "$work/fakebin/cosign" << 'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$COSIGN_LOG"
for a in "$@"; do [ "$a" = "${REFUSE:-}" ] && exit 1; done
exit 0
EOF
chmod +x "$work/fakebin/cosign"

check
export COSIGN_LOG="$work/cosign.log"
: > "$COSIGN_LOG"
if PATH="$work/fakebin:$PATH" "$v/ci/verify-signatures.sh" > "$work/out" 2>&1; then
  # Four images: staging's P1 pair, prod's P0 pair.
  [ "$(grep -c '^verify ' "$COSIGN_LOG")" = 4 ] \
    || fail "expected 4 cosign verify calls, got: $(cat "$COSIGN_LOG")"
  for ref in "$API@$(digest P1-api)" "$WEB@$(digest P1-web)" \
    "$API@$(image_digest "$v" prod "$API")" "$WEB@$(image_digest "$v" prod "$WEB")"; do
    grep -qF "verify $ref --certificate-oidc-issuer $issuer --certificate-identity-regexp $subject" "$COSIGN_LOG" \
      || fail "$ref not verified against the Kyverno policy's identity: $(cat "$COSIGN_LOG")"
  done
else
  fail "all signed, yet failed: $(cat "$work/out")"
fi

check
if PATH="$work/fakebin:$PATH" REFUSE="$WEB@$(digest P1-web)" "$v/ci/verify-signatures.sh" > "$work/out" 2>&1; then
  fail "staging's web unsigned, yet passed"
fi

# --- .github/rulesets/deploy.json ----------------------------------------
# The ruleset of `deploy`, as applied by the README's command: nothing
# reaches the branch but through a PR, nobody bypasses it, and every job
# of deploy.yml must be green before the merge.
ctx=ruleset
rs="$root/.github/rulesets/deploy.json"
q() { yq -p json -o json -I0 "$1" "$rs"; }
check
[ "$(q '.target')" = '"branch"' ] && [ "$(q '.enforcement')" = '"active"' ] \
  || fail "not an active branch ruleset"
check
[ "$(q '.conditions.ref_name.include')" = '["refs/heads/deploy"]' ] \
  || fail "does not target refs/heads/deploy alone: $(q '.conditions.ref_name.include')"
check
[ "$(q '.bypass_actors')" = '[]' ] || fail "bypass list not empty: $(q '.bypass_actors')"
for rule in deletion non_fast_forward pull_request required_status_checks; do
  check
  [ "$(R=$rule q '[.rules[] | select(.type == env(R))] | length')" = 1 ] \
    || fail "no '$rule' rule"
done
check
required=$(q '[.rules[] | select(.type == "required_status_checks") | .parameters.required_status_checks[].context] | sort')
jobs=$(yq -o json -I0 '.jobs | keys | sort' "$root/.github/workflows/deploy.yml")
[ "$required" = "$jobs" ] || fail "required checks $required, jobs of deploy.yml $jobs"

echo "$checks checks, $failures failed"
[ "$failures" -eq 0 ]
