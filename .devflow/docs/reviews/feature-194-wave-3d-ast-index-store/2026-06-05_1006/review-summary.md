# Code Review Summary

**Branch**: feature/194-wave-3d-ast-index-store -> main
**Date**: 2026-06-05_1006
**Cycle**: 2

## Merge Recommendation: APPROVED_WITH_CONDITIONS

The Wave 3d AST index store is well-architected and thoroughly tested. All 15 cycle-1 fixes remain intact with no regressions (regression score 10/10). This cycle 2 review identifies 8 new MEDIUM should-fix issues and 0 blocking issues. The issues cluster around three themes: (1) **unchecked table-offset arithmetic consolidated across multiple review domains**, (2) **module visibility parity with siblings**, and (3) **test coverage of public-API boundaries**. All are categorized as should-fix (not blocking) and should be resolved in-place before merge per ADR-001 ("if you see something, do something").

## Issue Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| Blocking | 0 | 0 | 0 | - | 0 |
| Should Fix | - | 0 | 8 | - | 8 |
| Pre-existing | - | - | 1 | 0 | 1 |

## Blocking Issues
None.

## Should-Fix Issues (MEDIUM Severity)

### 1. Duplicated Table-Offset Arithmetic with Inconsistent Overflow Posture

**Location**: `reader.rs:131-150`, `reader.rs:244-247`, `reader.rs:276-277`, `reader.rs:299-300`
**Confidence**: 88%
**Domain(s)**: Complexity, Security, Architecture (convergent finding across three reviewers)

The same "header + bigram_bytes + trigram_bytes [+ meta]" section-offset computation appears four times with inconsistent styles:
- `open()` uses `checked_mul`/`checked_add` and rejects overflow
- `file_meta()`, `lookup_bigram()`, `lookup_trigram()` recompute identically with bare `*` and `+`

**Why it matters**: The layout math is the most invariant-critical arithmetic in the module. It must stay in lockstep across all sites. A future layout change (e.g., inserting a section) could be applied inconsistently. The unchecked sites are provably safe today (because `open()` has already validated `expected_idx_size == idx_mmap.len()`), but the inconsistent style invites a future reviewer to re-flag or a maintainer to introduce a bug.

**Fix**: Extract three private accessor methods computing section boundaries from `self.header`:
```rust
fn bigram_table_range(&self) -> std::ops::Range<usize> {
    let start = HEADER_SIZE;
    start..start + (self.header.bigram_count as usize) * BIGRAM_ENTRY_SIZE
}
fn trigram_table_range(&self) -> std::ops::Range<usize> { /* ... */ }
fn meta_start(&self) -> usize { /* ... */ }
```
Then `lookup_bigram`/`lookup_trigram`/`file_meta` slice via these. Eliminates the checked-vs-unchecked divergence and creates one source of truth for the layout math.

---

### 2. Redundant Per-Posting-List Sort in `build()`

**Location**: `builder.rs:389-396`
**Confidence**: 88%
**Domain**: Performance

Postings are already sorted by construction (sequential FileId invariant enforced in `add_file_ngrams`), yet `build()` sorts every bigram and trigram posting list by `doc_id` anyway.

**Why it matters**: For a dense AST corpus (~161 distinct bigram keys, ~1000 files), this runs ~161K elements through `sort_unstable_by_key` on every build, doing work the construction order already guarantees. `sort_unstable` on pre-sorted input is near-O(n) but still incurs comparator calls and element scans per list. Scales O(distinct_keys × files) unnecessarily.

**Fix**: Replace with a `debug_assert!` documenting the invariant:
```rust
#[cfg(debug_assertions)]
for list in self.bigram_postings.values().chain(self.trigram_postings.values()) {
    debug_assert!(
        list.windows(2).all(|w| w[0].doc_id < w[1].doc_id),
        "postings must already be ascending by construction (sequential FileId invariant)"
    );
}
```

---

### 3. Builder Does Not Assert `count >= 1` at Public Boundary

**Location**: `builder.rs:235-254`
**Confidence**: 82%
**Domain**: Reliability

`add_file_ngrams` accepts an arbitrary `&AstNgramSet` whose entries have public fields (`extract.rs:38-61`). An external caller can construct an entry with `count == 0` and pass it in. The builder copies `count` into the posting list with no validation. The reader enforces the invariant at read time (rejects `count == 0` via `decode_posting`), so a zero-count index builds successfully, but every lookup then fails with `IndexCorrupted`. The invariant is enforced on read but not asserted on write — a boundary-validation asymmetry.

**Why it matters**: Violates the project rule "Assert preconditions and invariants in production code — not just tests" and "Validate at boundaries." This creates a latent corrupt-index condition discoverable only at query time, not at build time. The production path (`extract_ast_ngrams`) cannot produce `count == 0`, so this is reachable only via the public API — but the API is public and supported.

**Fix**: Assert the invariant at the boundary in the bigram/trigram merge loops:
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
Mirror for trigrams. Converts deferred query-time failure into immediate, located build-time error.

---

### 4. Module Visibility Over-Broad on Internal Submodules

