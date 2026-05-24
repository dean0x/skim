# Code Review Summary

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16_1248
**Reviewers**: Security, Architecture, Performance, Complexity, Consistency, Regression, Testing, Reliability, Rust, Dependencies

---

## Merge Recommendation: CHANGES_REQUESTED

**Summary**: The BM25F scoring engine is well-architected and thoroughly tested, but contains **3 blocking HIGH-severity issues** that must be resolved before merge:

1. **Config validation never called** — User-supplied `BM25FConfig` bypasses validation at trust boundaries (multiple reviewers flagged, 90%+ confidence)
2. **Unbounded memory allocation in classifier** — Per-byte vector scales linearly with file size (security + performance + reliability concern, 82-85% confidence across reviewers)
3. **Duplicated field count constant** — `FIELD_COUNT` and `SearchField::count()` can drift independently (architecture + consistency, 82-88% confidence)

These are fixable in <30 minutes with targeted changes. Approval gates on these resolutions.

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| Blocking | 0 | 3 | 2 | - | 5 |
| Should Fix | 0 | 0 | 5 | - | 5 |
| Pre-existing | 0 | 0 | 1 | - | 1 |
| **TOTAL** | **0** | **3** | **8** | **-** | **11** |

---

## Blocking Issues (MUST FIX)

### HIGH-1: BM25FConfig validation missing at trust boundaries (90% confidence)
**Files**: `crates/rskim-search/src/index/reader.rs:153, 265`
**Reviewers**: Reliability (90%), Rust (92%), Security (70% as suggestion)
**Impact**: CRITICAL for security + correctness

The `BM25FConfig` struct has a public `validate()` method that enforces `k1 >= 0`, `boosts >= 0`, `b in [0,1]`, but it is **never called** in either:
- `open_with_config()` (line 153) — accepts a config parameter without validation
- `search()` (line 265-266) — when `query.bm25f_config` overrides the reader's config

A caller can pass negative `k1` or `field_b > 1.0` without triggering an error. This produces mathematically invalid scores:
- Negative `k1`: The formula `tf_weighted / (tf_weighted + k1)` becomes division by a potentially negative denominator → non-finite scores
- `b > 1.0`: Violates invariant, produces unexpectedly high boost multipliers

**Fix**:
```rust
// In open_with_config():
pub fn open_with_config(dir: &Path, config: BM25FConfig) -> Result<Self> {
    config.validate()?;  // ADD THIS LINE
    let mut reader = Self::open(dir)?;
    reader.bm25f_config = config;
    Ok(reader)
}

// In search(), after resolving scoring_config:
let scoring_config: &BM25FConfig = match query.bm25f_config.as_ref() {
    Some(cfg) => {
        cfg.validate()?;  // ADD THIS LINE
        cfg
    }
    None => &self.bm25f_config,
};
```

---

### HIGH-2: Per-byte allocation in classify_source scales O(n) memory (82-85% confidence)
**File**: `crates/rskim-search/src/lexical/classifier.rs:116`
**Reviewers**: Security (82%), Performance (85%), Reliability (82%), Rust (82%)
**Impact**: DoS vulnerability + memory pressure at scale

Line 116: `vec![SearchField::Other; len]` allocates one byte per source byte. For the project's stated `u32::MAX` limit (~4GB), this is a 4GB allocation. Even realistically (100MB files), this is 100MB+ heap allocation.

The CLAUDE.md states files >100MB should be rejected, but this function lacks a guard. An attacker or inadvertent large input could exhaust memory.

**Fix**: Add upper bound at function entry:
```rust
pub fn classify_source(
    source: &str,
    lang: Language,
) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    if source.is_empty() {
        return Ok(Vec::new());
    }
    
    // Guard: reject sources exceeding project limits
    const MAX_CLASSIFY_SIZE: usize = 100 * 1024 * 1024; // 100 MB per CLAUDE.md
    if source.len() > MAX_CLASSIFY_SIZE {
        return Err(crate::SearchError::InvalidQuery(format!(
            "source too large for classification: {} bytes exceeds {}",
            source.len(), MAX_CLASSIFY_SIZE
        )));
    }
    
    // ... rest of function
}
```

---

### HIGH-3: Duplicated field count constant — multiple sources of truth (82-88% confidence)
**Files**: `crates/rskim-search/src/types.rs:98-100`, `crates/rskim-search/src/lexical/config.rs:26`
**Reviewers**: Architecture (85%), Consistency (85%), Complexity (65% as suggestion)
**Impact**: Silent breakage if field count ever changes

