# Reliability Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29T04:47:00Z

## Issues in Your Changes (BLOCKING)

### MEDIUM

**`compute_actual_sets` allocates O(F^2) HashSets per commit** - `crates/rskim-bench/src/cochange/validate.rs:681-692`
**Confidence**: 82%
- Problem: For each query in `known_ids`, `compute_actual_sets` builds a full `HashSet<FileId>` containing every other known id. For a commit with K mapped files this allocates K sets of size K-1 each, totaling O(K^2) memory. While `MAX_FILES_PER_COMMIT=500` bounds K, a 500-file commit still produces 500 sets of 499 elements (~250K entries, ~2 MB of HashSet overhead). The predicted_scratch reuse pattern (line 247) successfully avoids Q*T allocations for the *predicted* sets, but the *actual* sets are rebuilt identically for every commit without reuse.
- Fix: Pre-compute one `HashSet` of all `known_ids`, then for each query, compute intersection counts against `predicted_scratch` directly using the full set minus self, rather than materializing K separate sets. Alternatively, reuse a single scratch `HashSet<FileId>` for actual sets too:
```rust
// Instead of Vec<HashSet<FileId>>, compute actual membership inline:
// For each query q, actual = known_ids \ {q}
// Since predicted_scratch already exists, the intersection count
// can be computed without a separate HashSet allocation.
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **PID reuse race in SIGKILL + join pattern** - `crates/rskim-research/src/clone.rs:84-117` (Confidence: 65%) -- The `child_id` captured before `std::thread::spawn` could theoretically refer to a different process if the original process exits and the OS reuses the PID before the timeout branch executes `libc::kill`. This is an extremely narrow window and mitigated by the `handle.join()` call, but the pattern is inherently racy on POSIX systems. The prior resolution (cycle 2) addressed the detached-thread half of this; the PID reuse risk remains a theoretical concern.

- **`chrono_now` Gregorian arithmetic is hand-rolled and untested for edge dates** - `crates/rskim-bench/src/bin/cochange_validate.rs:223-283` (Confidence: 68%) -- The Hinnant civil_from_days algorithm is implemented inline to avoid a `chrono` dependency. While the constants are correctly attributed and the format test passes, there is no test for known leap-year or century boundary dates (e.g., 2000-02-29, 2100-03-01). A wrong date in the timestamp field would not break execution but would impair reproducibility manifest accuracy.

- **`jaccard_cache` Vec-of-Vec allocation per commit could be large** - `crates/rskim-bench/src/cochange/validate.rs:640-648` (Confidence: 62%) -- `compute_jaccard_cache` returns `Vec<Vec<(FileId, f64)>>` allocated fresh for each test commit. With `MAX_FILES_PER_COMMIT=500` files and potentially thousands of positive jaccard pairs per query, this could produce significant allocation pressure across 50k test commits. The positive-only filtering helps, but there is no size cap on the inner Vec.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | - | 0 | 0 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Condition
The single MEDIUM blocking finding (actual-set allocation pattern) is a performance-reliability concern: it works correctly but introduces unnecessary allocation pressure proportional to commit size squared. This is bounded by the existing `MAX_FILES_PER_COMMIT=500` guard, which prevents the worst case, so it does not rise to blocking severity. The code is safe to merge.

### Reliability Strengths (applies ADR-001)

This branch demonstrates strong reliability discipline, particularly given the prior resolution cycle (cycle 2) that addressed 19 of 20 issues:

1. **Bounded iteration** -- All loops operate on bounded inputs: `MAX_TEST_COMMITS=50_000`, `MAX_FILES_PER_COMMIT=500`, `MAX_FILES_FOR_EVALUATION=20_000`. The `evaluate_at_thresholds` function checks all three limits before entering the main loop. No unbounded loops exist in the diff.

2. **Assertion density** -- Guard conditions with `anyhow::bail!` at every phase boundary: `build_path_map` checks FileId overflow, `evaluate_at_thresholds` checks file/commit limits, `check_quality_gates` validates minimum commit counts and history span, `parse_thresholds` validates range and rejects NaN. The `temporal_split` function handles NaN, Inf, empty, and single-element inputs without panicking.

3. **Allocation discipline** -- `predicted_scratch` is pre-allocated once and reused across all (commit, threshold, query) iterations (line 247), eliminating the Q*T per-commit allocation pattern identified in cycle 2. `BTreeSet` is used in `build_path_map` to deduplicate while maintaining sort order. `Vec::split_off` in `temporal_split` achieves zero-copy train/test separation.

4. **Resource cleanup** -- `tempfile::tempdir()` in `build_and_evaluate` ensures the SQLite co-change index is cleaned up via RAII drop. Thread handles are joined after SIGKILL in both timeout helpers (addressed in cycle 2).

5. **Soft failure design** -- `validate_repo` converts all sub-phase errors into `RepoCochangeResult` fields (`error`, `quality_gate_reason`) so a single broken repo does not abort the entire parallel run.

6. **Input validation at boundaries** (avoids PF-002) -- `clone_with_history` validates HTTPS prefix, `extract_repo_name` rejects path-traversal names, `parse_thresholds` rejects NaN/out-of-range/empty input, `temporal_split` clamps NaN fractions to 0.8 default.
