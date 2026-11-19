#!/usr/bin/env bash
#
# Tests for scripts/ci/check-release-credentials.sh
#
# Usage:
#   bash scripts/ci/test-release-credentials.sh
#
# Each case puts stub `npm` and `curl` programs first on PATH. The stubs
# answer like the npm CLI, the registry and the GitHub API, and log each
# call, so no case reaches the network. The cases cover a token that can
# publish (with the package listing refused, as the registry does for a
# granular token), tokens that can not publish, and the results of the
# Homebrew dispatch.

set -uo pipefail

SCRIPT="$(cd "$(dirname "$0")" && pwd)/check-release-credentials.sh"
PLATFORMS="cli-darwin-arm64 cli-linux-x64"
NPM_TOKEN_VALUE="stub-npm-token"
TAP_TOKEN_VALUE="stub-tap-token"
TESTS_PASSED=0
TESTS_FAILED=0

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Stub npm. Behaviour comes from these variables:
#   STUB_WHOAMI     exit code of `npm whoami` (0 prints "dean0x")
#   STUB_LATEST     what `npm view <pkg> dist-tags.latest` prints
#   STUB_VIEW_FAIL  a package whose `npm view` fails (E404)
# `npm access` fails with E403, as for a granular token. `npm dist-tag add`
# exits 0 with no write when the tag already has the version, as npm 10 does.
cat > "$WORK/npm" <<'STUB'
#!/usr/bin/env bash
echo "npm $*" >> "$STUB_LOG"
case "$1" in
  whoami)
    [ "${STUB_WHOAMI:-0}" = 0 ] || { echo "npm error code E401" >&2; exit 1; }
    echo "dean0x" ;;
  view)
    [ "$2" = "${STUB_VIEW_FAIL:-}" ] && { echo "npm error code E404" >&2; exit 1; }
    echo "${STUB_LATEST:-3.0.0}" ;;
  dist-tag)
    echo "npm warn dist-tag add latest is already set to version ${3##*@}" >&2 ;;
  access)
    echo "npm error code E403" >&2; exit 1 ;;
  *)
    echo "stub npm: unexpected command $1" >&2; exit 99 ;;
esac
STUB

# Stub curl. It reads the config on stdin and logs which stub token it holds.
# Behaviour comes from these variables:
#   STUB_CURL_EXIT      a curl exit code with no response (prints 000)
#   STUB_PUT_CODE       HTTP code of a dist-tag PUT (default 200)
#   STUB_WRITE_DENY     a package whose PUT gets STUB_DENY_CODE, or "all"
#   STUB_DENY_CODE      HTTP code of a refused PUT (default 403)
#   STUB_DISPATCH_CODE  HTTP code of the dispatch (default 204)
cat > "$WORK/curl" <<'STUB'
#!/usr/bin/env bash
echo "curl $*" >> "$STUB_LOG"
config=$(cat)
case "$config" in
  *"Bearer stub-npm-token"*) echo "auth npm-token" >> "$STUB_LOG" ;;
  *"Bearer stub-tap-token"*) echo "auth tap-token" >> "$STUB_LOG" ;;
  *) echo "auth none" >> "$STUB_LOG" ;;
esac
out=/dev/null
url=""
while [ $# -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift ;;
    https://*) url="$1" ;;
  esac
  shift
done
if [ "${STUB_CURL_EXIT:-0}" != 0 ]; then
  printf '000'
  echo "curl: (7) Failed to connect" >&2
  exit "$STUB_CURL_EXIT"
fi
case "$url" in
  */dist-tags/latest)
    pkg="${url#https://registry.npmjs.org/-/package/}"
    pkg="${pkg%/dist-tags/latest}"
    if [ "${STUB_WRITE_DENY:-}" = all ] || [ "$pkg" = "${STUB_WRITE_DENY:-}" ]; then
      code="${STUB_DENY_CODE:-403}"
    else
      code="${STUB_PUT_CODE:-200}"
    fi ;;
  */dispatches) code="${STUB_DISPATCH_CODE:-204}" ;;
  *) code=404 ;;
