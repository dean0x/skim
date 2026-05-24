---
focus: rust
reviewer: Reviewer
timestamp: 2026-05-21T14:03:35Z
pr: 247
branch: feat/195-bm25f-bench
head: 101385eb
---

# Rust Review

## Summary

The rskim-bench crate is a well-structured, additive-only addition with strong Rust discipline: clippy clean with `unwrap_used = "deny"`, proper `anyhow` error propagation, `#[must_use]` annotations, and comprehensive tests (89 passing). The findings below are minor ergonomic and type-safety improvements -- nothing blocking.

## Findings

### should-fix -- `&PathBuf` parameter should be `&Path` (C-BORROW)
- **File:** `crates/rskim-bench/src/main.rs:128`
- **Confidence:** 95%
- **Description:** `open_corpus` accepts `corpus_dir: &PathBuf` instead of `&Path`. Per the Rust API guidelines (C-BORROW), functions should accept the borrowed form of a type (`&Path`) rather than a reference to the owned form (`&PathBuf`). `&PathBuf` auto-derefs to `&Path` at call sites, so this is not a functional bug, but it is non-idiomatic and constrains callers unnecessarily.
- **Suggestion:** Change the signature to `corpus_dir: &std::path::Path`. The `.clone()` on line 136 would then use `corpus_dir.to_path_buf()` instead, which is semantically clearer.

### should-fix -- `&Vec<ConfigMetrics>` return type should be `&[ConfigMetrics]` (C-BORROW)
- **File:** `crates/rskim-bench/src/harness.rs:203`
- **Confidence:** 92%
- **Description:** The `macro_average` function's trait bound specifies `F: Fn(&RepoBenchResult) -> &Vec<ConfigMetrics>`. Returning `&Vec<T>` instead of `&[T]` is non-idiomatic Rust -- it exposes the concrete container type when only a slice is needed. This limits the trait bound unnecessarily (callers must return a reference to a `Vec`, not any slice-like container).
- **Suggestion:** Change to `F: Fn(&RepoBenchResult) -> &[ConfigMetrics]`.

### should-fix -- Output format fields use `String` instead of `ValueEnum`
- **File:** `crates/rskim-bench/src/main.rs:60` (also lines 79, 105)
- **Confidence:** 88%
- **Description:** The `output` field on `BenchArgs`/`TuneArgs` and `format` field on `ReportArgs` accept arbitrary `String` values. Invalid inputs like `"xml"` or `"csv"` silently fall through to the markdown default via the `_ =>` match arm. This violates the principle of making illegal states unrepresentable. Clap's `ValueEnum` derive provides compile-time exhaustive matching and proper error messages for free.
- **Suggestion:**
  ```rust
  #[derive(Debug, Clone, clap::ValueEnum)]
  enum OutputFormat {
      Json,
      Markdown,
  }
  ```
  Then use `#[arg(long, default_value_t = OutputFormat::Markdown)] output: OutputFormat` and match on the enum variants.

### should-fix -- `BenchConfig` missing `Debug` derive
- **File:** `crates/rskim-bench/src/harness.rs:17`
- **Confidence:** 90%
- **Description:** `BenchConfig` is a public struct without a `Debug` derive. All other public types in this crate (`Qrel`, `EvalResult`, `ConfigMetrics`, `RepoBenchResult`, `BenchResult`, `TuningResult`, `IndexedFile`, `QrelInput`, `ExtractedSymbol`) derive `Debug`. This inconsistency makes it harder to debug benchmark runs and violates Rust API guidelines (C-DEBUG).
- **Suggestion:** Add `#[derive(Debug)]` to `BenchConfig`.

### should-fix -- `TuningResult` uses `Vec<f32>` for fixed-size-8 arrays
- **File:** `crates/rskim-bench/src/types.rs:97-98`
- **Confidence:** 82%
- **Description:** `TuningResult::best_field_boosts` and `best_field_b` are `Vec<f32>` but are always exactly 8 elements (matching `FIELD_COUNT`). In `result_to_config()` (tuning.rs:165-172), the code silently zero-fills if the Vec has fewer than 8 elements, which could produce incorrect BM25F configurations. Using `[f32; 8]` would make the invariant unrepresentable at the type level.
- **Suggestion:** Change both fields to `[f32; 8]`. This eliminates the heap allocation, removes the silent zero-fill path in `result_to_config`, and matches the `BM25FConfig` field types exactly. Serde supports fixed-size arrays natively.

### informational -- Redundant `tempfile` in `[dev-dependencies]`
- **File:** `crates/rskim-bench/Cargo.toml:35`
- **Confidence:** 90%
- **Description:** `tempfile` is listed in both `[dependencies]` (line 32) and `[dev-dependencies]` (line 35). Since it's already a normal dependency (needed by the binary for temp index dirs), the `[dev-dependencies]` entry is redundant -- Cargo will use the `[dependencies]` entry for both production and test builds.
- **Suggestion:** Remove the `[dev-dependencies]` entry for `tempfile`.

### informational -- Redundant symbol filtering (minor memory waste in qrel generation)
- **File:** `crates/rskim-bench/src/qrel.rs:68-89`
- **Confidence:** 80%
- **Description:** Phase 1 (lines 68-81) pushes ALL extracted symbols to `raw_symbols` regardless of whether they pass the name filter, but only adds filtered symbols to `df_map`. Phase 2 (lines 84-89) then re-applies the identical filter. This means symbols that fail the filter are allocated into `raw_symbols` only to be immediately discarded. For benchmark-scale corpora this is unlikely to matter, but it is wasted work.
- **Suggestion:** Move the filter check before the `raw_symbols.push()` call, or remove Phase 2 entirely since Phase 1 already guards the push.

## Suggestions (Lower Confidence)

- **`i as u32` truncation in FileId construction** - `crates/rskim-bench/src/main.rs:183,191,399` (Confidence: 65%) -- `usize` to `u32` cast via `as` silently truncates on 64-bit systems if corpus exceeds 4B files. Practically impossible for a benchmark harness, but `u32::try_from(i)?` is more robust Rust.

- **Search errors silently swallowed in evaluate_split** - `crates/rskim-bench/src/harness.rs:144` (Confidence: 70%) -- `.unwrap_or_default()` silently drops search errors. If the index reader fails for a specific query (e.g., corrupt segment), this inflates the denominator with zero-contribution queries, silently depressing MRR. Logging the error to stderr would aid debugging.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | - | 0 | 5 | 0 |
| Pre-existing | - | - | 0 | 2 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

Conditions: The five should-fix items are all minor ergonomic improvements (idiomatic Rust types, missing derives, enum for output format). None affect correctness for the current use case. The crate demonstrates strong discipline with `clippy::unwrap_used = "deny"`, proper `anyhow` error handling throughout, and good test coverage. Clippy passes with zero warnings. All 89 tests pass.
