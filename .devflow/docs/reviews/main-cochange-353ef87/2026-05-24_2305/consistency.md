# Consistency Review Report

**Branch**: main (commit 353ef87)
**Date**: 2026-05-24
**Focus**: Consistency of new `cochange/` module with existing crate patterns

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Sub-module visibility inconsistent with `index` module pattern** - `cochange/mod.rs:24-26`
**Confidence**: 92%
- Problem: The `cochange` module declares its sub-modules as `pub(crate) mod builder;`, `pub(crate) mod format;`, `pub(crate) mod reader;`. The established pattern in the `index` module (which has the same builder/format/reader structure) uses plain `mod builder;`, `mod format;`, `mod reader;`. No code outside the `cochange` module accesses these sub-modules directly, so the extra visibility is unnecessary and diverges from the existing convention.
- Fix: Change `pub(crate) mod` to `mod` for all three sub-modules:
```rust
mod builder;
mod format;
mod reader;
#[cfg(test)]
pub(crate) mod test_helpers;  // keep: test_helpers needs pub(crate) for cross-file test access
```

**`#[must_use]` with custom messages on Result-returning methods diverges from existing pattern** - `cochange/builder.rs:51,76` and `cochange/reader.rs:69,135,154`
**Confidence**: 82%
- Problem: The cochange module adds `#[must_use = "custom message"]` to 5 methods (`new`, `build`, `open`, `pair_count`, `jaccard`). The equivalent methods in the `index` module (`NgramIndexBuilder::new`, `NgramIndexReader::open`) have NO `#[must_use]` annotations. The compiler already emits `unused_must_use` warnings for `Result` types, making explicit `#[must_use]` on Result-returning functions redundant. The only `#[must_use]` in the index reader is on `stats()` which returns a non-Result type (`IndexStats`).
- Fix: Either remove the `#[must_use]` annotations from Result-returning methods (matching `index` pattern), or if the team prefers the cochange style, apply it consistently across both modules. The cochange approach is arguably better practice per CLAUDE.md Rust guidelines, but it should be a crate-wide decision, not a per-module divergence.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`atomic_write` as free function vs method** - `cochange/builder.rs:294` (Confidence: 68%) -- In the `index` module, `atomic_write` is a private method on `NgramIndexBuilder`. In the `cochange` module, it is a free function at module scope. Both work correctly, but the structural approach differs. Additionally, the cochange version adds Unix permission setting (`0o644`) that the index version lacks. Consider whether this permission hardening should be applied to both modules for consistency.

- **`test_helpers` module is novel pattern** - `cochange/mod.rs:28` (Confidence: 65%) -- The `index` module has no shared `test_helpers` module; each test file is self-contained. The `cochange` module introduces a `test_helpers.rs` with `pub(super)` helpers shared between `builder_tests.rs` and `reader_tests.rs`. This is a reasonable pattern for reducing duplication, but it is new to this crate. If the team adopts this pattern, consider backporting it to the `index` module where similar test setup duplication likely exists.

## Passed Checks

- **Module documentation style**: Module-level `//!` doc comments match the format and depth of `index/mod.rs` and `temporal/mod.rs` (architecture description, usage example, invariants).
- **File naming convention**: `builder.rs`, `format.rs`, `reader.rs`, `*_tests.rs` naming matches the `index` module exactly.
- **Test file organization**: `#[cfg(test)] #[path = "..._tests.rs"] mod tests;` pattern matches `index/builder.rs`, `index/reader.rs`, `index/format.rs`.
- **`#![allow(clippy::unwrap_used)]` in test files**: All three test files include this, matching `index` test files.
- **Import ordering**: std -> third-party -> `super::` -> `crate::` ordering matches all existing modules.
- **Section divider comments**: `// ============` for top-level sections, `// -------` for sub-sections within `impl` blocks, matching existing style.
- **Error handling pattern**: Uses `SearchError::IndexCorrupted` and `SearchError::Io` consistently with existing error variants. No new error variants introduced.
- **Types placement**: `CochangeStats` is placed in `types.rs` (pure types module) rather than in the cochange module, matching the established pattern where `IndexStats`, `HistoryResult`, etc. live in `types.rs`.
- **Re-export pattern in `lib.rs`**: `pub use cochange::{CochangeMatrixBuilder, CochangeMatrixReader};` follows the same structure as `pub use index::{NgramIndexBuilder, NgramIndexReader};`.
- **Alphabetical ordering in `lib.rs`**: Module declarations and re-exports maintain alphabetical order.
- **Constructor pattern**: `new(output_dir: PathBuf) -> Result<Self>` with existence check matches `NgramIndexBuilder::new`.
- **Reader `open` pattern**: `open(dir: &Path) -> Result<Self>` with magic/version/size/checksum validation matches `NgramIndexReader::open`.
- **`Send + Sync` documentation**: Both the cochange reader and index reader document their thread-safety rationale with the same comment style.
- **Derive ordering on structs**: `#[derive(Debug, Clone, Copy, PartialEq, Eq)]` for format structs matches existing patterns.
- **Serde derives on stats struct**: `CochangeStats` derives `Serialize, Deserialize` matching `IndexStats` pattern.
- **Binary search implementation**: Manual binary search in `lookup_pair` and `file_commits` follows the same style as `lookup_ngram` in `index/format.rs`.
- **Checked arithmetic**: Consistent use of `checked_mul`, `checked_add`, and `saturating_add` for overflow safety, matching `index` module patterns.
- **`is_multiple_of` usage**: Matches existing usage in `index/format.rs` and `index/reader.rs`.
- **CRC32 checksum pattern**: Single `compute_checksum` function delegated to `crc32fast::hash`, matching `index/format.rs` approach.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The `cochange` module demonstrates strong consistency with the existing `index` module in naming, file structure, test organization, error handling, documentation style, import ordering, and section divider conventions. The two MEDIUM issues are minor visibility and annotation divergences that do not affect correctness. The module clearly follows the established patterns for binary format modules in this crate. The `test_helpers` pattern is a reasonable evolution, and the `#[must_use]` additions are arguably improvements -- they should just be applied consistently across the crate rather than per-module.
