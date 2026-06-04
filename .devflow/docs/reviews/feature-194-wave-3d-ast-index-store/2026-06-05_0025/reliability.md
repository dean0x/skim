# Reliability Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_0025
**Scope**: `crates/rskim-search/src/ast_index/store/{builder.rs, reader.rs, format.rs}`
**PR**: #272 (Wave 3d — AST n-gram on-disk index format, builder & reader)

## Summary of Posture

This is genuinely careful defensive code. The codec layer (`format.rs`) and the
reader's `open()` validation are exemplary for the reliability rules in scope:

- **Bounded iteration**: every decode loop iterates over a count derived from a
  pre-validated length (`n = length / AST_POSTING_ENTRY_SIZE`, `n = data.len() / stride`).
  There are no attacker-driven unbounded loops — `open()` proves
  `idx_mmap.len() == expected_idx_size` *before* any iteration, and posting loops
  derive their bound from a length that is bounds- and alignment-checked first.
- **Panic-vs-Result**: the malformed-input decode paths return
  `Err(IndexCorrupted)` rather than panicking. No `unwrap`/`expect`/`panic!`
  outside `#[cfg(test)]` in the three files. (verified C3 holds — see below)
- **Integer overflow (PF-004 spirit)**: offset/length arithmetic in the builder
  serializer and reader `open()` uses `checked_mul`/`checked_add`/`u32::try_from`
  consistently. `node_count` uses `u32::try_from` (no silent `as u32`) — applies
  PF-004 analog; commit 9f300ea's fix is intact and no regression was introduced.

No CRITICAL or HIGH findings block this PR on reliability grounds. The findings
below are MEDIUM/LOW — primarily a documentation-vs-implementation gap on the
crash-safety claim, plus a few precision/assertion improvements.

---

## Issues in Your Changes (BLOCKING)

None at CRITICAL or HIGH confidence.

