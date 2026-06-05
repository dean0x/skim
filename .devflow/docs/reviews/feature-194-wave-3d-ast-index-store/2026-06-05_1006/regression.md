# Regression Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_1006
**PR**: #272 | **Cycle**: 2 (post cycle-1 resolution of 15 issues)
**Scope**: Wave 3d AST on-disk index store (`store/`, `ast_index/mod.rs`, `lib.rs`, `index/mod.rs`, `Cargo.toml`, bench). `.devflow/**` ignored.

## Summary of Verification

This is a **clean, additive-only** change at the public-API level. The cycle-1 resolution
commits (5990cee, 2c6ce88, plus 9f300ea/0ee9de4) introduced **no new regressions**. All
regression-risk areas flagged in the review hints were verified and found safe:

- **No public exports removed** across the full diff vs `main` (`git diff main...HEAD | grep "^-...pub"` → empty).
- **`index/mod.rs` `lang_map` widening** (`mod` → `pub(crate) mod`): visibility *widening* is
  non-breaking. Existing lexical consumers reach `lang_to_id` via `super::format::lang_to_id`
  (unchanged); the new AST store reaches it via `crate::index::lang_map::*` (newly enabled).
  All pre-existing `index::builder::tests::*` and `index::format::tests::*` pass unchanged.
- **`lib.rs` re-export additions** (`AstFileMetaEntry`, `AstIndexBuilder`, `AstIndexReader`,
  `AstPosting`): no name collisions. `AstBigramEntry`/`AstTrigramEntry` resolve from `extract`;
  the on-disk structs were renamed to `*TableEntry` (`pub(crate)`, not re-exported) in cycle-1
  precisely to avoid this collision. Build confirms no shadowing.
- **`Cargo.toml`** (`rayon` workspace dep + `[[bench]] ast_index_bench`): `cargo build --benches`
  clean; no feature/build regression.
- **Submodule visibility narrowing** (`pub mod builder/reader` → `mod` + `pub use`) in resolution
  commit is the intended cycle-1 fix (Issue #1, parity with lexical sibling). Re-exported types
  keep the public surface intact — not a regression.

**Build/test gates re-run on current HEAD:**
- `cargo build -p rskim-search` — 0 warnings, 0 errors
- `cargo clippy -p rskim-search --all-targets -- -D warnings` — clean
- `cargo build -p rskim-search --benches` — clean
- `cargo test -p rskim-search` — 640 passed, 0 failed, 1 ignored (matches resolution summary)

**Contract verification (C1–C7):** All honored in current code.
- C1: `lookup_postings_generic` enforces strictly-ascending `doc_id` defensively (reader.rs:362-374).
- C2: absent key → `Ok(Vec::new())` (reader.rs:282,305).
- C3: OOB/overflow/misalignment → `IndexCorrupted` via `checked_*` (reader.rs:340-357).
- C4: `decode_posting` rejects `count == 0` (format.rs).
- C6: `AstFileMetaEntry::language()` → `lang_from_id` (format.rs:214-216) — live use of the symbol
  whose dead re-export was removed in cycle-1.
- C7: `Send + Sync` documented + test A6.

**Cross-cycle awareness:** Resolution summary's 1 false positive (re-index read/write interleave)
re-checked — still documented as an explicit precondition in builder.rs:16-20; not re-introduced.
None of the 15 fixed issues were reverted.

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

None.

## Pre-existing Issues (Not Blocking)

None within regression scope. (The mmap-TOCTOU constraint at reader.rs:123-126 is an inherent,
documented mmap property shared with the lexical sibling — not introduced or worsened here.)

## Suggestions (Lower Confidence)

- **KNOWLEDGE.md size-ratio drift** — `.devflow/features/ast-index/KNOWLEDGE.md:548,574`
  (Confidence: 70%) — Documents the A16 bound as `< 3.0×`, but the live test
  (`reader_tests.rs:466`) and bench (`ast_index_bench.rs:8`) were tightened to `< 1.8×` in
  cycle-1. Documentation-only drift in a `.devflow/` artifact (out of code-review scope, and the
  prompt directs ignoring `.devflow/**`); noted for the next knowledge refresh. Code itself is
  internally consistent.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 10
**Recommendation**: APPROVED
