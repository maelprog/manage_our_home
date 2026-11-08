#!/usr/bin/env bash
# Tests of ci/mobile-release-trigger.sh (#743), offline: a throwaway copy
# of the overlays as a git repository of its own, pushes to it as `deploy`
# receives them, and a stand-in gh answering the API calls from fixtures.
# Which push launches the APK release, which does nothing and why, and
# which is red. Needs git and yq (mikefarah, v4) on PATH.
#
#   ci/mobile-release-trigger-test.sh
#
# Exits non-zero if any check fails, after running all of them.

set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
overlays=apps/manage-our-home/overlays
REPO=owner/repo

failures=0
checks=0
ctx=""
fail() { echo "FAIL [$ctx] $*" >&2; failures=$((failures + 1)); }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Full SHAs of main, by name: A…, B…; their first 7 characters are what
# the overlays' comments carry.
full() { printf '%s' "$1" | sha1sum | cut -d' ' -f1; }
short() { full "$1" | cut -c1-7; }

# --- the stand-in gh -----------------------------------------------------
# `gh api <path>` prints $FAKE_GH/<path, / as _>, or fails as an unknown
# path or a refused call would; every call is logged.
mkdir -p "$work/fakebin" "$work/api"
cat > "$work/fakebin/gh" << 'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FAKE_GH_LOG"
[ "$1" = api ] || { echo "fake gh: only 'api' is known" >&2; exit 2; }
f="$FAKE_GH/$(printf '%s' "$2" | tr '/' '_')"
[ -f "$f" ] || { echo "gh: Not Found (HTTP 404) $2" >&2; exit 1; }
cat "$f"
EOF
chmod +x "$work/fakebin/gh"
export FAKE_GH="$work/api" FAKE_GH_LOG="$work/gh.log"

api() { # path < body
  cat > "$FAKE_GH/$(printf '%s' "$1" | tr '/' '_')"
}
# The commit endpoint of a name of main: its full SHA.
api_commit() { # name
  printf '{"sha": "%s", "commit": {"message": "x"}}\n' "$(full "$1")" | api "repos/$REPO/commits/$(short "$1")"
}
files_json() { # file...
  local files="" f
  for f in "$@"; do files="$files${files:+, }{\"filename\": \"$f\", \"status\": \"modified\"}"; done
  printf '[%s]' "$files"
}
# The compare endpoint between two names of main, with the files given;
# AHEAD commits (1 by default).
api_compare() { # before after status file...
  local b=$1 a=$2 status=$3
  shift 3
  printf '{"status": "%s", "ahead_by": %s, "files": %s}\n' "$status" "${AHEAD:-1}" "$(files_json "$@")" \
    | api "repos/$REPO/compare/$(full "$b")...$(full "$a")"
}
# A page of 100 commits of that compare: the commits <name>-<from> to
# <name>-<to>, each commit endpoint listing apps/api/src/<n>.rs.
api_compare_page() { # before after page name from to
  local shas="" i
  for i in $(seq "$5" "$6"); do
    shas="$shas${shas:+, }{\"sha\": \"$(full "$4-$i")\"}"
    printf '{"sha": "%s", "files": %s}\n' "$(full "$4-$i")" "$(files_json "apps/api/src/$i.rs")" \
      | api "repos/$REPO/commits/$(full "$4-$i")"
  done
  printf '{"commits": [%s]}\n' "$shas" \
    | api "repos/$REPO/compare/$(full "$1")...$(full "$2")?per_page=100&page=$3"
}
api_commit_files() { # name file...
  local n=$1
  shift
  printf '{"sha": "%s", "files": %s}\n' "$(full "$n")" "$(files_json "$@")" | api "repos/$REPO/commits/$(full "$n")"
}
for n in A B C D E F G H; do api_commit "$n"; done

