# Complexity Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17T15:13

## Issues in Your Changes (BLOCKING)

### HIGH

**`walk_and_read` closure nesting depth (4 levels)** - `crates/rskim/src/cmd/search/walk.rs:252-298`
**Confidence**: 85%
- Problem: The parallel walker callback has 4 levels of nesting: `run(|| { Box::new(move |entry_result| { match { match { ... } } }) })`. The outer closure returns a Box containing an inner closure with a match on `entry_result`, then a nested match on `classify_entry`. This creates a cognitively dense block that requires careful reading to understand the cap-checking, lock acquisition, and state-transition logic interleaved in one place.
- Fix: Extract the inner closure body into a named function that takes the shared state as parameters. The parallel walker API requires the Box-returning closure, but the body can delegate:

```rust
fn handle_entry(
    entry_result: Result<ignore::DirEntry, ignore::Error>,
    root: &Path,
    files: &Mutex<Vec<ReadFile>>,
    skipped: &Mutex<Vec<SkipReason>>,
    file_count: &AtomicUsize,
    cap_reached: &AtomicBool,
    max_files: usize,
) -> WalkState {
    if file_count.load(Ordering::Relaxed) >= max_files {
        if !cap_reached.swap(true, Ordering::Relaxed) {
            let mut s = skipped.lock().unwrap();
            if s.len() < MAX_SKIP_REASONS {
                s.push(SkipReason::CapReached);
            }
        }
        return WalkState::Quit;
    }
    // ... match on entry_result and classify_entry ...
    WalkState::Continue
}
```

This reduces the nesting depth inside `walk_and_read` to 2 levels (the required closure shape) and makes the logic independently testable.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`build_index` function length (100 lines, 7 numbered phases)** - `crates/rskim/src/cmd/search/index.rs:150-250`
**Confidence**: 82%
- Problem: `build_index` orchestrates 7 sequential pipeline phases in a single function body (resolve cache dir, walk, empty check, load manifest, classify, build index, save manifest). At 100 lines it is within tolerable limits, but the numbered comment sections (1-6) suggest natural extraction points. Adding any new phase (e.g., incremental deletion detection) will push it past 50 lines of pure logic.
- Fix: The current structure is acceptable for a pipeline orchestrator where the phases are inherently sequential and share local state. No immediate refactor required, but if future phases are added, consider extracting phases 4b+5 (classify and build) into a `classify_and_build(read_files, manifest, config) -> (Layer, FileManifest)` helper.

**`classify_entry` handles 5 skip conditions in sequence (75 lines)** - `crates/rskim/src/cmd/search/walk.rs:139-214`
**Confidence**: 80%
- Problem: `classify_entry` performs file-type check, language detection, size pre-screen, open-and-read (with 4-arm match), minification check, and SHA computation in one function. Each step is an early return on failure, which keeps nesting low, but the function spans 75 lines with mixed concerns (I/O, heuristics, hashing).
- Fix: The function uses the early-return pattern correctly and each section is well-commented. The complexity is inherent to the classification pipeline, and extracting sub-functions would fragment the linear flow without reducing cognitive load. Acceptable as-is; flagging as informational for awareness if new skip conditions are added.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Triple-zip iterator chain readability** - `crates/rskim/src/cmd/search/index.rs:221-226` (Confidence: 68%) -- The `read_files.iter().zip(classified).zip(path_keys).enumerate()` pattern produces deeply nested tuple destructuring `(idx, ((rf, (field_map, _)), path_key))`. While idiomatic Rust, the nested parentheses require careful reading. A named struct or `itertools::izip!` macro could improve clarity.

- **`FileManifest::load` handles 6 early-return conditions** - `crates/rskim/src/cmd/search/manifest.rs:129-209` (Confidence: 65%) -- The load function has sequential guards (file-not-found, oversized file, empty header, corrupt header, wrong version, wrong root) before the main parse loop. Each is a distinct early return. The linear structure is appropriate for a fail-soft parser, but grouping the header validation into a `validate_header` helper would reduce the main function to ~60 lines.

- **`walk_and_read` Arc/Mutex boilerplate** - `crates/rskim/src/cmd/search/walk.rs:235-239` (Confidence: 62%) -- Five `Arc::new(...)` declarations at the top of `walk_and_read` followed by five `Arc::clone(...)` in the closure is verbose but required by the `ignore` crate's parallel walker API. A `WalkState` struct could reduce the cloning boilerplate, though this is a stylistic preference.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The codebase demonstrates strong complexity management overall. Functions are well-decomposed with single responsibilities (separate `classify_entry`, `open_and_read`, `is_minified`, `sha256_hex` helpers). Early returns keep nesting shallow. The `ReadOutcome` enum eliminates stringly-typed error matching. Named constants replace all magic values. The one blocking item (walker closure nesting) is a direct consequence of the `ignore` crate's parallel API design -- extracting the body into a named function would improve maintainability without changing behavior.
