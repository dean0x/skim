# Dependencies Review Report

**Branch**: feature-187-wave-3a--cst-linearization--pre-order-tr -> main
**Date**: 2026-06-01

## Issues in Your Changes (BLOCKING)

No blocking dependency issues found.

## Issues in Code You Touched (Should Fix)

No should-fix dependency issues found.

## Pre-existing Issues (Not Blocking)

No pre-existing dependency issues identified.

## Suggestions (Lower Confidence)

No lower-confidence suggestions.

## Analysis Notes

### tree-sitter = { workspace = true } (production dependency)

- **Version**: 0.25.10 (workspace-pinned)
- **Already transitive**: rskim-core v2.10.0 already depends on tree-sitter v0.25.10. The Cargo resolver deduplicates this -- `cargo tree` shows a single `tree-sitter v0.25.10 (*)` entry, confirming zero binary size increase.
- **Justified usage**: `linearize.rs:233` accepts `&tree_sitter::Tree` directly, requiring the crate as a direct dependency rather than re-exporting from rskim-core. This is the correct Rust pattern -- depend on what you use directly.
- **Workspace consistency**: Uses `{ workspace = true }` referencing the workspace-level `tree-sitter = "0.25"` definition. No version drift possible.
- **No new transitive deps introduced**: tree-sitter was already fully resolved in the dependency graph.

### criterion = { workspace = true } (dev-dependency)

- **Version**: 0.5.1 (workspace-pinned at `criterion = "0.5"`)
- **Dev-only**: Listed under `[dev-dependencies]`, has zero impact on production binary size or compile time for downstream consumers.
- **Justified usage**: Powers `benches/linearize_bench.rs` with `criterion_group!` / `criterion_main!` macros. Standard Rust benchmarking crate.
- **Workspace consistency**: Uses `{ workspace = true }`, consistent with how other crates in this workspace declare criterion.
- **`harness = false`**: Correctly configured in the `[[bench]]` section, required for criterion benchmarks.

### Cargo.lock

- Lockfile updated with both additions (`criterion` and `tree-sitter` in rskim-search's dependency list).
- No version conflicts or duplicate tree-sitter versions detected.
- `foldhash` has two versions (0.1.5, 0.2.0) -- this is pre-existing from gix and unrelated to this PR.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Dependencies Score**: 10/10
**Recommendation**: APPROVED

**Rationale**: Both dependency additions are well-justified, workspace-consistent, and introduce no new risk. tree-sitter was already transitively present (zero binary size impact). criterion is dev-only and workspace-standard. No CVEs, no version drift, no supply chain concerns. Lockfile is properly updated. Applies ADR-001 -- no issues to fix.
