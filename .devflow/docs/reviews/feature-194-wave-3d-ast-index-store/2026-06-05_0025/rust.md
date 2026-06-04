# Rust Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**PR**: #272
**Date**: 2026-06-05

## Summary of assessment

This is a carefully written, idiomatic Rust codec. The diff demonstrates strong Rust
discipline: all fallible paths return `Result`/`SearchError`, `read_array` replaces every
panicking slice index in the codec, `checked_*` arithmetic guards every size computation at
`open`, `unsafe` is confined to the two unavoidable `Mmap::map` calls and each carries a
`// SAFETY:` comment, byte order is uniformly little-endian via `to_le_bytes`/`from_le_bytes`,
and `Send + Sync` is asserted at compile time (test A6) rather than via a manual `unsafe impl`.
`build_from_files` uses `par_iter().map(...).collect::<Vec<Result<_>>>()` — a pure map with no
shared mutable state — and merges sequentially, which is sound. `node_count` narrowing uses
`u32::try_from` (applies PF-004 analog; no silent `as u32`). Clippy is clean (`-D warnings`,
verified) and all 74 store tests pass.

No blocking issues. The items below are low/medium-confidence polish suggestions in code that
was touched.

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`read_array` length error message reports total `data.len()` rather than available bytes** — `format.rs:200-212`
**Confidence**: 88%
- Problem: When the slice is shorter than `start+N`, the error reports `got {data.len()}`
  (the whole buffer length), but the actual shortfall is at `start`. For a truncated header
  this reads e.g. "need 8 bytes at offset 18, got 47" which is confusing because 47 > 8.
  This is a diagnostics-quality issue only — the error path is correct and never panics.
- Fix: report `data.len().saturating_sub(start)` (available bytes from `start`) instead of
  `data.len()`, or include both: `got {} bytes available from offset {start}`.

## Pre-existing Issues (Not Blocking)

None observed in the touched modules.

## Suggestions (Lower Confidence)

- **Unchecked `as usize` multiplications in the lookup/`file_meta_at` hot paths** — `reader.rs:248`, `reader.rs:271-273`, `reader.rs:344-347` (Confidence: 70%) — `(header.bigram_count as usize) * AST_BIGRAM_ENTRY_SIZE` and siblings use raw `*` rather than the `checked_mul` used in `open()`. These are provably safe today because `open()` already validated `idx_mmap.len() == expected_idx_size` using `checked_mul` over the same counts, so any overflowing count is rejected before these run. Consider a one-line comment noting the invariant ("counts validated non-overflowing in `open`") so a future refactor that splits `open` from these accessors does not silently reintroduce a panic risk.

- **`build_from_files` parallel extraction is not short-circuited on first error** — `builder.rs:256-270` (Confidence: 65%) — `par_iter().map(...).collect()` runs `linearize_source` for every file even if an early file errors; the first `Err` only surfaces during the sequential merge loop. For the documented inputs (errors are rare grammar-load failures) this is harmless and arguably preferable (deterministic ordering). Noting only because for very large corpora a failing build does more work than necessary before reporting. No change recommended unless build latency on error becomes a concern.

- **`avg_*` cast chain `(sum as f64 / n) as f32`** — `builder.rs:303-305` (Confidence: 62%) — the `u64 as f64` and `f64 as f32` narrowing casts are intentional and the decoded values are re-validated finite/`>= 0.0` in `decode_header`, so round-trip safety holds. Purely stylistic: an explicit helper or `#[allow]` with a one-word rationale would make the intent self-documenting alongside the PF-004 discipline applied elsewhere in the file.

## Notable strengths (no action)

- `decode_posting` enforces C4 (`count >= 1`) at decode time, and `lookup_postings_generic`
  re-validates offset/length bounds + `len % 8 == 0` alignment with `checked_add` — C3 is
  upheld defensively even though the builder is the only writer.
- CRC32 serialization order in `serialize_index` (bigram + trigram + meta) exactly matches the
  reader's contiguous `idx_mmap[48..expected_idx_size]` slice — verified consistent.
- Empty-corpus guard (`post_mmap = None` when `postings_file_size == 0`) correctly avoids
  mmap-ing a zero-length file, and `lookup_postings_generic` returns `Ok(vec![])` for that case.
- `#[cfg(debug_assertions)]` block asserting monotonic, contiguous posting offsets is a good
  use of `debug_assert` for an internal invariant in a non-hot build path.
- `Send + Sync` established structurally (Copy header + memmap2 guarantees + no interior
  mutability) and verified by a compile-time test rather than an `unsafe impl` — correct (C7).

## Summary Table

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Rust Score**: 9/10
**Recommendation**: APPROVED
