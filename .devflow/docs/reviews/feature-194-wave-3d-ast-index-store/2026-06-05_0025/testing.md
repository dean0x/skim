# Testing Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05 00:25
**PR**: #272 (Wave 3d — AST n-gram on-disk index store)
**Scope**: `store/format_tests.rs`, `store/builder_tests.rs`, `store/reader_tests.rs`, `benches/ast_index_bench.rs`

## Summary of Assessment

This is a strong, well-organized test suite. Acceptance criteria A1–A14 are covered, the
corruption matrix (bad magic, bad version, CRC flip, truncation, appended byte, overflow
header) is thorough, the empty-postings guard (`post_mmap = None` when `postings_file_size == 0`)
is tested at both builder and reader level (A7), `build_from_files` determinism is tested
byte-for-byte against the sequential path, and `count == 0` rejection (C4) is covered. Assertions
are behavior-focused (observable lookup results, error message substrings) rather than
implementation-coupled. No flaky/non-deterministic patterns were found — the determinism test
directly guards the area the working memory flagged.

The findings below are real coverage gaps in the **C3 contract** ("malformed entry → IndexCorrupted")
and the **C1 contract** (postings sorted/deduped), plus stale comments. None are CRITICAL.

## Issues in Your Changes (BLOCKING)

_None._ No blocking test defects found.

## Issues in Code You Touched (Should Fix)

### HIGH

**C3 reader-level posting bounds/alignment paths are untested** — `reader_tests.rs`, `reader.rs:292-340`
**Confidence**: 90%
- Problem: The C3 contract guarantee is "Malformed entry (bad offset/len, OOB, `len % 8 != 0`) → `Err(IndexCorrupted)`". `lookup_postings_generic` has four distinct corruption branches — `posting_offset` exceeds usize (line 308), slice overflow (312), `end > post_mmap.len()` OOB (315), and `length` not aligned to 8 (321). **None of these branches is exercised by any test.** The format-level unit tests (`lookup_bigram_rejects_non_multiple_of_stride`) test alignment of the *entry table* stride, not the *posting list* alignment in the `.skpost` file. The corruption matrix (a11) only flips bytes in `.skidx` and is caught by CRC before any posting offset is ever dereferenced — so the reader's posting-bounds guards are dead-untested.
- Impact: The single most important guarantee Wave 3f's query engine relies on (C3) is asserted in the contract table but not verified at the layer that enforces it. A regression that drops a bounds check would not be caught.
- Fix: Add reader tests that construct a valid index, then corrupt a *bigram/trigram entry's* `posting_offset`/`posting_length` to point OOB or to a non-8-multiple length, **recompute the CRC over the modified idx payload** (so CRC passes and the lookup path is reached), reopen, and assert `lookup_bigram(...)` returns `Err(IndexCorrupted)` for each of: OOB offset, misaligned length, offset-exceeds-usize. Example skeleton:

```rust
#[test]
fn lookup_rejects_oob_posting_offset() {
    let (_d, mut idx, post) = build_single_file_index();
    // bigram entry table starts at AST_HEADER_SIZE; posting_offset is at
    // entry_start + 4 (after u32 key). Overwrite with a huge offset.
    let off = AST_HEADER_SIZE + 4;
    idx[off..off + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    // Recompute CRC over payload so we exercise the lookup path, not the CRC gate.
    let payload_crc = compute_checksum(&idx[AST_HEADER_SIZE..]);
    idx[38..42].copy_from_slice(&payload_crc.to_le_bytes()); // checksum field offset
    // ... write to dir, open, assert lookup_bigram(...).is_err() with "out of bounds"
}
```

### MEDIUM