# --- the fixture: `deploy` -----------------------------------------------
d="$work/deploy"
mkdir -p "$d"
cp -r "$root/apps" "$root/ci" "$d/"
git -C "$d" init --quiet
commit() { # message
  git -C "$d" -c user.name=t -c user.email=t@t add -A
  git -C "$d" -c user.name=t -c user.email=t@t commit --quiet --allow-empty -m "$1"
  git -C "$d" rev-parse HEAD
}
# Set the comment of every digest of an overlay, as ci/set-images.sh
# writes it, and optionally the api's digest.
comment() { # overlay text
  T="$2" yq -i '(.images[] | select(.name | test("^ghcr\.io/maelprog/manage-our-home-")) | .digest) line_comment = strenv(T)' \
    "$d/$overlays/$1/kustomization.yaml"
}
digest() { # overlay seed
  D="sha256:$(printf '%s' "$2" | sha256sum | cut -d' ' -f1)" \
    yq -i '(.images[] | select(.name == "ghcr.io/maelprog/manage-our-home-api") | .digest) = strenv(D)' \
    "$d/$overlays/$1/kustomization.yaml"
}

comment prod "v0.40.0: main $(short A)"
comment staging "main $(short A)"
c0=$(commit "prod at A")

# expect <release|nothing|red> <description> <before> <after> [grep of the output]
expect() {
  local want=$1 what=$2 before=$3 after=$4 pattern=${5:-}
  local got rc=0
  checks=$((checks + 1))
  : > "$work/output"
  : > "$FAKE_GH_LOG"
  ( cd "$work" && PATH="$work/fakebin:$PATH" GITHUB_REPOSITORY=$REPO GITHUB_OUTPUT="$work/output" \
    "$d/ci/mobile-release-trigger.sh" "$before" "$after" ) > "$work/out" 2>&1 || rc=$?
  if [ "$rc" -ne 0 ]; then
    got=red
  elif grep -qx 'release=true' "$work/output"; then
    got=release
  elif grep -qx 'release=false' "$work/output"; then
    got=nothing
  else
    got="no verdict"
  fi
  if [ "$got" != "$want" ]; then
    fail "$what: expected $want, got $got (exit $rc) — $(cat "$work/out") — output: $(cat "$work/output")"
    return
  fi
  if [ -n "$pattern" ] && ! grep -q "$pattern" "$work/out"; then
    fail "$what: not said '$pattern' — $(cat "$work/out")"
  fi
}
released_sha() { sed -n 's/^sha=//p' "$work/output"; }

# --- the cases -----------------------------------------------------------
ctx=decide

# A merge of prod that moves it to a commit of main touching apps/mobile/.
comment prod "v0.41.0: main $(short B)"
digest prod B
c1=$(commit "prod at B")
api_compare A B ahead apps/api/src/main.rs apps/mobile/android/app/build.gradle.kts
expect release "prod moved, apps/mobile/ touched" "$c0" "$c1"
[ "$(released_sha)" = "$(full B)" ] \
  || fail "released '$(released_sha)', expected the full SHA of B $(full B)"
[ "$(grep -c '^sha=' "$work/output")" = 1 ] || fail "sha written more than once: $(cat "$work/output")"
grep -q "compare/$(full A)...$(full B)" "$FAKE_GH_LOG" \
  || fail "the compare was not asked between the full SHAs of A and B: $(cat "$FAKE_GH_LOG")"

# A merge of prod that moves it to a commit of main not touching it.
comment prod "v0.42.0: main $(short C)"
digest prod C
c2=$(commit "prod at C")
api_compare B C ahead apps/api/src/main.rs apps/web/src/app.rs apps/mobile-notes.md
expect nothing "prod moved, apps/mobile/ not touched" "$c1" "$c2" "apps/mobile/"
grep -q '^sha=' "$work/output" && fail "nothing to release, yet a sha was written: $(cat "$work/output")"

