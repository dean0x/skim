# Complexity Review Report

**Branch**: feature/185-temporal-metadata-storage -> main
**Date**: 2026-05-26T13:09

## Issues in Your Changes (BLOCKING)

### HIGH

**`compute_file_temporal_stats` exceeds 50-line function length threshold** - `scoring.rs:214-275`
**Confidence**: 82%
- Problem: At 61 lines, `compute_file_temporal_stats` crosses the 50-line warning threshold. It contains two sequential loops over commit data (deduplication pass at line 246, then accumulation at line 254), each with nested conditionals. Maximum nesting depth reaches 4 levels (for -> if -> if -> body). The function is still comprehensible within 5 minutes, but sits in the zone where further growth would push it past the critical threshold.
- Mitigating factor: The function follows an established pattern already used by the sibling `compute_file_risk_scores` (83 lines, pre-existing). The PR author documented the algorithm in the doc comment, and the function is pure with no I/O. The two-pass structure (dedup then accumulate) is inherently sequential and would not benefit much from extraction unless the dedup pass were reused elsewhere.
- Fix (optional): Extract the per-commit deduplication (lines 245-252) into a helper function like `dedup_changed_files(commit, seen_buf) -> &HashSet<String>`. This would reduce the main function to ~48 lines and isolate the borrow-first optimization for independent testing.

```rust
fn dedup_changed_files<'a>(
    commit: &CommitInfo,
    buf: &'a mut HashSet<String>,
) -> &'a HashSet<String> {
    buf.clear();
    for file in &commit.changed_files {
        let path_cow = file.path_str();
        let path_ref: &str = &path_cow;
        if !buf.contains(path_ref) {
            buf.insert(path_cow.into_owned());
        }
    }
    buf
}
```

### MEDIUM

**Magic number `86_400.0` appears in two functions without a named constant** - `scoring.rs:129`, `scoring.rs:233`
**Confidence**: 85%
- Problem: The literal `86_400.0` (seconds per day) appears in both `compute_file_risk_scores` (line 129, pre-existing) and the new `compute_file_temporal_stats` (line 233). While most developers will recognize it, the duplication across two functions in the same file is a readability and maintainability concern -- a named constant communicates intent immediately.
- Fix: Add a module-level constant and use it in both functions.

```rust
/// Seconds in one day, used for epoch-to-days conversion.
const SECS_PER_DAY: f64 = 86_400.0;
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`compute_file_risk_scores` at 83 lines exceeds the 50-line critical threshold** - `scoring.rs:101-184`
**Confidence**: 90%
- Problem: This function was not modified in this PR but is in the same file. At 83 lines it exceeds the critical threshold for function length. It shares the same structural pattern as `compute_file_temporal_stats` (single-pass accumulation with borrow-first optimization). Consider extracting shared patterns if this file grows further.

## Suggestions (Lower Confidence)

- **Structural repetition in store/load methods** - `storage_ops.rs` (Confidence: 72%) -- The three `store_*` methods (lines 100-155) and three `load_*` methods (lines 183-258) follow identical patterns (capacity check -> transaction -> insert helper for stores; prepare -> query_map -> collect for loads). This is a deliberate three-module separation per the feature knowledge, and Rust's type system makes generic abstraction over different row types non-trivial without macros. The repetition is currently 3 instances of each pattern (well under the 5+ consolidation threshold) and each method is short (11-21 lines). Not flagging as blocking, but worth watching if more table types are added.

- **`sync` method at 47 lines is approaching the warning threshold** - `storage_ops.rs:304-351` (Confidence: 65%) -- The `sync` method orchestrates capacity validation, three insert helpers, timestamp computation, and meta writes. It is readable and well-structured, but at 47 lines it is near the 50-line warning zone. The early-return capacity checks (lines 311-321) could be extracted into a validation helper if the method grows further.

- **`run_migrations` at 48 lines** - `storage.rs:84-132` (Confidence: 62%) -- Most of this is a SQL string literal (CREATE TABLE statements). The cyclomatic complexity is low (one forward-compat guard + one version gate). The length is driven by DDL, not logic. Would only become a concern if many more schema versions are added.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Complexity Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The PR introduces a well-structured SQLite persistence layer with clean module separation (storage.rs for schema/connection, storage_types.rs for row types, storage_ops.rs for CRUD). Function lengths are generally excellent -- 10 of the 12 new functions are under 30 lines. The `compute_file_temporal_stats` function at 61 lines is the only one crossing a threshold, and it follows an established pattern from the pre-existing `compute_file_risk_scores`. Cyclomatic complexity is low throughout (max nesting depth of 4 in `compute_file_temporal_stats`). The magic number `86_400.0` should be extracted to a named constant. No CRITICAL complexity issues found.
