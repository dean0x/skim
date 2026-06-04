# Performance Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**PR**: #272
**Date**: 2026-06-05_0025
**Scope**: ast_index/store/{builder,reader,format}.rs, benches/ast_index_bench.rs, Cargo.toml (rayon)

## Summary of Posture

This is library code on a hot-but-bounded path. The build path is one-shot (re-index),
the read path (`lookup_bigram`/`lookup_trigram`) is the per-query hot path for Wave 3f.
The A15 bench measures ~12.8 ms for 1000 files against a 10 s target — three orders of
magnitude of margin, so build-side allocation is not a practical bottleneck at the stated
corpus size. Findings below are calibrated against that reality: most allocation concerns
are MEDIUM/LOW because the data sizes are bounded (vocab ~161 distinct n-grams for Rust,
max 100K nodes/file, file_count u32-bounded). The one structural concern worth flagging is
the unbounded peak-memory profile of `build_from_files` as corpus size scales beyond the
benched 1000 files.

The code is well-engineered for performance fundamentals: `with_capacity` is used
consistently, `sort_unstable` everywhere, `Copy` on-disk structs, header decoded once out
of mmap, binary search reads keys lazily without decoding whole entries, zero-copy mmap
slices into postings. No N+1, no blocking I/O in a request path, no O(n^2).

## Issues in Your Changes (BLOCKING)

None. No CRITICAL or HIGH-confidence blocking performance defects in the changed lines.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`build_from_files` materializes the entire extracted corpus in memory before merging** — `builder.rs:256-277`
**Confidence**: 88%
- Problem: The rayon stage collects `Vec<Result<(FileId, Language, AstNgramSet, u32)>>`
  for ALL files at once (line 256-270), then the sequential merge consumes it (274-277).
  Peak memory holds every file's full `AstNgramSet` (each containing `Vec<AstBigramEntry>`
  + `Vec<AstTrigramEntry>`, 12–16 B per entry plus Vec overhead) simultaneously, on top of
  the `bigram_postings`/`trigram_postings` HashMaps being built. For the benched 1000
  small files this is trivial, but the A15 target is a floor, not the real corpus —
  Wave 4 builds over whole repos. Memory is O(total distinct n-grams across all files)
  held twice transiently (extracted Vec + merged maps) at the crossover point.
- Impact: For a large monorepo (100K+ files) this is a memory spike with no upper bound
  beyond the file set. The `extracted` Vec is fully alive until the merge loop drains it;
  `for result in extracted` moves out element-by-element but the backing Vec allocation
  (and any not-yet-consumed `AstNgramSet`s) persists for the whole loop.
- Fix: Two low-cost options. (a) Chunk the parallel stage: process files in batches of
  e.g. 4096, merging each batch before extracting the next — caps peak memory at
  batch_size worth of sets. (b) If determinism/order allows, use a parallel fold into
  per-thread partial HashMaps then reduce — but that complicates the sequential-FileId
  invariant, so (a) is the pragmatic fit. At minimum, document the peak-memory profile
  as a known bound the way KNOWLEDGE.md documents the size ratio, and reference #273.
  This is informational-leaning given current corpus sizes; flagging because the parallel
  path is the new primitive and the memory profile is invisible at the benched scale.

**`AstNgramSet` is cloned/moved through the rayon boundary with full ownership** — `builder.rs:268, 275-276`
**Confidence**: 82%
- Problem: Each parallel task returns an owned `AstNgramSet` by value (line 268), then the
  merge borrows it via `&set` (line 276) and iterates `&set.bigrams` / `&set.trigrams`
  (line 162, 173), pushing into the postings maps. The sets are never reused — they are
  dropped after merge. This is correct, but the postings `push` per entry into a HashMap
  with `or_default()` (line 163-169) reallocates the per-key `Vec<AstPostingEntry>`
  repeatedly as posting lists grow across files. With ~161 hot keys each accumulating up
  to `file_count` postings, those inner Vecs grow geometrically with no pre-sizing.
- Impact: For 1000 files, each of ~161 dense keys grows its Vec to ~1000 entries via
  doubling reallocation — ~10 reallocations/key, bounded and cheap here. At repo scale
  (100K files) each hot key's Vec reallocates ~17 times copying up to 100K*8B = 800KB on
  the final grow. Bounded but a measurable allocation cost as corpus grows.