**C1 sorted/unique posting guarantee has no negative or robustness test** — `builder_tests.rs:186-216`, `reader.rs:48-51`
**Confidence**: 82%
- Problem: C1 ("postings sorted ascending by doc_id, at most one per doc_id") is verified only on the happy path in `a2_posting_merge_sorted_unique_doc_ids`, where files are added in already-ascending FileId order (0,1,2). The builder sorts via `sort_unstable_by_key` (builder.rs:311-315), but no test inserts files whose contributing FileIds arrive such that the unsorted-then-sorted path is actually distinguishable from a no-op. The helper docstring `build_3_file_index` claims "out of order by doc_id" but FileIds are still added 0,1,2 sequentially (they must be — the sequential-FileId guard forbids otherwise), so the sort is never observably exercised. The "at most one per doc_id" half is guaranteed structurally by the FileId-uniqueness guard rather than a dedup step, and the reader does not re-validate it — acceptable, but the test comment in `a2` ("C2: no duplicate doc_ids") mislabels the contract (that is C1's uniqueness clause; C2 is the absent-key→empty rule, correctly tested elsewhere).
- Impact: The sort step in `build()` is effectively untested for any input that would fail without it. Low real risk because FileIds are monotonic by construction, but the test gives false confidence that the sort is validated.
- Fix: Either (a) add an assertion that directly proves sortedness is enforced — e.g. a property-style check that `postings.windows(2).all(|w| w[0].doc_id < w[1].doc_id)` over a larger merged corpus — or (b) document in `a2` that doc_id ordering follows FileId monotonicity by construction and the sort is defense-in-depth. Also fix the mislabeled `// C2` comment at `builder_tests.rs:213` (it is the C1 uniqueness clause).

**A16 size-ratio bound `< 3.0` is a weak guard but not a tautology** — `reader_tests.rs:432-508`
**Confidence**: 88%
- Problem: Assessing the recently-changed bound (commit 0ee9de4) as requested. Measured baseline is ~1.23x; the asserted bound is `< 3.0`. This is **not** a tautology — 3.0 is ~2.4x the baseline, so it would fire on a genuine O(files^2) posting regression (which would push the ratio to tens or hundreds). It is, however, a loose fence: a 2x bloat regression (e.g. accidental double-emission of postings, or storing weights again) would slip under 3.0 undetected. The rationale comment block is excellent and empirically grounded. The test is deterministic (fixed corpus generator, no timing) so it is not flaky.
- Impact: The guard catches catastrophic bloat but not moderate (1.5x–2x) regressions. Given the comment documents the ~1.23x baseline precisely, a tighter bound would be more valuable.
- Fix: Consider tightening to `< 1.8` (≈1.45x margin over the 1.23x baseline) to catch moderate regressions while still tolerating corpus/vocabulary drift. Keep the existing rationale block. Alternatively, assert both a lower and upper sanity bound (`0.5 < ratio < 1.8`) so an accidental near-empty index also fails. Tracking real compression in #273 is appropriate (avoids PF-002 — not dodging via deferral, since the active guard remains).

## Pre-existing Issues (Not Blocking)

_None applicable — all reviewed files are new in this PR._

## Suggestions (Lower Confidence)

- **Stale comments reference the obsolete `#[ignore]` / 5% A16 design** — `reader_tests.rs:395` (`// A16 (as a #[ignore] test): index size < 5% of source bytes`) and `benches/ast_index_bench.rs:8` (`A16 ... is tested as an #[ignore] unit test`). (Confidence: 95%) — Both contradict the current active `< 3.0` non-ignored test; the section-header comment at line 395 directly mislabels the test that follows it. Update both to describe the active `< 3.0x` bound. (Listed as a suggestion because they are comments, not test logic — but worth fixing under ADR-001 "fix noticed issues immediately"; they will mislead the next reader.)

- **No test that a corrupted FileMetaEntry `lang_id` round-trips to `None`** — `reader_tests.rs:143-165` (Confidence: 70%) — C6 lang recovery is tested only for valid lang_ids (Rust/Python/Go → Some). A `file_meta` carrying an out-of-range `lang_id` (e.g. 200) should round-trip through `lang_from_id` to `None`; no test pins that the decoder does not panic or misclassify. Low risk since `lang_from_id` is a simple match, but it is part of the C6 surface.

- **`a12`/`a13` assert only the message substring, coupling to error-string wording** — `reader_tests.rs:336-363` (Confidence: 65%) — These match on `"IO error"`/`"No such file"`. A localized or reworded `SearchError::Io` Display would break the test without a behavior change. Prefer matching on the error variant (`matches!(err, SearchError::Io(_))`) where the variant is the contract. Minor — the substring approach is used consistently across the suite.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 1 | 2 | - |
| Pre-existing | - | - | 0 | 0 |

**Testing Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

Coverage of A1–A14 and the corruption matrix is genuinely good and the determinism test
addresses the prior flaky-test concern. The one material gap is that the C3 contract's
reader-level posting-bounds/alignment branches (`lookup_postings_generic`) are unverified —
they are only reachable past the CRC gate, which no test crosses with a corrupted entry. Adding
CRC-recomputing reader corruption tests for OOB offset, misaligned posting length, and
offset-exceeds-usize closes the most important hole. Tightening the A16 bound and fixing the
two stale `#[ignore]`/5% comments are quick wins under ADR-001.
