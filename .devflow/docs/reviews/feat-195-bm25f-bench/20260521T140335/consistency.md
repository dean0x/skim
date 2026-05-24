---
focus: consistency
reviewer: Reviewer
timestamp: 2026-05-21T14:03:35Z
pr: 247
branch: feat/195-bm25f-bench
head: 101385eb
---

# Consistency Review

## Summary

The new `rskim-bench` crate is broadly consistent with the workspace's established patterns (edition, authors, license, lint config, section separators, `anyhow` for application-level errors, `#[must_use]` on pure functions). The main consistency issues are: an inconsistent CLI flag name for output format across subcommands, a public struct missing `#[derive(Debug)]` that every other public struct in the crate has, a dead public type, and a return-type inconsistency in `report::to_json` that breaks the "all fallible functions return `anyhow::Result`" pattern used by the rest of the crate. Minor annotation comment gaps exist relative to rskim-core but are low severity.

## Findings

### BLOCKING

#### HIGH -- Inconsistent format flag name across subcommands

- **File:** `crates/rskim-bench/src/main.rs:58-105`
- **Confidence:** 95%
- **Description:** `BenchArgs` and `TuneArgs` use `--output` (field name `output`) for the format flag, while `ReportArgs` uses `--format` (field name `format`) for the identical concept. This is an internal inconsistency within the same binary -- users must remember two different flag names for the same thing. Additionally, `ReportArgs` doc comment at line 99 references `bench --output json`, further highlighting the inconsistency.
- **Suggestion:** Standardise on one name. `--format` is more descriptive and matches the `--format` convention used by the main `skim` binary's subcommands (e.g., `skim stats --format json`). Rename `output` to `format` in `BenchArgs` and `TuneArgs`, and update the match arms in `run_bench` and `run_tune` accordingly (`args.format.as_str()`).

#### MEDIUM -- `BenchConfig` missing `#[derive(Debug)]`

- **File:** `crates/rskim-bench/src/harness.rs:17`
- **Confidence:** 92%
- **Description:** Every other public struct in this crate (`Qrel`, `EvalResult`, `ConfigMetrics`, `RepoBenchResult`, `BenchResult`, `ConvergenceStep`, `TuningResult`, `IndexedFile`, `ExtractedSymbol`, `QrelInput`) derives `Debug`. `BenchConfig` is the sole exception. This breaks the workspace pattern where all public types implement `Debug` (enforced by convention in rskim-core, rskim-search, and rskim-research). It also makes debugging harder -- `BenchConfig` values cannot be `dbg!()` printed.
- **Suggestion:** Add `#[derive(Debug)]` to `BenchConfig`:
  ```rust
  #[derive(Debug)]
  pub struct BenchConfig {
  ```

### MEDIUM -- `report::to_json` returns `Result<String, serde_json::Error>` instead of `anyhow::Result<String>`

- **File:** `crates/rskim-bench/src/report.rs:19`
- **Confidence:** 85%
- **Description:** Every other fallible public function in this crate returns `anyhow::Result<T>` (`generate_qrels`, `validate_qrel_coverage`, `run_on_files`, `evaluate_split`, `result_to_config`). `to_json` is the only function returning a raw library error type (`serde_json::Error`). While callers in `main.rs` add `?` inside an `anyhow::Result` context so it auto-converts, this forces callers outside that context to handle a different error type than the rest of the crate's API.
- **Suggestion:** Change to `anyhow::Result<String>` for consistency:
  ```rust
  pub fn to_json(
      result: &BenchResult,
      tuning: Option<&TuningResult>,
  ) -> anyhow::Result<String> {
  ```
  The `?` operator on `serde_json` calls will auto-convert via `anyhow::Error::from`.

## Issues in Code You Touched (Should Fix)

### MEDIUM -- Dead public type `EvalResult`

- **File:** `crates/rskim-bench/src/types.rs:24-34`
- **Confidence:** 95%
- **Description:** `EvalResult` is defined as a public struct with `Serialize`/`Deserialize` derives but is never referenced anywhere in the crate (not in harness, report, main, tests, or integration tests). The workspace convention is to delete dead code rather than keep it around -- CLAUDE.md explicitly states "Delete dead code -- commented-out code is not version control."
- **Suggestion:** Remove `EvalResult` from `types.rs`. If it is intended for future use, leave a `// TODO:` comment documenting the planned use case.

### LOW -- Missing justification comments on `#[allow(clippy::...)]` annotations in test modules

- **File:** `crates/rskim-bench/src/configs.rs:89`, `metrics.rs:80`, `split.rs:60`, `harness.rs:243`, `qrel.rs:216`, `tuning.rs:197`, `report.rs:129`
- **Confidence:** 80%
- **Description:** The rskim-core crate consistently includes an inline comment explaining each `#[allow]` annotation (e.g., `#[allow(clippy::expect_used)] // Allow expect in tests - it's acceptable for test code to panic on unexpected errors`). The new crate's test modules use bare `#[allow(clippy::unwrap_used)]` without any comment. While not functionally significant, this is a minor style deviation from the established workspace pattern.
- **Suggestion:** Add inline comments matching the existing convention:
  ```rust
  #[allow(clippy::unwrap_used)] // Unwrapping is acceptable in tests
  ```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Inconsistent `let _` discard pattern** - `crates/rskim-bench/src/main.rs:278` (`let _base = builder.build()`) vs `crates/rskim-bench/src/harness.rs:79` (`let _base_layer = builder.build()`) -- minor naming inconsistency in discarded bindings. (Confidence: 65%)

- **`TuningResult` uses `Vec<f32>` for field arrays while `BM25FConfig` uses `[f32; 8]`** - `crates/rskim-bench/src/types.rs:97-98` -- The `best_field_boosts` and `best_field_b` fields are `Vec<f32>` but `BM25FConfig` uses fixed-size arrays `[f32; 8]`. This forces `result_to_config()` to do manual copy-by-index. Using `[f32; 8]` would be more type-safe and eliminate the conversion code. However, the `Vec` choice may be intentional for JSON serialization flexibility. (Confidence: 72%)

- **`harness::evaluate_split` silently swallows search errors** - `crates/rskim-bench/src/harness.rs:144` -- `layer.search(&query).unwrap_or_default()` silently treats search errors as empty results. The `main.rs` tuning closure does the same at line 294. While acceptable for a benchmark harness where partial failures should degrade gracefully, this deviates from the workspace's "fail loud with clear error messages" principle in CLAUDE.md. (Confidence: 65%)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 1 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 7/10
**Recommendation**: CHANGES_REQUESTED