# A merge of prod that changes the version and the digest, not the commit.
comment prod "v0.42.1: main $(short C)"
digest prod C2
c3=$(commit "prod: v0.42.1, same commit")
expect nothing "prod's version and digest moved, same commit of main" "$c2" "$c3" "$(short C)"
[ -s "$FAKE_GH_LOG" ] && fail "same commit, yet the API was asked: $(cat "$FAKE_GH_LOG")"

# A merge of staging alone.
comment staging "main $(short D)"
digest staging D
c4=$(commit "staging at D")
expect nothing "staging moved, prod not" "$c3" "$c4" "overlays/prod"
[ -s "$FAKE_GH_LOG" ] && fail "prod unchanged, yet the API was asked: $(cat "$FAKE_GH_LOG")"

# A merge of anything else (README, CI).
echo "x" >> "$d/README.md"
c5=$(commit "a README")
expect nothing "neither overlay moved" "$c4" "$c5"

# Prod's commit unreadable before (the first merge that carries one).
comment prod "v0.43.0"
c6=$(commit "prod without its commit")
comment prod "v0.43.0: main $(short E)"
c7=$(commit "prod at E")
expect release "prod's commit unreadable before" "$c6" "$c7"
[ "$(released_sha)" = "$(full E)" ] || fail "released '$(released_sha)', expected $(full E)"
grep -q compare "$FAKE_GH_LOG" && fail "nothing to compare with, yet compare was asked: $(cat "$FAKE_GH_LOG")"

# No commit before the push: a created branch (github.event.before is
# zeros), or one this checkout does not have.
expect release "no commit before the push" 0000000000000000000000000000000000000000 "$c7"
[ "$(released_sha)" = "$(full E)" ] || fail "released '$(released_sha)', expected $(full E)"
expect release "a commit before the push not in the checkout" "$(full nowhere)" "$c7"

# Prod's commit unreadable after: red, whatever it was before.
comment prod "v0.44.0"
c8=$(commit "prod without its commit, again")
expect red "prod's commit unreadable after" "$c7" "$c8" "overlays/prod"
expect red "prod's commit unreadable after, unreadable before too" "$c6" "$c8"
# Prod overlay gone after.
git -C "$d" rm --quiet "$overlays/prod/kustomization.yaml"
c9=$(commit "prod overlay removed")
expect red "prod overlay removed" "$c8" "$c9"
git -C "$d" checkout --quiet "$c8" -- "$overlays/prod/kustomization.yaml"

# Back to a commit of main before the one prod ran (a revert): the compare
# says `behind` and lists no file — nothing.
comment prod "v0.42.0: main $(short C)"
c10=$(commit "revert to C")
comment prod "v0.41.0: main $(short B)"
c11=$(commit "revert to B")
api_compare C B behind
expect nothing "prod back to an older commit of main" "$c10" "$c11" "behind"

# The compare lists at most 300 files, apps/api/ first: a list that long
# is completed by the files of each of its commits, 100 commits a page.
comment prod "v0.45.0: main $(short F)"
c12=$(commit "prod at F")
many=()
for i in $(seq 1 300); do many+=("apps/api/src/f$i.rs"); done
AHEAD=150 api_compare B F ahead "${many[@]}"
api_compare_page B F 1 P 1 100
api_compare_page B F 2 P 101 150
expect nothing "300 files compared, none of 150 commits in apps/mobile/" "$c11" "$c12" "150 commits"
grep -q "compare/$(full B)...$(full F)?per_page=100&page=2" "$FAKE_GH_LOG" \
  || fail "page 2 of the commits not asked: $(grep compare "$FAKE_GH_LOG")"
