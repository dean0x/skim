# Complexity Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_1006
**PR**: #272 (Review CYCLE 2)
**Scope**: `crates/rskim-search/src/ast_index/store/{format,builder,reader}.rs`, `benches/ast_index_bench.rs`, `ast_index/mod.rs`, `lib.rs`, `index/mod.rs`, `Cargo.toml`

## Cross-Cycle Note

Read `2026-06-05_0025/resolution-summary.md` (15 fixed, 1 false positive). Cycle-1 fixes verified present in current code: the `serialize_entry_table` extraction (builder.rs:117-154), the C1 defensive doc_id monotonicity check (reader.rs:362-379, collapsed-if form), `read_array` reporting bytes-from-offset (format.rs:227-240), and the `u32::try_from` node-count narrowing (builder.rs:294-300, 340-346). None of those are re-flagged. The findings below are NEW complexity observations not covered by cycle 1.

## Overall Assessment

This is clean, well-factored code. The `serialize_entry_table` extraction did **not** over-abstract — it has 6 parameters (at the threshold) but each is load-bearing, the generic `<K, E, const N>` signature is justified by genuine bigram/trigram duplication it removes, and the body is ~37 lines of linear logic. The `binary_search_entries` generic helper (format.rs:533-561) is similarly well-judged. Encode/decode functions are flat, single-purpose, and individually trivial. The offset arithmetic in `open()` is exemplary (consistent `checked_*` throughout). No CRITICAL or HIGH blocking issues.

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Duplicated table-offset arithmetic with inconsistent overflow posture (4 sites)** — Confidence: 88%
- `reader.rs:131-150` (`open`, checked), `reader.rs:244-247` (`file_meta`, unchecked), `reader.rs:276-277` (`lookup_bigram`, unchecked), `reader.rs:299-300` (`lookup_trigram`, unchecked)
- Problem: The same "header + bigram_bytes + trigram_bytes [+ meta]" section-offset computation appears four times. `open()` uses `checked_mul`/`checked_add` and rejects overflow; the other three recompute the identical offsets with bare `*` and `+`. This is a maintainability/readability hazard: the layout math is the single most invariant-critical arithmetic in the module, and it lives in four places that must stay in lockstep. A future layout change (e.g. inserting a section) must be applied consistently to all four, and the inconsistent style invites a reader to wonder whether the unchecked sites are a latent bug.
- Note on correctness: the unchecked sites are **not** currently a panic risk — `open()` has already proven `expected_idx_size == idx_mmap.len()` fits in `usize`, so the recomputed sub-offsets cannot overflow. This is purely a complexity/duplication finding, not a safety bug. `applies ADR-001` (fix noticed issues regardless of scope).
- Fix: Extract three private accessors computing the section boundaries once from `self.header`, e.g.:
  ```rust
  fn bigram_table_range(&self) -> std::ops::Range<usize> {
      let start = HEADER_SIZE;
      start..start + (self.header.bigram_count as usize) * BIGRAM_ENTRY_SIZE
  }
  fn trigram_table_range(&self) -> std::ops::Range<usize> { /* continues after bigram */ }
  fn meta_start(&self) -> usize { /* after trigram */ }
  ```
  Then `lookup_bigram`/`lookup_trigram`/`file_meta` slice via these, and `open()` reuses the same `*_bytes` locals. One source of truth for the layout math; the checked-vs-unchecked divergence disappears.

## Pre-existing Issues (Not Blocking)

None applicable — `store/` is entirely new in this PR.

## Suggestions (Lower Confidence)

- **`serialize_index` is ~130 lines** - `builder.rs:425-555` (Confidence: 70%) — Over the 50-line HIGH threshold, but it is a flat sequence of clearly-commented serialization phases (postings estimate → bigram table → trigram table → meta → debug asserts → CRC → counts → header → assemble) with nesting depth ≤ 2 and no branching complexity. The cycle-1 `serialize_entry_table` extraction already removed the worst duplication. Splitting further (e.g. a `compute_checksum_payload` or `assemble_header` helper) would marginally reduce length but mostly shuffle linear code behind call boundaries. Low value; noted only for completeness.
- **`binary_search_entries` takes 5 params incl. two closures** - `format.rs:533-561` (Confidence: 62%) — At the parameter threshold, and the two `impl Fn` closures plus `read_key`/`decode_found` naming require a moment to map. This is an acceptable generic-codec trade-off (removes a full duplicate binary search), but a brief doc example of one call site would cut the time-to-understand. Style-level only.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Complexity Score**: 9
**Recommendation**: APPROVED

The single MEDIUM (duplicated offset arithmetic) is a worthwhile cleanup that improves maintainability and removes a checked/unchecked inconsistency, but it is not correctness-blocking and the unchecked sites are provably non-overflowing post-`open`. Per ADR-001 it is surfaced for resolution rather than silently deferred.
