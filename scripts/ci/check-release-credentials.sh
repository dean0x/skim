#!/usr/bin/env bash
#
# Check the publish credentials of a release before the first publish.
#
# Usage:
#   scripts/ci/check-release-credentials.sh npm
#   scripts/ci/check-release-credentials.sh homebrew
#
# A crates.io version can not be removed, so a release that can not reach
# every registry must stop before the first publish (#607). The release
# workflow runs this script in its `check-credentials` job, and every publish
# job needs that job.
#
# npm (env: NODE_AUTH_TOKEN, NPM_PLATFORM_PACKAGES)
#   1. `npm whoami` must pass. A token that is not valid fails with E401.
#   2. For each package (rskim and @rskim/<platform>), read the `latest`
#      dist-tag, then set `latest` to that same version with
#      `npm dist-tag add`. The registry accepts that write only from a token
#      that can write the package, so a read-only token or a token without
#      that package fails with E403. The tag keeps its value, so the probe
#      changes nothing.
#   The script does not read `npm access list packages <user>`. That
#   listing tells what the USER can do, not what the TOKEN can do, and the
#   registry refuses it (E403) for a granular token that can publish.
#
# homebrew (env: HOMEBREW_TAP_TOKEN)
#   Send the same repository dispatch as the `trigger-homebrew` job, with an
#   event type that no tap workflow listens for. The tap workflow runs only
#   for `update-formula`. HTTP 204 means the token can send the dispatch; any
#   HTTP error fails the check.

set -uo pipefail

readonly TAP_REPO="dean0x/homebrew-tap"
readonly SEMVER_RE='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'

fail() {
  echo "::error::$1"
  exit 1
}

check_npm() {
  local user
  if ! user=$(npm whoami); then
    fail "npm check: NPM_TOKEN is not valid (npm whoami failed)"
  fi
  echo "npm token user: ${user}"

  local packages=(rskim)
  local platform
  for platform in ${NPM_PLATFORM_PACKAGES:?NPM_PLATFORM_PACKAGES is not set}; do
    packages+=("@rskim/${platform}")
  done

  local failed=0 pkg latest
  for pkg in "${packages[@]}"; do
    if ! latest=$(npm view "${pkg}" dist-tags.latest) || ! [[ "${latest}" =~ ${SEMVER_RE} ]]; then
      echo "::error::npm check: can not read the latest version of ${pkg}"
      failed=1
      continue
    fi
    if npm dist-tag add "${pkg}@${latest}" latest > /dev/null; then
      echo "✅ ${pkg}: the token can write (latest stays ${latest})"
    else
      echo "::error::npm check: the token of ${user} can not write ${pkg}, so it can not publish it"
      failed=1
    fi
  done
  return "${failed}"
}

check_homebrew() {
  if ! curl -sS --fail-with-body -X POST \
    -H "Authorization: Bearer ${HOMEBREW_TAP_TOKEN:-}" \
    -H "Accept: application/vnd.github+json" \
    "https://api.github.com/repos/${TAP_REPO}/dispatches" \
    -d '{"event_type":"skim-release-credential-check"}'; then
    echo ""
    fail "Homebrew tap check: HOMEBREW_TAP_TOKEN can not send a dispatch to ${TAP_REPO}"
  fi
  echo "✅ Homebrew tap dispatch accepted"
}

case "${1:-}" in
  npm) check_npm ;;
  homebrew) check_homebrew ;;
  *)
    echo "usage: $0 npm|homebrew" >&2
    exit 2
    ;;
esac