- Fix: Optional micro-opt — if the file count is known up front in `build_from_files`,
  the merge could reserve posting-list capacity for known-dense keys, but the key set is
  not known until merge. Realistically this is acceptable as-is; documenting it suffices.
  Do NOT pre-allocate every key to file_count capacity (that would blow up memory for the
  long tail of rare keys). Leave as-is unless profiling at repo scale shows it dominates.

## Pre-existing Issues (Not Blocking)

None relevant — this is a new submodule.

## Suggestions (Lower Confidence)

- **Posting decode loops one entry at a time with bounds-implicit indexing** —
  `reader.rs:331-338` (Confidence: 70%) — `lookup_postings_generic` decodes postings in a
  `for i in 0..n` loop calling `decode_posting` per 8-byte chunk, each doing a `read_array`
  with `checked_add` + `try_into`. The slice bounds are already validated (end <= mmap.len,
  length aligned) before the loop, so the per-entry `checked_add`/`get` re-validation is
  redundant work on the read hot path. A `chunks_exact(8)` iterator with direct
  `from_le_bytes` on `[0..4]`/`[4..8]` would drop the per-entry overhead. Minor; postings
  lists are short and this is correctness-conservative, but it is the actual per-query hot
  path for Wave 3f so worth noting.

- **`serialize_index` double-buffers entry bytes before assembling `.skidx`** —
  `builder.rs:430-508` (Confidence: 65%) — bigram/trigram entries are encoded into
  `bigram_entries_buf` / `trigram_entries_buf`, then those buffers are CRC'd and finally
  copied again into `skidx_buf` (line 506-508). The intermediate buffers exist to feed CRC
  in serialization order. Could CRC incrementally while extending `skidx_buf` directly,
  eliminating two transient allocations and two copies of the entry tables. Build-side, one
  shot, bounded by entry count (u32) — negligible at current scale, hence low confidence.

## Verification Notes

- Confirmed against KNOWLEDGE.md: A15 ~12.8 ms vs 10 s target; A16 ratio guard relaxed to
  <3.0x (measured ~1.23x), context commit 0ee9de4 / issue #273. The relaxation is sound —
  uncompressed structural trigram indexes are dense by design (O(vocab x files)); compression
  is correctly deferred to #273. No performance regression from the relaxed assertion.
- Confirmed `build_from_files` is a NEW primitive (lexical sibling has no parallel build
  path), so the MEDIUM findings are genuinely introduced here, not inherited.
- Confirmed binary search (`format.rs:502-530`) reads only the 4/8-byte key per probe via
  the `read_key` closure rather than decoding full entries — correct zero-allocation search.
- Confirmed `add_file_ngrams` (`builder.rs:138-198`) has no unnecessary clones: it borrows
  `&set` and copies only the 8-byte `AstPostingEntry` value per push. The `seen_file_ids`
  HashSet is redundant with the sequential `id.0 != file_count` check (the sequential guard
  already implies uniqueness), a minor memory/insert cost but a defensible defense-in-depth
  choice — not flagged as a perf issue.
- Verified bench correctly excludes setup (`iter_batched` setup closure clones sources and
  builds the file vec outside the timed closure); the timed closure does an extra
  `Vec<(FileId,&str,Language)>` rebuild (line 53-56) inside timing, adding ~1000 small
  allocations to the measured path. Negligible vs the build cost but technically inflates
  the measurement slightly. Acceptable.

## Summary
| Category    | CRITICAL | HIGH | MEDIUM | LOW |
|-------------|----------|------|--------|-----|
| Blocking    | 0        | 0    | -      | -   |
| Should Fix  | -        | 0    | 2      | -   |
| Pre-existing| -        | -    | 0      | 0   |

**Performance Score**: 8/10
**Recommendation**: APPROVED

The build and read paths are sound for the current bounded corpus and meet the A15/A16
targets with large margin. The two MEDIUM findings concern peak-memory scaling of the new
`build_from_files` primitive beyond the benched 1000-file size — worth documenting as a
known bound (alongside the size-ratio note in KNOWLEDGE.md) and revisiting in Wave 4 /
#273, but not merge-blocking given the measured headroom.
