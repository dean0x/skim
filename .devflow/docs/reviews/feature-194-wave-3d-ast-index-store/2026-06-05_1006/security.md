# Security Review Report

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_1006
**PR**: #272 (Wave 3d AST index store)
**Focus**: security (untrusted on-disk binary parsing, mmap)
**Cycle**: 2 (cycle 1 fixed 15 issues / 1 FP; this pass targets new issues + regressions)

## Scope & Threat Model

Reviewed the binary-parsing surface that consumes attacker-controllable on-disk
files (`ast_index.skidx` / `ast_index.skpost`, mmap'd):
`store/format.rs`, `store/reader.rs`, `store/builder.rs`, plus `Cargo.toml`.
Threat actor: a malicious or corrupt index file placed in `.skim/`. Attack
goals considered: OOB read / memory unsafety, integer overflow leading to bad
slice bounds, panic-driven DoS, CRC bypass, and the mmap TOCTOU posture.

**Overall assessment: the parsing surface is well-hardened.** The decode path is
bounds-checked via a single `read_array` choke point using `checked_add` + slice
`get`, `open()` establishes a strong size invariant with `checked_mul`/`checked_add`,
and all post-`open()` arithmetic is provably safe under that invariant. No new
CRITICAL or HIGH security regression was introduced by the cycle-1 resolution
commits.

## Issues in Your Changes (BLOCKING)

### CRITICAL
None.

### HIGH
None.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Unchecked multiply/index in hot lookup paths relies on an undocumented `open()` invariant** — `reader.rs:244-247`, `reader.rs:277`, `reader.rs:299-301`
**Confidence**: 80%
- Problem: `file_meta`, `lookup_bigram`, and `lookup_trigram` compute slice
  bounds with raw `(self.header.bigram_count as usize) * BIGRAM_ENTRY_SIZE` (and
  trigram/meta equivalents) and then index the mmap with unchecked range syntax,
  e.g. `&self.idx_mmap[bigram_start..bigram_end]` (line 278) and
  `&self.idx_mmap[trigram_start..trigram_end]` (line 301). These are memory-safe
  *only* because `open()` (reader.rs:131-157) already proved via `checked_mul`/
  `checked_add` that `bigram_bytes + trigram_bytes + meta_bytes + HEADER_SIZE ==
  idx_mmap.len()`. The safety of the lookup methods is therefore a non-local
  invariant with no assertion or comment at the use sites. A future refactor that
  adds a second `AstIndexReader` constructor, or relaxes the `open()` size check,
  would silently turn these into panics (index-out-of-range) or, worse, logic on
  truncated data. This is defense-in-depth, not a live exploit: today `open()` is
  the only constructor and fields are private, so the invariant holds.
- Impact: No current vulnerability. Latent panic-DoS / correctness risk if the
  `open()` invariant is ever weakened. The asymmetry is notable: the posting-list
  path in `lookup_postings_generic` (reader.rs:340-357) is fully `checked_*` and
  bounds-validated against the *separate* `.skpost` mmap (whose size is validated
  independently), yet the `.skidx` table slices skip the same rigor because they
  trust `open()`.
- Fix: Add a `debug_assert!` documenting the invariant at each use site, e.g.
  `debug_assert!(bigram_end <= self.idx_mmap.len())` before the slice, and a one-
  line comment ("bounds guaranteed by open() size validation") so the coupling is
  explicit and a regression trips in debug/test builds. `debug_assert!` for module-
  boundary invariants matches the repo's Rust guidance and keeps the hot path
  allocation-free. Optionally precompute the three section ranges once in `open()`
  and store them on the struct, eliminating the recomputation entirely.

## Pre-existing Issues (Not Blocking)

**mmap TOCTOU: concurrent truncation/overwrite of a mapped file is UB (SIGBUS)** — `reader.rs:123-126`, `reader.rs:183-184`
**Confidence**: 90%
- Problem: `unsafe { Mmap::map(&idx_file) }` maps a file that another process can
  truncate or overwrite after validation. Accessing a shrunk mapping faults
  (SIGBUS) rather than returning a `Result`. This is explicitly documented in the
  SAFETY comments and is inherent to all mmap-based readers in this codebase
  (shared with the lexical sibling). It was classified in cycle 1 as an inherent,
  non-actionable constraint.
- Impact: A local actor who can write to `.skim/` can crash a reader process. Not
  a memory-disclosure vector (mmap shrink faults rather than reads neighbor data),
  and the trust boundary is the local filesystem, which already implies write
  access. Acceptable for a library; matches the documented posture.
- Fix: None required for this PR. If hardening is ever desired, copy the validated
  region out of the mmap (defeats the zero-copy goal) or catch SIGBUS — both out
  of scope. Track with the existing Wave-4 follow-up.

## Cross-Cycle Notes

Verified against cycle-1 `resolution-summary.md`. The cycle-1 fixes are present
and intact in current code, with no regressions introduced:
- O(n) doc_id monotonicity guard — present (`reader.rs:362-374`), strict-ascending
  check rejects CRC-valid-but-unsorted hostile postings (defends C1).
- `decode_posting` `count == 0` rejection — present (`format.rs:420-424`).
- `read_array` reports bytes-available-from-offset — present (`format.rs:227-240`).
- `u32::try_from` node_count narrowing (no silent `as u32`) — present
  (`builder.rs:294-300`, `builder.rs:340-346`; avoids PF-004 analogue).
- `serialize_entry_table` posting-length overflow guard — present
  (`builder.rs:134-144`, `checked_mul` + `u32::try_from`).
The cycle-1 false positive (re-index interleave) was not re-raised; the
documented precondition (builder.rs:16-20) remains accurate.

`Cargo.toml`: only adds workspace `rayon` (already vendored elsewhere) and a bench
target — no new external supply-chain surface, no version-pinning concern.

## Suggestions (Lower Confidence)

- **Header count/avg fields are outside the CRC32 envelope** — `format.rs:467-469`, `reader.rs:159-169` (Confidence: 70%) — The checksum covers only `idx_mmap[HEADER_SIZE..]`; the 48-byte header (magic, counts, avgs, postings_file_size) is not checksum-protected. This is safe because every header field is independently validated (size cross-check at reader.rs:152, avg finite/non-negative at format.rs:295-313, magic/version at format.rs:280-292), but a bit-flip in `bigram_count` that still satisfies the size equation could mis-partition the tables without tripping CRC. Consider folding the encoded header (minus the checksum field) into the CRC in a future format-version bump for full integrity coverage.
- **`postings_file_size == 0` short-circuits the `.skpost` CRC/existence entirely** — `reader.rs:173` (Confidence: 60%) — When the header claims zero postings, `.skpost` is never opened or validated; a lookup with a non-zero `posting_length` entry then returns `Ok(vec![])` (reader.rs:335-338) rather than `IndexCorrupted`. Benign (no unsafe read), but a malformed index with non-empty entry tables yet `postings_file_size == 0` is silently treated as empty rather than flagged corrupt. Low priority.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 1 | 0 |

**Security Score**: 9/10
**Recommendation**: APPROVED

The binary parser is conservatively written: a single bounds-checked `read_array`
primitive, `checked_*` arithmetic throughout `open()` and the posting path,
explicit CRC validation, count>=1 and strict-ascending doc_id enforcement against
hostile-but-CRC-valid input, and no `unwrap`/`expect`/`panic!` outside tests
(enforced by crate-level clippy `deny`). The one MEDIUM is a defense-in-depth /
documentation hardening item on a latent (not live) invariant, suitable to fix
in-place but not merge-blocking. The mmap TOCTOU posture is inherent and already
accepted.
