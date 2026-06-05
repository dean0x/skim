# Performance Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_1006
**PR**: #272 (CYCLE 2)
**Scope**: AST on-disk index store (`store/format.rs`, `builder.rs`, `reader.rs`, `benches/ast_index_bench.rs`)

Cross-cycle: read `resolution-summary.md` (cycle 1, 15 fixed). Did NOT re-flag the
already-resolved items: `serialize_entry_table` dedup, A16 bound tightening (3.0→1.8),
O(n) doc_id monotonicity check, peak-memory documentation. This cycle focuses on NEW
performance findings only.

Decisions consulted: ADR-002 (A16 grounded guard), PF-005 (verify acceptance criteria),
PF-004 (widen-before-narrow — confirmed applied at builder.rs:139-144, 294-300, 340-346).

## Issues in Your Changes (BLOCKING)

None. The hot paths are correct, zero-copy on read, and meet the offline `<10s/1000 files`
acceptance target with large margin (measured ~12.8ms). No CRITICAL or HIGH blocking
performance defects in the changed lines.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Redundant per-posting-list sort in `build()` — postings are already sorted by construction** — `builder.rs:389-396`
**Confidence**: 88%
- Problem: `build()` sorts every bigram and trigram posting list by `doc_id`:
  ```rust
  for list in self.bigram_postings.values_mut() {
      list.sort_unstable_by_key(|p| p.doc_id);
  }
  for list in self.trigram_postings.values_mut() {
      list.sort_unstable_by_key(|p| p.doc_id);
  }
  ```
  But `add_file_ngrams` enforces the sequential-FileId invariant (`id.0 != self.file_count → Err`,
  `file_count` strictly increments at builder.rs:225-230, 267). Every push to a posting list
  therefore happens in strictly ascending `doc_id` order already. The sort is provably
  redundant: it can never reorder anything within a single build.
- Impact: For a dense AST corpus (each of ~161 distinct bigram keys appears in ~every file),
  this is ~161 lists × ~1000 entries = ~161K elements (plus the trigram equivalent) run through
  `sort_unstable_by_key` on every build, doing work that the construction order already
  guarantees. `sort_unstable` on pre-sorted input is near-O(n) but still does the comparator
  call + scan per element, plus the closure-key extraction. Minor in absolute terms at current
  scale (build is ~12.8ms total) but it is pure waste that grows O(distinct_keys × files).
- Fix: Replace the sorts with a `debug_assert!` that documents and verifies the invariant
  instead of paying for it in release builds:
  ```rust
  #[cfg(debug_assertions)]
  for list in self.bigram_postings.values().chain(self.trigram_postings.values()) {
      debug_assert!(
          list.windows(2).all(|w| w[0].doc_id < w[1].doc_id),
          "postings must already be ascending by construction (sequential FileId invariant)"
      );
  }
  ```
  If you prefer to keep the defensive sort (cheap insurance against a future change to the
  merge order), add a comment noting it is defensive and normally a no-op, so a future reader
  does not assume postings can arrive unsorted.

## Pre-existing Issues (Not Blocking)

None applicable — this is a new module.

## Suggestions (Lower Confidence)

- **Default SipHash hasher on internal integer-keyed posting maps** - `builder.rs:71-73` (Confidence: 70%) — `bigram_postings: HashMap<u32, …>` and `trigram_postings: HashMap<u64, …>` use std's default SipHash, which is DoS-resistant but slow. Keys are internal packed n-gram integers, not adversarial input, so a faster hasher (`rustc_hash::FxHashMap` / `ahash`) would cut hashing cost in the per-n-gram merge loop, which runs millions of times on a real corpus. Offline build path, so low priority, but it is the standard Rust win for internal int-keyed maps. Verify it does not introduce a new dependency the project wants to avoid before adopting.

- **`total_posting_bytes_est` capacity pre-pass iterates all postings** - `builder.rs:434-445` (Confidence: 62%) — `serialize_index` does a full `.values().map(|v| v.len()).sum()` over both maps purely to size `postings_buf`. This is a second O(total postings) pass before the serialization pass that re-walks the same data. Could be folded into a running counter maintained during merge, but the pre-pass enables one `Vec::with_capacity` (avoiding reallocs), so the tradeoff is reasonable as-is — noting only because it is a measurable extra scan over the largest data structure.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Performance Score**: 9/10
**Recommendation**: APPROVED

### Rationale

The hot paths are sound:
- **Read path** is genuinely zero-copy: `lookup_bigram`/`lookup_trigram` binary-search over an
  mmap slice (`format.rs:533-561`) and `lookup_postings_generic` slices directly into
  `post_mmap` without intermediate copies (`reader.rs:359`). The only allocation is the result
  `Vec`, pre-sized via `Vec::with_capacity(n)` (reader.rs:361). Good.
- **Binary search** is O(log n) with checked stride validation; no per-entry allocation.
- **Build path** parallelises the pure extract step with rayon and merges sequentially —
  the documented peak-memory bound (materialise-then-merge) is acknowledged and tracked in #273.
- A15 (`<10s/1000 files`) passes with ~780× margin; A16 size-ratio guard is empirically grounded
  per ADR-002 / PF-005.

The single MEDIUM (redundant sort) is a real but small inefficiency that does not threaten the
acceptance target. It is worth fixing because the cost scales O(distinct_keys × files) and the
work is provably unnecessary given the builder's own sequential-FileId invariant — but it does
not block merge.
