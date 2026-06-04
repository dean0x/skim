# Code Review Summary

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_0025
**PR**: #272 (Wave 3d — AST n-gram on-disk index format, builder & reader)

## Merge Recommendation: CHANGES_REQUESTED

**Rationale**: One HIGH blocking issue (consistency: public submodule visibility) must be resolved before merge. Additionally, 1 HIGH issue in testing (unverified reader bounds-checking path), 2 CRITICAL issues in documentation (contract guarantees not surfaced at API), and several MEDIUM issues across security, architecture, performance, complexity, testing, and reliability each require fixes. The codebase demonstrates strong foundational engineering (no panics on malformed input, checked arithmetic throughout, comprehensive test coverage of happy paths), but the issues listed below—particularly the API consistency gap and the untested critical posting bounds-checking path—are straightforward to resolve and should be fixed before merge to maintain code quality standards and prevent regressions.

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| Blocking | 0 | 1 | 0 | - | 1 |
| Should Fix | 0 | 1 | 10 | 0 | 11 |
| Pre-existing | 0 | 0 | 0 | 1 | 1 |
| **Total** | **0** | **2** | **10** | **1** | **13** |

---

## Blocking Issues

### HIGH: Module Visibility Breaks Encapsulation Parity with Siblings — `ast_index/store/mod.rs:30,32`

**Confidence**: 92% (consistency review)

**Problem**: The new store module declares `pub mod builder;` and `pub mod reader;`, whereas both sibling modules (lexical index and cochange matrix) keep submodules private and expose only types via `pub use`. This widens the public API surface beyond what the documented "mirrors the lexical index pattern" claim states.

**Fix**: Make submodules private to match siblings:
```rust
mod builder;
pub(crate) mod format;
mod reader;

pub use builder::AstIndexBuilder;
pub use format::AstFileMetaEntry;
pub use reader::{AstIndexReader, AstPosting};
```

**Impact**: Public API surface consistency; prevents downstream code from depending on internal paths like `ast_index::store::builder::AstIndexBuilder`.

---

## Critical Issues (Should Fix Before Merge)

### CRITICAL: C3/C5/C6/C7 Contract Guarantees Not Documented at API Surface — `reader.rs:50`, `format.rs:164`

**Confidence**: 92% (documentation review)

**Problem**: The PR specifies seven contract guarantees (C1–C7) in KNOWLEDGE.md and the review brief, but in code rustdoc only C1–C4 are documented. C5 (count is structural TF), C6 (file_meta language recovery), and C7 (Send+Sync) are omitted from the API surface. Additionally, `AstPostingEntry` doc references a `.count` field that does not exist on the format-side `AstBigramEntry`/`AstTrigramEntry` structs, causing confusion about provenance.

**Fix**: 
1. Update `AstPosting` doc to "(C1–C7)" and enumerate C3, C5
2. Add `// C6:` note to `file_meta()` rustdoc pointing at `lang_from_id`
3. Add `/// C7: AstIndexReader is Send + Sync` near the existing prose block
4. Disambiguate `AstPostingEntry` count provenance reference to point at `extract.rs` types

**Impact**: Maintainability; Wave 3f code depends on these contract IDs as the cross-wave coupling vocabulary.

---

## High Priority (Should Fix)

### HIGH: C3 Reader-Level Posting Bounds/Alignment Paths Are Untested — `reader.rs:292-340`, `reader_tests.rs`

**Confidence**: 90% (testing review)

**Problem**: The C3 contract ("Malformed entry → IndexCorrupted") includes four reader-side checks in `lookup_postings_generic`:
- `posting_offset` exceeds usize (line 308)
- slice overflow (312)
- `end > post_mmap.len()` OOB (315)
- `length` not aligned to 8 (321)

**None of these branches is exercised by any test.** The format-level corruption matrix only flips bytes in `.skidx`, which is caught by CRC before any posting offset is dereferenced. The reader's posting-bounds guards are dead-untested.

**Fix**: Add reader tests that construct a valid index, corrupt a bigram/trigram entry's `posting_offset`/`posting_length`, **recompute the CRC** so the lookup path is reached (not the CRC gate), and assert `lookup_bigram(...)` returns `Err(IndexCorrupted)` for each of: OOB offset, misaligned length, offset-exceeds-usize.