esac
echo "{\"stub\":\"HTTP $code\"}" > "$out"
printf '%s' "$code"
STUB
chmod +x "$WORK/npm" "$WORK/curl"

# run_check MODE [VAR=value ...] — runs the script with the stubs and a fresh
# log. Sets OUT (stdout and stderr), CODE (exit code) and CALLS (the log).
run_check() {
  local mode="$1"
  shift
  export STUB_LOG="$WORK/calls.log"
  : > "$STUB_LOG"
  OUT=$(env PATH="$WORK:$PATH" NPM_PLATFORM_PACKAGES="$PLATFORMS" \
    NODE_AUTH_TOKEN="$NPM_TOKEN_VALUE" HOMEBREW_TAP_TOKEN="$TAP_TOKEN_VALUE" \
    "$@" bash "$SCRIPT" "$mode" 2>&1)
  CODE=$?
  CALLS=$(cat "$STUB_LOG")
}

expect() {
  local name="$1"
  shift
  if "$@"; then
    echo "✓ $name"
    TESTS_PASSED=$((TESTS_PASSED + 1))
  else
    echo "✗ $name"
    echo "  exit: $CODE"
    echo "  output: $OUT"
    echo "  calls: $CALLS"
    TESTS_FAILED=$((TESTS_FAILED + 1))
  fi
}

count() { grep -c -- "$1" <<< "$2"; }

case_publish_token_passes() {
  run_check npm
  [ "$CODE" = 0 ] &&
    [ "$(count '^curl .*-X PUT' "$CALLS")" = 3 ] &&
    grep -q -- '-X PUT .*-d "3.0.0" https://registry.npmjs.org/-/package/rskim/dist-tags/latest$' <<< "$CALLS" &&
    grep -q -- '-d "3.0.0" https://registry.npmjs.org/-/package/@rskim%2fcli-darwin-arm64/dist-tags/latest$' <<< "$CALLS" &&
    grep -q -- '-d "3.0.0" https://registry.npmjs.org/-/package/@rskim%2fcli-linux-x64/dist-tags/latest$' <<< "$CALLS"
}

case_no_listing_no_dist_tag_no_publish() {
  run_check npm
  ! grep -q '^npm access' <<< "$CALLS" &&
    ! grep -q '^npm dist-tag' <<< "$CALLS" &&
    ! grep -q '^npm publish' <<< "$CALLS"
}

case_probe_keeps_value() {
  run_check npm STUB_LATEST=2.9.1-rc.1
  [ "$CODE" = 0 ] &&
    grep -q -- '-d "2.9.1-rc.1" https://registry.npmjs.org/-/package/rskim/dist-tags/latest$' <<< "$CALLS"
}

case_tokens_not_in_argv() {
  run_check npm
  local npm_calls="$CALLS"
  run_check homebrew
  ! grep -q "$NPM_TOKEN_VALUE\|$TAP_TOKEN_VALUE" <<< "$npm_calls$CALLS" &&
    [ "$(count '^auth npm-token$' "$npm_calls")" = 3 ] &&
    [ "$(count '^auth tap-token$' "$CALLS")" = 1 ]
}

case_invalid_token_fails() {
  run_check npm STUB_WHOAMI=1
  [ "$CODE" = 1 ] && grep -q 'NPM_TOKEN is not valid' <<< "$OUT" &&
    ! grep -q '^curl' <<< "$CALLS"
}

case_read_only_token_fails() {
  run_check npm STUB_WRITE_DENY=all
  [ "$CODE" = 1 ] && [ "$(count 'HTTP 403), so it can not publish it' "$OUT")" = 3 ]
}

case_one_package_denied_fails() {
  run_check npm STUB_WRITE_DENY=@rskim%2fcli-linux-x64
  [ "$CODE" = 1 ] &&
    grep -q 'can not write @rskim/cli-linux-x64 (HTTP 403)' <<< "$OUT" &&
    [ "$(count 'can not publish it' "$OUT")" = 1 ]
}

case_auth_refused_fails() {
  run_check npm STUB_WRITE_DENY=all STUB_DENY_CODE=401
  [ "$CODE" = 1 ] && [ "$(count 'HTTP 401), so it can not publish it' "$OUT")" = 3 ]
}

