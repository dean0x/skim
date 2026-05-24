# Consistency Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### MEDIUM

**`index::run` does not accept `&AnalyticsConfig` unlike other subcommand `run` functions** - `crates/rskim/src/cmd/search/index.rs:60`
**Confidence**: 85%
- Problem: Every other `run()` entry point in the codebase accepts `(args: &[String], analytics: &crate::analytics::AnalyticsConfig)` as its signature. The search `mod.rs` correctly receives `_analytics` and passes it through to the dispatcher, but `index::run()` drops the analytics parameter entirely. While the index builder does not record analytics today, this deviates from the established interface contract and will require a signature change later if analytics recording is added (e.g., indexing duration, file count).
- Fix: Add the analytics parameter to `index::run` for interface consistency, even if unused for now:
  ```rust
  pub(super) fn run(args: &[String], _analytics: &crate::analytics::AnalyticsConfig) -> anyhow::Result<ExitCode> {
  ```
  And update the call site in `mod.rs`:
  ```rust
  return index::run(rest, _analytics);
  ```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`IndexConfig.effective_max_files()` uses `pub fn` while struct is `pub(super)`** - `crates/rskim/src/cmd/search/types.rs:33` (Confidence: 65%) -- The `HeatmapConfig` equivalent uses `pub(crate)` for both struct and method visibility. While `pub fn` on a `pub(super)` struct is technically scoped to the struct's visibility anyway, explicitly marking it `pub(super) fn` would be more self-documenting and match the visibility style used by peer modules.

- **`manifest.rs` `entries` field accessed directly in test via `manifest.entries.len()`** - `crates/rskim/src/cmd/search/manifest_tests.rs:256` (Confidence: 70%) -- The test accesses a non-pub field `entries` directly, which works because the test module is declared inline with `#[path = "manifest_tests.rs"]`. This is a valid Rust pattern used consistently in this module's test files, but differs from the heatmap module's approach of testing through public API only.

- **`walk.rs` uses `Arc<Mutex<Vec>>` where `crossbeam` scoped threads could avoid the Arc** - `crates/rskim/src/cmd/search/walk.rs:235-238` (Confidence: 62%) -- The `ignore` crate's `build_parallel()` requires `'static` closures so `Arc` is necessary here. This is the correct pattern given the constraint.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new `cmd/search/` module demonstrates excellent internal consistency:
- Section separator style (`// ====...`) matches the codebase convention perfectly (used in heatmap, stats, discover, infra).
- Error handling consistently uses `anyhow::Result` with `.context()` / `anyhow::anyhow!()` macros — matching existing patterns.
- `eprintln!` prefix messages follow the established `"skim search index: ..."` / `"skim search index [debug]: ..."` convention used by heatmap (`"skim heatmap: ..."`) and other modules.
- Visibility scoping (`pub(super)` for cross-file, internal `fn` for private) aligns with the heatmap and init module precedents.
- Test module pattern (`#[cfg(test)] #[path = "..."] mod tests;`) with `#![allow(clippy::unwrap_used)]` at file top is used identically across all four test files.
- Debug gate pattern (`crate::debug::is_debug_enabled()`) matches the discover and heatmap modules — no raw `std::env::var_os` calls.
- Cache directory resolution via `crate::cmd::resolve_cache_dir()` matches the hook and rewrite modules.
- Module doc comments (`//!`) follow the same style as existing modules.

The one blocking issue (missing analytics parameter) is a minor interface consistency gap that should be addressed before merge to avoid future refactoring.
