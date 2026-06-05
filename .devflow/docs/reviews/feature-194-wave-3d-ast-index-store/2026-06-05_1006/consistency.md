# Consistency Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_1006
**PR**: #272 (CYCLE 2)
**Scope**: `crates/rskim-search/src/ast_index/store/` (format/builder/reader + tests), `benches/ast_index_bench.rs`, `ast_index/mod.rs`, `lib.rs`, `index/mod.rs`, `Cargo.toml`. `.devflow/**` ignored.
**Benchmark siblings**: `crates/rskim-search/src/cochange/` and `crates/rskim-search/src/index/`

## Cross-Cycle Awareness

PRIOR_RESOLUTIONS (`2026-06-05_0025/resolution-summary.md`) parsed successfully. Cycle 1 fixed 15 issues including the major consistency items: struct renames to `*TableEntry`, dropping the `AST_` constant prefix (`SKAX_MAGIC`), builder/reader module visibility (`pub mod` → private `mod` + `pub use`), and the `read_array` offset-relative error message. The one False Positive (re-index interleave doc) was verified against current code and is not re-raised. None of the Cycle-1 fixes were reverted. This report focuses only on NEW issues not covered by Cycle 1.

## Issues in Your Changes (BLOCKING)

None. The changed code is strongly consistent with the sibling `cochange`/`index` modules.

Verified consistent (no findings):
- **Magic / version / size constants** — `SKAX_MAGIC` follows the `SK{XX}_MAGIC` pattern (cf. `SKCC_MAGIC`); bare `HEADER_SIZE` / `*_ENTRY_SIZE` / `FILE_META_SIZE` match the cochange naming convention. `FORMAT_VERSION: u16 = 1` matches both siblings verbatim.
- **`*TableEntry` struct naming** — `AstBigramTableEntry` / `AstTrigramTableEntry` are internally consistent and disambiguated in rustdoc from `extract::AstBigramEntry`. No residual `AstBigramEntry`/`AstTrigramEntry` collision in the store module.
- **Error variants** — `SearchError::IndexCorrupted` for all malformed-data paths, `SearchError::InvalidQuery` for FileId guards, `SearchError::Io` for missing dirs/files. Matches both siblings exactly.
- **Atomic-write pattern** — `AstIndexBuilder::atomic_write` (`NamedTempFile::new_in` + `write_all` + `sync_all` + Unix `0o644` `set_permissions` + `persist`) is byte-for-byte the cochange `atomic_write`, including the `0o644` permission step the lexical index omits. Correctly the stronger of the two postures.
- **Numeric casts** — `postings_buf.len() as u64`, `.len() as u64` for average accumulation, and `(x as f64 / n) as f32` match the index builder's established pattern (index/builder.rs:263,266,314,362). PF-004-analog narrowing (`u32::try_from`) is correctly applied where truncation would corrupt data: `posting_length`, `node_count`, `bigram_count`/`trigram_count` (avoids PF-004).
- **Test module wiring** — `#[cfg(test)] #[path = "..._tests.rs"] mod tests;` matches siblings.
- **`Result<T>` alias + `?` propagation** — no `.unwrap()`/`expect()` outside `#[cfg(test)]`; consistent with the "no panic outside tests" invariant documented in all three `format.rs` headers.
- **Size-ratio bound** — `< 1.8×` is internally consistent across `reader_tests.rs` (test + WHY comment) and `benches/ast_index_bench.rs` header (applies ADR-002 / avoids PF-005).

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Internal submodules exposed as `pub` — deviates from sibling encapsulation convention (2 occurrences)** — Confidence: 85%
- `crates/rskim-search/src/ast_index/mod.rs:35` — `pub mod store;`
- `crates/rskim-search/src/ast_index/store/mod.rs:31` — `pub(crate) mod format;`
- Problem: The established sibling convention is that a module's *internal* submodules are private `mod` and their public items surface only through `pub use`. In both `cochange/mod.rs` and `index/mod.rs`, every internal submodule (`builder`, `reader`, `format`, and even `lang_map` which is `pub(crate)` only because it is genuinely cross-module) follows this. The store sub-tree breaks the pattern in two places:
  - `store/mod.rs` already correctly privatizes `builder`/`reader` (`mod builder; mod reader;` + `pub use`) per the Cycle-1 fix, but `format` is declared `pub(crate) mod format;`. The siblings declare `mod format;` (fully private). Verified: nothing outside `store/` references `store::format` — `builder.rs`/`reader.rs` use `super::format::…` and `format_tests.rs` uses `super::*`, both of which work with a private `mod format;`. The `pub(crate)` is broader than required.
  - `ast_index/mod.rs` declares `pub mod store;`. Its public items are already re-exported one line below via `pub use store::{AstFileMetaEntry, AstIndexBuilder, AstIndexReader, AstPosting};`, and again at crate root in `lib.rs`. Verified: nothing references the `ast_index::store::` path directly (grep across `src/`, `benches/`, `tests/` returns only the in-module `pub use`). The bench and lib.rs both consume via the flat `rskim_search::{AstIndexBuilder, …}` re-export. So `pub mod store;` could be `mod store;` to match how `cochange`/`index` keep their internal submodules private.
- Impact: Low functional risk (the extra-public surface is unused), but it widens the crate's API/visibility surface beyond the sibling baseline and beyond what Cycle 1 established for `builder`/`reader`. A future caller could begin depending on `ast_index::store::format::…` internals, defeating the encapsulation the rename/visibility work in Cycle 1 was aiming for. `store` is an internal submodule of `ast_index` (analogous to `builder`/`reader`/`format`), not a crate-level module like `cochange`/`index` — so it should follow the internal-submodule convention.
- Fix: `crates/rskim-search/src/ast_index/store/mod.rs:31` → `mod format;`. `crates/rskim-search/src/ast_index/mod.rs:35` → `mod store;`. Both compile unchanged because all external access already goes through `pub use`. (If a future test or bench is intended to reach `store::format` internals directly, prefer `#[cfg(test)] pub(crate)` and document the reason, matching how `lang_map` is scoped.)

## Pre-existing Issues (Not Blocking)

None within scope. The `KNOWLEDGE.md` feature doc carries stale references (`AST_HEADER_SIZE`/`AST_BIGRAM_ENTRY_SIZE` constant names at lines 387-391; `< 3.0×` size bound at line 574; `pub(crate) mod format` description) that no longer match the post-Cycle-1 code. This is `.devflow/**` documentation drift, explicitly out of scope for this review, and is already noted as a doc-refresh follow-up.

## Suggestions (Lower Confidence)

- **`compute_checksum` rustdoc omits the "CRC32 is not cryptographic / cache is rebuildable" caveat** - `crates/rskim-search/src/ast_index/store/format.rs:462` (Confidence: 60%) — The cochange sibling documents this caveat; the AST and the lexical `index` siblings both omit it. Since one of two siblings omits it too, this is not a clear deviation — optional nicety, not an inconsistency.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Consistency Score**: 9
**Recommendation**: APPROVED_WITH_CONDITIONS

The Cycle-1 rename and visibility work produced an internally consistent module that closely mirrors the `cochange`/`index` siblings. The single remaining deviation is over-broad visibility on two internal submodule declarations (`pub mod store`, `pub(crate) mod format`) where the siblings keep internal submodules private — a low-risk, one-line-each fix that completes the encapsulation parity Cycle 1 began. No blocking issues.