**Example**:
```rust
#[test]
fn lookup_rejects_oob_posting_offset() {
    let (_d, mut idx, post) = build_single_file_index();
    // Corrupt bigram posting_offset to a huge value
    let off = AST_HEADER_SIZE + 4;
    idx[off..off + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    // Recompute CRC so lookup path is exercised
    let payload_crc = compute_checksum(&idx[AST_HEADER_SIZE..]);
    idx[38..42].copy_from_slice(&payload_crc.to_le_bytes());
    // Open and assert lookup_bigram(...).is_err() with "out of bounds"
}
```

**Impact**: C3 is the most important guarantee Wave 3f's query engine relies on; regression that drops bounds check would not be caught.

---

## Medium Priority (Should Fix)

### MEDIUM: `AstFileMetaEntry` Leaks On-Disk Encoding Into Public API — `format.rs:184-190`

**Confidence**: 80% (architecture review)

**Problem**: `AstFileMetaEntry` is re-exported at crate root but contains a raw `lang_id: u8` field. To get a usable `Language`, callers must independently call `lang_from_id` — which is `pub(crate)` and unreachable outside the crate. External consumers cannot use the language field.

**Fix**: Add an accessor:
```rust
impl AstFileMetaEntry {
    #[must_use]
    pub fn language(&self) -> Option<rskim_core::Language> {
        crate::index::lang_map::lang_from_id(self.lang_id)
    }
}
```

**Impact**: API usability; hides encoding and makes the C6 contract (language recovery) satisfiable from outside the crate.

---

### MEDIUM: Dead Re-Export `lang_from_id` in format.rs — `format.rs:36-38`

**Confidence**: 85% (architecture review)

**Problem**: `format.rs` re-exports `pub(crate) use crate::index::lang_map::lang_from_id` under `#[allow(unused_imports)]`, but nothing imports it through this path. Tests import directly from `crate::index::lang_map`. The re-export is dead plumbing masked by an `#[allow]` directive, violating the project's zero-warnings policy.

**Fix**: Remove lines 36-38. Tests and Wave 3f will import directly from the canonical path.

**Impact**: Code cleanliness; avoids dead-code accumulation.

---

### MEDIUM: Duplicate Type Names Without Role Distinction — `format.rs:129,147` vs `extract.rs:38,52`

**Confidence**: 88% (consistency review)

**Problem**: `AstBigramEntry` and `AstTrigramEntry` are defined in both `extract.rs` (extraction structs with `ngram` + `count`) and `format.rs` (on-disk lookup-table entries with `key` + `posting_offset` + `posting_length`). Two semantically different structs share one name within the same `ast_index` subtree. Neither sibling has this collision; they use role-descriptive names like `SkidxEntry` or `PairEntry`.

**Fix**: Rename on-disk structs to avoid collision:
```rust
struct AstBigramTableEntry { /* ... */ }
struct AstTrigramTableEntry { /* ... */ }
// or
struct AstBigramSkidxEntry { /* ... */ }
struct AstTrigramSkidxEntry { /* ... */ }
```

**Impact**: Reader clarity; prevents latent shadowing issues with future `use crate::ast_index::AstBigramEntry` imports.

---

### MEDIUM: Redundant `AST_` Prefix on All Format Constants — `format.rs:49-71`

**Confidence**: 85% (consistency review)

**Problem**: The new module prefixes all seven format constants with `AST_` (`AST_FORMAT_VERSION`, `AST_HEADER_SIZE`, etc.), whereas both siblings use bare, module-scoped names (`FORMAT_VERSION`, `HEADER_SIZE`, `SKIDX_MAGIC`). The `AST_` prefix is pure redundancy at every call site (constants are `pub(crate)` and always referenced as `super::format::`).

**Fix**: Drop the `AST_` prefix to match siblings. Rename `AST_SKIDX_MAGIC` to `SKAX_MAGIC` (matching tag-prefix convention with `SKIDX_MAGIC`/`SKCC_MAGIC`):
```rust
const FORMAT_VERSION: u32 = 1;
const HEADER_SIZE: usize = 48;
const BIGRAM_ENTRY_SIZE: usize = 16;
const TRIGRAM_ENTRY_SIZE: usize = 20;
const POSTING_ENTRY_SIZE: usize = 8;
const FILE_META_SIZE: usize = 5;
const SKAX_MAGIC: &[u8; 4] = b"SKAX";
```

**Impact**: Style consistency across the three parallel format modules (lexical, cochange, ast_index).

