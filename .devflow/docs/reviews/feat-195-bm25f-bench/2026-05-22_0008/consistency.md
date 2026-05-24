# Consistency Review Report

**Branch**: feat/195-bm25f-bench -> main
**Date**: 2026-05-22

## Issues in Your Changes (BLOCKING)

### HIGH

**Missing `#![allow(clippy::unwrap_used)]` in integration test file** - `crates/rskim-bench/tests/integration.rs:1`
**Confidence**: 95%
- Problem: The crate's `Cargo.toml` sets `unwrap_used = "deny"` and `expect_used = "deny"` under `[lints.clippy]`. Every inline test module in `src/` has an `#[allow(clippy::unwrap_used)]` annotation. However, the integration test file `tests/integration.rs` has no such annotation despite using `.unwrap()` in 17+ locations and `.unwrap_err()` in 2 locations. This triggers 19 clippy errors when running `cargo clippy -p rskim-bench --tests`. The newly added tests (`extract_symbols_dispatch_integration`, `run_on_files_too_few_qrels_returns_error`, `aggregate_results_rejects_mismatched_config_names`) all use `.unwrap()` and `.unwrap_err()` without the required allow annotation.
- Fix: Add `#![allow(clippy::unwrap_used, clippy::expect_used)]` at the top of `tests/integration.rs`:
```rust
//! Integration tests for rskim-bench.
//!
//! These tests run the full pipeline ...
#![allow(clippy::unwrap_used, clippy::expect_used)] // test code -- unwrap/expect acceptable for test assertions
```

**Unused import `SearchField` in test module** - `crates/rskim-bench/src/report.rs:146`
**Confidence**: 95%
- Problem: The test module imports `rskim_search::{FIELD_COUNT, SearchField}` but only uses `FIELD_COUNT`. The `SearchField` import is unused and triggers a compiler warning. This is a newly added import in this PR (the old code used hardcoded field name strings and did not import `SearchField` in tests).
- Fix: Remove `SearchField` from the import:
```rust
use rskim_search::FIELD_COUNT;
```

### MEDIUM

**Inconsistent `#[allow(clippy::unwrap_used)]` comment style across extract modules (3 occurrences)** - `crates/rskim-bench/src/extract/go.rs:109`, `crates/rskim-bench/src/extract/python.rs:114`, `crates/rskim-bench/src/extract/rust_lang.rs:122`
**Confidence**: 85%
- Problem: This PR added explanatory comment suffixes to `#[allow(clippy::unwrap_used)]` annotations in most modules (e.g., `// test code -- unwrap acceptable for test assertions`). However, the three extract sub-modules (`go.rs`, `python.rs`, `rust_lang.rs`) still use the bare `#[allow(clippy::unwrap_used)]` without the comment suffix. The PR was inconsistent in applying its own convention -- it updated `configs.rs`, `metrics.rs`, `qrel.rs`, `report.rs`, `split.rs`, `tuning.rs`, and `harness.rs` but missed the extract modules.
- Fix: Add the comment suffix to each:
```rust
#[allow(clippy::unwrap_used)] // test code -- unwrap acceptable for test assertions
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`walk_ast_with_parser` is currently unused externally** - `crates/rskim-bench/src/extract/mod.rs:45` (Confidence: 65%) -- The `pub(crate)` function is documented as enabling parser reuse across files, but no caller uses it directly; only `walk_ast` calls it. This is harmless forward-looking API surface, but consider marking it `#[allow(dead_code)]` or adding a `#[cfg(test)]` usage if the intended parser-reuse pattern is not coming soon.

- **`field_display_name` match is not exhaustive via `_ =>` and may drift if new `SearchField` variants are added** - `crates/rskim-bench/src/report.rs:32` (Confidence: 60%) -- The function uses explicit match arms without a wildcard, which is good. However, the eight hardcoded strings duplicate what could be derived from `SearchField`'s debug representation or a `Display` impl on the enum itself. Minor DRY concern only.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

### Positive Consistency Observations

The PR demonstrates strong internal consistency in several areas:

1. **FIELD_COUNT constant adoption**: All hardcoded `8` values for field array sizes have been consistently replaced with `FIELD_COUNT` across configs, tuning, types, and tests.
2. **Error handling pattern**: The shift from `aggregate_results` returning `BenchResult` to `anyhow::Result<BenchResult>` is applied consistently at all call sites.
3. **Single-reader pattern**: The removal of `open_with_config` per-config reader creation in favour of a single reader with `SearchQuery::bm25f_config` overrides is consistently applied in both `run_on_files` and `run_tune`.
4. **OutputFormat enum**: The string-based format matching (`"json"`, `"markdown"`) is consistently replaced with a typed `OutputFormat` enum across all three subcommands.
5. **`QrelInput` borrowing**: The `content: String` to `content: &'a str` change is consistently propagated through all callers.
6. **DRY extraction**: The `walk_ast` helper and `load_repo_files`, `build_index`, `make_train_qrels` decompositions consistently reduce duplication.
7. **`repo_url` parameter**: The previously error-prone pattern of mutating `result.repo_url` after construction is consistently replaced by passing `repo_url` directly to `run_on_files`.

The two blocking issues are straightforward fixes (missing crate-level clippy allow, unused import). The medium-severity annotation inconsistency in extract modules is a minor polish item.
