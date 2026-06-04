# Architecture Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29

## Issues in Your Changes (BLOCKING)

### MEDIUM

**`test_utils` module unconditionally compiled into library binary** - `crates/rskim-bench/src/cochange/mod.rs:25-71`
**Confidence**: 82%
- Problem: The `test_utils` module is explicitly *not* gated by `#[cfg(test)]` to allow integration tests to import it. While the comment explains why, this means test infrastructure (helper functions, synthetic commit builders) is compiled into the production library binary. This is a modularity issue — test infrastructure leaks into production.
- Fix: Use a Cargo feature gate `#[cfg(any(test, feature = "test-utils"))]` on the module, and have the integration tests enable that feature via `[dev-dependencies]` with `features = ["test-utils"]`. This keeps the module available for integration tests without polluting the production binary:

```rust
// mod.rs
#[cfg(any(test, feature = "test-utils"))]
pub mod test_utils;
```

```toml
# Cargo.toml
[features]
test-utils = []

[dev-dependencies]
rskim-bench = { path = ".", features = ["test-utils"] }
```

**`train.to_vec()` full-clone in `build_and_evaluate` creates unnecessary allocation** - `crates/rskim-bench/src/cochange/validate.rs:602`
**Confidence**: 83%
- Problem: The `train` slice is cloned entirely via `.to_vec()` just to satisfy the `HistoryResult { commits: Vec<CommitInfo> }` struct requirement for the builder. For repositories with thousands of commits, each containing file change vectors, this is a significant allocation. This violates the DIP principle — the `CochangeMatrixBuilder::build()` interface forces the caller to own the data rather than borrowing it.
- Fix: Since the builder API is in `rskim-search` (not changed in this PR), the immediate fix is to accept this as a necessary cost and document it. However, architecturally, the `CochangeMatrixBuilder::build()` should accept `&[CommitInfo]` rather than requiring `HistoryResult` ownership. For this PR, add a comment acknowledging the allocation:

```rust
// NOTE: to_vec() is required because CochangeMatrixBuilder::build() takes
// HistoryResult by reference but HistoryResult owns its commits Vec.
// Future: refactor builder to accept &[CommitInfo] directly.
let history_for_builder = rskim_search::HistoryResult {
    commits: train.to_vec(),
    ...
};
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`commit` field unused by co-change validator but required by `RepoEntry`** - `crates/rskim-research/cochange-corpus.toml` / `crates/rskim-research/src/config.rs:20`
**Confidence**: 80%
- Problem: The `cochange-corpus.toml` entries all include a `commit` field (validated as a 40-char hex SHA), but `cochange_validate.rs` never uses it — it always clones HEAD and processes full history. The `commit` field is present only because `RepoEntry` requires it. This creates a maintenance burden: when upstream repos push new commits, these pinned SHAs become stale with no way to detect drift, yet they serve no purpose for this benchmark.
- Fix: Consider making `commit` optional in `RepoEntry` (via `Option<String>`) and only validating it when present. Alternatively, add a comment in `cochange-corpus.toml` explaining the field is required by the shared type but not used by this benchmark. The simplest immediate fix:

```rust
// config.rs
pub struct RepoEntry {
    pub url: String,
    #[serde(default)]
    pub commit: Option<String>,  // Only needed for pinned-SHA clones
    pub language: String,
    #[serde(default)]
    pub deep_clone: bool,
}
```

This would require updating the existing BM25F benchmark code that uses `commit`, but keeps the type honest.

## Pre-existing Issues (Not Blocking)

(none at CRITICAL severity)

## Suggestions (Lower Confidence)

- **Duplicated timeout/kill pattern** - `crates/rskim-bench/src/cochange/validate.rs:646-693` and `crates/rskim-research/src/clone.rs:74-116` (Confidence: 70%) — The `capture_head_sha` function reimplements the same spawn-thread/timeout/SIGKILL pattern that already exists in `git_run_with_timeout`. Could reuse that function with stdout capture.

- **`validate_repo` returns `Ok(error_result(...))` — soft-failure in a Result** - `crates/rskim-bench/src/cochange/validate.rs:370-409` (Confidence: 65%) — The function signature is `-> anyhow::Result<RepoCochangeResult>` but it never returns `Err` in practice — all failures are wrapped in `Ok(error_result(...))`. This makes the Result type misleading. A dedicated enum (`enum RepoOutcome { Success(RepoCochangeResult), Failed { url, reason } }`) would make the "never fails" contract explicit.

- **Hardcoded thread-pool size (3)** - `crates/rskim-bench/src/bin/cochange_validate.rs:122-124` (Confidence: 62%) — The rayon pool is capped at 3 threads with comment "DD-3" but no CLI flag overrides this. For users with more cores and bandwidth, this is unnecessarily restrictive. Consider exposing `--jobs` like the main skim binary does.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The architecture is well-structured overall. The new `cochange` module follows clean separation of concerns with distinct submodules for types, validation logic, deny-list filtering, temporal splitting, and reporting. The layering is correct: `bin/cochange_validate.rs` (CLI) -> `validate.rs` (orchestration) -> `temporal_split.rs` / `deny_list.rs` / `report.rs` (pure logic). Dependencies flow inward properly. The `deep_clone` extension to `RepoEntry` uses `#[serde(default)]` for backward compatibility. The PR adds a well-isolated benchmark binary without coupling to the core `rskim` crate.

Conditions:
1. Address the `test_utils` production leak (feature-gate it) — applies ADR-001 (fix noticed issues immediately).
2. Document the `train.to_vec()` allocation intent or file follow-up to refactor builder API.