---

### MEDIUM: Binary-Search Invariants Never Validated on Read — `reader.rs:251,276`, `format.rs:502-530`

**Confidence**: 85% (security review)

**Problem**: `binary_search_entries` assumes bigram/trigram entry tables are sorted ascending by key, and lookup contract C1/C2 (postings sorted by doc_id, at most one per doc_id) is asserted as a builder-only invariant. With an attacker-controlled `.skidx` that carries valid CRC but unsorted entry keys or unsorted postings, the reader silently returns wrong/missing results.

**Fix**: Either:
1. **Document explicitly** at the `lookup_*` API that C1/C2 are trusted-builder-only and NOT validated against hostile files, or
2. **Add lightweight validation** — in `lookup_postings_generic`, assert each decoded `doc_id` is strictly greater than previous and return `IndexCorrupted` otherwise (cheap O(n) check over list already being iterated).

Option (b) is more defensive given the threat model (attacker-controlled files).

**Impact**: Correctness/integrity; downstream code trusts C1–C5 as hard guarantees but reader does not enforce them.

---

### MEDIUM: `build_from_files` Materializes Entire Corpus Before Merging — `builder.rs:256-277`

**Confidence**: 88% (performance review)

**Problem**: The rayon stage collects `Vec<Result<(FileId, Language, AstNgramSet, u32)>>` for ALL files at once, then sequential merge consumes it. Peak memory holds every file's full `AstNgramSet` simultaneously on top of the `bigram_postings`/`trigram_postings` HashMaps being built. For the benched 1000 files this is trivial, but for large monorepos (100K+ files) this is a memory spike with no upper bound.

**Fix**: Two options:
1. **Chunk the parallel stage**: Process files in batches (e.g., 4096), merging each batch before extracting next — caps peak memory at batch_size
2. **Document the peak-memory profile** as a known bound (like KNOWLEDGE.md documents size ratio) and reference #273

Option (a) is pragmatic; option (b) sufficient if Wave 4 re-indexes in chunks.

**Impact**: Memory scaling beyond benched corpus sizes; deferred to Wave 4 if documentation + #273 tracking in place.

---

### MEDIUM: `serialize_index` Has Duplicated Bigram/Trigram Entry Blocks — `builder.rs:345-511`

**Confidence**: 88% (complexity review)

**Problem**: The function spans 166 lines with two structurally identical 25-line blocks (bigram entries, trigram entries) differing only in key type (u32 vs u64), entry constructor, and error-message hex width. The reader already avoided this duplication via `binary_search_entries` and `lookup_postings_generic`, but the builder's serialize path is the one place not unified.

**Fix**: Extract a generic helper:
```rust
fn serialize_entry_table<K: Copy, E>(
    postings_buf: &mut Vec<u8>,
    keys: &[K],
    postings: &HashMap<K, Vec<AstPostingEntry>>,
    make_entry: impl Fn(K, u64, u32) -> E,
) -> Result<Vec<E>> { /* ... */ }
```

Or lighter: factor just the `byte_len`/`u32::try_from` computation into a shared `fn posting_byte_len(list_len: usize) -> Result<u32>`.

