# Consistency Review Report

**Branch**: feat/178-two-file-mmap-index -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**Inconsistent numeric cast strategy: `as u32` alongside `u32::try_from` in the same module** - `builder.rs:152`, `builder.rs:177`, `builder.rs:195`, `builder.rs:238`, `builder.rs:240`
**Confidence**: 85%
- Problem: The builder carefully uses `u32::try_from(content.len())` with a proper error path at line 127, but then uses unchecked `as u32`/`as u64`/`as f32` casts elsewhere in the same module. For example, `pos as u32` on line 152 is safe only because `doc_length` was already validated to fit `u32`, and `entries.len() as u32` on line 238 would silently truncate if there were more than 2^32 distinct bigrams (unlikely but the pattern is inconsistent). The crate's own `types.rs` and `ngram.rs` use minimal casts; the builder mixes checked and unchecked approaches within the same function scope.
- Fix: For casts that are provably safe (e.g., `pos as u32` after doc_length validation), add a brief inline comment noting why the cast is safe. For casts that are not provably bounded (e.g., `entries.len() as u32` -- there are at most 65,536 distinct u16 bigrams so this is safe, but the safety argument is non-obvious), either add a comment or use `u32::try_from` consistently. Example:
  ```rust
  // Safe: at most 65,536 distinct u16 bigram keys, well within u32.
  ngram_count: entries.len() as u32,
  ```

### MEDIUM

**Inconsistent clippy allow lists across test files** - `builder_tests.rs:3`, `format_tests.rs:3`, `reader_tests.rs:3`, `lang_map_tests.rs:3`
**Confidence**: 82%
- Problem: The new test files use two different allow-list patterns. `builder_tests.rs`, `format_tests.rs`, and `reader_tests.rs` all use `#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]`, while `lang_map_tests.rs` uses only `#![allow(clippy::unwrap_used)]`. The existing `ngram_tests.rs` in the same crate also uses only `#![allow(clippy::unwrap_used)]`. There should be a single convention for test file clippy allows.
- Fix: Decide on a consistent allow list. Since `lang_map_tests.rs` and the pre-existing `ngram_tests.rs` both use the minimal `#![allow(clippy::unwrap_used)]`, and the other test files additionally use `expect` and `panic` macros, the pragmatic approach is: allow what each file actually uses. Verify that `builder_tests.rs`, `format_tests.rs`, and `reader_tests.rs` actually need all three allows. If a file never calls `expect!()` or `panic!()`, remove the unnecessary allows to match the minimal convention.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Mixed test naming convention across the crate** - `ngram_tests.rs` vs `builder_tests.rs` etc. (Confidence: 65%) -- The pre-existing `ngram_tests.rs` uses bare function names without `test_` prefix (e.g., `from_bytes_to_bytes_roundtrip`), while all new index test files and `types.rs` use `test_` prefix (e.g., `test_header_roundtrip`). Both conventions work; the new code is internally consistent and matches the `types.rs` convention. Not blocking, but a future cleanup could unify the convention crate-wide.

- **`reader.rs` uses `super::format::POSTING_ENTRY_SIZE` repeatedly instead of importing** - `reader.rs:170`, `reader.rs:173`, `reader.rs:175` (Confidence: 70%) -- Within `lookup_postings`, the code references `super::format::POSTING_ENTRY_SIZE` three times. The top-level import block already imports many items from `super::format` but omits `POSTING_ENTRY_SIZE`. Adding it to the import would match the pattern used elsewhere in the file and reduce visual noise.

- **Section banner placement in `lang_map.rs` differs from other modules** - `lang_map.rs:11-17` (Confidence: 62%) -- The `lang_map.rs` module places its `#[cfg(test)]` block at the top of the file (before the actual functions), while `builder.rs`, `format.rs`, and `reader.rs` all place tests at the bottom. This is a minor structural inconsistency.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new index module is well-structured and demonstrates strong internal consistency. Module naming (builder/reader/format), error handling patterns (Result types throughout, SearchError variants used correctly), doc comment style, section banners (matching `types.rs` / `ngram.rs` convention), and the `#[path = "..._tests.rs"]` test organization all match existing crate patterns. The trait implementations (LayerBuilder, SearchLayer) faithfully follow the contracts defined in `types.rs`. The primary consistency concern is the mixed checked/unchecked numeric cast strategy within builder.rs, which should be addressed with either uniform use of `try_from` or explicit safety comments on `as` casts.