n=$(grep -cE "^api repos/$REPO/commits/[0-9a-f]{40}$" "$FAKE_GH_LOG" || true)
[ "$n" = 150 ] || fail "expected the files of 150 commits asked, got $n"
api_commit_files P-120 apps/api/src/x.rs apps/mobile/lib/main.dart
expect release "300 files compared, commit 120 of 150 in apps/mobile/" "$c11" "$c12" "$(full P-120)"
[ "$(released_sha)" = "$(full F)" ] || fail "released '$(released_sha)', expected $(full F)"
api_commit_files P-120 apps/mobile-notes.md apps/api/src/apps/mobile/x.rs
expect nothing "300 files compared, a commit beside apps/mobile/, not in it" "$c11" "$c12" "150 commits"
api_commit_files P-120 apps/api/src/120.rs
printf '{"commits": [{"sha": "%s"}, {"sha": "%s; echo pwned"}]}\n' "$(full P-101)" "$(full P-102)" \
  | api "repos/$REPO/compare/$(full B)...$(full F)?per_page=100&page=2"
expect red "300 files compared, a page of commits listing a SHA that is not one" "$c11" "$c12" "lists '$(full P-102);' as a commit"
api_compare_page B F 2 P 101 150
api_commit_files P-30 "${many[@]}"
expect release "300 files compared, a commit of 300 files" "$c11" "$c12" "$(full P-30) of it lists 300"
api_commit_files P-30 apps/api/src/30.rs
printf '{"commits": []}\n' | api "repos/$REPO/compare/$(full B)...$(full F)?per_page=100&page=2"
expect red "300 files compared, page 2 of the commits empty" "$c11" "$c12" "page 2 lists none"
api_compare_page B F 2 P 101 150
printf '{"sha": "%s"}\n' "$(full P-140)" | api "repos/$REPO/commits/$(full P-140)" # no files
rm "$FAKE_GH/$(printf '%s' "repos/$REPO/commits/$(full P-140)" | tr '/' '_')"
expect red "300 files compared, the files of a commit refused" "$c11" "$c12" "$(full P-140)"
api_compare_page B F 2 P 101 150
AHEAD=null api_compare B F ahead "${many[@]}"
expect red "300 files compared, no number of commits" "$c11" "$c12"
AHEAD=150 api_compare B F ahead "${many[@]}"

# The API refuses or does not know: red, never a guess.
comment prod "v0.46.0: main $(short G)"
c13=$(commit "prod at G")
expect red "the compare refused" "$c12" "$c13"
# A full SHA that is not one, or not the short one's: on a push with no
# commit before it, so that the SHA alone decides.
zeros=0000000000000000000000000000000000000000
printf '{"sha": "%s"}\n' "$(full H)" | api "repos/$REPO/commits/$(short G)"
expect red "the API resolves the short SHA to another commit" "$zeros" "$c13"
printf '{"sha": "%s; echo pwned"}\n' "$(full G)" | api "repos/$REPO/commits/$(short G)"
expect red "the API's SHA is not 40 hexadecimal characters" "$zeros" "$c13"
printf '{"sha": "%s"}\n' "$(full G | tr a-f A-F)" | api "repos/$REPO/commits/$(short G)"
expect red "the API's SHA in capitals" "$zeros" "$c13"
api_compare F G ahead apps/mobile/x
api_commit G
rm "$FAKE_GH/$(printf '%s' "repos/$REPO/commits/$(short F)" | tr '/' '_')"
expect red "the commit before unknown to the API" "$c12" "$c13"
api_commit F
expect release "the same push, once the API answers" "$c12" "$c13"
[ "$(released_sha)" = "$(full G)" ] || fail "released '$(released_sha)', expected $(full G)"

# The arguments: two of them, the after one a commit of the checkout.
ctx=usage
checks=$((checks + 1))
if ( cd "$work" && GITHUB_OUTPUT="$work/output" GITHUB_REPOSITORY=$REPO "$d/ci/mobile-release-trigger.sh" "$c0" ) > /dev/null 2>&1; then
  fail "one argument accepted"
fi
expect red "the commit after not in the checkout" "$c0" "$(full nowhere)"

echo "$checks checks, $failures failed"
[ "$failures" -eq 0 ]