**Impact**: Maintainability; changes to posting-serialization contract (e.g., #273 compression) must be made in two places without this.

---

### MEDIUM: Atomic-Write Crash-Safety Claim Overstates Guarantee — `builder.rs:1-9,513-531`

**Confidence**: 85% (reliability review)

**Problem**: Module doc claims crash safety: "A partial write leaves no `.skidx`". Implementation does `write_all + sync_all + persist` but **does not fsync the containing directory**. On POSIX, the rename is a directory-metadata operation; without directory fsync, a crash after `persist()` can lose the rename — and the relative ordering of the two renames (`.skpost` before `.skidx`) is not guaranteed durable.

**Fix**: Either:
1. **Add directory fsync** after both `persist` calls to guarantee rename durability, or
2. **Soften the doc comment** to state that rename durability depends on the filesystem and is not guaranteed across power loss without directory fsync

Option (b) is minimal and keeps parity with cochange sibling. Option (a) is stronger. Recommend (b) now + tracked follow-up for (a) to stay consistent with cochange.

**Impact**: Power-loss safety; reader's size + CRC checks still catch most torn states as `IndexCorrupted` rather than UB, so degrades to "rebuild required" rather than silent corruption.

---

### MEDIUM: Re-Index Read/Write Interleave Can Yield Silent Wrong Results — `builder.rs:11-15,336-338`

**Confidence**: 82% (reliability review)

**Problem**: On re-index, `.skpost` is `persist`ed before `.skidx`. A concurrent reader that opened with OLD `.skidx` but reads NEW `.skpost` uses stale offsets/lengths against new postings. CRC32 covers only `.skidx`, so it passes. Reader re-validates bounds, but stale offset can land in-bounds and decode into structurally-valid-but-wrong postings — silent wrong results.

**Fix**: Documented as a precondition ("NOT concurrency-safe ... callers MUST serialize re-index"). No code change required. If hardening desired (track as follow-up): add generation/build-id to both file headers and have reader verify they match — turns silent-wrong-result into `IndexCorrupted`. Reserved 6 bytes available in header.

**Impact**: Silent wrong query results during re-index race; documented precondition but highest-impact reliability caveat relying entirely on caller discipline.

---

### MEDIUM: C1 Sorted/Unique Posting Guarantee Has No Negative Test — `builder_tests.rs:186-216`

**Confidence**: 82% (testing review)

**Problem**: C1 is verified only on happy path in `a2_posting_merge_sorted_unique_doc_ids`, where files are added in already-ascending FileId order. The builder sorts via `sort_unstable_by_key`, but no test exercises the sort observably — FileIds are monotonic by construction, so sort is effectively untested for any input that would fail without it.

**Fix**: Either:
1. Add property-style check: `postings.windows(2).all(|w| w[0].doc_id < w[1].doc_id)` over larger merged corpus, or
2. Document in test that doc_id ordering follows FileId monotonicity by construction and sort is defense-in-depth

Also fix mislabeled `// C2` comment at `builder_tests.rs:213` (it is the C1 uniqueness clause).

**Impact**: False confidence that sort is validated; low real risk because FileIds are monotonic by construction.

---

### MEDIUM: `read_array` Error Message Reports Total Buffer Length Instead of Available Bytes — `format.rs:200-212`

**Confidence**: 88% (rust review)

**Problem**: When slice is shorter than `start+N`, error reports `got {data.len()}` (whole buffer), but actual shortfall is at `start`. For truncated header this reads e.g. "need 8 bytes at offset 18, got 47" which is confusing (47 > 8). Error path is correct and never panics; this is diagnostics-quality only.

**Fix**: Report `data.len().saturating_sub(start)` (available bytes from offset) instead of `data.len()`, or include both: `got {} bytes available from offset {start}`.

**Impact**: Diagnostics clarity; easier debugging of malformed input.

---

## Medium Priority Suggestions (Lower Confidence)

- **A16 size-ratio bound `< 3.0` is loose but not a tautology** — `reader_tests.rs:432-508` (88%) — Consider tightening to `< 1.8` (≈1.45x margin over 1.23x baseline) to catch moderate regressions while tolerating drift, or assert both lower and upper bounds (`0.5 < ratio < 1.8`)

- **Stale comments reference obsolete `#[ignore]` / 5% A16 design** — `reader_tests.rs:395`, `benches/ast_index_bench.rs:8` (95%) — Update to describe active `< 3.0x` bound (ADR-001: fix noticed issues immediately)

- **Unchecked `as usize` multiplications in lookup hot paths** — `reader.rs:248,271-273,344-347` (70%) — Provably safe today because `open()` validates counts, but add a comment noting the invariant so future refactors do not reintroduce panic risk

---

## Pre-existing Issues (Not Blocking)

- **mmap TOCTOU is documented but inherent** — `reader.rs:112-115,174-175` (90%) — Concurrent file truncation/overwrite is UB. This is an inherent mmap constraint shared with lexical index and explicitly out of scope per documented re-index concurrency posture. Not introduced as new risk.

---

## Summary by Reviewer Focus

| Focus | Blocking | Should Fix | Pre-existing | Score | Status |
|-------|----------|-----------|--------------|-------|--------|
| Security | 0 | 1 MEDIUM | 0 | 9/10 | APPROVED_WITH_CONDITIONS |
| Architecture | 0 | 2 MEDIUM | 0 | 8/10 | APPROVED_WITH_CONDITIONS |
| Performance | 0 | 2 MEDIUM | 0 | 8/10 | APPROVED |
| Complexity | 0 | 1 MEDIUM | 0 | 8/10 | APPROVED_WITH_CONDITIONS |
| Consistency | 1 HIGH | 2 MEDIUM | 0 | 8/10 | APPROVED_WITH_CONDITIONS |
| Regression | 0 | 0 | 0 | 10/10 | APPROVED |
| Testing | 0 | 1 HIGH, 2 MEDIUM | 0 | 8/10 | APPROVED_WITH_CONDITIONS |
| Reliability | 0 | 2 MEDIUM | 1 LOW | 9/10 | APPROVED_WITH_CONDITIONS |
| Rust | 0 | 1 MEDIUM | 0 | 9/10 | APPROVED |
| Dependencies | 0 | 0 | 0 | 10/10 | APPROVED |
| Documentation | 0 | 2 MEDIUM | 0 | 9/10 | APPROVED_WITH_CONDITIONS |

---

## Convergence Status

**Cycle**: 1 (first review)
**Prior Resolution**: (none) — first review cycle for this branch
**Assessment**: Initial comprehensive review. No prior false positives to measure against.

---

## Action Plan

**Before Merge (Blocking + CRITICAL)**:
1. **Fix HIGH consistency issue**: Make `builder`/`reader` submodules private, align with siblings
2. **Fix CRITICAL documentation**: Surface C3/C5/C6/C7 in rustdoc, disambiguate `AstPostingEntry.count` reference
3. **Fix HIGH testing issue**: Add CRC-recomputing reader corruption tests for posting bounds/alignment (OOB offset, misaligned length, offset-exceeds-usize)

**Before Merge (High Priority MEDIUM)**:
4. Add `language()` accessor to `AstFileMetaEntry` or demote to `pub(crate)`
5. Fix binary-search invariant validation: either document trust boundary or add O(n) doc_id monotonicity check
6. Rename on-disk entry structs to avoid collision with extraction-side types
7. Drop redundant `AST_` prefix on format constants, use `SKAX_MAGIC` convention
8. Remove dead `lang_from_id` re-export in `format.rs`

**Optional/Tracked Follow-ups (Wave 4 / Issue Tickets)**:
9. Implement directory fsync for atomic write durability (vs softening doc claim) — #273
10. Document peak-memory profile of `build_from_files` or implement batch-chunking
11. Add generation marker to header reserved bytes to detect re-index interleave race (vs doc-as-precondition)

**Documentation Fixes (ADR-001: Fix Noticed Issues Immediately)**:
12. Update stale `#[ignore]`/5% comments to describe active `< 3.0x` A16 bound
13. Factor `serialize_index` bigram/trigram duplication into shared helper

---

## Strengths (No Action Required)

- **Zero-panic design**: No `unwrap`/`expect`/`panic!` outside `#[cfg(test)]`; malformed input returns `Err(IndexCorrupted)`
- **Comprehensive checked arithmetic**: Offset/length computations use `checked_mul`/`checked_add`/`try_from` consistently
- **Strong test coverage**: A1–A14 acceptance criteria tested, corruption matrix thorough, determinism verified, empty-postings guard tested
- **Clean layering**: format.rs pure codec (no std::fs), builder owns writes, reader owns mmap reads, no cycles
- **Faithful mirroring**: Test wiring, error variants, atomic-write strategy, cast patterns match siblings
- **Performance headroom**: A15 ~12.8 ms vs 10 s target (3x faster); A16 guard at <3.0x reasonable
- **PF-004 adherence**: `node_count` uses `u32::try_from`, not `as u32`; no silent narrowing
- **Send+Sync verified**: Compile-time structural test, not manual `unsafe impl`

---

## Overall Assessment

This PR demonstrates strong foundational engineering: careful input validation, no panics on malformed input, comprehensive checked arithmetic, thorough test coverage, and clean architectural layering. The issues identified are real but all resolvable through mechanical fixes (visibility alignment, doc surfacing, adding missing tests, renaming for clarity) with no changes to core logic or behavior. The codebase is ready for a focused revision cycle addressing the blocking consistency issue, critical documentation gaps, and high-priority test coverage before merge.

**Recommendation**: **CHANGES_REQUESTED** — Fix the 1 HIGH blocking issue + 1 CRITICAL documentation issue + 1 HIGH testing issue, then targeted MEDIUM fixes across architecture/performance/complexity/consistency. Estimated effort: 3–4 hours. Full merge readiness expected after revision.
