# Complexity Review Report

**Branch**: PR #242 -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**`Pipeline::run()` is a 134-line method with high cyclomatic complexity** - `index.rs:184-318`
**Confidence**: 85%
- Problem: `Pipeline::run()` spans 134 lines (lines 184-318) and contains producer thread spawning, consumer loop, error handling, manifest writing, and result assembly. The cyclomatic complexity is approximately 12 (early return for empty, `if force` branch, match on `read_and_classify`, `if tx.send().is_err()`, `if let Err(e)` on `add_file_classified`, `if debug_enabled`, `if pf.cache_hit`, `join().map_err()`). This exceeds the warning threshold of 10 and the 50-line function length threshold for HIGH severity.
- Impact: Difficult to test individual pipeline stages in isolation. A developer modifying one stage (e.g., the consumer loop) must mentally trace the entire method to understand side effects. The interleaving of producer and consumer setup in a single scope makes the control flow harder to follow than necessary.
- Fix: Extract the consumer loop body (lines 256-292) into a `consume_processed_files()` method that takes `rx`, `builder`, `new_manifest`, and `debug_enabled`, returning `(next_file_id, cache_hits)`. This would reduce `run()` to ~80 lines and make the consumer independently testable:
```rust
fn consume_files(
    rx: crossbeam_channel::Receiver<ProcessedFile>,
    builder: &mut NgramIndexBuilder,
    manifest: &mut FileManifest,
    debug: bool,
) -> anyhow::Result<(u32, u32)> {
    let mut next_file_id: u32 = 0;
    let mut cache_hits: u32 = 0;
    for pf in rx {
        // ... existing consumer body ...
    }
    Ok((next_file_id, cache_hits))
}
```

### MEDIUM

**`walk_metadata()` duplicates the structural pattern of `walk_and_read()` almost line-for-line** - `walk.rs:370-453`
**Confidence**: 82%
- Problem: `walk_metadata()` (84 lines) replicates the same Arc/Mutex/AtomicUsize/AtomicBool setup, parallel walker closure, `Arc::try_unwrap`, truncation, and sorting pattern as the test-only `walk_and_read()` (54 lines of shared logic shape). While the inner classification differs (`MetaOutcome` vs `EntryOutcome`), the outer orchestration is structurally identical. The parallel walker closure body in `walk_metadata()` (lines 389-429) is 40 lines deep with 4 nesting levels (closure > match > match > if).
- Impact: Any bug fix to the walker orchestration pattern (e.g., the `cap_reached` TOCTOU atomics, poisoned-lock handling, truncation) must be applied in both places. The `walk_and_read()` is now `#[cfg(test)]` only, which mitigates production risk, but the duplication still increases maintenance cost.
- Fix: Consider extracting a generic `parallel_walk<T>()` function parameterized by the classification function and outcome type, so the walker orchestration is written once. Alternatively, since `walk_and_read` is test-only, build it on top of `walk_metadata` + `open_and_read` to eliminate the duplication entirely.

**`read_and_classify()` constructs `ProcessedFile` in two near-identical branches** - `index.rs:329-392`
**Confidence**: 80%
- Problem: The function constructs `ProcessedFile` in two places (cache-hit path at lines 369-377, and cache-miss path at lines 383-391) with 6 of 7 fields identical between them. Only `field_map` and `cache_hit` differ.
- Impact: If a new field is added to `ProcessedFile`, both construction sites must be updated. This is a minor DRY violation but worth noting given the function already has 4 early-return paths from the `ReadOutcome` match.
- Fix: Construct a base `ProcessedFile` first and then set `field_map`/`cache_hit` conditionally:
```rust
let (field_map, cache_hit) = if !force
    && let Some(cached) = manifest.lookup(&path_key)
    && cached.sha256 == sha
{
    (decode_field_map(&cached.field_map), true)
} else {
    (run_classify(&content, entry.lang, debug), false)
};

Ok(ProcessedFile {
    rel_path: entry.rel_path.clone(),
    lang: entry.lang,
    content,
    sha256: sha,
    mtime: entry.mtime,
    field_map,
    cache_hit,
})
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

### MEDIUM

**`classify_entry()` (test-only) mirrors `classify_entry_metadata()` + `open_and_read()` logic** - `walk.rs:144-218`
**Confidence**: 82%
- Problem: The test-only `classify_entry()` duplicates the language detection, size pre-screen, and `open_and_read()` dispatch that now also exists in `classify_entry_metadata()` + `read_and_classify()`. This is 75 lines of code that overlaps substantially with the production path.
- Impact: Pre-existing tech debt. The test-only classification path could silently diverge from production behavior, making walk tests less trustworthy.

## Suggestions (Lower Confidence)

- **`walk_metadata` closure nesting depth reaches 4 levels** - `walk.rs:389-429` (Confidence: 70%) -- The closure body uses `match > match > if` nesting that could be flattened with early-continue or helper extraction, but the current depth of 4 is at the warning threshold rather than critical.

- **`ProcessedFile` has 7 fields** - `types.rs:108-123` (Confidence: 65%) -- At 7 fields this struct is at the boundary of the "5+ parameters" warning. However, since this is a data transfer struct (not a function parameter list), the fields are all semantically necessary and individually well-documented.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Complexity Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The PR successfully reduces complexity in several ways: it extracts the monolithic `build_index()` function into a `Pipeline` struct with clear stage separation, introduces `walk_metadata()` to decouple the walk from content reading, and moves SHA computation into the classify phase. The streaming producer/consumer design with bounded channels is well-reasoned for memory control.

The main condition is the `Pipeline::run()` method length (134 lines, cyclomatic complexity ~12). Extracting the consumer loop into a separate method would bring this under the 100-line/CC-10 thresholds and make the pipeline stages independently testable. The `read_and_classify` dual-construction is a minor DRY issue worth addressing. The `walk_metadata`/`walk_and_read` duplication is partially mitigated by `walk_and_read` being test-only, but a future consolidation pass would reduce maintenance risk.

Overall this is a net complexity improvement over the pre-PR state -- the old `build_index()` was a single function doing everything; the new design separates concerns into Pipeline, WalkEntry, ProcessedFile, and read_and_classify with clear data flow boundaries.
