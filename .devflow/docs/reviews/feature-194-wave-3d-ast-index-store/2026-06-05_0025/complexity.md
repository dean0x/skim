# Complexity Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05 00:25
**PR**: #272
**Scope**: `crates/rskim-search/src/ast_index/store/{format,builder,reader}.rs`

## Summary of Assessment

This is well-structured codec code. The author already applied the two abstractions
that matter most for noise reduction: `binary_search_entries` unifies bigram/trigram
lookup (the u32→u64 widening closure is the right call), and `lookup_postings_generic`
unifies posting decode across both n-gram arities. The encode/decode functions are flat,
single-purpose, and individually explainable in well under 5 minutes. Offset/length
arithmetic uses `checked_*`/`try_from` consistently and reads clearly.

The one genuine complexity finding is `serialize_index` in builder.rs: it is ~166 lines
and contains two near-identical 25-line entry-serialization blocks that could be unified
the same way the reader already unified its read path. Everything else is at or below
the project's complexity thresholds. PF-004's "widen before narrowing" rule is honored
(`u32::try_from` for node_count, no silent `as u32`).

## Issues in Your Changes (BLOCKING)

None. No CRITICAL or HIGH complexity violations in the changed lines.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`serialize_index` is a 166-line function with duplicated bigram/trigram entry blocks** — Confidence: 88%
- `crates/rskim-search/src/ast_index/store/builder.rs:345-511`
- Problem: The function spans 166 lines (threshold: >50 is HIGH territory; the body
  here is broken into clearly-commented sections, which keeps it readable, so this lands
  at MEDIUM not HIGH). Two blocks — bigram entries (lines 368-393) and trigram entries
  (lines 396-421) — are structurally identical: take sorted keys, look up the posting
  list, compute `checked_mul(AST_POSTING_ENTRY_SIZE)`, `u32::try_from` the byte length,
  append encoded postings to `postings_buf`, push an entry. The only differences are the
  key type (u32 vs u64), the entry struct constructor, and the error-message hex width
  (`{key:#010x}` vs `{key:#018x}`). This is the exact duplication the prompt flagged.
  Notably, the reader side already avoided this duplication via `binary_search_entries`
  and `lookup_postings_generic` — the builder's serialize path is the one place the
  pattern was NOT unified.
- Impact: A change to the posting-serialization contract (e.g. adding a per-list checksum,
  changing offset semantics, or the #273 compression follow-up) must be made in two places
  and kept in sync. Divergence here would corrupt one entry table silently.
- Fix: Extract a generic helper mirroring the reader's approach, e.g.:
  ```rust
  // Serialize one entry table; returns (entries_buf, Vec<Entry>) or pushes into
  // postings_buf and a typed entry vec. Key encoding via a closure, entry build via closure.
  fn serialize_entry_table<K: Copy, E>(
      postings_buf: &mut Vec<u8>,
      keys: &[K],
      postings: &HashMap<K, Vec<AstPostingEntry>>,
      hex_width: usize,           // or just drop the width distinction in the error
      make_entry: impl Fn(K, u64, u32) -> E,
  ) -> Result<Vec<E>> { ... }
  ```
  Even a lighter touch — factoring just the `byte_len`/`u32::try_from` length computation
  into a `fn posting_byte_len(list_len: usize) -> Result<u32>` shared by both loops —
  removes the most error-prone duplicated arithmetic without introducing generics.

## Pre-existing Issues (Not Blocking)

None relevant. The lexical `index/` sibling uses a single n-gram table (no bigram/trigram
split), so the dual-table layout is new to this PR, not inherited debt.

## Suggestions (Lower Confidence)

- **`serialize_index` debug-assertion block adds cognitive load mid-function** -
  `crates/rskim-search/src/ast_index/store/builder.rs:443-462` (Confidence: 65%) — The
  `#[cfg(debug_assertions)]` contiguity check recomputes expected offsets in a way that
  duplicates the offset logic already implicit in the serialization loops. It is correct
  and valuable as an invariant guard, but reading it requires re-deriving the layout. If
  `serialize_index` is split per the finding above, this block fits naturally as a small
  post-condition check on the combined entry list.

- **Three identical `avg_* must be finite and >= 0.0` validation blocks in `decode_header`** -
  `crates/rskim-search/src/ast_index/store/format.rs:266-285` (Confidence: 62%) — Three
  copies of the same finite/non-negative f32 check differing only in field name and offset.
  A `read_avg_f32(data, offset, field_name) -> Result<f32>` helper would collapse 20 lines
  to 3 call sites. Minor; the current form is fully readable.

## Notes on Concerns Raised in the Prompt (assessed, not flagged)

- **Cyclomatic complexity of encode/decode**: Low. Encoders are straight-line
  `copy_from_slice` sequences (complexity 1). Decoders are a length-guard + field reads;
  `decode_header` is the highest at ~7 branches (one early-return guard + three avg-validation
  blocks), still within the <10 warning band.
- **Binary-search / validation nesting**: `binary_search_entries` (format.rs:502-530) is a
  textbook bounded loop, nesting depth 2, explicitly terminating (`lo < hi`, `hi = mid` /
  `lo = mid + 1`). No reliability concern. `lookup_postings_generic` is a flat sequence of
  guard clauses then one bounded `for i in 0..n` loop — exactly the early-return style the
  complexity skill recommends.
- **Offset/length arithmetic readability**: Good. Every multiplication/addition that could
  overflow uses `checked_mul`/`checked_add`/`try_from` with a descriptive `IndexCorrupted`
  message. `expected_idx_size` (reader.rs:137-141) chains `checked_add` legibly.
- **Is the three-section codec harder than necessary?**: No. The header / entry-table /
  file-meta split mirrors the lexical sibling and is the conventional inverted-index layout.
  The separate bigram (16B) / trigram (20B) tables are inherent to the data model (u32 vs u64
  keys) and cannot be collapsed without wasting 4 bytes per bigram entry.
- **bigram/trigram duplication that could be unified**: Mostly already unified on the read
  path (the single MEDIUM finding is the remaining write-path duplication).
- **PF-004 adherence**: Honored. `add_file`/`build_from_files` use `u32::try_from(lin.nodes.len())`
  with an `IndexCorrupted` error rather than `as u32` (builder.rs:221-227, 260-266). avoids PF-004.
- **ADR-001 adherence**: FileId guards mirror the lexical builder and fix-noticed-issues
  posture is reflected in the duplicate/sequential checks (builder.rs:145-157). applies ADR-001.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Complexity Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The single MEDIUM (write-path entry-table duplication in `serialize_index`) is worth
addressing while the code is fresh and before the #273 compression work touches the same
loops, but it is not merge-blocking. Codec readability, bounded loops, and overflow
discipline are all strong.
