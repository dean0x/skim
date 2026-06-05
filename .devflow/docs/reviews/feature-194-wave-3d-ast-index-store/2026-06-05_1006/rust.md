# Rust Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_1006
**PR**: #272 (Wave 3d AST on-disk index store)
**Cycle**: 2 (cycle 1 fixed 15 issues; this pass hunts NEW issues only)

## Scope

Substantive review of `crates/rskim-search/src/ast_index/store/{format,builder,reader}.rs`,
`benches/ast_index_bench.rs`, and the `mod.rs`/`lib.rs`/`index/mod.rs`/`Cargo.toml` edits.
`.devflow/**` ignored. Idiomatic-Rust lens: ownership/borrowing, lifetimes, `Result`/`thiserror`
error handling, safe integer conversions (PF-004), `unsafe` mmap soundness + `Send`/`Sync` (C7),
byte-level codec correctness, zero-copy slicing, and rayon parallelism soundness.

## Verification performed

- `cargo clippy -p rskim-search --all-targets -- -D warnings` — **clean (0 warnings)**.
  The PR's clippy-clean claim is verified, including `--all-targets` (tests + benches).
- Grepped all `unsafe` in the store module: exactly two — `Mmap::map(&idx_file)` (reader.rs:126)
  and `Mmap::map(&post_file)` (reader.rs:184). Both carry `// SAFETY:` comments documenting the
  mmap TOCTOU constraint. No `unsafe impl Send/Sync`; `AstIndexReader: Send + Sync` is sound
  auto-derivation (`AstSkidxHeader: Copy`, `Mmap: Send + Sync`, `Option<Mmap>` inherits) and is
  compile-time pinned by test A6. C7 holds.
- Grepped all `as`/`unwrap`/`expect`/`panic!` in non-test source: no `.unwrap()`/`.expect()`/
  `panic!()` outside `#[cfg(test)]`; every `as` cast is widening or provably non-narrowing
  (details below). PF-004 (no silent narrowing) is honored — `u32::try_from` is used at every
  narrowing boundary (`node_count`, posting `byte_len → u32`, `bigram_count`/`trigram_count`,
  `postings_file_size → usize`).
- Confirmed `reader.rs::file_meta` overflow posture matches the lexical sibling
  (`index/reader.rs::file_meta_at`) exactly — same unchecked `(idx as usize) * SIZE` derivation
  guarded by a `checked_add(FILE_META_SIZE).filter(e <= mmap.len())` bound. Parity, not a new issue.

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

None at >= 80% confidence.

## Pre-existing Issues (Not Blocking)

None new. The mmap TOCTOU constraint (reader.rs:123-126, 183-184) is the same inherent,
already-documented item from cycle 1 — not actionable, shared posture with the lexical index.

## Suggestions (Lower Confidence)

- **`reserved` header bytes are skipped, not validated as zero on decode** —
  `format.rs:286-326` (Confidence: 60%) — `decode_header` never inspects `[38..44]`. This is the
  correct forward-compat choice (lets a future version repurpose the bytes), but the symmetric
  defensive option used elsewhere in this PR (e.g. the C1 ascending-doc_id check) would be to
  assert-zero now and relax later when a meaning is assigned. Pure style/robustness preference;
  current behavior is intentional per the builder doc ("reserved header bytes stay zero").

- **`avg_*` computed as `(u64 as f64 / n) as f32` then re-validated `is_finite() && >= 0.0`
  on read** — `builder.rs:383-385`, `format.rs:294-313` (Confidence: 60%) — write and read paths
  are internally consistent and the read-side guard is genuinely useful against a corrupt/hostile
  header. The `u64 as f64` precision loss is unreachable in practice (would need > 2^53 distinct
  n-grams). No action needed; noted only for completeness.

## Notes (informational, not findings)

- **Integer-conversion audit (PF-004 focus)**: every `as` cast in the store source is safe.
  `usize → u64` (builder.rs:133,264,265,535) is widening on all supported targets;
  `u32 → usize` and `posting_length as usize` (reader.rs) are widening; the `(count as usize) * SIZE`
  recomputations in `file_meta`/`lookup_bigram`/`lookup_trigram` reuse products already proven
  non-overflowing by the `checked_mul` chain in `open()`. No narrowing `as` cast exists in source.
- **Byte-level codec**: encode/decode are exact inverses; all multi-byte integers use
  `to_le_bytes`/`from_le_bytes` consistently; `read_array::<N>` uses `checked_add` + `get(..)` so
  every fixed-width read is panic-free. CRC32 covers a single contiguous `[HEADER_SIZE..expected]`
  slice and the builder hashes bigram+trigram+meta in the identical serialization order — codec
  and verifier agree.
- **rayon soundness** (`build_from_files`, builder.rs:336-357): `par_iter().map(..).collect()`
  into `Vec<Result<..>>` is order-preserving; the closure is pure (`linearize_source` +
  `extract_ast_ngrams`, no shared mutable state); sequential merge replays in order so the
  sequential-FileId invariant holds. The per-item `?` correctly funnels extraction errors into
  the collected `Result`. Sound.
- **Ownership/borrowing**: the reader stores owned `Mmap`s and slices into them locally within
  each method (`&self.idx_mmap[..]`, `&post_mmap[start..end]`); returned `Vec<AstPosting>` is owned,
  so no lifetime is leaked from the mmap — clean and idiomatic. Borrowing API surface
  (`&AstNgramSet`, `&[(FileId, &str, Language)]`, `&Path`) follows C-BORROW.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Rust Score**: 10
**Recommendation**: APPROVED

Cycle 1 resolved the substantive Rust-idiom findings (no-silent-narrowing via `u32::try_from`,
C1 defensive doc_id-ascending check, `read_array` bounded reads, module visibility parity,
generic `serialize_entry_table` dedup). This cycle 2 pass found no new blocking, should-fix, or
high-confidence pre-existing Rust issues. `unsafe` is minimal and sound, `Send`/`Sync` is correctly
auto-derived, the byte codec is exact, integer conversions honor PF-004, and rayon parallelism is
data-race-free. Clippy is clean under `-D warnings --all-targets`.
