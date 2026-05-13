---
format_version: 1
project_type: rust
version_strategy: manual
tag_format: "v{version}"
created: 2026-05-13T00:00:00Z
last_updated: 2026-05-13T00:00:00Z
---

## Packages

Single workspace with two published crates (order matters):

| Crate | Version File | Publish Target |
|-------|-------------|----------------|
| rskim-core | `crates/rskim-core/Cargo.toml` | crates.io |
| rskim | `crates/rskim/Cargo.toml` (package version + rskim-core dependency version) | crates.io + npm |

## Pre-release Checks

1. Working directory clean (`git status --porcelain`)
2. On a release branch (`release/v{version}`)
3. Tag does not already exist
4. `cargo fmt -- --check`
5. `cargo clippy -- -D warnings`
6. `cargo test --all-features`

## Changelog

Format: keep-a-changelog
Unreleased section: `## [Unreleased]`
Release section: `## [{version}] - {YYYY-MM-DD}`

## Build & Test

- build_tool: cargo
- test_tool: cargo
- release_prep_script: `./scripts/release-prep.sh {version}`

## Publish

- Method: CI-driven (triggered by tag push)
- Pipeline: `.github/workflows/release.yml`
- Sequence: test -> build (7 targets) -> GitHub Release -> crates.io (rskim-core first, then rskim) -> npm (7 platform pkgs + main) -> Homebrew tap dispatch

## Version Bump (3 files, 4 edits)

Automated by `release-prep.sh`:
- `crates/rskim-core/Cargo.toml` — package version
- `crates/rskim/Cargo.toml` — package version + rskim-core dependency version
- `cargo check` to update `Cargo.lock`
- Syncs test count in README.md and CLAUDE.md
- Syncs version string in README.md

## Release Branch Workflow

1. Create branch: `git checkout -b release/v{version} main`
2. Run `./scripts/release-prep.sh {version}` (pre-flight + version bumps + doc sync)
3. Write CHANGELOG entry manually
4. Update CLAUDE.md subcommand descriptions if changed
5. Commit: `release: v{version} — {summary}`
6. Push branch, open PR to main
7. After merge: `git tag v{version}` on main, `git push origin main --tags`

## Post-release Verification

1. `cargo install rskim` — shows new version
2. `npx rskim --version` — shows new version
3. GitHub Release page — 7 binary assets attached
4. `brew update && brew info dean0x/tap/skim` — formula updated (PF-001)

## Post-release Cleanup

- Delete release branch: `git branch -d release/v{version}`
- Rebuild local dev binary: `cargo build --release`
