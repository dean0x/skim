# Security Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**PR**: #272
**Date**: 2026-06-05 00:25
**Focus**: security (untrusted/corrupted binary index parsing over mmap)
**Scope**: ast_index/store/{format,builder,reader}.rs + tests, index/mod.rs, lib.rs, Cargo.toml/Cargo.lock (rayon)

## Threat model applied

The `.skidx`/`.skpost` files are treated as attacker-controlled (a malicious or
corrupted on-disk index). CRC32 is a corruption detector, NOT an integrity/auth
control — an attacker who can write the file can recompute the CRC trivially. So
findings focus on memory safety, panics, integer overflow in offset arithmetic,
OOB mmap slice reads, and DoS via crafted sizes/counts. SQL/network/auth are out
of scope for this code.

## Overall assessment

This codec is **well-hardened against malformed input**. The decode path uses a
checked `read_array` helper, `decode_header` validates magic/version/finite-avg,
`open()` re-derives every section size with `checked_mul`/`checked_add` and
rejects any `idx_mmap.len()` mismatch before trusting counts, and
`lookup_postings_generic` bounds-checks offset+length against the postings mmap
and enforces 8-byte alignment. `decode_posting` enforces `count >= 1` (C4). No
`unwrap`/`expect`/`panic!` exists outside `#[cfg(test)]` (clippy `unwrap_used`/
`expect_used = deny` enforces this crate-wide). The pattern faithfully mirrors
the already-reviewed lexical sibling (`index/reader.rs`). No CRITICAL or HIGH
memory-safety issues were found in attacker-reachable paths.

## Issues in Your Changes (BLOCKING)

None.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Binary-search invariants (entry sort order, C1/C2 posting order) are never validated on read** — `crates/rskim-search/src/ast_index/store/reader.rs:251`, `reader.rs:276`, `format.rs:502-530`
**Confidence**: 85%
- Problem: `binary_search_entries` assumes the bigram/trigram entry tables are sorted ascending by key, and the lookup contract (C1: postings sorted ascending by `doc_id`, C2: at most one posting per `doc_id`) is asserted as a *builder* invariant only. With an attacker-controlled `.skidx` that carries a valid (recomputed) CRC32 but unsorted entry keys, unsorted postings, or duplicate `doc_id`s, the reader silently returns wrong/missing results — binary search may report a present key as absent, or hand Wave 3f scoring duplicate/out-of-order postings that violate the documented C1/C2 guarantees its BM25 math relies on.
- Impact: Correctness/integrity, not memory safety (all slice reads remain bounds-checked, so no OOB/panic). The risk is that downstream query/scoring code trusts C1–C5 as hard guarantees per the KNOWLEDGE contract, but the reader does not enforce them — it delegates entirely to the builder. CRC32 does not close this gap because it is not a tamper-evident control.
- Fix: Either (a) document explicitly at the `lookup_*` API and in C1/C2 that ordering/uniqueness is trusted-builder-only and NOT validated against hostile files, so Wave 3f does not over-trust it; or (b) add a lightweight monotonicity check — in `lookup_postings_generic`, assert each decoded `doc_id` is strictly greater than the previous and return `IndexCorrupted` otherwise (cheap, O(n) over a list already being iterated). Option (b) makes C1/C2 a read-side guarantee and is the more defensive choice given the stated attacker-controlled-file threat model (avoids PF-002 — do not defer by reclassifying).

### LOW

**Unchecked offset arithmetic in `file_meta_at` can wrap on 32-bit targets** — `crates/rskim-search/src/ast_index/store/reader.rs:347`
**Confidence**: 80%
- Problem: `meta_start + (file_index as usize) * AST_FILE_META_SIZE` uses plain `*` and `+`. `file_index` is a caller-supplied `u32` not validated against `header.file_count`. On a 32-bit `usize` platform, `(file_index as usize) * 5` overflows for `file_index > usize::MAX / 5`; in release builds this wraps and the subsequent `checked_add(...).filter(|&e| e <= idx_mmap.len())` could then pass for a wrapped-small `offset`, returning the wrong `AstFileMetaEntry` instead of `IndexCorrupted`. In debug builds it panics. On 64-bit (the primary target) this cannot overflow, so impact is limited.
- Fix: Use checked arithmetic to match the rest of the module: `(file_index as usize).checked_mul(AST_FILE_META_SIZE).and_then(|o| meta_start.checked_add(o))...`, mapping `None` to `IndexCorrupted`. Note the lexical sibling (`index/reader.rs:183`) has the same shape, so consider fixing both per ADR-001 (fix noticed issues regardless of scope).

**`lookup_bigram`/`lookup_trigram` slice arithmetic relies on an invariant proven in a different function** — `crates/rskim-search/src/ast_index/store/reader.rs:248`, `reader.rs:271-274`
**Confidence**: 80%
- Problem: `bigram_start + (bigram_count as usize) * AST_BIGRAM_ENTRY_SIZE` (and the trigram analog) use unchecked `*`/`+` and then directly index `&self.idx_mmap[start..end]`. These are sound *today* because `open()` already proved via `checked_mul`/`checked_add` that the same products fit within `idx_mmap.len()`. But the safety is non-local: it depends on `open()` being the only constructor and on that validation never being weakened. A future refactor that adds another constructor, or relaxes the exact-size check, silently turns these into panics/OOB.
- Fix: This is a defense-in-depth/maintainability note, not an exploitable bug in current code. Optionally cache the validated section ranges (`bigram_range: Range<usize>`, etc.) as struct fields computed once in `open()`, and have the lookup methods slice with those — removing the re-derivation and the implicit cross-function invariant. Mirrors lexical sibling, so low urgency.

## Pre-existing Issues (Not Blocking)

**mmap TOCTOU is documented but inherent** — `crates/rskim-search/src/ast_index/store/reader.rs:112-115`, `174-175`
**Confidence**: 90%
- The `unsafe { Mmap::map(...) }` SAFETY comment correctly notes that concurrent truncation/overwrite by another process is UB. This is an inherent mmap constraint shared with the lexical index and is explicitly out of scope per the documented re-index concurrency posture (callers must serialize). No action required; flagged only for completeness. Not introduced as a new risk by this PR.

## Suggestions (Lower Confidence)

- **Header fields are outside CRC coverage** - `crates/rskim-search/src/ast_index/store/format.rs:439`, `reader.rs:153` (Confidence: 70%) — CRC covers only `[48..expected_idx_size]`, so `count`/`size` header fields are protected indirectly by the exact-size equality check rather than the checksum. This is a deliberate, documented design (matches KNOWLEDGE.md) and the size check is an adequate guard; noting only that a self-describing-header tamper that keeps total size constant (e.g. swapping bigram_count/trigram_count where 16B vs 20B differ would change size, but other reshuffles might not) is caught by size but not CRC. Consider folding the header (minus the checksum field) into the CRC in a future format version bump for stronger tamper-evidence.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 1 | 2 |
| Pre-existing | - | - | 0 | 1 |

**Security Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The codec is robust against malformed input: no panics, no OOB, checked arithmetic
throughout the size-validation chain, and `count >= 1` enforced. The one
substantive item is the MEDIUM — C1/C2 ordering/uniqueness is a builder-only
invariant the reader does not enforce against hostile files, which matters because
downstream scoring treats C1–C5 as hard guarantees. Recommend either documenting
the trust boundary at the lookup API or adding the cheap O(n) monotonic-`doc_id`
check in `lookup_postings_generic`. The two LOW items (32-bit overflow in
`file_meta_at`, non-local slice invariant) mirror the lexical sibling and are
defense-in-depth.
