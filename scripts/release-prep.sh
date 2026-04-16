#!/bin/bash

# Release preparation script for skim/rskim
#
# Usage:
#   ./scripts/release-prep.sh <version>
#
# Example:
#   ./scripts/release-prep.sh 2.5.0
#
# What it does:
#   1. Validates semver format
#   2. Detects current version from Cargo.toml
#   3. Runs pre-flight checks (fmt, clippy, tests)
#   4. Bumps version in all 3 locations (4 edits)
#   5. Runs cargo check to update Cargo.lock
#   6. Syncs test count in README.md and CLAUDE.md
#   7. Syncs version string in README.md
#   8. Prints summary with manual steps remaining

set -euo pipefail

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
RESET='\033[0m'

# Script must be run from repo root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# ─────────────────────────────────────────────────────────────────────────────
# Step 1: Validate input
# ─────────────────────────────────────────────────────────────────────────────
echo ""
echo "══════════════════════════════════════════════════"
echo "  skim release-prep"
echo "══════════════════════════════════════════════════"
echo ""

if [ $# -ne 1 ]; then
  echo -e "${RED}✗${RESET} Usage: $0 <version>"
  echo "  Example: $0 2.5.0"
  exit 1
fi

NEW_VERSION="$1"

if ! [[ "$NEW_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]; then
  echo -e "${RED}✗${RESET} Invalid version format: '$NEW_VERSION'"
  echo "  Must be semver: X.Y.Z or X.Y.Z-prerelease"
  exit 1
fi

echo -e "${GREEN}✓${RESET} Version format valid: $NEW_VERSION"

# ─────────────────────────────────────────────────────────────────────────────
# Step 2: Detect current version
# ─────────────────────────────────────────────────────────────────────────────
echo ""
echo "── Detecting current version ──"

CORE_TOML="${REPO_ROOT}/crates/rskim-core/Cargo.toml"

if [ ! -f "$CORE_TOML" ]; then
  echo -e "${RED}✗${RESET} Cannot find $CORE_TOML"
  exit 1
fi

OLD_VERSION=$(grep '^version = ' "$CORE_TOML" | head -1 | sed 's/version = "\(.*\)"/\1/')

if [ -z "$OLD_VERSION" ]; then
  echo -e "${RED}✗${RESET} Could not extract current version from $CORE_TOML"
  exit 1
fi

if ! [[ "$OLD_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?$ ]]; then
  echo -e "${RED}✗${RESET} Detected version '$OLD_VERSION' is not valid semver"
  exit 1
fi

echo -e "${GREEN}✓${RESET} Current version: $OLD_VERSION"
echo -e "${GREEN}✓${RESET} Target version:  $NEW_VERSION"

if [ "$OLD_VERSION" = "$NEW_VERSION" ]; then
  echo -e "${YELLOW}⚠${RESET}  New version is same as current ($OLD_VERSION). Nothing to do."
  exit 0
fi

# ─────────────────────────────────────────────────────────────────────────────
# Step 3: Pre-flight checks
# ─────────────────────────────────────────────────────────────────────────────
echo ""
echo "── Pre-flight checks ──"

cd "$REPO_ROOT"

echo "Running cargo fmt --check..."
if ! cargo fmt -- --check 2>&1; then
  echo -e "${RED}✗${RESET} cargo fmt check failed — run 'cargo fmt' and retry"
  exit 1
fi
echo -e "${GREEN}✓${RESET} cargo fmt"

echo "Running cargo clippy..."
if ! cargo clippy --all-features -- -D warnings 2>&1; then
  echo -e "${RED}✗${RESET} cargo clippy failed — fix warnings before releasing"
  exit 1
fi
echo -e "${GREEN}✓${RESET} cargo clippy"

echo "Running cargo test --all-features (this may take a while)..."
TEST_OUTPUT=$(cargo test --all-features 2>&1)
TEST_EXIT=$?

if [ $TEST_EXIT -ne 0 ]; then
  echo "$TEST_OUTPUT" | tail -20
  echo -e "${RED}✗${RESET} cargo test failed — fix failing tests before releasing"
  exit 1
fi
echo -e "${GREEN}✓${RESET} cargo test"

# Extract test count from harness summary line: "PASS: 2629 | FAIL: 0 | SKIP: 0"
RAW_COUNT=$(echo "$TEST_OUTPUT" | grep -E 'PASS: [0-9]+' | tail -1 | sed 's/.*PASS: \([0-9]*\).*/\1/')

if [ -z "$RAW_COUNT" ]; then
  echo -e "${YELLOW}⚠${RESET}  Could not extract test count from test output — test count sync will be skipped"
  RAW_COUNT=""
fi

if [ -n "$RAW_COUNT" ]; then
  echo -e "${GREEN}✓${RESET} Test count: $RAW_COUNT"
fi

# ─────────────────────────────────────────────────────────────────────────────
# Step 4: Version bump
# ─────────────────────────────────────────────────────────────────────────────
echo ""
echo "── Version bump: $OLD_VERSION → $NEW_VERSION ──"

RSKIM_TOML="${REPO_ROOT}/crates/rskim/Cargo.toml"

# Validate old strings exist before replacing
if ! grep -q "^version = \"${OLD_VERSION}\"" "$CORE_TOML"; then
  echo -e "${RED}✗${RESET} Expected 'version = \"${OLD_VERSION}\"' in $CORE_TOML — not found"
  exit 1
fi

if ! grep -q "^version = \"${OLD_VERSION}\"" "$RSKIM_TOML"; then
  echo -e "${RED}✗${RESET} Expected 'version = \"${OLD_VERSION}\"' in $RSKIM_TOML — not found"
  exit 1
fi

if ! grep -q "rskim-core = { version = \"${OLD_VERSION}\"" "$RSKIM_TOML"; then
  echo -e "${RED}✗${RESET} Expected 'rskim-core = { version = \"${OLD_VERSION}\"' in $RSKIM_TOML — not found"
  exit 1
fi

# Bump rskim-core package version (first occurrence only)
sed -i '' "0,/^version = \"${OLD_VERSION}\"/{s/^version = \"${OLD_VERSION}\"/version = \"${NEW_VERSION}\"/}" "$CORE_TOML"
echo -e "${GREEN}✓${RESET} crates/rskim-core/Cargo.toml — package version"

# Bump rskim package version (first occurrence only — line 3)
sed -i '' "0,/^version = \"${OLD_VERSION}\"/{s/^version = \"${OLD_VERSION}\"/version = \"${NEW_VERSION}\"/}" "$RSKIM_TOML"
echo -e "${GREEN}✓${RESET} crates/rskim/Cargo.toml — package version"

# Bump rskim-core dependency version in rskim Cargo.toml
sed -i '' "s/rskim-core = { version = \"${OLD_VERSION}\"/rskim-core = { version = \"${NEW_VERSION}\"/" "$RSKIM_TOML"
echo -e "${GREEN}✓${RESET} crates/rskim/Cargo.toml — rskim-core dependency version"

# ─────────────────────────────────────────────────────────────────────────────
# Step 5: Cargo.lock update
# ─────────────────────────────────────────────────────────────────────────────
echo ""
echo "── Updating Cargo.lock ──"

if ! cargo check 2>&1; then
  echo -e "${RED}✗${RESET} cargo check failed after version bump"
  exit 1
fi
echo -e "${GREEN}✓${RESET} Cargo.lock updated"

# ─────────────────────────────────────────────────────────────────────────────
# Step 6: Test count sync
# ─────────────────────────────────────────────────────────────────────────────
echo ""
echo "── Test count sync ──"

if [ -n "$RAW_COUNT" ]; then
  # Format with comma: 2629 → 2,629
  # For numbers >= 1000, insert comma 3 chars from end
  FORMATTED_COUNT=$(echo "$RAW_COUNT" | sed 's/\([0-9]\)\([0-9]\{3\}\)$/\1,\2/')

  README="${REPO_ROOT}/README.md"
  CLAUDE_MD="${REPO_ROOT}/CLAUDE.md"

  # README.md: replace "N,NNN tests passing" pattern
  if grep -q '[0-9],[0-9][0-9][0-9] tests passing' "$README"; then
    sed -i '' "s/[0-9][0-9]*,[0-9][0-9][0-9] tests passing/${FORMATTED_COUNT} tests passing/" "$README"
    echo -e "${GREEN}✓${RESET} README.md — test count updated to $FORMATTED_COUNT"
  else
    echo -e "${YELLOW}⚠${RESET}  README.md — test count pattern not found (manual update required)"
  fi

  # CLAUDE.md: replace all occurrences of "N,NNN tests passing" (2 locations)
  if grep -q '[0-9],[0-9][0-9][0-9] tests passing' "$CLAUDE_MD"; then
    sed -i '' "s/[0-9][0-9]*,[0-9][0-9][0-9] tests passing/${FORMATTED_COUNT} tests passing/g" "$CLAUDE_MD"
    echo -e "${GREEN}✓${RESET} CLAUDE.md — test count updated to $FORMATTED_COUNT (all occurrences)"
  else
    echo -e "${YELLOW}⚠${RESET}  CLAUDE.md — test count pattern not found (manual update required)"
  fi
else
  echo -e "${YELLOW}⚠${RESET}  Skipping test count sync (count not available)"
fi

# ─────────────────────────────────────────────────────────────────────────────
# Step 7: Version string sync in README.md
# ─────────────────────────────────────────────────────────────────────────────
echo ""
echo "── Version string sync ──"

README="${REPO_ROOT}/README.md"

if grep -q "\*\*Current\*\*: v${OLD_VERSION} — Stable" "$README"; then
  sed -i '' "s/\*\*Current\*\*: v${OLD_VERSION} — Stable/**Current**: v${NEW_VERSION} — Stable/" "$README"
  echo -e "${GREEN}✓${RESET} README.md — version string updated to v${NEW_VERSION}"
else
  echo -e "${YELLOW}⚠${RESET}  README.md — version string pattern not found (manual update required)"
fi

# ─────────────────────────────────────────────────────────────────────────────
# Step 8: Summary
# ─────────────────────────────────────────────────────────────────────────────
echo ""
echo "══════════════════════════════════════════════════"
echo -e "${GREEN}  Release prep complete: $OLD_VERSION → $NEW_VERSION${RESET}"
echo "══════════════════════════════════════════════════"
echo ""
echo "Files updated automatically:"
echo "  • crates/rskim-core/Cargo.toml — package version"
echo "  • crates/rskim/Cargo.toml — package version + rskim-core dependency"
echo "  • Cargo.lock — propagated by cargo check"
if [ -n "$RAW_COUNT" ]; then
echo "  • README.md — version string + test count (${FORMATTED_COUNT})"
echo "  • CLAUDE.md — test count (${FORMATTED_COUNT}, 2 locations)"
else
echo "  • README.md — version string"
fi
echo ""
echo -e "${YELLOW}Manual steps remaining:${RESET}"
echo "  1. Write CHANGELOG.md entry for [${NEW_VERSION}]"
echo "  2. Update subcommand descriptions in CLAUDE.md if new subcommands were added"
echo "  3. Review git diff to confirm all changes look correct"
echo "  4. Create release commit:"
echo "       git commit -m 'release: v${NEW_VERSION} — <summary>'"
echo ""
