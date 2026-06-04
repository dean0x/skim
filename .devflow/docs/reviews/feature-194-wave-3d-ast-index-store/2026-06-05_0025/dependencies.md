# Dependencies Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**PR**: #272
**Date**: 2026-06-05 00:25

## Scope

Reviewed the dependency surface of this PR:
- `crates/rskim-search/Cargo.toml` — added `rayon = { workspace = true }` and a `[[bench]]` entry
- `Cargo.lock` — single-line delta

## Verification of Stated Concerns

All concerns raised in the review request were checked against current code and verified:

| Claim | Verdict | Evidence |
|-------|---------|----------|
| rayon already a workspace dependency | CONFIRMED | `Cargo.toml:35` declares `rayon = "1.10"` in `[workspace.dependencies]` |
| rayon already used elsewhere (no new transitive risk) | CONFIRMED | 4 crates use it: `rskim-bench`, `rskim-research`, `rskim-search`, `rskim` |
| Cargo.lock delta reflects only this addition | CONFIRMED | `git diff --numstat` = `1 0 Cargo.lock`; the single added line is `rayon` inside the `rskim-search` package dependency node |
| Nothing unexpected slipped in | CONFIRMED | No new `[[package]]` nodes, no version changes, no source changes |
| criterion already a dev-dependency for benches | CONFIRMED | `crates/rskim-search/Cargo.toml:32` `criterion = { workspace = true }` (pre-existing; `linearize_bench` already uses it) |
| No version drift / duplicate versions | CONFIRMED | Exactly one `rayon` package node in lock (`v1.11.0`); no duplicate entries |
| workspace-version pattern used consistently | CONFIRMED | New entry uses `{ workspace = true }` matching all sibling deps in the file |
| New `[[bench]]` target backed by a real file | CONFIRMED | `crates/rskim-search/benches/ast_index_bench.rs` exists |

Decision context: `applies ADR-001` (fix noticed issues) and `avoids PF-003` (verified the lock/manifest claims against the actual files rather than trusting the diff description) were both honored during this review.

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

None.

## Pre-existing Issues (Not Blocking)

None worth flagging. The lockfile is committed, all workspace deps follow the
`{ workspace = true }` indirection pattern, and the resolved rayon version
(`1.11.0`) is the current stable line — fully consistent with the user's
"keep dependencies current" standing preference.

Note on `feedback_deps_current`: the workspace pins `rayon = "1.10"` (a caret
range resolving to `1.11.0`) and `criterion = "0.5"`. Both are current stable
lines as of this review; `1.10` is a floor, not a ceiling, and resolves forward
correctly. No churn needed. Criterion 0.6 exists but bumping it is out of this
PR's scope and would touch an unrelated workspace pin.

## Suggestions (Lower Confidence)

None.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Dependencies Score**: 10
**Recommendation**: APPROVED

This is a textbook-clean dependency change: a workspace-managed dependency
already present elsewhere is wired into one more crate, the lockfile delta is a
single line with no transitive surprises, the new bench target is backed by a
real file using an already-declared dev-dependency, and the workspace-version
indirection convention is followed consistently. No security, version, health,
license, or supply-chain concerns.
