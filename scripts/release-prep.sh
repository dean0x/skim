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
#   3. Checks that every version-bearing line holds the current version
#      (before the long pre-flight run, and before any edit)
#   4. Runs pre-flight checks (fmt, clippy, tests)
#   5. Bumps the package version of every published crate, every versioned
#      internal dependency, and the README status line
#   6. Checks that no old version string is left, then runs cargo check to
#      update Cargo.lock
#   7. Prints summary with manual steps remaining

set -euo pipefail

# Colors
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
RESET='\033[0m'

# Script must be run from repo root
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# Every crate published to crates.io. All of them carry the same version as
# `rskim`, because `rskim` depends on each one (rskim-proxy and rskim-contract
# through the optional `proxy` feature). release.yml publishes the same set in
# dependency order; keep the two lists in sync.
PUBLISHED_MANIFESTS=(
  "crates/rskim-core/Cargo.toml"
  "crates/rskim-contract/Cargo.toml"
  "crates/rskim-llm/Cargo.toml"
  "crates/rskim-tokens/Cargo.toml"
  "crates/rskim-compress/Cargo.toml"
  "crates/rskim-proxy/Cargo.toml"
  "crates/rskim/Cargo.toml"
)

# A versioned internal dependency, in the one form the bump edits:
#   rskim-<name> = { version = "X.Y.Z", path = "..." ... }
INTERNAL_DEP_RE='^rskim-[a-z-]+ = \{ *version = "'

README_STATUS_RE='\*\*Current\*\*: v[0-9][0-9A-Za-z.-]* — Stable'

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

cd "$REPO_ROOT"

CORE_TOML="crates/rskim-core/Cargo.toml"

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

# Regex-escaped forms (a bare "." in a version would match any character).
OLD_RE="${OLD_VERSION//./\\.}"

# ─────────────────────────────────────────────────────────────────────────────
# Step 3: Version-line check (before pre-flight, before any edit)
# ─────────────────────────────────────────────────────────────────────────────
echo ""
echo "── Version-line check ──"

# Every published crate must be at the current version.
for manifest in "${PUBLISHED_MANIFESTS[@]}"; do
  if ! grep -q "^version = \"${OLD_RE}\"$" "$manifest"; then
    echo -e "${RED}✗${RESET} Expected 'version = \"${OLD_VERSION}\"' in $manifest — not found"
    echo "  Every published crate moves in lockstep; fix the drift first."
    exit 1
  fi
done
echo -e "${GREEN}✓${RESET} ${#PUBLISHED_MANIFESTS[@]} published crates at $OLD_VERSION"

# Every tracked manifest: a versioned internal dependency must pin the current
# version, in the one form the bump edits. Any other pin (an old version, a
# table form, `version` after `path`) would survive the bump and break
# `cargo check`, so it stops the script here.
TRACKED_MANIFESTS=()
while IFS= read -r manifest; do
  TRACKED_MANIFESTS+=("$manifest")
done < <(git ls-files '*Cargo.toml')

