#!/usr/bin/env bash
# Whether a push to `deploy` launches the release of the Android APK (#743),
# for .github/workflows/mobile-release-trigger.yml. The APK follows prod: it
# is released when prod moves to a commit of `main` that changes a file of
# apps/mobile/.
#
#   ci/mobile-release-trigger.sh <before> <after>
#
# <before> and <after> are the commits of `deploy` before and after the
# push (github.event.before and github.sha), in the checkout this script is
# in, with its history. The commit of `main` prod runs is read from the
# comment of the api digest in the prod overlay, `# vX.Y.Z: main <sha7>`,
# as promote.yml's commit_of reads it.
#
#   - the prod overlay unchanged: nothing (a merge of staging, of the CI…);
#   - the commit unreadable after: red — prod would run an unknown commit;
#   - the commit unreadable before (or no commit before the push): release;
#   - the same commit before and after (a new version, or digest, of the
#     same commit): nothing;
#   - otherwise the API's compare of `main`, before...after: release if a
#     file of apps/mobile/ changed, nothing if none did. A move back to an
#     older commit (a revert) compares as `behind`, with no file: nothing.
#     The compare lists at most 300 files, and apps/api/ alone can fill
#     it (6a4bd32...5de48f0: 300 files, none under apps/mobile/ though a
#     commit of it changes that, the last one under apps/api/): a list of 300 is then completed by the files of each
#     commit of the compare, one by one, until one changes apps/mobile/. A
#     commit listing 300 files of its own is taken as one that does. A
#     file changed by one commit and put back by a later one counts.
#
# Writes `release=true` and `sha=<the full SHA of after's commit>`, or
# `release=false`, to $GITHUB_OUTPUT; says why on stdout. The full SHA is
# the API's, checked to be 40 hexadecimal characters that start with the
# short one. Any failure of the API is red, never a guess.
#
# Needs git, yq (mikefarah, v4) and gh on PATH, GITHUB_REPOSITORY and
# GITHUB_OUTPUT set, and GH_TOKEN for gh (contents: read).

set -euo pipefail

if [ $# -ne 2 ]; then
  echo "usage: $0 <before> <after>" >&2
  exit 2
fi
before=$1 after=$2
: "${GITHUB_REPOSITORY:?}" "${GITHUB_OUTPUT:?}"
root=$(cd "$(dirname "$0")/.." && pwd)
prod=apps/manage-our-home/overlays/prod/kustomization.yaml
API_IMAGE=ghcr.io/maelprog/manage-our-home-api

g() { git -C "$root" "$@"; }
nothing() {
  echo "::notice::no APK release: $*"
  echo "release=false" >> "$GITHUB_OUTPUT"
  exit 0
}
red() {
  echo "::error::$*" >&2
  exit 1
}
# The commit of `main` of the prod overlay at a commit of `deploy`, or
# nothing.
commit_at() { # commit
  g show "$1:$prod" 2> /dev/null \
    | N="$API_IMAGE" yq '.images[] | select(.name == env(N)) | .digest | line_comment' - 2> /dev/null \
    | grep -oE 'main [0-9a-f]{7,40}' | head -n 1 | cut -d' ' -f2 || true
}
# The full SHA of a commit of the repository, from the API.
full_sha() { # short
  local sha
  sha=$(gh api "repos/${GITHUB_REPOSITORY}/commits/$1" | yq -p json '.sha') \
    || red "the API does not resolve commit $1 of main"
  [[ "$sha" =~ ^[0-9a-f]{40}$ ]] && [[ "$sha" == "$1"* ]] \
    || red "the API resolves commit $1 of main to '${sha}', not a full SHA of it"
  echo "$sha"
}
release() { # short of after, why
  local sha
  sha=$(full_sha "$1")
  echo "::notice::APK release of main ${sha}: $2"
  {
    echo "release=true"
    echo "sha=${sha}"
  } >> "$GITHUB_OUTPUT"
  exit 0
}

g cat-file -e "${after}^{commit}" 2> /dev/null || red "commit ${after} is not in this checkout of deploy"
has_before=false
if [[ "$before" =~ ^[0-9a-f]{7,40}$ ]] && g cat-file -e "${before}^{commit}" 2> /dev/null; then
  has_before=true
fi

if [ "$has_before" = true ] && g diff --quiet "$before" "$after" -- "$prod"; then
  nothing "the push leaves $prod as it was"
fi

a=$(commit_at "$after")
[ -n "$a" ] || red "$prod at ${after}: no 'main <sha>' in the comment of the api digest, the commit of main prod runs is unknown"

b=""
[ "$has_before" = false ] || b=$(commit_at "$before")
if [ -z "$b" ]; then
  release "$a" "the commit prod ran before the push is unknown (no commit before it, or no 'main <sha>' in $prod)"
fi
if [ "${a::7}" = "${b::7}" ]; then
  nothing "prod still runs main ${a} (its version or digests moved, not its commit)"
fi

A=$(full_sha "$a")
B=$(full_sha "$b")
compare=$(gh api "repos/${GITHUB_REPOSITORY}/compare/${B}...${A}") \
  || red "the API does not compare main ${b}...${a}"
status=$(yq -p json '.status' <<< "$compare")
count=$(yq -p json '.files // [] | length' <<< "$compare")
mobile=$(yq -p json '[.files // [] | .[] | .filename | select(test("^apps/mobile/"))] | length' <<< "$compare")
if [ "$mobile" -gt 0 ]; then
  release "$a" "prod moved from main ${b} to ${a}, which changes ${mobile} file(s) of apps/mobile/"
fi
if [ "$count" -lt 300 ]; then
  nothing "prod moved from main ${b} to ${a} (${status}), and no file of apps/mobile/ changed (${count} compared)"
fi

# 300 files: the commits of the compare, 100 a page, and the files of each.
total=$(yq -p json '.ahead_by' <<< "$compare")
[[ "$total" =~ ^[0-9]+$ ]] || red "the compare of main ${b}...${a} gives no number of commits ('${total}')"
seen=0 page=1
while [ "$seen" -lt "$total" ]; do
  shas=$(gh api "repos/${GITHUB_REPOSITORY}/compare/${B}...${A}?per_page=100&page=${page}" | yq -p json '.commits // [] | .[] | .sha') \
    || red "the API does not list page ${page} of the commits of main ${b}...${a}"
  [ -n "$shas" ] || red "the compare of main ${b}...${a} has ${total} commits, page ${page} lists none after ${seen}"
  for c in $shas; do
    [[ "$c" =~ ^[0-9a-f]{40}$ ]] || red "the compare of main ${b}...${a} lists '${c}' as a commit"
    files=$(gh api "repos/${GITHUB_REPOSITORY}/commits/${c}" | yq -p json '.files // [] | .[] | .filename') \
      || red "the API does not list the files of commit ${c}"
    if grep -q '^apps/mobile/' <<< "$files"; then
      release "$a" "prod moved from main ${b} to ${a}, and commit ${c} of it changes apps/mobile/ (the compare lists 300 files, at most what the API shows)"
    fi
    if [ "$(grep -c . <<< "$files")" -ge 300 ]; then
      release "$a" "prod moved from main ${b} to ${a}, and commit ${c} of it lists 300 files, at most what the API shows: apps/mobile/ may be among the others"
    fi
    seen=$((seen + 1))
  done
  page=$((page + 1))
done
nothing "prod moved from main ${b} to ${a} (${status}), and none of its ${seen} commits changes a file of apps/mobile/ (the compare lists 300 files, at most what the API shows)"
