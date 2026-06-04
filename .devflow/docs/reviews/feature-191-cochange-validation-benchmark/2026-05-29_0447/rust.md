# Rust Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29T04:47:00Z

## Issues in Your Changes (BLOCKING)

### HIGH

**Clippy `cmp_owned` violations (2 occurrences)** - Confidence: 95%
- `crates/rskim-bench/src/cochange/deny_list.rs:314`, `crates/rskim-bench/src/cochange/deny_list.rs:315`
- Problem: `PathBuf::from("src/main.rs")` and `PathBuf::from("src/lib.rs")` allocate a new `PathBuf` solely for comparison against `f.path`. Clippy's `cmp_owned` lint fires and fails CI because the Cargo.toml enforces `[lints.clippy] unwrap_used = "deny"` (and the global `-D warnings` flag treats all warnings as errors). `cargo clippy -p rskim-bench --all-targets --all-features -- -D warnings` currently fails with 2 errors.
- Fix: Replace `PathBuf::from(...)` with a `Path` reference or use the string directly, since `PathBuf` implements `PartialEq<&str>` via `Path`:
  ```rust
  // Before
  assert!(files.iter().any(|f| f.path == PathBuf::from("src/main.rs")));
  assert!(files.iter().any(|f| f.path == PathBuf::from("src/lib.rs")));
  // After
  assert!(files.iter().any(|f| f.path.as_path() == Path::new("src/main.rs")));
  assert!(files.iter().any(|f| f.path.as_path() == Path::new("src/lib.rs")));
  ```
  applies ADR-001 (fix noticed issues immediately)

### MEDIUM

**`chrono_now()` hand-rolled calendar arithmetic is fragile and untestable** - `crates/rskim-bench/src/bin/cochange_validate.rs:223-283` - Confidence: 82%
- Problem: The function implements Hinnant's civil_from_days algorithm from scratch (~60 lines of integer arithmetic with 12 named constants) to avoid adding a dependency. While the algorithm is correct and well-documented, it has no unit tests covering edge cases like leap year boundaries (e.g., 2000-02-29, 2100-03-01). The single test (`chrono_now_produces_iso8601_format`) only validates format at the current system time, not correctness across calendar boundaries. The CLAUDE.md doc comment is thorough, but the code is not deterministically testable because it reads `SystemTime::now()`.
- Fix: Either (a) extract the `secs -> (y, m, d, h, min, s)` conversion into a pure function that accepts `u64` epoch seconds and add targeted tests for known dates:
  ```rust
  #[cfg(test)]
  fn epoch_to_iso8601(secs: u64) -> String { /* same body minus SystemTime::now() */ }
  
  #[test]
  fn epoch_2000_02_29() {
      assert_eq!(epoch_to_iso8601(951782400), "2000-02-29T00:00:00Z");
  }
  ```
  or (b) use `time` crate's `OffsetDateTime::now_utc()` (already in the Cargo.lock via transitive deps) to eliminate the hand-rolled logic entirely.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`f64` equality in `compute_f1` denominator check** - `crates/rskim-bench/src/cochange/validate.rs:134` (Confidence: 65%) -- `if denom == 0.0` uses exact float equality. While safe here because 0.0 + 0.0 = 0.0 exactly in IEEE 754, an epsilon comparison would be more defensive against future changes where precision and recall could be near-zero but not exactly zero.

- **`split_off` index with `min(commits.len() - 1)` relies on checked subtraction** - `crates/rskim-bench/src/cochange/temporal_split.rs:92` (Confidence: 62%) -- `commits.len() - 1` is safe because the function returns early when `commits.len() <= 1`, but the safety margin is subtle. A `saturating_sub(1)` would make the intent clearer and survive future refactoring that might remove the early-return guards.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The codebase demonstrates strong Rust practices: proper error propagation with `?` and `anyhow`, `#[must_use]` annotations on pure functions, `Result` types throughout (applies ADR-001), clear ownership transfer semantics (e.g., `temporal_split` takes `Vec<CommitInfo>` by value, `build_and_evaluate` documents ownership of `train`), and thoughtful resource bounds (MAX_FILES_FOR_EVALUATION, MAX_TEST_COMMITS, MAX_FILES_PER_COMMIT). The `test-utils` feature gate (avoids PF-002 by keeping test helpers out of production builds) and clippy lints in Cargo.toml (`unwrap_used = "deny"`, `expect_used = "deny"`) are well-configured.

The BLOCKING issue is the clippy `cmp_owned` failure which prevents the crate from compiling under `--all-targets` with `-D warnings`. The MEDIUM issue (untestable calendar arithmetic) is a lower priority but worth addressing for long-term maintainability.
