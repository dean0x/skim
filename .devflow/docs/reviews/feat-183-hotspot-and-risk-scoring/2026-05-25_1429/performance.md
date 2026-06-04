# Performance Review Report

**Branch**: feat-183-hotspot-and-risk-scoring -> main
**Date**: 2026-05-25
**PR**: #252

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Redundant String allocation in hot loop** - `crates/rskim-search/src/temporal/scoring.rs:112`
**Confidence**: 90%
- Problem: `file.path_str().into_owned()` allocates a new `String` on every file-commit pair, even when the file path already exists in the `accum` HashMap. In a typical repository with 10,000 commits each touching 5 files, this produces ~50,000 allocations, but only ~2,000 are for genuinely new keys. The remaining ~48,000 allocations are immediately discarded after the `entry()` probe finds an existing key.
- Impact: Unnecessary allocator pressure proportional to total file-commit pairs rather than unique files. In large repositories (100K+ commit-file pairs), this can produce measurable overhead from allocation + deallocation churn.
- Fix: Use `get_mut` with a borrowed `&str` key first (exploiting `HashMap<String, _>::get_mut(&str)` via `Borrow<str>`), and only call `into_owned()` for new entries:

```rust
for file in &commit.changed_files {
    let cow = file.path_str();
    if let Some((weighted_total, weighted_fix_total)) = accum.get_mut(cow.as_ref()) {
        *weighted_total += w;
        if is_fix {
            *weighted_fix_total += w;
        }
    } else {
        let init = if is_fix { (w, w) } else { (w, 0.0) };
        accum.insert(cow.into_owned(), init);
    }
}
```

This eliminates all allocations on the common path (re-visiting an already-seen file) and only allocates when a genuinely new file path is encountered for the first time. The trade-off is a double hash probe on first insertion, but that is dominated by the allocation cost saved on all subsequent visits.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**HashMap capacity heuristic over-allocates for typical workloads** - `crates/rskim-search/src/temporal/scoring.rs:96`
**Confidence**: 80%
- Problem: `HashMap::with_capacity(commits.len().min(50_000))` uses commit count as the capacity hint, but the number of unique files is typically 5-20x smaller than the commit count. For a 10,000-commit history, this allocates space for 10,000 entries when only ~500-2,000 unique files exist. Each unused slot is ~72 bytes (key pointer + two f64s + hash + metadata).
- Impact: Moderate memory over-allocation (hundreds of KB wasted) but no correctness issue. Over-allocation is strictly better than under-allocation (which causes rehashing), so this is a trade-off, not a bug. The 50K cap is a good safety bound.
- Fix: Consider using total unique file count from a pre-pass, or a more conservative heuristic such as `commits.len().min(50_000) / 4` or a fixed reasonable default like `4096`. If the `into_owned()` optimization above is applied, the number of unique files can be tracked naturally. However, this is low priority since the current heuristic errs on the safe side.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Missing criterion benchmarks for scoring module** - `crates/rskim-search/src/temporal/scoring.rs` (Confidence: 70%) -- The codebase has criterion benchmarks for `rskim-core` transforms but none for `rskim-search` scoring. Adding a benchmark with 10K/50K/100K synthetic commits would establish a performance baseline, catch regressions, and validate the allocation optimization above. The PR description mentions "pure computation, no I/O" which makes it ideal for micro-benchmarking.

- **`decay_weight` called per-commit even when all files in a commit share the same weight** - `crates/rskim-search/src/temporal/scoring.rs:109` (Confidence: 65%) -- The weight `w` is computed once per commit and reused for all files in that commit, which is already correct. However, commits with identical timestamps (e.g., batch imports, cherry-picks) will redundantly compute the same `exp()` value. In practice, the `exp()` call is fast (~4ns on modern hardware) and timestamps rarely collide, so this is not actionable.

- **`FileRiskScores` does not derive `Copy`** - `crates/rskim-search/src/types.rs:269` (Confidence: 60%) -- The struct contains only two `f64` fields (16 bytes total) and is a prime candidate for `#[derive(Copy)]`. Adding `Copy` would allow pass-by-value without clone overhead and could enable the compiler to keep values in registers when consumers pattern-match on results. Low priority since the struct is returned in a HashMap and typically accessed by reference.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The implementation follows sound performance principles: single-pass algorithm, pre-computed fix classification (avoiding regex in the hot loop), pre-allocated HashMap, and `#[inline]` on the hot arithmetic function. The primary finding is an avoidable allocation pattern in the inner loop that causes O(total_file_touches) String allocations instead of O(unique_files). This is a well-known Rust HashMap pattern (borrow-check-then-insert vs. unconditional entry) and the fix is straightforward. The overall design is efficient and appropriate for the workload described.
