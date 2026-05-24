---
title: "Wave 1a: Sparse N-gram Extraction Algorithm"
issue: "#177"
status: planned
created: 2026-05-13
---

# Wave 1a: Sparse N-gram Extraction Algorithm

## Goal

Implement core sparse n-gram extraction in `crates/rskim-search/src/ngram.rs`: a new `Ngram` type and two extraction functions (document + query) that power the lexical search index.

## Scope

- **In scope**: Bigram extraction, border-weighted selectivity, covering-set query extraction, `Ngram` type
- **Out of scope**: Variable-length n-grams (2-5 bytes), index construction, BM25F scoring, mmap format

## Design Decisions

### D1: Ngram type — u16 newtype (bigrams only)

The existing weight table (`BIGRAM_WEIGHTS`) uses `(u16, f32)` tuples with encoding `(byte1 << 8) | byte2`. Variable-length n-grams (2-5 bytes) have no algorithm, no weight data, and no prior art in the codebase. Wave 1a implements bigrams only.

```rust
#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Ngram(pub u16);
```

Follows the `FileId(pub u32)` pattern from `types.rs`. Inner field is `pub` for index builder efficiency. `#[repr(transparent)]` ensures zero-cost ABI. Does NOT derive Serialize/Deserialize (internal index type, no JSON boundary in Wave 1a).

### D2: Border weight multiplier — 3.5x (from research)

Issue #177 says 2x, but `rskim-research/src/validate.rs:20` uses `const BORDER_MULTIPLIER: f64 = 3.5` which was empirically validated. The research validation suite was calibrated against 3.5x. Using the validated value.

```rust
pub const BORDER_MULTIPLIER: f32 = 3.5;
```

### D3: Aggregation — max-weight deduplication

When the same bigram appears at multiple byte positions, keep the occurrence with the highest weight. Border positions get 3.5x multiplier, so border occurrences naturally win. This is semantically correct for document fingerprinting: "what bigrams exist and how selective are they?"

### D4: Return type — infallible Vec

Byte scanning over `&str` cannot fail (Rust guarantees valid UTF-8, algorithm is pure byte iteration). Returning `Result` would add ceremony with no value. Functions are `#[must_use]`.

### D5: Two-tier API — `_with_weights` variant

Public convenience functions delegate to `_with_weights` variants accepting explicit weight tables. Enables:
- Unit tests with small synthetic tables (no coupling to 16K production table)
- Future per-field weight tables (Wave 1c BM25F)

### D6: Module placement — flat `src/ngram.rs`

Not `lexical/ngram.rs`. Creating a subdirectory for one file is premature. Restructure when Wave 1b+ adds more files.

### D7: Intentional code duplication from rskim-research

Port `encode_bigram`, `decode_bigram`, `token_border_ranges`, `is_border_bigram` (~80 lines). rskim-research is a dev-only binary, not a runtime dependency. Document source of truth.

### D8: Output ordering

- `extract_ngrams`: unsorted (caller decides order for posting lists)
- `extract_query_ngrams`: sorted by weight descending (selectivity ordering for probing)

## File Changes

| File | Action | Lines |
|------|--------|-------|
| `crates/rskim-search/src/ngram.rs` | CREATE | ~350 |
| `crates/rskim-search/src/lib.rs` | MODIFY | +5 |

## Public API

```rust
pub const BORDER_MULTIPLIER: f32 = 3.5;

#[repr(transparent)]
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Ngram(pub u16);

impl Ngram {
    #[must_use]
    pub fn from_bytes(b1: u8, b2: u8) -> Self;
    #[must_use]
    pub fn to_bytes(self) -> (u8, u8);
    #[must_use]
    pub fn key(self) -> u16;
}
impl fmt::Display for Ngram { ... }

#[must_use]
pub fn extract_ngrams(text: &str) -> Vec<(Ngram, f32)>;
#[must_use]
pub fn extract_ngrams_with_weights(text: &str, weights: &[(u16, f32)]) -> Vec<(Ngram, f32)>;

#[must_use]
pub fn extract_query_ngrams(query: &str) -> Vec<(Ngram, f32)>;
#[must_use]
pub fn extract_query_ngrams_with_weights(query: &str, weights: &[(u16, f32)]) -> Vec<(Ngram, f32)>;
```

Private helpers: `token_border_ranges`, `is_border_bigram`, `lookup_weight`.

## Test Strategy (TDD)

6 RED-GREEN-REFACTOR cycles, ~35 tests total. All tests use a synthetic weight table.

| Cycle | Focus | Count |
|-------|-------|-------|
| 1 | Ngram encode/decode roundtrip (incl. exhaustive 256x256) | 7 |
| 2 | Document extraction: edge cases, dedup, unicode, CJK | 10 |
| 3 | Border detection: ranges, boundary conditions | 7 |
| 4 | Query extraction: border weighting, covering set | 8 |
| 5 | Convenience API wiring with BIGRAM_WEIGHTS | 2 |
| 6 | Performance: 1000-line file < 1ms | 1 |

## Performance

- O(n log W) document extraction (n=bytes, W=16,325)
- ~200μs for 40KB file (well under 1ms target)
- Memory: HashMap bounded by u16 key space (~65K max)

## Integration with Future Waves

- **Wave 1b** (index format): `extract_ngrams` output → posting lists
- **Wave 1c** (BM25F): `_with_weights` variant → per-field weight tables
- **Wave 1d** (query engine): `extract_query_ngrams` → candidate probe set
- No breaking changes — pure API addition

## Design Review Notes

- No anti-patterns detected (N+1, god functions, parallelism, caching, decomposition)
- Add `debug_assert!` on sorted invariant in `_with_weights` functions
- f32 precision: BORDER_MULTIPLIER is f32 while research uses f64; accumulate in f64, cast at output

## Gap Analysis Summary

| Severity | Finding | Resolution |
|----------|---------|------------|
| BLOCKING | Variable-length n-grams undefined | Defer; bigrams only |
| BLOCKING | Border multiplier 2x vs 3.5x | Use 3.5x from research |
| BLOCKING | Aggregation semantics undefined | Max-weight dedup |
| BLOCKING | Return type vs Result guideline | Infallible Vec |
| Should-address | Weight table not injectable | `_with_weights` pattern |
| Should-address | Premature module hierarchy | Flat `src/ngram.rs` |
| Should-address | CJK byte-level behavior | Document explicitly |
| Deferred | Variable-length extension | Future wave |
| Deferred | weights.rs binary format (#176) | Depends on consumer API |

## PR Description Guidance

- **Problem**: Wave 1 needs bigram extraction with border-weighted selectivity for the sparse n-gram search index
- **Key Changes**: Ngram type, document extraction with max-weight dedup, query extraction with covering-set heuristic
- **Breaking Changes**: None
- **Reviewer Focus**: Border detection parity with research crate, max-weight dedup correctness, covering-set position coverage, f32 precision
