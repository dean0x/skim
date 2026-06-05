# Reliability Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_1006
**Focus**: reliability (bounded iteration, assertion density, allocation discipline, indirection limits)
**Cycle**: 2 (cycle 1 fixed 15 issues; this review targets NEW issues only)
**PR**: #272

## Scope

Substantive files reviewed against `git diff main...HEAD`:
`ast_index/store/{format.rs, builder.rs, reader.rs}`, `benches/ast_index_bench.rs`,
and the small support edits in `ast_index/mod.rs`, `store/mod.rs`, `lib.rs`,
`index/mod.rs`, `Cargo.toml`. `.devflow/**` ignored per instructions.

The store layer is mature after cycle 1. Decode loops are bounded by validated
slice lengths; all multi-byte reads use `read_array` with `checked_add`; size
validation in `open()` uses `checked_*` throughout; `file_count` uses
`checked_add`; `node_count` narrowing uses `u32::try_from` (avoids PF-004);
posting-length narrowing uses `u32::try_from` (avoids PF-004); the empty-postings
`post_mmap=None` guard is present; the C1 strictly-ascending `doc_id` defensive
check is present in `lookup_postings_generic`; and the atomic-write path is a
verbatim match of the cochange sibling (confirmed against
`cochange/builder.rs:331`). Malformed input returns `IndexCorrupted` rather than
panicking on every path I traced.

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Builder does not assert its own `count >= 1` output invariant at the public trust boundary** — `crates/rskim-search/src/ast_index/store/builder.rs:235-254`
**Confidence**: 82%
- Problem: `add_file_ngrams` is a public API (`AstIndexBuilder` is re-exported from
  the crate root) that accepts an arbitrary `&AstNgramSet`. `AstBigramEntry` /
  `AstTrigramEntry` have public fields (`extract.rs:38-61`) and are also re-exported
  from the crate root, so an external caller can construct an entry with `count == 0`
  and pass it in. The builder copies `count: entry.count` straight into the posting
  list (`builder.rs:241` and `:251`) with no validation. The reader enforces the
  C4 invariant (`decode_posting` rejects `count == 0`, `format.rs:420`), so such an
  index builds successfully and `open()` succeeds, but **every `lookup_bigram` /
  `lookup_trigram` that touches the offending posting list then fails with
  `IndexCorrupted` at query time**. The invariant is enforced on read but not
  asserted on write — a boundary-validation asymmetry. Project rule: "Assert
  preconditions and invariants in production code — not just tests" and "Validate
  at boundaries."
- Impact: A latent corrupt-index condition that is only discoverable at query time
  rather than at build time. The production path (`extract_ast_ngrams`) cannot
  produce `count == 0` (every map entry is `saturating_add(1)`'d at least once
  before storage, `extract.rs:216`), so this is reachable only via the public
  `add_file_ngrams` / `build_from_files` entry points with caller-constructed
  sets — but those are public, supported APIs.
- Fix: Assert the invariant where the data crosses the boundary, e.g. in the
  bigram/trigram merge loops:
  ```rust
  for entry in &set.bigrams {
      if entry.count == 0 {
          return Err(SearchError::InvalidQuery(format!(
              "AstNgramSet bigram for file {} has count == 0 (must be >= 1)",
              id.0
          )));
      }
      self.bigram_postings.entry(entry.ngram.key()).or_default()
          .push(AstPostingEntry { doc_id: id.0, count: entry.count });
  }
  ```
  (mirror for trigrams). This converts a deferred query-time `IndexCorrupted`
  into an immediate, located build-time error and makes the build/read contract
  symmetric.

## Pre-existing Issues (Not Blocking)

None identified in the diff's blast radius. The directory-fsync rename-durability
limitation (`builder.rs:11-14`) is an inherent, documented property shared with the
cochange sibling and was already softened/documented in cycle 1 — not re-flagged.

## Suggestions (Lower Confidence)

- **Unchecked accumulator arithmetic in `add_file_ngrams`** —
  `crates/rskim-search/src/ast_index/store/builder.rs:263-265` (Confidence: 62%) —
  `total_node_count`, `total_distinct_bigrams`, and `total_distinct_trigrams` use
  plain `+=` / `as u64` while `file_count` uses `checked_add`. Overflow requires
  ~2^32 files or pathological per-file counts and is physically unreachable
  (`file_count` itself caps at u32::MAX), so this is a consistency-of-discipline
  nit rather than a real outage. A debug build would panic on overflow; release
  would wrap a statistics-only average. Optional: use `saturating_add` for parity
  with the rest of the builder's overflow discipline.

- **Capacity-hint multiply not checked** —
  `crates/rskim-search/src/ast_index/store/builder.rs:434-445` (Confidence: 60%) —
  `total_posting_bytes_est` computes `(... ).sum::<usize>() * POSTING_ENTRY_SIZE`
  with an unchecked `*` to size a `Vec::with_capacity`. Only a capacity hint; the
  actual byte writes downstream use `checked_mul` (`serialize_entry_table:134`).
  Worst case is a debug-build panic on an absurd corpus. Optional `saturating_mul`
  for hygiene.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Reliability Score**: 9
**Recommendation**: APPROVED_WITH_CONDITIONS

The store layer's bounded-iteration and overflow discipline is strong and the
cycle-1 fixes hold. The one MEDIUM (build-side `count >= 1` boundary assertion) is
a defense-in-depth / assertion-density gap that aligns directly with the project's
"assert invariants in production code" and "validate at boundaries" rules and with
ADR-001 ("if you see something, do something"); recommend fixing it in this PR
rather than deferring. The two suggestions are optional hygiene only.
