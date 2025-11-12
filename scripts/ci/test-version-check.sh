#!/bin/bash

# Test suite for version consistency check script
# This validates the regex extraction and comparison logic used in the release workflow

# Don't exit on error - we want to run all tests
set +e

TESTS_PASSED=0
TESTS_FAILED=0

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
RESET='\033[0m'

echo ""
echo "🧪 Version Check Validation Tests"
echo ""

# Helper function to run a test
test_case() {
  local name="$1"
  local test_fn="$2"

  if $test_fn; then
    echo -e "${GREEN}✓${RESET} $name"
    ((TESTS_PASSED++))
  else
    echo -e "${RED}✗${RESET} $name"
    ((TESTS_FAILED++))
  fi
}

# Test 1: Extract version from Cargo.toml (standard format)
test_extract_standard() {
  local test_file=$(mktemp)
  echo 'version = "0.6.1"' > "$test_file"

  local result=$(grep '^version = ' "$test_file" | head -1 | sed 's/version = "\(.*\)"/\1/')
  rm "$test_file"

  [[ "$result" == "0.6.1" ]]
}

# Test 2: Extract version from Cargo.toml with comments
test_extract_with_comments() {
  local test_file=$(mktemp)
  cat > "$test_file" <<'EOF'
# version = "wrong"
version = "1.2.3"
# Another comment
EOF

  local result=$(grep '^version = ' "$test_file" | head -1 | sed 's/version = "\(.*\)"/\1/')
  rm "$test_file"

  [[ "$result" == "1.2.3" ]]
}

# Test 3: Extract version (multiline, only get first)
test_extract_multiline() {
  local test_file=$(mktemp)
  cat > "$test_file" <<'EOF'
version = "1.0.0"
dependencies = {
  version = "2.0.0"
}
EOF

  local result=$(grep '^version = ' "$test_file" | head -1 | sed 's/version = "\(.*\)"/\1/')
  rm "$test_file"

  [[ "$result" == "1.0.0" ]]
}

# Test 4: Semantic version validation - valid versions
test_semver_valid() {
  local version="1.2.3"
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]
}

test_semver_valid_prerelease() {
  local version="1.2.3-beta.1"
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]
}

test_semver_valid_rc() {
  local version="2.0.0-rc.2"
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]
}

test_semver_valid_alpha() {
  local version="0.1.0-alpha"
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]
}

# Test 5: Semantic version validation - invalid versions
test_semver_invalid_v_prefix() {
  local version="v1.2.3"
  ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]
}

test_semver_invalid_two_parts() {
  local version="1.2"
  ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]
}

test_semver_invalid_non_numeric() {
  local version="1.2.x"
  ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]
}

test_semver_invalid_latest() {
  local version="latest"
  ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]
}

test_semver_invalid_injection() {
  local version='1.0.0$(whoami)'
  ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]
}

test_semver_invalid_command() {
  local version='1.0.0; rm -rf /'
  ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]
}

# Test 6: Tag version extraction simulation
test_tag_extraction() {
  local GITHUB_REF="refs/tags/v1.2.3"
  local TAG_VERSION="${GITHUB_REF#refs/tags/v}"

  [[ "$TAG_VERSION" == "1.2.3" ]]
}

test_tag_extraction_prerelease() {
  local GITHUB_REF="refs/tags/v1.2.3-rc.1"
  local TAG_VERSION="${GITHUB_REF#refs/tags/v}"

  [[ "$TAG_VERSION" == "1.2.3-rc.1" ]]
}

# Test 7: Version comparison
test_version_match() {
  local TAG_VERSION="1.2.3"
  local CARGO_VERSION="1.2.3"

  [[ "$TAG_VERSION" == "$CARGO_VERSION" ]]
}

test_version_mismatch() {
  local TAG_VERSION="1.2.3"
  local CARGO_VERSION="1.2.2"

  ! [[ "$TAG_VERSION" == "$CARGO_VERSION" ]]
}

# Run all tests
test_case "Extract version: standard format" test_extract_standard
test_case "Extract version: with comments" test_extract_with_comments
test_case "Extract version: multiline (first only)" test_extract_multiline
test_case "Semver validation: 1.2.3 (valid)" test_semver_valid
test_case "Semver validation: 1.2.3-beta.1 (valid)" test_semver_valid_prerelease
test_case "Semver validation: 2.0.0-rc.2 (valid)" test_semver_valid_rc
test_case "Semver validation: 0.1.0-alpha (valid)" test_semver_valid_alpha
test_case "Semver validation: v1.2.3 (invalid - v prefix)" test_semver_invalid_v_prefix
test_case "Semver validation: 1.2 (invalid - two parts)" test_semver_invalid_two_parts
test_case "Semver validation: 1.2.x (invalid - non-numeric)" test_semver_invalid_non_numeric
test_case "Semver validation: latest (invalid - string)" test_semver_invalid_latest
test_case "Semver validation: command injection attempt" test_semver_invalid_injection
test_case "Semver validation: shell command attempt" test_semver_invalid_command
test_case "Tag extraction: standard tag" test_tag_extraction
test_case "Tag extraction: prerelease tag" test_tag_extraction_prerelease
test_case "Version comparison: matching versions" test_version_match
test_case "Version comparison: mismatched versions" test_version_mismatch

# Summary
echo ""
echo "=================================================="
echo -e "${GREEN}Passed: $TESTS_PASSED${RESET}"
if [ $TESTS_FAILED -gt 0 ]; then
  echo -e "${RED}Failed: $TESTS_FAILED${RESET}"
  exit 1
else
  echo -e "${GREEN}All tests passed!${RESET}"
  exit 0
fi
