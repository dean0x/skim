# Consistency Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19
**Scope**: Incremental (459d0af...HEAD, 3 commits)

## Issues in Your Changes (BLOCKING)

### HIGH

**Removed `-j` short alias for `--json` flag** - `crates/rskim/src/cmd/search/mod.rs:136`
**Confidence**: 95%
- Problem: The previous code accepted `"--json" | "-j"` but the refactored `parse_flags` only matches `"--json"`. The `-j` alias was silently dropped. With the new unrecognised-flag rejection at line 168, passing `-j` now returns an error instead of being accepted. This is a breaking change for any scripts or agent hooks that use the short form.
- Fix:
```rust
// Line 136 — restore the short alias:
"--json" | "-j" => json = true,
```
Also update the unrecognised-flag error message at line 171 to list `-j` among valid flags.

### MEDIUM

**`SearchAction` missing `Eq` derive despite having `PartialEq`** - `crates/rskim/src/cmd/search/mod.rs:90`
**Confidence**: 82%
- Problem: The `SearchAction` enum derives `Debug, PartialEq` but not `Eq`. In this codebase, types that derive `PartialEq` consistently also derive `Eq` when all variants satisfy it (see `IndexResult` in `types.rs:126` which derives `PartialEq, Eq`). Since `SearchAction` contains only unit variants and `String` (which is `Eq`), the `Eq` bound is satisfied and should be included for consistency.
- Fix:
```rust
#[derive(Debug, PartialEq, Eq)]
enum SearchAction {
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Inconsistent metadata call count in `extract_snippet`** - `crates/rskim/src/cmd/search/snippet.rs:124-139`
**Confidence**: 80%
- Problem: The function calls `std::fs::metadata(&abs_path)` twice in sequence -- once for the mtime guard (line 124) and once for the size guard (line 137). Both are on the same file. The first already obtains the full `Metadata` struct (which includes file size). The second syscall is redundant. While not a pattern violation per se, the rest of the codebase avoids unnecessary duplicate syscalls (the index pipeline reads mtime from a single `metadata()` call in `walk.rs`).
- Fix: Combine into a single metadata call and extract both mtime and size from it.

## Pre-existing Issues (Not Blocking)

(none above CRITICAL threshold)

## Suggestions (Lower Confidence)

- **`StalenessCheck::Display` truncation may panic on short stored/current strings** - `crates/rskim/src/cmd/search/staleness.rs:43-44` (Confidence: 65%) -- The `Display` impl slices `&stored[..8.min(stored.len())]`. While `8.min(stored.len())` is safe for byte indexing, if the string contained multi-byte UTF-8 characters the slice could panic at a non-char-boundary. In practice, these are hex SHA strings (ASCII only), so the risk is theoretical. Still, using `.get(..8).unwrap_or(stored)` would be more defensive and consistent with Rust idioms for safe slicing.

- **`check_staleness` signature inconsistency with rest of module** - `crates/rskim/src/cmd/search/staleness.rs:189-192` (Confidence: 62%) -- `check_staleness` is the only `pub(super)` function in the staleness module that returns a bare tuple rather than `anyhow::Result`. The other public function `auto_refresh_if_stale` returns `anyhow::Result<(bool, FileManifest)>`. Since `check_staleness` internally calls `FileManifest::load` which can error (currently mapped to `NoStoredHead`), returning a Result would be more consistent. However, the current approach has a deliberate design choice (soft degradation), so this may be intentional.

- **Test `test_parse_flags_short_n_missing_value_is_error` checks for `--limit` in message** - `crates/rskim/src/cmd/search/mod.rs:599` (Confidence: 70%) -- The test asserts the error message contains `"--limit requires a value"` when `-n` is used without a value. This is technically correct (the code shares the same error path) but a user passing `-n` would see `--limit requires a value` which may be confusing. A more consistent UX would mention the flag the user actually typed.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The refactoring from boolean flags to `SearchAction` enum is a strong consistency improvement -- it replaces an error-prone cascade of `if flags.X` checks with an exhaustive `match`, making invalid states unrepresentable. The `Result`-returning `parse_flags` aligns with the project's engineering principles (never throw, return Result types). The `HashMap` to `BTreeMap` change in `FileManifest` is well-motivated and consistently applied. The single blocking HIGH issue is the dropped `-j` alias, which is a breaking regression that should be restored before merge.