Three independent sources define the same value:
1. `SearchField::count()` → hardcoded `8` (types.rs:98)
2. `SearchField::ALL.len()` → 8-element array (types.rs:101+)
3. `pub const FIELD_COUNT: usize = 8` (config.rs:26)

The code also uses raw literal `8` in array declarations across builder.rs and format.rs (12 occurrences). If a 9th field variant is added:
- Array declarations `[u64; 8]`, `[f32; 8]`, etc. remain at 8 → silent memory corruption
- The hardcoded `8` in `count()` must be manually updated
- `FIELD_COUNT` must be manually updated

No compile error warns of the mismatch.

**Fix**: Make `SearchField::count()` derive from `ALL`:
```rust
// In types.rs:
impl SearchField {
    pub const fn count() -> usize {
        Self::ALL.len()  // Derive from the array
    }
}

// In config.rs:
pub const FIELD_COUNT: usize = SearchField::count();

// Then replace all [u64; 8], [f32; 8], [u32; 8] literals with [u64; FIELD_COUNT], etc.
// The compiler will reject inconsistent array sizes at compile time.
```

Alternatively, add a compile-time assertion in config.rs:
```rust
const _: () = {
    const_assert_eq!(FIELD_COUNT, SearchField::ALL.len());
};
```

---

## Should-Fix Issues (SHOULD address before merge, or in immediate follow-up)

### MEDIUM-1: Binary search called per-byte in builder hot path (90% confidence)
**File**: `crates/rskim-search/src/index/builder.rs:161`
**Reviewer**: Performance (90%)
**Impact**: O(n log m) instead of O(n) performance regression

`add_file_classified()` calls `resolve_field(pos, &field_map)` once per 2-byte window. For 100KB file: ~100,000 binary searches. Total: O(file_size * log(field_ranges)).

**Fix**: Replace binary search with linear scan (field_map is pre-sorted):
```rust
let mut range_idx = 0;
for (pos, window) in bytes.windows(2).enumerate() {
    // Advance range_idx to the range containing pos
    while range_idx < field_map.len() && field_map[range_idx].0.end <= pos {
        range_idx += 1;
    }
    let field_id = if range_idx < field_map.len() && field_map[range_idx].0.contains(&pos) {
        field_map[range_idx].1.discriminant()
    } else {
        SearchField::Other.discriminant()
    };
    // ... rest of loop
}
```
Converts to O(file_size) with a single linear pass.

---

### MEDIUM-2: Test does not verify custom config actually changes scoring (85% confidence)
**File**: `crates/rskim-search/src/index/reader_tests.rs:541`
**Reviewer**: Testing (85%)
**Impact**: Test gap — regression in `open_with_config` would pass silently

`test_open_with_config_stores_config` sets `k1 = 2.0` but only checks "search doesn't panic." Doesn't assert custom config actually changes scores vs default.

**Fix**: Compare scores with and without config override:
```rust
let default_reader = NgramIndexReader::open(dir.path()).unwrap();
let default_results = default_reader.search(&SearchQuery::new("main")).unwrap();

let custom_reader = NgramIndexReader::open_with_config(
    dir.path(),
    BM25FConfig { k1: 2.0, ..BM25FConfig::default() }
).unwrap();
let custom_results = custom_reader.search(&SearchQuery::new("main")).unwrap();

assert_ne!(
    custom_results[0].score, default_results[0].score,
    "custom k1 should produce different scores than default"
);
```

---

### MEDIUM-3: `sort_by` instead of `sort_unstable_by` (82-83% confidence)
**File**: `crates/rskim-search/src/index/reader.rs:336`
**Reviewers**: Performance (83%), Consistency (80%), Regression (82%), Rust (82%)
**Impact**: Minor performance regression + inconsistency

Switched from `sort_unstable_by` to `sort_by` (stable sort). Stable sort allocates extra memory; unstable sort is faster. Since FileId tie-breaking already provides deterministic ordering, stability is unnecessary.

**Fix**:
```rust
scored.sort_unstable_by(|a, b| {
    b.1.partial_cmp(&a.1)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.0.cmp(&b.0))
});
```

---

### MEDIUM-4: Missing invariant validation in index decode (80% confidence)
**File**: `crates/rskim-search/src/index/format.rs:330-347`
**Reviewer**: Security (80%)
**Impact**: Corrupted index produces nonsensical scores (not memory unsafe)

`FileMetaEntry` doc comment (lines 145-146) states invariant: `field_lengths[0..8].sum() == doc_length`, marked "upheld by builder; validated by reader." But `decode_file_meta()` does NOT validate this.

Corrupted or malicious index could supply mismatched sums → BM25F produces invalid scores.

