# Testing Review Report

**Branch**: refactor-230-232-233-tech-debt-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**No test for producer thread panic propagation** - `crates/rskim/src/cmd/search/index.rs:294-302`
**Confidence**: 85%
- Problem: The `Pipeline::run()` method includes panic-propagation logic (`producer_handle.join().map_err(...)`) that converts a producer-thread panic into an `anyhow::Error`. This error path is entirely untested. The `downcast_ref::<String>()` branch and the `"<non-string panic>"` fallback are both exercised only in production. A bug here (e.g. a silent swallow of the panic, or a wrong downcast) would hide producer crashes in production builds.
- Fix: Add a test that forces a panic in the producer path and asserts that `Pipeline::run()` returns `Err` with a message containing the panic payload. This could be done by creating a test helper that injects a panicking `read_and_classify` callback, or by constructing a `WalkEntry` with an impossible path that triggers a panic in the producer closure.

**No test for `read_and_classify` error paths** - `crates/rskim/src/cmd/search/index.rs:329-392`
**Confidence**: 82%
- Problem: The new `read_and_classify` function has 5 distinct error/skip paths (NonUtf8, TooLarge, Io, Minified, and cache-miss classify). While `test_streaming_skipped_includes_minified` covers the minified path end-to-end, the remaining 4 error paths of this function are only tested indirectly through `walk_and_read` (which is now `#[cfg(test)]`-only code). The production code path goes through `read_and_classify`, which has its own error-mapping logic distinct from `classify_entry`. A bug in the `SkipReason` construction (e.g. using `entry.abs_path` vs `entry.rel_path`) would not be caught.
- Fix: Add unit tests for `read_and_classify` directly, at least for the NonUtf8 and TooLarge paths, by constructing `WalkEntry` values with appropriate file fixtures and an empty `FileManifest`.

### MEDIUM

**SHA-256 walk tests now test `sha256_hex` utility rather than walk behavior** - `crates/rskim/src/cmd/search/walk_tests.rs:170-259`
**Confidence**: 82%
- Problem: Three walk tests (`test_walk_sha256_is_64_hex_chars`, `test_walk_sha256_is_deterministic`, `test_walk_sha256_changes_when_content_changes`) were adapted after SHA was removed from `ReadFile`. They now compute SHA via `sha256_hex(f.content.as_bytes())` inline. This means these tests now validate `sha256_hex` behavior (which is a pure utility function), not that the walk-and-index pipeline correctly associates SHA with files. The production code path where SHA is actually computed and used (inside `read_and_classify`) is not exercised by these walk-level tests.
- Fix: Either (a) add integration tests at the pipeline level that verify SHA values in the final manifest match file content, or (b) rename these tests to clarify they now test `sha256_hex` and add a comment noting that the pipeline-level SHA behavior is covered by `test_sha_computed_in_classify_phase` and `test_index_incremental_modified_file_reindexed`.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`walk_and_read` / `classify_entry` / `handle_entry` are `#[cfg(test)]`-only but not tested as such** - `crates/rskim/src/cmd/search/walk.rs:127-521`
**Confidence**: 80%
- Problem: `walk_and_read`, `classify_entry`, `handle_entry`, `EntryOutcome`, and `ReadFile` are all marked `#[cfg(test)]`. This means the entire test-only walk code path is a separate codebase from the production `walk_metadata` + `read_and_classify` pipeline. Walk tests exercise `walk_and_read` (test-only code), while production uses `walk_metadata` + `read_and_classify`. This creates a coverage gap: walk tests validate test-only code, and the production code path has only pipeline-level integration tests. If the test-only code diverges from production (e.g. a bug fix applied to `classify_entry_metadata` but not `classify_entry`), the walk tests would still pass while production is broken.
- Fix: Consider adding a handful of walk-level tests that use `walk_metadata` directly for the core behaviors (skip non-UTF8, skip too-large, skip minified, skip unsupported language). The existing `test_walk_metadata_returns_sorted_entries`, `test_walk_metadata_respects_max_files_cap`, and `test_walk_metadata_includes_mtime` are a good start but do not cover skip behaviors.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **No test for consumer-side `add_file_classified` failure** - `crates/rskim/src/cmd/search/index.rs:259-273` (Confidence: 70%) -- The consumer loop has fail-soft logic that logs and continues on `add_file_classified` errors, preserving the `next_file_id` invariant. This is difficult to test without mocking `NgramIndexBuilder`, but the invariant (FileId not incremented on failure) is critical for index correctness.

- **Mtime is persisted but never used for pre-screening in this PR** - `crates/rskim/src/cmd/search/index.rs:364-367` (Confidence: 65%) -- The manifest now stores `mtime` and the `WalkEntry` carries it, but the 4-tier cache logic in `read_and_classify` only checks SHA, not mtime. The tests verify mtime persistence but not mtime-based pre-screening. This may be intentional (mtime screening deferred to a follow-up PR), but it means the mtime tests validate storage mechanics without validating the optimization they enable.

- **No test for `CHANNEL_CAPACITY` backpressure behavior** - `crates/rskim/src/cmd/search/index.rs:162` (Confidence: 62%) -- The bounded channel is a key memory-safety mechanism. While it is exercised by all pipeline tests, no test specifically validates that backpressure works (e.g. a slow consumer with a fast producer does not OOM).

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The PR introduces a significant architectural change (monolithic pipeline to streaming producer/consumer with bounded channels) and adds good new tests for the new components: streaming pipeline correctness, minified file handling, max_files enforcement, mtime persistence, and backward compatibility. The existing test suite (59 tests passing) provides solid coverage of the happy path and incremental build behavior.

However, the new `read_and_classify` function -- which is the core of the producer thread and the primary production code path -- lacks direct unit tests for its error branches. The producer panic propagation path is also untested. Additionally, the `#[cfg(test)]`-only retention of `walk_and_read` creates a divergent test-only code path that could mask production regressions. These gaps represent real risk for a concurrency-sensitive streaming pipeline where error handling correctness is critical.