**Location**: `ast_index/mod.rs:35` (`pub mod store;`), `ast_index/store/mod.rs:31` (`pub(crate) mod format;`)
**Confidence**: 85%
**Domain**: Consistency

The established sibling convention (`cochange/`, `index/`) is that internal submodules are private `mod` with public items surfaced only through `pub use`. The store tree breaks this in two places:
- `store/mod.rs` declares `format` as `pub(crate) mod format` (siblings use private `mod`)
- `ast_index/mod.rs` declares `store` as `pub mod store` (but it is an internal submodule, not a crate-level module)

**Why it matters**: Widens the crate's API surface beyond the sibling baseline and beyond what cycle-1 established for `builder`/`reader`. A future caller could depend on `ast_index::store::format::…` internals, defeating the encapsulation cycle-1 was aiming for. `store` is internal to `ast_index` (analogous to `builder`/`reader`/`format`), not a top-level crate module.

**Fix**: 
- `crates/rskim-search/src/ast_index/store/mod.rs:31` → `mod format;` (remove `pub(crate)`)
- `crates/rskim-search/src/ast_index/mod.rs:35` → `mod store;` (remove `pub`)

Both compile unchanged because all external access already goes through `pub use`.

---

### 5. C6 Language-Recovery Contract Verified Through Wrong API

**Location**: `reader_tests.rs:142-165` (`a5_lang_recovery_from_file_meta`)
**Confidence**: 90%
**Domain**: Testing

Cycle-1 added `AstFileMetaEntry::language() -> Option<Language>` to "satisfy C6 externally" (resolution-summary.md). The C6 rustdoc contract (reader.rs:58-59, format.rs:193-198) documents the recovery path as `file_meta(i).language()`. However, the only C6 test calls the lower-level `lang_from_id(meta.lang_id)` directly and never invokes the `language()` accessor. The public-API method that Wave 3f will call is therefore untested, and its documented `None` future-compat branch (unrecognised `lang_id`) is never exercised.

**Why it matters**: The public contract is not genuinely tested. The underlying `lang_from_id` path is tested, but the accessor that callers will use is silent, making the C6 guarantee apparent rather than actual.

**Fix**: Assert through the contract API and cover the `None` path:
```rust
// Replace the lang_from_id calls with the C6 accessor:
assert_eq!(reader.file_meta(0).unwrap().language(), Some(Language::Rust));

// Add a None-path test using a hand-written file_meta with out-of-range lang_id:
let meta = AstFileMetaEntry { lang_id: 250, node_count: 1 };
assert_eq!(meta.language(), None, "unrecognised lang_id must map to None (C6 future-compat)");
```

---

### 6. No Multi-Language Round-Trip Coverage Beyond Three IDs

**Location**: `reader_tests.rs:45-79` (`build_3_file_index`) + `builder_tests.rs:308-331`
**Confidence**: 82%
**Domain**: Testing

The 3-file fixture mixes Rust/Python/Go (good spot-check), but no test builds an index spanning 4+ languages and asserts every file's `lang_id` round-trips correctly and postings are independently retrievable. The multi-language surface (C6 across the full `lang_to_id`/`lang_from_id` table) is only spot-checked on three IDs. Since `lang_map` was widened to `pub(crate)` for this PR and is shared with the lexical index, a regression in mapping for a less-common language (Kotlin, Swift, C#) would go uncaught.

**Why it matters**: C6 breadth gap. Low cost to close; higher confidence that language-recovery works across the full supported set.

**Fix**: Add a parametrized round-trip over a representative span of languages asserting `file_meta(i).language() == Some(expected_lang)` for each.

---

### 7. `atomic_write` Triplicated Across Three Builders

**Location**: `ast_index/store/builder.rs:561`, `index/builder.rs:87`, `cochange/builder.rs:331`
**Confidence**: 82%
**Domain**: Architecture

`fn atomic_write(dir, path, data)` now exists as a near-identical copy in three sibling builders. The AST copy adds an extra step (Unix `set_permissions(0o644)`) that the lexical index copy does not have — the three copies have already begun to diverge. This is a DRY/SRP smell: crash-safe atomic file replacement is one concern (one reason to change) but is owned in three places.

**Why it matters**: The PR doc and module rustdoc explicitly defer a directory fsync "as a follow-up" in all three builders. With three copies, that follow-up is three edits. The lexical copy already lacks the `0o644` step — exactly the drift this duplication invites. Per ADR-001, this should be surfaced for a fix-now decision.

**Fix**: Extract a single `pub(crate) fn atomic_write(dir, path, data, mode: Option<u32>)` (or an `AtomicFile` helper) into a shared module (e.g., `crates/rskim-search/src/io_util/` or extend `crate::index` to house shared primitives), and have all three builders call it. This is Wave-4 scope (touches the lexical sibling) but the duplication is introduced *in this PR* by the AST builder, so it is in-scope for a consolidate-now-or-track decision.

---

### 8. Internal Submodules Over-Abstract or Have Asymmetric Overflow Discipline

**Location**: `builder.rs:263-265`, `builder.rs:434-445`
**Confidence**: 62-70% (lower-confidence; optional improvements)
**Domain**: Reliability