**Fix**: Add validation in `decode_file_meta`:
```rust
let sum: u32 = field_lengths.iter().sum();
if sum != doc_length {
    return Err(SearchError::IndexCorrupted(format!(
        "file_meta: field_lengths sum {} != doc_length {}",
        sum, doc_length
    )));
}
```

---

### MEDIUM-5: Position collection before filtering (85% confidence)
**File**: `crates/rskim-search/src/index/reader.rs:293-296`
**Reviewer**: Rust (85%)
**Impact**: Wasted memory for filtered-out documents

Position accumulation happens before language filter (line 313) and doc_id range check. Positions collected for documents that will be skipped.

**Fix**: Move position collection after filtering passes:
```rust
// Accumulate TF first (needed for scoring)
for p in &postings {
    let field_idx = p.field_id as usize;
    if field_idx < FIELD_COUNT {
        tf_per_doc.entry(p.doc_id).or_insert([0.0; FIELD_COUNT])[field_idx] += 1.0;
    }
}

// Later, after scoring and filtering, collect positions only for scored docs:
for p in &postings {
    if doc_scores.contains_key(&p.doc_id) {
        let pos = p.position as usize;
        doc_positions.entry(p.doc_id).or_default().push(pos..pos + 2);
    }
}
```

---

## Pre-existing Issues (Not Blocking)

### MEDIUM-1: Unsafe mmap without SIGBUS handling (65% confidence)
**File**: `crates/rskim-search/src/index/reader.rs:85-86`
**Reviewer**: Security (65%)
**Note**: Pre-existing, acceptable for pre-1.0, worth hardening later

mmap usage acknowledges but does not defend against concurrent file modification (SIGBUS on truncate). This is an inherent design constraint and acceptable for current use case.

---

## Blocked PR Features

The following behaviors are blocked until HIGH issues are fixed:

| Blocked | Reason |
|---------|--------|
| Merging to main | HIGH-1, HIGH-2, HIGH-3 must be resolved |
| Network-facing deployment | HIGH-1 (config validation) must be added |
| Production indexing of large files >10MB | HIGH-2 (classifier memory bound) must be enforced |
| Field count evolution | HIGH-3 (compile-time binding) must be added |

---

## Action Plan

### Phase 1 (Blocking): 15 minutes
1. Add `config.validate()?` calls in `open_with_config()` and `search()` — resolves HIGH-1
2. Add `MAX_CLASSIFY_SIZE` guard in `classify_source()` — resolves HIGH-2
3. Refactor `FIELD_COUNT` to derive from `SearchField::ALL.len()` — resolves HIGH-3
4. Test locally: `cargo test --all-features` — verify no regressions

### Phase 2 (Should-Fix): 10 minutes
5. Replace binary search with linear scan in `add_file_classified()` — improves performance
6. Switch `sort_by` → `sort_unstable_by` in reader — consistency + perf
7. Add config-override test assertion — fills test gap

### Phase 3 (Optional follow-up PR)
8. Add invariant check in `decode_file_meta()` — hardens against corruption
9. Extract `search()` method into sub-functions (accumulate, build_results) — complexity reduction
10. Add unit tests for `resolve_field` binary search edge cases

---

## Summary Statistics

- **Total Issues Found**: 11 (across 10 reviewers)
- **Blocking PRs**: 3 (HIGH severity in your changes)
- **Should-Fix**: 5 (MEDIUM, in code you touched or added)
- **Pre-existing**: 1 (MEDIUM, noted but not blocking)
- **Reviewer Consensus**: 82-92% average confidence on blocking issues (high confidence)
- **Estimated Fix Time**: 25-30 minutes for all blocking + should-fix issues

---

## Quality Assessment

**Strengths**:
- BM25F formula implementation is mathematically correct with proper zero-guards
- Test coverage is comprehensive (197 new tests, all passing)
- New lexical module is well-structured with clear separation of concerns
- Backward compatibility preserved (empty field_map → flat BM25 behavior)
- Error handling uses Result types consistently throughout
- Format v2 design is clean with proper CRC32 checksums
- Performance benchmarks verified (determinism AC4 tested)

**Weaknesses**:
- Config validation method exists but is never called — validation-at-boundaries principle violated
- Memory scaling of classifier not defended against large inputs — DoS risk
- Field count duplicated across three locations with no compile-time binding
- Binary search performance pattern when linear scan is available
- Test for config override is a smoke test rather than behavior verification
- Unnecessary use of stable sort where unstable would suffice

**Overall Quality Score**: 7.5/10 (would be 8.5+ after blocking fixes)

---

**Next Step**: Address the 3 blocking HIGH issues, re-run `cargo test`, then push back for final approval.
