# Reliability Review Report

**Branch**: main (PR #253)
**Date**: 2026-05-26
**Focus**: Reliability -- bounded iteration, assertion density, allocation discipline, resource lifecycle

## Issues in Your Changes (BLOCKING)

### HIGH

**u32 counter overflow in `compute_file_temporal_stats` hot loop** - `crates/rskim-search/src/temporal/scoring.rs:251-259`
**Confidence**: 85%
- Problem: The `FileTemporalStats` fields (`total_commits`, `fix_commits`, `changes_30d`, `changes_90d`) are `u32` and incremented with `+= 1` in an unbounded loop. A repository with more than 4,294,967,295 commits touching a single file would silently wrap in release mode (Rust default wrapping semantics for `+=` on integers) or panic in debug mode. While no real repository reaches this today, the code has zero bounds-checking or saturating arithmetic, and the `commits` slice length is externally controlled by the caller with no upper bound enforced.
- Fix: Use `saturating_add(1)` instead of `+= 1` to prevent silent wrapping. This is the standard defensive pattern for counters derived from unbounded external input:
```rust
entry.total_commits = entry.total_commits.saturating_add(1);
if is_fix {
    entry.fix_commits = entry.fix_commits.saturating_add(1);
}
if in_30d {
    entry.changes_30d = entry.changes_30d.saturating_add(1);
}
if in_90d {
    entry.changes_90d = entry.changes_90d.saturating_add(1);
}
```

### MEDIUM

**No upper bound on `store_*` / `sync` input slice lengths** - `crates/rskim-search/src/temporal/storage_ops.rs:29-98,250-325`
**Confidence**: 82%
- Problem: The `store_hotspots`, `store_risks`, `store_cochanges`, and `sync` methods accept `&[HotspotRow]` / `&[RiskRow]` / `&[CochangeRow]` with no size guard. The co-change matrix builder has `MAX_PAIRS = 2_000_000` to bound memory, but the SQLite storage layer has no analogous limit. A caller passing a pathologically large slice (e.g., millions of rows from a corrupted pipeline) will cause unbounded INSERT iterations within a single SQLite transaction, growing the WAL file and potentially exhausting disk. The reliability Iron Law states "every loop must have a fixed upper bound."
- Fix: Add a capacity constant and check at the top of `sync` (and optionally in individual `store_*` methods):
```rust
const MAX_ROWS_PER_TABLE: usize = 500_000;

pub fn sync(&self, hotspots: &[HotspotRow], risks: &[RiskRow], cochanges: &[CochangeRow], git_head: &str) -> Result<()> {
    if hotspots.len() > MAX_ROWS_PER_TABLE
        || risks.len() > MAX_ROWS_PER_TABLE
        || cochanges.len() > MAX_ROWS_PER_TABLE
    {
        return Err(SearchError::CapacityExceeded(format!(
            "temporal sync row count exceeds {MAX_ROWS_PER_TABLE}: hotspots={}, risks={}, cochanges={}",
            hotspots.len(), risks.len(), cochanges.len()
        )));
    }
    // ... existing code
}
```

**`unchecked_transaction` used without documenting safety invariant** - `crates/rskim-search/src/temporal/storage_ops.rs:30,54,84,257`
**Confidence**: 80%
- Problem: `unchecked_transaction()` is used 4 times. In rusqlite, `unchecked_transaction()` differs from `transaction()` in that it does not check whether a transaction is already active -- if one is, SQLite silently ignores the `BEGIN` and the `COMMIT` at the end commits the outer transaction. This is a correctness hazard if `store_hotspots` is ever called from within `sync` (which it is not today, but the API is public and nothing prevents a caller from composing them). The doc comments do not mention this invariant.
- Fix: Add a safety note to each `store_*` method:
```rust
/// # Safety invariant
///
/// Uses `unchecked_transaction` — must NOT be called from within an
/// existing transaction on the same connection. Use [`sync`] for
/// multi-table atomic writes.
```
  Alternatively, switch to `self.conn.transaction()` which returns an error if a transaction is already active, making misuse a runtime error rather than silent corruption.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`is_fix_commit` called inline per commit in `compute_file_temporal_stats` instead of pre-classified** - `crates/rskim-search/src/temporal/scoring.rs:228`
**Confidence**: 82%
- Problem: The feature knowledge explicitly warns against this pattern: "Calling `is_fix_commit` inside the per-file loop -- the pre-classification step in `compute_file_risk_scores` exists to avoid this." While `compute_file_temporal_stats` calls `is_fix_commit` once per commit (not per file), the knowledge doc and the sister function `compute_file_risk_scores` both pre-classify into a `Vec<bool>` to make the pattern explicit and consistent. The inline call is not inside the per-file inner loop so the performance impact is identical (one regex eval per commit), but the inconsistency creates a maintenance hazard: a future refactor could move it inside the `for path in &seen_in_commit` loop without realizing the cost.
- Fix: Pre-classify fix flags before the main loop, matching the pattern in `compute_file_risk_scores`:
```rust
let fix_flags: Vec<bool> = commits.iter().map(|c| super::is_fix_commit(&c.message)).collect();

for (commit, &is_fix) in commits.iter().zip(fix_flags.iter()) {
    // ...
}
```

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`TemporalDb::open` silently ignores `set_permissions` failure** - `crates/rskim-search/src/temporal/storage.rs:180`
**Confidence**: 88%
- Problem: `let _ = std::fs::set_permissions(db_path, perms);` discards the error. The feature knowledge documents this gotcha: "TemporalDb::open sets file permissions to 0o600 on Unix but silently ignores the error if set_permissions fails (e.g., on read-only filesystems or Docker volumes). Sensitive data in the database is not protected in those environments." On a reliability review this is notable because the code advertises security properties (0o600) that it cannot guarantee.
- Fix: Log the error to stderr when `SKIM_DEBUG` is set, or return it as a non-fatal warning.

**`decay_weight` panics on invalid `half_life_days` via `assert!` in production code** - `crates/rskim-search/src/temporal/scoring.rs:62-65`
**Confidence**: 85%
- Problem: The function uses `assert!` (not `debug_assert!`) for the `half_life_days > 0.0 && half_life_days.is_finite()` check. This means a caller passing `0.0` or `NaN` causes a panic in release builds. The CLAUDE.md Rust rules say "debug_assert! for invariants in hot paths -- assert! at module boundaries." `decay_weight` is a leaf function called in a hot loop, not a module boundary. The feature knowledge acknowledges this: "decay_weight panics in debug builds when half_life_days <= 0.0 (debug_assert!)" -- but the actual code uses `assert!`, not `debug_assert!`, contradicting the knowledge doc.
- Fix: Either (a) change to `debug_assert!` and return a safe fallback (e.g., `1.0`) for invalid input in release, or (b) return `Result<f64>` to make the error explicit. Given this is called in a hot loop, option (a) is more practical.

## Suggestions (Lower Confidence)

- **Missing `#[must_use]` on `store_*` and `sync` methods** - `crates/rskim-search/src/temporal/storage_ops.rs:29,53,83,250` (Confidence: 70%) -- These methods return `Result<()>` which callers could silently discard. The `load_*` and `schema_version` methods already have `#[must_use]`; the store methods should be consistent.

- **`HashSet<String>` allocation in dedup buffer** - `crates/rskim-search/src/temporal/scoring.rs:245` (Confidence: 65%) -- Each commit iteration calls `file.path_str().into_owned()` for every file in the commit, allocating a `String` even when the path was already in `seen_in_commit`. Since the dedup buffer is cleared each iteration but not shrunk, it retains capacity across commits (good), but the per-file allocation is avoidable if the dedup check used a borrowed key first (matching the pattern in `compute_file_risk_scores` lines 141-146).

- **Performance test thresholds are hardcoded wall-clock limits** - `crates/rskim-search/src/temporal/storage_perf_tests.rs:140-225` (Confidence: 62%) -- Tests like `load_10k_hotspots_under_100ms` and `sync_10k_each_under_500ms` use `Instant::now()` wall-clock thresholds. These may flake on slow CI runners or heavily loaded machines. Consider using relative comparisons or marking them `#[ignore]` with a dedicated benchmark suite.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 2 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The core architecture is sound: WAL mode with busy timeout, forward-compat schema guard, atomic `sync` transaction, and proper error conversion at the rusqlite boundary. The primary reliability concern is the unbounded `u32` counter increment in `compute_file_temporal_stats` which uses wrapping arithmetic on externally-sized input, and the absence of row-count bounds on the SQLite store methods. The `unchecked_transaction` usage is safe today but fragile against future composition.