if [ ${#TRACKED_MANIFESTS[@]} -eq 0 ]; then
  echo -e "${RED}✗${RESET} git ls-files found no Cargo.toml — run from a git checkout"
  exit 1
fi

# Candidates: an inline pin in any spelling (indented, dotted key, `version`
# after `path`), a renamed dependency (`package = "rskim-..."`), or a table
# form (`[dependencies.rskim-...]`, also quoted target tables). Every
# candidate that is not the one canonical form at the current version fails.
BAD_PINS=$(grep -nE "(^[[:space:]]*rskim-[a-z-]+[[:space:]]*[.=].*version)|(package[[:space:]]*=[[:space:]]*\"rskim-)|(^[[:space:]]*\[.*dependencies\.[\"']?rskim-)" \
  "${TRACKED_MANIFESTS[@]}" \
  | grep -vE ":[0-9]+:rskim-[a-z-]+ = \{ *version = \"${OLD_RE}\"" || true)

if [ -n "$BAD_PINS" ]; then
  echo -e "${RED}✗${RESET} Internal dependency pins that the bump can not update:"
  echo "$BAD_PINS"
  echo "  Write each as: rskim-<name> = { version = \"${OLD_VERSION}\", path = \"...\" }"
  exit 1
fi
echo -e "${GREEN}✓${RESET} internal dependency pins at $OLD_VERSION"

README="README.md"
README_HITS=$(grep -c "$README_STATUS_RE" "$README" || true)
if [ "$README_HITS" != "1" ]; then
  echo -e "${RED}✗${RESET} README.md — expected 1 '**Current**: vX.Y.Z — Stable' line, found $README_HITS"
  exit 1
fi
echo -e "${GREEN}✓${RESET} README.md status line found"

# ─────────────────────────────────────────────────────────────────────────────
# Step 4: Pre-flight checks
# ─────────────────────────────────────────────────────────────────────────────
echo ""
echo "── Pre-flight checks ──"

echo "Running cargo fmt --check..."
if ! cargo fmt -- --check; then
  echo -e "${RED}✗${RESET} cargo fmt check failed — run 'cargo fmt' and retry"
  exit 1
fi
echo -e "${GREEN}✓${RESET} cargo fmt"

echo "Running cargo clippy..."
if ! cargo clippy --all-features -- -D warnings; then
  echo -e "${RED}✗${RESET} cargo clippy failed — fix warnings before releasing"
  exit 1
fi
echo -e "${GREEN}✓${RESET} cargo clippy"

echo "Running cargo test --all-features (this may take a while)..."
# `|| TEST_EXIT=$?` keeps `set -e` from ending the script before the tail prints.
TEST_EXIT=0
TEST_OUTPUT=$(cargo test --all-features 2>&1) || TEST_EXIT=$?

if [ $TEST_EXIT -ne 0 ]; then
  echo "$TEST_OUTPUT" | tail -20
  echo -e "${RED}✗${RESET} cargo test failed — fix failing tests before releasing"
  exit 1
fi
echo -e "${GREEN}✓${RESET} cargo test"

# ─────────────────────────────────────────────────────────────────────────────
# Step 5: Version bump
# ─────────────────────────────────────────────────────────────────────────────
echo ""
echo "── Version bump: $OLD_VERSION → $NEW_VERSION ──"

# Package versions (^version is unique per file — no range needed).
for manifest in "${PUBLISHED_MANIFESTS[@]}"; do
  sed -i '' "s/^version = \"${OLD_RE}\"$/version = \"${NEW_VERSION}\"/" "$manifest"
  echo -e "${GREEN}✓${RESET} $manifest — package version"
done

# Versioned internal dependencies, in every tracked manifest.
for manifest in "${TRACKED_MANIFESTS[@]}"; do
  if grep -qE "${INTERNAL_DEP_RE}${OLD_RE}\"" "$manifest"; then
    sed -i '' -E "s/^(rskim-[a-z-]+ = \{ *version = \")${OLD_RE}\"/\1${NEW_VERSION}\"/" "$manifest"
    echo -e "${GREEN}✓${RESET} $manifest — internal dependency versions"
  fi
done

sed -i '' "s/${README_STATUS_RE}/**Current**: v${NEW_VERSION} — Stable/" "$README"
echo -e "${GREEN}✓${RESET} README.md — status line"

# ─────────────────────────────────────────────────────────────────────────────
# Step 6: Post-bump check and Cargo.lock update
# ─────────────────────────────────────────────────────────────────────────────
echo ""
echo "── Post-bump check ──"

LEFT_OVER=$( { grep -nE "^version = \"${OLD_RE}\"$" "${PUBLISHED_MANIFESTS[@]}" || true; \
  grep -nE "${INTERNAL_DEP_RE}${OLD_RE}\"" "${TRACKED_MANIFESTS[@]}" || true; } )
if [ -n "$LEFT_OVER" ]; then
  echo -e "${RED}✗${RESET} Old version $OLD_VERSION is still present:"
  echo "$LEFT_OVER"
  exit 1
fi
echo -e "${GREEN}✓${RESET} no $OLD_VERSION left in the version lines"

NEW_RE="${NEW_VERSION//./\\.}"
for manifest in "${PUBLISHED_MANIFESTS[@]}"; do
  if ! grep -q "^version = \"${NEW_RE}\"$" "$manifest"; then
    echo -e "${RED}✗${RESET} $manifest — package version is not $NEW_VERSION after the bump"
    exit 1
  fi
done
if [ "$(grep -c "\*\*Current\*\*: v${NEW_RE} — Stable" "$README" || true)" != "1" ]; then
  echo -e "${RED}✗${RESET} README.md — status line is not v$NEW_VERSION after the bump"
  exit 1
fi
echo -e "${GREEN}✓${RESET} ${#PUBLISHED_MANIFESTS[@]} published crates and README.md at $NEW_VERSION"

echo ""
echo "── Updating Cargo.lock ──"

if ! cargo check; then
  echo -e "${RED}✗${RESET} cargo check failed after version bump"
  exit 1
fi
echo -e "${GREEN}✓${RESET} Cargo.lock updated"

# ─────────────────────────────────────────────────────────────────────────────
# Step 7: Summary
# ─────────────────────────────────────────────────────────────────────────────
echo ""
echo "══════════════════════════════════════════════════"
echo -e "${GREEN}  Release prep complete: $OLD_VERSION → $NEW_VERSION${RESET}"
echo "══════════════════════════════════════════════════"
echo ""
echo "Files updated automatically:"
for manifest in "${PUBLISHED_MANIFESTS[@]}"; do
  echo "  • $manifest — package version + internal dependency versions"
done
echo "  • Cargo.lock — propagated by cargo check"
echo "  • README.md — status line"
echo ""
echo -e "${YELLOW}Manual steps remaining:${RESET}"
echo "  1. Write CHANGELOG.md entry for [${NEW_VERSION}]"
echo "  2. Update subcommand descriptions in CLAUDE.md if new subcommands were added"
echo "  3. Review git diff to confirm all changes look correct"
echo "  4. Create release commit:"
echo "       git commit -m 'release: v${NEW_VERSION} — <summary>'"
echo ""
