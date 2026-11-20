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
# npm (env: ACTIONS_ID_TOKEN_REQUEST_URL, ACTIONS_ID_TOKEN_REQUEST_TOKEN,
#      NPM_PLATFORM_PACKAGES)
#   The release publishes to npm with trusted publishing (OIDC), so there is
#   no npm token. First, `npm --version` must be 11.5.1 or later, the first
#   npm with trusted publishing. Then the check does the same token exchange
#   as `npm publish`
#   (npm CLI `lib/utils/oidc.js`), for each package, and publishes nothing:
#   1. Get a GitHub OIDC ID token with the audience `npm:registry.npmjs.org`.
#      The job needs `permissions: id-token: write`.
#   2. For each package (rskim and @rskim/<platform>), send
#      `POST /-/npm/v1/oidc/token/exchange/package/<pkg>` with the ID token.
#      Only HTTP 2xx with a token in the body passes. The registry gives a
#      token only when the package has a trusted publisher for this
#      repository and this workflow file.
#   `npm publish` falls back to other credentials when the exchange fails,
#   so the publish job alone does not show a missing trusted publisher.
#   The exchange does not show the allowed actions of the trusted publisher
#   (`npm publish` or only `npm stage publish`).
#
# homebrew (env: HOMEBREW_TAP_TOKEN)
#   Send the same repository dispatch as the `trigger-homebrew` job, with an
#   event type that no tap workflow listens for. The tap workflow runs only
#   for `update-formula`. Only HTTP 204 passes.
#
# curl reads each token from a config on stdin, so no token is in argv. The
# script prints no token and no response body that can hold one.

set -uo pipefail

readonly NPM_REGISTRY="https://registry.npmjs.org"
readonly NPM_AUDIENCE="npm%3Aregistry.npmjs.org"
readonly NPM_MIN_VERSION="11.5.1"
readonly TAP_REPO="dean0x/homebrew-tap"

fail() {
  echo "::error::$1"
  exit 1
}

# auth_config TOKEN — a curl config line that sends TOKEN as a Bearer header.
auth_config() {
  printf 'header = "Authorization: Bearer %s"\n' "$1"
}

# http_request TOKEN ACCEPT METHOD URL BODY OUT — writes the response body to
# the file OUT and prints the HTTP status code (000 when curl gets no
# response). An empty BODY sends no body.
http_request() {
  local code
  local body_args=()
  [ -n "$5" ] && body_args=(-H "Content-Type: application/json" -d "$5")
  code=$(auth_config "$1" | curl -sS -K - -o "$6" -w '%{http_code}' \
    -X "$3" -H "Accept: $2" ${body_args[@]+"${body_args[@]}"} "$4") || code="000"
  echo "${code}"
}

# json_field FILE FIELD — prints the string value of FIELD, or nothing.
json_field() {
  jq -r --arg f "$2" 'if type == "object" then (.[$f] // empty) | strings else empty end' "$1" 2>/dev/null
}

check_npm() {
  local npm_version major minor patch
  npm_version=$(npm --version 2>/dev/null)
  if ! [[ "${npm_version}" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    fail "npm check: can not read the npm version (trusted publishing needs npm ${NPM_MIN_VERSION} or later)"
  fi
  major=${BASH_REMATCH[1]} minor=${BASH_REMATCH[2]} patch=${BASH_REMATCH[3]}
  if ((major < 11 || (major == 11 && (minor < 5 || (minor == 5 && patch < 1))))); then
    fail "npm check: npm ${npm_version} has no trusted publishing (it needs npm ${NPM_MIN_VERSION} or later)"
  fi
  echo "npm ${npm_version}"

  if [ -z "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" ] || [ -z "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:-}" ]; then
    fail "npm check: no GitHub OIDC token request (the job needs permissions: id-token: write)"
  fi

  local packages=(rskim)
  local platform
  for platform in ${NPM_PLATFORM_PACKAGES:?NPM_PLATFORM_PACKAGES is not set}; do
    packages+=("@rskim/${platform}")
  done

  local body_file sep code id_token
  body_file=$(mktemp)
  # Remove the file on every exit, because it can hold a token. The path
  # comes from mktemp, so it is safe to expand it into the trap now.
  trap "rm -f '${body_file}'" EXIT

  sep="?"
  [[ "${ACTIONS_ID_TOKEN_REQUEST_URL}" == *"?"* ]] && sep="&"
  code=$(http_request "${ACTIONS_ID_TOKEN_REQUEST_TOKEN}" application/json GET \
    "${ACTIONS_ID_TOKEN_REQUEST_URL}${sep}audience=${NPM_AUDIENCE}" "" "${body_file}")
  id_token=$(json_field "${body_file}" value)
  if ! [[ "${code}" =~ ^2[0-9][0-9]$ ]] || [ -z "${id_token}" ]; then
    fail "npm check: GitHub gave no OIDC ID token (HTTP ${code})"
  fi
  echo "::add-mask::${id_token}"

  local failed=0 pkg token
  for pkg in "${packages[@]}"; do
    : > "${body_file}"
    code=$(http_request "${id_token}" application/json POST \
      "${NPM_REGISTRY}/-/npm/v1/oidc/token/exchange/package/${pkg/\//%2f}" "" "${body_file}")
    token=$(json_field "${body_file}" token)
    if [[ "${code}" =~ ^2[0-9][0-9]$ ]] && [ -n "${token}" ]; then
      echo "::add-mask::${token}"
      echo "✅ ${pkg}: trusted publishing accepts this workflow"
    else
      echo "::error::npm check: ${pkg} has no trusted publisher for this repository and workflow (HTTP ${code}: $(json_field "${body_file}" message))"
      failed=1
    fi
  done
  return "${failed}"
}

check_homebrew() {
  local body_file code
  body_file=$(mktemp)
  trap "rm -f '${body_file}'" EXIT
  code=$(http_request "${HOMEBREW_TAP_TOKEN:-}" application/vnd.github+json POST \
    "https://api.github.com/repos/${TAP_REPO}/dispatches" \
    '{"event_type":"skim-release-credential-check"}' "${body_file}")
  if [ "${code}" != "204" ]; then
    fail "Homebrew tap check: HOMEBREW_TAP_TOKEN can not send a dispatch to ${TAP_REPO} (HTTP ${code}: $(json_field "${body_file}" message))"
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
