# Regression Review Report

**Branch**: refactor-230-232-233-tech-debt-pipeline -> main
**Date**: 2026-05-17T23:15

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**mtime pre-screening declared but not implemented** - `crates/rskim/src/cmd/search/index.rs:358-367`
**Confidence**: 82%
- Problem: The PR description and doc comments reference "4-tier mtime/SHA cache" and "mtime pre-screening" (lines 10, 325, 361), yet the actual cache logic at lines 364-367 always computes SHA first (`sha256_hex(content.as_bytes())` on line 359) and only checks `cached.sha256 == sha`. The `mtime` field is stored in `WalkEntry`, passed through `ProcessedFile`, and persisted in `ManifestEntry`, but it is never consulted to skip SHA computation. The intent-vs-reality gap means the mtime optimization described in `ManifestEntry` (line 63: "skip SHA computation when the file has not changed") is not realized. Future developers reading the "4-tier" comments may assume mtime skips are happening when they are not.
- Fix: Either implement the mtime pre-screen (e.g., if mtime matches cached mtime, skip SHA computation and assume cache hit) or update the doc comments to accurately describe the current 2-tier logic (force flag + SHA match). If the intent is to implement mtime pre-screening in a follow-up, add a TODO comment explicitly.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`file_count` semantics changed without documentation** - `crates/rskim/src/cmd/search/index.rs:313`
**Confidence**: 80%
- Problem: Previously `file_count` was `to_u32_capped(read_files.len())` which counted all files that passed the walk phase, regardless of whether `add_file_classified` succeeded. Now `file_count` equals `next_file_id`, which only counts files that were successfully indexed. This is arguably a bug fix (the FileId sequential invariant fix), but it changes what `file_count` reports in the stderr summary line (line 72: `"indexed {} files"`). If any downstream consumer (e.g., monitoring, logs) relied on the old semantics (files walked = files reported), they would see different numbers when files fail classification. The change is likely correct behavior but undocumented as intentional.
- Fix: The new semantics are better (count successfully indexed files, not attempted). No code fix needed, but consider adding a brief comment at line 313 noting that `file_count` reflects successfully indexed files only, to make the contract explicit.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Producer thread panic payload not fully captured** - `crates/rskim/src/cmd/search/index.rs:295-301` (Confidence: 65%) -- `downcast_ref::<String>()` misses `&str` panic payloads (from `panic!("literal")`); adding a `downcast_ref::<&str>()` fallback would capture more panic messages.

- **Early return on u32 overflow drops receiver without joining producer** - `crates/rskim/src/cmd/search/index.rs:276-278` (Confidence: 62%) -- If `checked_add(1)` overflows (requiring 4+ billion files), the `?` returns early, dropping `rx` before `producer_handle.join()`. The producer thread would be detached. Purely theoretical at 50K default cap, but architecturally the join should be in a scope guard or the overflow should be handled before the early return.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The PR is a well-structured refactor that correctly fixes a real bug (FileId sequential invariant with `enumerate()` + fail-soft `continue`) and moves from batch to streaming architecture. The key regression vectors were checked:

1. **No removed exports** -- all changes are `pub(super)` scoped within the search module. No external API surface is affected.
2. **No broken behavior** -- the old `walk_and_read` + rayon classify pipeline is functionally equivalent to the new `walk_metadata` + bounded-channel streaming pipeline. Tests (67 passing) cover cold build, incremental cache hits, force flag, max_files cap, and minified file skipping.
3. **Backward compatibility** -- `ManifestEntry.mtime` uses `#[serde(default)]` so old manifests without the field deserialize cleanly as `None`. A dedicated test (`test_mtime_backward_compat_none`) validates this.
4. **No incomplete migration** -- `walk_and_read`, `classify_entry`, `handle_entry`, `EntryOutcome`, and `ReadFile` are all gated behind `#[cfg(test)]`. Production code exclusively uses the new `walk_metadata` + `read_and_classify` path.
5. **FileId fix is correct** -- the old `enumerate()` approach would gap FileIds on `add_file_classified` failures; the new manual counter only increments on success, preserving the builder's sequential invariant.

The only condition is clarifying the mtime documentation to match the actual implementation (mtime is stored but not yet used for pre-screening optimization).