---

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Atomic-write crash-safety claim overstates what the code guarantees — no directory fsync** — `builder.rs:336-338`, `builder.rs:513-531`
**Confidence**: 85%
- Problem: The module doc (`builder.rs:1-9`) and `atomic_write` doc
  (`builder.rs:513-516`) claim crash safety: "A partial write (e.g. power loss
  between the two) leaves no `.skidx`". The implementation does `write_all` +
  `sync_all` (fsync of file *contents*) + `persist` (rename). It does **not**
  fsync the containing directory after either `persist`. On POSIX, the rename is
  a directory-metadata operation; without an fsync on `output_dir`, a crash after
  `persist()` returns can lose the rename (the directory entry is not yet durable)
  — and the *relative ordering* of the two renames (`.skpost` before `.skidx`) is
  likewise not guaranteed durable across a crash. The "reader finds `.skidx` ⇒
  `.skpost` is coherent" invariant therefore holds in steady state but is weaker
  than stated under power loss. (per the reliability rule: "every resource must
  have a known lifetime" / durability must be explicit, not assumed)
- Impact: A power-loss window can leave a `.skidx` referencing a `.skpost` that
  was not durably written, or lose the index entirely. In practice the reader's
  size + CRC checks on `.skidx` and the per-lookup offset-bounds re-validation
  against the actual `.skpost` length catch most torn states as `IndexCorrupted`
  rather than UB — so this degrades to "rebuild required," not silent corruption.
  This is the same posture as the cochange sibling (the PR explicitly mirrors it),
  so consistency is preserved; the issue is that the doc comment promises more
  than the code delivers.
- Fix: Either (a) open `output_dir` and `sync_all()` it after both `persist`
  calls to make the renames durable, or (b) soften the doc comment to state that
  durability of the rename depends on the filesystem and is not guaranteed across
  power loss without a directory fsync. Option (b) is the minimal, honest fix and
  keeps parity with cochange; option (a) is the stronger guarantee. Recommend (b)
  now + a tracked follow-up for (a) across both this and the cochange sibling so
  they stay consistent.

**Re-index read/write interleave can yield silently-wrong (non-erroring) results** — `builder.rs:11-15`, `builder.rs:336-338`
**Confidence**: 82%
- Problem: On *re-index* of an existing directory, `.skpost` is `persist`ed
  (overwriting the old postings) before `.skidx`. A concurrent reader that opened
  with the OLD `.skidx` but reads against the NEW `.skpost` (or one opening in the
  window between the two persists) uses old offsets/lengths against new postings.
  CRC32 covers only `.skidx`, so it passes. The reader re-validates that
  `offset+length <= post_mmap.len()` (`reader.rs:315`) and alignment, but a stale
  offset can still land in-bounds and decode into structurally-valid-but-wrong
  postings — wrong results with no error. (reliability rule: assert invariants;
  the cross-file coherence invariant is unguarded at read time)
- Impact: Silent wrong query results during a re-index race. The module doc
  (`builder.rs:11-15`) explicitly documents "NOT concurrency-safe ... callers MUST
  serialize re-index operations against concurrent reads," so this is a documented
  precondition rather than a defect. Flagged because it is the highest-impact
  reliability caveat in the design and currently relies entirely on caller
  discipline with no in-format guard.
- Fix: No code change required to satisfy the documented contract. If hardening is
  desired later (track as follow-up, do not block): add a generation/build-id to
  both file headers and have the reader verify they match at `open()` — turning a
  silent-wrong-result into an `IndexCorrupted`. The reserved 6 header bytes
  (`format.rs:90`) are an available home for a generation marker.

### LOW

**Production invariants downgraded to `#[cfg(debug_assertions)]` / absent on the build path** — `builder.rs:443-462`
**Confidence**: 80%
- Problem: The contiguous-offsets invariant for posting entries is asserted only
  under `debug_assertions` (`builder.rs:444`). The reliability rules call for
  asserting invariants in *production* code, not just debug/test builds. Release
  builds (the shipped binary) carry no check that the serializer produced
  contiguous, monotonic offsets. Separately, `add_file_ngrams` does not assert the
  documented `count >= 1` precondition on incoming `set` entries before writing
  them as postings — it trusts `extract_ast_ngrams`; if a future caller passes a
  hand-built `AstNgramSet` with `count == 0`, the builder writes it and the
  *reader* later rejects it as `IndexCorrupted` (`format.rs:392`). The invariant is
  enforced at the wrong boundary (read side, after persistence) rather than at the
  write boundary.
- Impact: Low — both conditions are currently guaranteed by upstream construction.
  This is defense-in-depth, not a live bug.
- Fix: Keep the offset-contiguity `debug_assert` (it is hot-path-adjacent, so
  debug-only is defensible per the Rust rule "debug_assert! for invariants in hot
  paths"). For the write boundary, consider a cheap `if entry.count == 0 { return
  Err(IndexCorrupted) }` guard in `add_file_ngrams`'s merge loops so a bad
  `AstNgramSet` fails loud at the producing boundary rather than at read time.

**`as u64` / `as usize` narrowings on length sums lack the `try_from` treatment applied elsewhere** — `builder.rs:191-192`, `builder.rs:303-305`, `builder.rs:371`, `builder.rs:399`, `builder.rs:494`
**Confidence**: 80%
- Problem: The builder is rigorous about `checked_*`/`try_from` on the
  size-critical paths (posting byte lengths, counts), but a few arithmetic spots
  use plain `as` casts: `set.bigrams.len() as u64` (`:191`), the `f64 as f32`
  average casts (`:303-305`), `postings_buf.len() as u64` for offsets (`:371`,
  `:399`) and `postings_file_size` (`:494`). These are widenings (`usize`→`u64`,
  `len()`→`u64`) or lossy-but-acceptable float casts, so none can truncate on the
  64-bit targets in scope — but they are stylistically inconsistent with the
  PF-004-spirit discipline applied two lines away (`u32::try_from` on the same
  `len()`-derived values). On a 32-bit target, `postings_buf.len() as u64` is a
  safe widening; the reverse `usize::try_from` on read (`reader.rs:168`,
  `reader.rs:308`) already guards the narrowing direction, so this is symmetric and
  safe.
- Impact: None functionally on supported targets; consistency/auditability only.
- Fix: Optional. Leave the widening `as u64` casts (they cannot lose data); the
  `f64 as f32` average casts are intentional and bounded. No change strictly
  required — noting for completeness given the PF-004 focus.

---

## Pre-existing Issues (Not Blocking)

**`unsafe { Mmap::map }` UB window on concurrent external truncation** — `reader.rs:112-115`, `reader.rs:174-175`
**Confidence**: 90%
- Problem: `Mmap::map` is `unsafe` because the mapped file must not be mutated for
  the lifetime of the map; the SAFETY comment correctly documents that concurrent
  truncation/overwrite is UB. This is inherent to all mmap-based indexes in this
  crate (the lexical index has the identical pattern) and is not introduced by this
  PR — it is the same accepted constraint codebase-wide.
- Impact: UB only under the documented-as-forbidden concurrent-mutation scenario,
  which the "serialize re-index against reads" contract already prohibits.
- Fix: None for this PR. Informational — consistent with the existing lexical
  index posture.

---

## Suggestions (Lower Confidence)

- **Empty-postings guard relies on `posting_length == 0` rather than the `None`
  mmap** — `reader.rs:298` (Confidence: 70%) — the early `posting_length == 0`
  return precedes the `post_mmap` `None` check; both are correct, but a one-line
  assert that a non-empty `posting_length` implies `post_mmap.is_some()` would make
  the "entry says non-empty but no postings file exists" corruption case an
  explicit `IndexCorrupted` rather than the current `Ok(vec![])` (`reader.rs:303-306`
  silently returns empty for that inconsistent state).
- **`read_key` closures in `format.rs` re-implement a 4/8-byte slice read instead
  of reusing `read_array`** — `format.rs:460-463`, `format.rs:483-486` (Confidence:
  65%) — they use direct `data[off..off+4]` indexing (safe here because
  `binary_search_entries` validated `len % stride == 0` and `off < len`), but
  routing through the existing `read_array` helper would keep all slice access on
  one audited, overflow-checked path.

---

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 2 | 2 |
| Pre-existing | - | - | 0 | 1 |

**Reliability Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

Conditions (non-blocking, documentation-honesty + defense-in-depth):
1. Reconcile the `atomic_write` crash-safety doc comment with the actual
   guarantee (add directory fsync, or soften the claim) — `builder.rs:1-9, 513-516`.
2. (Optional, recommended follow-up) Add a generation marker in the reserved
   header bytes to convert the re-index race from silent-wrong-result to
   `IndexCorrupted`.

The codec and reader-validation layers are strong: bounded iteration throughout,
no panic-on-malformed-input paths (C3 verified), and overflow-checked offset/length
arithmetic on the load-bearing paths. PF-004 discipline (`u32::try_from` for
`node_count`, no silent `as u32` narrowing) is correctly applied and the 9f300ea
fix is intact.
