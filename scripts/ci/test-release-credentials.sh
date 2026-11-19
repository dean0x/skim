#!/usr/bin/env bash
#
# Tests for scripts/ci/check-release-credentials.sh
#
# Usage:
#   bash scripts/ci/test-release-credentials.sh
#
# Each case puts stub `npm` and `curl` programs first on PATH. The stubs
# answer like the registry and the GitHub API, and log each call, so no case
# reaches the network. The cases cover a token that can publish (with the
# package listing refused, as the registry does for a granular token), tokens
# that can not publish, and both results of the Homebrew dispatch.

set -uo pipefail

SCRIPT="$(cd "$(dirname "$0")" && pwd)/check-release-credentials.sh"
PLATFORMS="cli-darwin-arm64 cli-linux-x64"
TESTS_PASSED=0
TESTS_FAILED=0

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Stub npm. Behaviour comes from these variables:
#   STUB_WHOAMI      exit code of `npm whoami` (0 prints "dean0x")
#   STUB_LATEST      what `npm view <pkg> dist-tags.latest` prints
#   STUB_VIEW_FAIL   a package whose `npm view` fails (E404)
#   STUB_WRITE_DENY  a package whose `npm dist-tag add` fails (E403), or "all"
# `npm access` always fails with E403, as for a granular token.
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
    pkg="${3%@*}"
    if [ "${STUB_WRITE_DENY:-}" = all ] || [ "$pkg" = "${STUB_WRITE_DENY:-}" ]; then
      echo "npm error code E403" >&2; exit 1
    fi
    echo "+latest: $3" ;;
  access)
    echo "npm error code E403" >&2; exit 1 ;;
  *)
    echo "stub npm: unexpected command $1" >&2; exit 99 ;;
esac
STUB

# Stub curl: STUB_CURL is its exit code (0 = HTTP 204, 22 = HTTP error).
cat > "$WORK/curl" <<'STUB'
#!/usr/bin/env bash
echo "curl $*" >> "$STUB_LOG"
if [ "${STUB_CURL:-0}" != 0 ]; then
  echo '{"message":"Bad credentials","status":"401"}'
  echo "curl: (22) The requested URL returned error: 401" >&2
  exit "$STUB_CURL"
fi
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
    NODE_AUTH_TOKEN=stub HOMEBREW_TAP_TOKEN=stub "$@" bash "$SCRIPT" "$mode" 2>&1)
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

case_publish_token_passes() {
  run_check npm
  [ "$CODE" = 0 ] &&
    [ "$(grep -c '^npm dist-tag add' <<< "$CALLS")" = 3 ] &&
    grep -qx 'npm dist-tag add rskim@3.0.0 latest' <<< "$CALLS" &&
    grep -qx 'npm dist-tag add @rskim/cli-darwin-arm64@3.0.0 latest' <<< "$CALLS" &&
    grep -qx 'npm dist-tag add @rskim/cli-linux-x64@3.0.0 latest' <<< "$CALLS"
}

case_no_listing_no_publish() {
  run_check npm
  ! grep -q '^npm access' <<< "$CALLS" && ! grep -q '^npm publish' <<< "$CALLS"
}

case_tag_keeps_value() {
  run_check npm STUB_LATEST=2.9.1-rc.1
  [ "$CODE" = 0 ] && grep -qx 'npm dist-tag add rskim@2.9.1-rc.1 latest' <<< "$CALLS"
}

case_invalid_token_fails() {
  run_check npm STUB_WHOAMI=1
  [ "$CODE" = 1 ] && grep -q 'NPM_TOKEN is not valid' <<< "$OUT" &&
    ! grep -q '^npm dist-tag' <<< "$CALLS"
}

case_read_only_token_fails() {
  run_check npm STUB_WRITE_DENY=all
  [ "$CODE" = 1 ] && [ "$(grep -c 'can not publish it' <<< "$OUT")" = 3 ]
}

case_one_package_denied_fails() {
  run_check npm STUB_WRITE_DENY=@rskim/cli-linux-x64
  [ "$CODE" = 1 ] &&
    grep -q 'can not write @rskim/cli-linux-x64' <<< "$OUT" &&
    [ "$(grep -c 'can not publish it' <<< "$OUT")" = 1 ]
}

case_unreadable_version_fails() {
  run_check npm STUB_VIEW_FAIL=rskim
  [ "$CODE" = 1 ] && grep -q 'can not read the latest version of rskim' <<< "$OUT" &&
    ! grep -q '^npm dist-tag add rskim@' <<< "$CALLS"
}

case_bad_version_text_fails() {
  run_check npm 'STUB_LATEST=3.0.0 latest'
  [ "$CODE" = 1 ] && ! grep -q '^npm dist-tag' <<< "$CALLS"
}

case_no_platform_list_fails() {
  run_check npm NPM_PLATFORM_PACKAGES=
  [ "$CODE" != 0 ] && ! grep -q '^npm dist-tag' <<< "$CALLS"
}

case_homebrew_accepted_passes() {
  run_check homebrew
  [ "$CODE" = 0 ] && grep -q 'dispatch accepted' <<< "$OUT" &&
    grep -q 'repos/dean0x/homebrew-tap/dispatches' <<< "$CALLS" &&
    grep -q 'skim-release-credential-check' <<< "$CALLS"
}

case_homebrew_refused_fails() {
  run_check homebrew STUB_CURL=22
  [ "$CODE" = 1 ] && grep -q 'HOMEBREW_TAP_TOKEN can not send a dispatch' <<< "$OUT"
}

case_unknown_mode_fails() {
  run_check crates
  [ "$CODE" = 2 ] && [ -z "$CALLS" ]
}

echo ""
echo "Release credential check tests"
echo ""
expect "a token that can publish passes when the package listing is refused" case_publish_token_passes
expect "the npm check never lists packages and never publishes" case_no_listing_no_publish
expect "the probe sets latest to the version it read" case_tag_keeps_value
expect "a token that is not valid fails before any probe" case_invalid_token_fails
expect "a read-only token fails for every package" case_read_only_token_fails
expect "a token without one package fails for that package" case_one_package_denied_fails
expect "an unreadable latest version fails with no probe for that package" case_unreadable_version_fails
expect "a latest version that is not semver fails with no probe" case_bad_version_text_fails
expect "an empty platform package list fails with no probe" case_no_platform_list_fails
expect "an accepted Homebrew dispatch passes" case_homebrew_accepted_passes
expect "a refused Homebrew dispatch fails" case_homebrew_refused_fails
expect "an unknown mode exits 2 and calls nothing" case_unknown_mode_fails
echo ""
echo "Passed: $TESTS_PASSED, failed: $TESTS_FAILED"
[ "$TESTS_FAILED" = 0 ]
