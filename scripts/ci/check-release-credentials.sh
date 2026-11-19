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
#      dist-tag with `npm view`. Then send
#      `PUT /-/package/<pkg>/dist-tags/latest` with that same version, and
#      require HTTP 2xx. The registry accepts that write only from a token
#      that can write the package, so a read-only token or a token without
#      that package gets 401 or 403. The tag keeps its value, so the probe
#      changes nothing.
#   The probe uses curl, not `npm dist-tag add`: npm sends no PUT when the
#   tag already has the version, so it would exit 0 for any valid token.
#   The script does not read `npm access list packages <user>`. That
#   listing tells what the USER can do, not what the TOKEN can do, and the
#   registry refuses it (E403) for a granular token that can publish.
#
# homebrew (env: HOMEBREW_TAP_TOKEN)
#   Send the same repository dispatch as the `trigger-homebrew` job, with an
#   event type that no tap workflow listens for. The tap workflow runs only
#   for `update-formula`. Only HTTP 204 passes.
#
# curl reads each token from a config on stdin, so no token is in argv.

set -uo pipefail

readonly NPM_REGISTRY="https://registry.npmjs.org"
readonly TAP_REPO="dean0x/homebrew-tap"
readonly SEMVER_RE='^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$'

fail() {
  echo "::error::$1"
  exit 1
}

# auth_config TOKEN — a curl config line that sends TOKEN as a Bearer header.
auth_config() {
  printf 'header = "Authorization: Bearer %s"\n' "$1"
}

# http_status TOKEN ACCEPT METHOD URL BODY — prints the HTTP status code (000 when
# curl gets no response). The response body goes to stderr for the log.
http_status() {
  local body_file code
  body_file=$(mktemp)
  code=$(auth_config "$1" | curl -sS -K - -o "${body_file}" -w '%{http_code}' \
    -X "$3" -H "Content-Type: application/json" -H "Accept: $2" -d "$5" "$4") || code="000"
  cat "${body_file}" >&2
  rm -f "${body_file}"
  echo "${code}"
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

  local failed=0 pkg latest code
  for pkg in "${packages[@]}"; do
    if ! latest=$(npm view "${pkg}" dist-tags.latest) || ! [[ "${latest}" =~ ${SEMVER_RE} ]]; then
      echo "::error::npm check: can not read the latest version of ${pkg}"
      failed=1
      continue
    fi
    code=$(http_status "${NODE_AUTH_TOKEN:-}" application/json PUT \
      "${NPM_REGISTRY}/-/package/${pkg/\//%2f}/dist-tags/latest" "\"${latest}\"")
    if [[ "${code}" =~ ^2[0-9][0-9]$ ]]; then
      echo "✅ ${pkg}: the token can write (latest stays ${latest})"
    else
      echo "::error::npm check: the token of ${user} can not write ${pkg} (HTTP ${code}), so it can not publish it"
      failed=1
    fi
  done
  return "${failed}"
}

check_homebrew() {
  local code
  code=$(http_status "${HOMEBREW_TAP_TOKEN:-}" application/vnd.github+json POST \
    "https://api.github.com/repos/${TAP_REPO}/dispatches" \
    '{"event_type":"skim-release-credential-check"}')
  if [ "${code}" != "204" ]; then
    fail "Homebrew tap check: HOMEBREW_TAP_TOKEN can not send a dispatch to ${TAP_REPO} (HTTP ${code})"
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