case_redirect_fails() {
  run_check npm STUB_PUT_CODE=307
  [ "$CODE" = 1 ] && [ "$(count 'HTTP 307)' "$OUT")" = 3 ]
}

case_no_response_fails() {
  run_check npm STUB_CURL_EXIT=7
  [ "$CODE" = 1 ] && [ "$(count 'HTTP 000)' "$OUT")" = 3 ]
}

case_unreadable_version_fails() {
  run_check npm STUB_VIEW_FAIL=rskim
  [ "$CODE" = 1 ] && grep -q 'can not read the latest version of rskim' <<< "$OUT" &&
    ! grep -q '/-/package/rskim/' <<< "$CALLS" &&
    [ "$(count '^curl .*-X PUT' "$CALLS")" = 2 ]
}

case_bad_version_text_fails() {
  run_check npm 'STUB_LATEST=3.0.0 latest'
  [ "$CODE" = 1 ] && ! grep -q '^curl' <<< "$CALLS"
}

case_no_platform_list_fails() {
  run_check npm NPM_PLATFORM_PACKAGES=
  [ "$CODE" != 0 ] && ! grep -q '^curl' <<< "$CALLS"
}

case_homebrew_accepted_passes() {
  run_check homebrew
  [ "$CODE" = 0 ] && grep -q 'dispatch accepted' <<< "$OUT" &&
    grep -q -- '-X POST .*skim-release-credential-check.* https://api.github.com/repos/dean0x/homebrew-tap/dispatches$' <<< "$CALLS"
}

case_homebrew_refused_fails() {
  run_check homebrew STUB_DISPATCH_CODE=401
  [ "$CODE" = 1 ] && grep -q 'HOMEBREW_TAP_TOKEN can not send a dispatch to dean0x/homebrew-tap (HTTP 401)' <<< "$OUT"
}

case_homebrew_redirect_fails() {
  run_check homebrew STUB_DISPATCH_CODE=307
  [ "$CODE" = 1 ] && grep -q '(HTTP 307)' <<< "$OUT"
}

case_homebrew_no_response_fails() {
  run_check homebrew STUB_CURL_EXIT=6
  [ "$CODE" = 1 ] && grep -q '(HTTP 000)' <<< "$OUT"
}

case_unknown_mode_fails() {
  run_check crates
  [ "$CODE" = 2 ] && [ -z "$CALLS" ]
}

echo ""
echo "Release credential check tests"
echo ""
expect "a token that can publish passes when the package listing is refused" case_publish_token_passes
expect "the npm check never lists packages, never uses npm dist-tag and never publishes" case_no_listing_no_dist_tag_no_publish
expect "the probe sets latest to the version it read" case_probe_keeps_value
expect "no token is in argv, and each call sends its own token" case_tokens_not_in_argv
expect "a token that is not valid fails before any probe" case_invalid_token_fails
expect "a read-only token (HTTP 403) fails for every package" case_read_only_token_fails
expect "a token without one package fails for that package" case_one_package_denied_fails
expect "a refused authentication (HTTP 401) fails" case_auth_refused_fails
expect "a redirect (HTTP 307) on the probe fails" case_redirect_fails
expect "no response from the registry fails" case_no_response_fails
expect "an unreadable latest version fails with no probe for that package" case_unreadable_version_fails
expect "a latest version that is not semver fails with no probe" case_bad_version_text_fails
expect "an empty platform package list fails with no probe" case_no_platform_list_fails
expect "an accepted Homebrew dispatch (HTTP 204) passes" case_homebrew_accepted_passes
expect "a refused Homebrew dispatch (HTTP 401) fails" case_homebrew_refused_fails
expect "a redirected Homebrew dispatch (HTTP 307) fails" case_homebrew_redirect_fails
expect "no response to the Homebrew dispatch fails" case_homebrew_no_response_fails
expect "an unknown mode exits 2 and calls nothing" case_unknown_mode_fails
echo ""
echo "Passed: $TESTS_PASSED, failed: $TESTS_FAILED"
[ "$TESTS_FAILED" = 0 ]