Two optional hygiene findings:
- **Unchecked accumulator arithmetic** (`total_node_count`, `total_distinct_bigrams`, `total_distinct_trigrams` use `+=` while `file_count` uses `checked_add`). Overflow requires ~2^32 files or pathological per-file counts and is physically unreachable (file_count itself caps at u32::MAX), so this is a consistency-of-discipline nit rather than a real outage.
- **Capacity-hint multiply not checked** (`total_posting_bytes_est` computes `(... ).sum::<usize>() * POSTING_ENTRY_SIZE` with unchecked `*` to size `Vec::with_capacity`). Only a capacity hint; actual byte writes use `checked_mul`. Worst case is a debug-build panic on an absurd corpus.

**Why it matters**: Consistency with the rest of the builder's overflow discipline.

**Fix** (optional): Use `saturating_add` / `saturating_mul` for parity with `file_count` use of `checked_add`. Not merge-blocking; improvement if you want full consistency.

---

## Pre-existing Issues (Not Blocking)

### mmap TOCTOU: Concurrent Truncation/Overwrite of Mapped File is UB

**Location**: `reader.rs:123-126`, `reader.rs:183-184`
**Confidence**: 90%
**Classification**: Inherent, non-actionable constraint

**Note**: This is a documented, inherent property of mmap-based readers shared with the lexical sibling. A local actor who can write to `.skim/` can crash a reader process via SIGBUS. This is acceptable for a library; matches the documented posture. Tracked as a Wave-4 follow-up if hardening is ever desired. Not a regression; not actionable for this PR.

---

## Convergence Status

**Cycle**: 2 (second review)
**Prior Resolution**: Available (cycle 1 fixed 15 issues, 1 false positive, 0 deferred)
**Prior FP Ratio**: 6.25% (1 of 16 findings)
**Assessment**: Converging — most issues resolved. No convergence warning (FP ratio well below 70% threshold). All 15 cycle-1 fixes verified intact with no regressions (regression score 10/10, rust score 10/10, dependencies score 10/10).

**Cross-Cycle Verification**:
- ✅ Module visibility fixes (cycle 1, issues #1, #3) verified present and correct
- ✅ C3/C5/C6/C7 contract rustdoc (cycle 1, issue #2) verified present and accurate
- ✅ `serialize_entry_table` extraction (cycle 1, issue #8) verified present and correctly removes duplication
- ✅ O(n) doc_id monotonicity check (cycle 1) verified present and enforced (`reader.rs:362-374`)
- ✅ `u32::try_from` node_count narrowing (cycle 1, PF-004 analog) verified present (`builder.rs:294-300, 340-346`)
- ✅ False positive (re-index concurrency precondition) verified still documented (`builder.rs:16-20`); not re-introduced

**Reviewer Convergence**: Three independent reviewers (security, complexity, architecture) flagged the same `reader.rs` offset-arithmetic cluster (#1 above) without mutual coordination — a strong signal of genuine maintainability concern.

---

## Action Plan

1. **Consolidate table-offset arithmetic** (`reader.rs`) — Extract three accessor methods; used by `lookup_bigram`, `lookup_trigram`, `file_meta`, and `open()`. Highest priority due to convergence across three domains.
2. **Remove redundant posting sorts** (`builder.rs:389-396`) — Replace with `debug_assert!` documenting the sequential-FileId invariant.
3. **Assert `count >= 1` at build boundary** (`builder.rs:235-254`) — Add guards in bigram/trigram merge loops; fail fast at build time, not at query time.
4. **Narrow module visibility** (`ast_index/mod.rs:35`, `store/mod.rs:31`) — Change `pub mod store` → `mod store` and `pub(crate) mod format` → `mod format`; all external access goes through `pub use` unchanged.
5. **Enhance C6 test coverage** (`reader_tests.rs`) — Test the public `language()` accessor directly and cover the `None` branch; add parametrized multi-language round-trip.
6. **Track `atomic_write` consolidation** — Raise a Wave-4 issue for extraction into shared module; for now, surface the duplication and let the user decide fix-now-or-track per ADR-001.
7. **Optional**: Align accumulator overflow discipline (`builder.rs:263-265`, `:434-445`) — Use `saturating_add`/`saturating_mul` for consistency if desired.

---

## Summary

**Cycle 1 Status**: 15 issues fixed, 1 false positive, 0 deferred; no regressions.
**Cycle 2 Status**: 8 new MEDIUM should-fix issues identified (0 blocking, 0 high, 0 critical). All are reasonable in-place fixes; no fundamental architectural concerns.

The Wave 3d AST index store is well-designed, thoroughly tested, and ready for merge **once the 8 should-fix issues are resolved**. The issues are clustered and actionable: offset arithmetic consolidation, redundant sort removal, boundary validation strengthening, visibility narrowing, and test coverage completeness.

**Recommendation**: APPROVED_WITH_CONDITIONS — Fix the 8 should-fix issues (prioritizing the offset-arithmetic consolidation per convergence signal) before merging to main.
