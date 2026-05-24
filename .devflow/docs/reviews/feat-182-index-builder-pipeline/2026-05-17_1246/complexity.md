# Complexity Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**`walk_and_read` error handling branch has high nesting and string-matching heuristic** - `crates/rskim/src/cmd/search/walk.rs:179-196`
**Confidence**: 85%
- Problem: The error-handling branch inside `walk_and_read` at lines 179-196 has 4 levels of nesting with a string-matching heuristic (`e.to_string().contains("too large")`) to distinguish error types. This is fragile — it couples the caller to the exact error message returned by `open_and_read`, meaning a typo or message change silently breaks the classification. The pattern is also harder to maintain as error variants grow.
- Fix: Replace the string-matching heuristic with a structured error type or a custom wrapper around `io::Error`. For example, introduce a local enum:

```rust
enum ReadResult {
    Ok(String),
    NonUtf8,
    TooLarge,
    IoError(io::Error),
}

fn open_and_read(path: &Path) -> ReadResult { ... }
```

This eliminates the string-matching branch entirely and reduces nesting by one level.

### MEDIUM

**`build_index` function length approaching complexity threshold** - `crates/rskim/src/cmd/search/index.rs:150-239`
**Confidence**: 82%
- Problem: `build_index` is 90 lines with 6 numbered pipeline steps, a conditional early return, parallel classification, sequential accumulation, and two persistence calls. While each step is well-commented and the logic is linear, the function sits at the upper edge of the maintainability sweet spot. As the pipeline grows (e.g., adding incremental diff logic mentioned in the PR description), this will cross the 100-line threshold quickly.
- Fix: No immediate action required — this is acceptable for a pipeline orchestrator today. When the next feature is added (incremental diff logic), extract steps 4-6 into a helper like `classify_and_build(read_files, manifest, cache_dir)` to keep the orchestrator under 50 lines of logic.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`walk_and_read` function length** - `crates/rskim/src/cmd/search/walk.rs:96-225` (Confidence: 70%) — At ~130 lines, this walker function handles 7 distinct skip conditions in a linear for-loop. Each branch is simple, but the aggregate length makes it harder to hold in working memory. Consider extracting inner match arms into a `process_entry` helper when the next condition is added.

- **`open_and_read` relies on `io::Error::other` string message** - `crates/rskim/src/cmd/search/walk.rs:248` (Confidence: 65%) — Using `io::Error::other("too large")` as a sentinel that is later matched with `.to_string().contains("too large")` (line 184) is an implicit contract. If this message is ever changed, the two sides silently decouple. A typed error would be more reliable, but the current scope is narrow enough that this may be intentional simplicity.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The overall design exhibits low cyclomatic complexity — functions are linear pipelines with clear single-responsibility separation (walk, classify, build, manifest). The clap migration reduced argument parsing complexity significantly (removed the manual `next_value` state machine). The bounded ancestor loop in `discover_project_root` is a good reliability improvement.

The one blocking HIGH issue (string-matching error classification) is a maintainability concern that should be addressed before the codebase grows — it creates an implicit coupling between `open_and_read`'s error message text and `walk_and_read`'s dispatch logic. Converting to a typed result would be a small, low-risk change.
