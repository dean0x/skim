# Resolution Summary

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_1006
**Review**: .devflow/docs/reviews/feature-194-wave-3d-ast-index-store/2026-06-05_1006
**PR**: #272
**Command**: /resolve
**Cycle**: 2

## Decisions Citations

- applies ADR-001 — batch-1-reader-and-tests (issue 1), batch-2-builder (issues 2, 3, 7, 8), batch-3-visibility (issue 4)
- avoids PF-004 — batch-2-builder (issue 8: saturating_add/saturating_mul, no silent overflow)
- avoids PF-002 — all batches: every finding fixed in-place; nothing misclassified as pre-existing or deferred to dodge effort (0 deferred)

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 8 |
| Fixed | 8 |
| False Positive | 0 |
| Deferred | 0 |
| Blocked | 0 |

Plus 1 pre-existing/inherent item (mmap TOCTOU) — not actionable, documented as an inherent mmap constraint shared with the lexical/cochange sibling readers.

## Fixed Issues
| Issue | File:Line | Batch |
|-------|-----------|-------|
| Consolidate duplicated table-offset arithmetic: extract private `bigram_table_range`/`trigram_table_range`/`meta_start`; `file_meta`/`lookup_bigram`/`lookup_trigram` delegate to them (saturating arithmetic), `open()` keeps the checked overflow-rejecting validation — one source of truth for layout math | `ast_index/store/reader.rs` (helpers ~107-139; sites 244/276/299) | batch-1 |
| C6 contract tested through the public accessor: rewrite `a5_lang_recovery_from_file_meta` to assert via `file_meta(i).language()`; add `a5_lang_recovery_unrecognised_lang_id_returns_none` covering the `None` future-compat branch; drop unused `lang_from_id` import | `ast_index/store/reader_tests.rs:142-198` | batch-1 |
| Multi-language round-trip breadth: add `a8_multi_language_round_trip` building a 5-language index (Rust/Python/Go/Kotlin/Java), asserting each `file_meta(i).language()` and independent posting retrieval | `ast_index/store/reader_tests.rs:211-278` | batch-1 |
| Remove redundant per-posting-list `sort_unstable_by_key` (provably no-op under the sequential-FileId invariant); replace with a `#[cfg(debug_assertions)]` windows(2) ascending assertion documenting the invariant | `ast_index/store/builder.rs:389` | batch-2 |
| Assert `count >= 1` at the public build boundary in `add_file_ngrams` (both bigram/trigram merge loops) → `InvalidQuery` with file id; converts deferred query-time `IndexCorrupted` into an immediate located build-time error | `ast_index/store/builder.rs:235` | batch-2 |
| Consolidate triplicated `atomic_write` into new `pub(crate) fn atomic_write` in `src/io_util.rs`; all three builders delegate. Lexical builder gains the previously-missing `sync_all` + `0o644` (drift corrected); cochange/AST drop their local copies | `io_util.rs` (new); `ast_index/store/builder.rs:561`, `index/builder.rs:87`, `cochange/builder.rs:331` | batch-2 |
| Align accumulator overflow discipline: `+=` → `saturating_add` for `total_node_count`/`total_distinct_bigrams`/`total_distinct_trigrams`; unchecked `*` → `saturating_mul` for the `total_posting_bytes_est` capacity hint | `ast_index/store/builder.rs:263-265, 434` | batch-2 |
| Narrow internal submodule visibility to sibling parity: `pub mod store;` → `mod store;` and `pub(crate) mod format;` → `mod format;` (all external access already flows through `pub use`) | `ast_index/mod.rs:35`, `ast_index/store/mod.rs:31` | batch-3 |

## False Positives
None.

## Deferred to Tech Debt
None. Issue 7 (`atomic_write` triplication) was a deliberate fix-now decision under ADR-001 despite touching the lexical and cochange siblings beyond Wave-3d scope — the duplication was introduced by this PR's AST builder, the consolidation is a contained "careful fix" (one shared helper, no on-disk behavior change beyond correcting the lexical builder's missing fsync/permissions drift), and all sibling tests pass.

## Blocked
None.

## Cross-Batch Note
Batch-2 introduced `src/io_util.rs` + `pub(crate) mod io_util;` in `lib.rs` for issue 7. Batch-1, running in parallel, encountered the in-progress `io_util.rs` and idempotently ensured the same `lib.rs` declaration so its own verification could compile. Post-integration check confirmed a single `mod io_util;` declaration (lib.rs:29) — no duplicate.

## Verification (integrated tree, all batches combined)
- `cargo build -p rskim-search` — clean (0 warnings)
- `cargo clippy -p rskim-search --all-targets -- -D warnings` — clean (0 warnings)
- `cargo fmt -p rskim-search` — clean
- `cargo test -p rskim-search` — **645 pass, 0 fail, 4 skipped** (was 640; +5 from the new C6 `None`-path test, the 5-language round-trip, and the rewritten/expanded A5)

## Follow-up Notes
- PR #272 body still states "632/636 tests" — actual is now 645. Documentation drift in the PR body; update when next touching it.
- `.devflow/features/ast-index/KNOWLEDGE.md` was flagged (cycle-2 consistency, low-confidence) as carrying stale constant names and a `< 3.0×` size bound versus the live `< 1.8×`; out of code-review scope (a Dream/knowledge-refresh concern), not resolved here.
