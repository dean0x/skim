# Consistency Review Report

**Branch**: refactor-230-232-233-tech-debt-pipeline -> main
**Date**: 2026-05-17T23:15

## Issues in Your Changes (BLOCKING)

### HIGH

**Inline walker logic in walk_metadata duplicates handle_entry pattern** - `walk.rs:389-429`
**Confidence**: 85%
- Problem: `walk_and_read` delegates its parallel walker body to the extracted `handle_entry` function (lines 472-521), which was specifically refactored to "reduce nesting depth and enable independent unit testing" (per the doc comment). The new `walk_metadata` function (lines 383-430) inlines the identical structural pattern (cap check, match on entry_result, accept/skip/transparent dispatch, error handling, skipped guard) directly in the closure instead of extracting a comparable `handle_metadata_entry` helper. This creates two implementations of the same concurrency pattern that must be kept in sync.
- Fix: Extract a `handle_metadata_entry` function mirroring `handle_entry` and delegate from the `walk_metadata` closure, matching the established pattern:

```rust
fn handle_metadata_entry(
    entry_result: Result<ignore::DirEntry, ignore::Error>,
    entries: &Mutex<Vec<WalkEntry>>,
    skipped: &Mutex<Vec<SkipReason>>,
    entry_count: &AtomicUsize,
    cap_reached: &AtomicBool,
    max_files: usize,
    root: &Path,
) -> WalkState {
    // ... same structure as handle_entry but using classify_entry_metadata
}
```

### MEDIUM

**Inconsistent `sha256` field removal from ReadFile vs retention in comment** - `walk_tests.rs:176,209,239`
**Confidence**: 82%
- Problem: The `sha256` field was removed from `ReadFile` (types.rs), but three walk tests now re-derive the SHA inline via `sha256_hex(f.content.as_bytes())`. This is functionally correct but introduces an inconsistency: the tests are testing `sha256_hex` correctness rather than verifying the walk output contract. The test names (`test_walk_sha256_*`) suggest they belong to the walk module's contract, but they now test a function from the same module that is unrelated to walking. The doc comments added ("SHA is now computed by the classify phase; derive it from content here") acknowledge this mismatch but leave it in place.
- Fix: Consider renaming these tests to `test_sha256_hex_*` and moving them into a dedicated section, or converting them to directly test `sha256_hex` since that is what they actually exercise. Alternatively, if the intent is to verify the walk+SHA pipeline end-to-end, add a note to the section header clarifying these are cross-cutting integration tests.

**`ReadOutcome` and helpers visibility inconsistency** - `walk.rs:76,566,597`
**Confidence**: 80%
- Problem: `ReadOutcome`, `open_and_read`, and `is_minified` were changed from private (`fn`/`enum`) to `pub(super)` to allow `read_and_classify` in `index.rs` to use them. However, `sha256_hex` was already `pub(super)` before this PR. The inconsistency is that these items are now part of the cross-module API surface but lack the `#[must_use]` annotations that other `pub(super)` return types in this codebase use (e.g., `IndexConfig::effective_max_files` at types.rs:36 has `#[must_use]`). This is a minor style inconsistency, not a bug.
- Fix: No action strictly required. If the team wants consistency, add `#[must_use]` to `open_and_read` (since ignoring its return value is always a bug) and consider documenting the cross-module API boundary in `mod.rs`.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Stale doc comment references "rayon worker pool"** - `index.rs:411`
**Confidence**: 85%
- Problem: The `run_classify` doc comment (line 411-412) says "The caller hoists the env-var check once before the rayon worker pool so that this function never performs a syscall on the hot path." The PR removed rayon in favor of the crossbeam-channel producer/consumer pattern, but this doc comment was not updated.
- Fix: Update the comment to reference the producer thread:

```rust
/// The caller hoists the env-var check once before the producer thread so
/// that this function never performs a syscall on the hot path.
```

**Module doc comment still references `walk_and_read`** - `walk.rs:5`
**Confidence**: 82%
- Problem: The module-level doc comment at the top of `walk.rs` (line 5) reads "`walk_and_read` stops after `max_files` files have been accepted." In production, `walk_and_read` is now `#[cfg(test)]` only. The primary production function is `walk_metadata`. The module doc should mention `walk_metadata` as the production entry point.
- Fix: Update the opening doc paragraph:

```rust
//! `walk_metadata` (production) and `walk_and_read` (tests) stop after
//! `max_files` files have been accepted.
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **EntryOutcome vs MetaOutcome naming asymmetry** - `walk.rs:128,301` (Confidence: 65%) -- `EntryOutcome` and `MetaOutcome` serve the same structural role (Accept/Skip/Transparent) but have different naming conventions. `EntryOutcome` uses the "full noun" pattern while `MetaOutcome` uses an abbreviation. Consider `MetadataOutcome` or `MetaEntryOutcome` for consistency, though since `EntryOutcome` is now `#[cfg(test)]` only, this is cosmetic.

- **`_reason` suppression in producer error arm** - `index.rs:238` (Confidence: 62%) -- The `Err(_reason)` binding suppresses the skip reason without logging it even under `SKIM_DEBUG`. Every other error path in the pipeline logs under debug. Consider adding a `if debug_enabled { eprintln!(...) }` block here for consistency with the fail-soft pattern elsewhere in the file.

- **`cache_hits` uses `saturating_add` but `next_file_id` uses `checked_add`** - `index.rs:276,280` (Confidence: 70%) -- Two adjacent counters in the same loop use different overflow strategies. `next_file_id` returns an error on overflow (correctness-critical since FileId is a u32 key), while `cache_hits` saturates (display-only counter). The different strategies are arguably correct for their purposes, but the inconsistency is worth a brief inline comment explaining why they differ.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The PR demonstrates strong overall consistency: the new `Pipeline` struct follows existing patterns (section headers, doc comments, error handling), the streaming types (`WalkEntry`, `ProcessedFile`) parallel the existing `ReadFile` structure, `ManifestEntry.mtime` backward compatibility via `#[serde(default)]` is correct, the `configure_builder` extraction eliminates a real DRY violation, and the test style (section headers, assertion messages, helper patterns) matches the established conventions. The one structural inconsistency worth addressing is the inlined walker logic in `walk_metadata` that breaks the pattern established by `handle_entry` -- all other findings are documentation drift from the architectural change.
