# Rust Review Report

**Branch**: feat/177-sparse-ngram-algorithm -> main
**Date**: 2026-05-14T01:19

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Quadratic covering-set termination check** - `ngram.rs:277`
**Confidence**: 85%
- Problem: Inside the greedy covering-set loop, `covered.iter().all(|&c| c)` performs a full O(n) scan of the `covered` vector on every iteration. For a query of length `q`, the loop runs up to `q-1` candidates, making the early-exit check O(q^2) in the worst case. Queries are typically short so this is unlikely to be a bottleneck in practice, but it is unnecessary overhead for a hot path in a search index.
- Fix: Track a `remaining` counter initialized to `bytes.len()` and decrement it when newly covering a position. Break when `remaining == 0`:

```rust
let mut remaining = bytes.len();
for (ngram, w, pos) in candidates {
    let newly_covered = !covered[pos] as usize + !covered[pos + 1] as usize;
    if newly_covered > 0 {
        covered[pos] = true;
        covered[pos + 1] = true;
        remaining -= newly_covered;
        selected.push((ngram, w));
    }
    if remaining == 0 {
        break;
    }
}
```

**Public inner field on Ngram newtype weakens encapsulation** - `ngram.rs:46`
**Confidence**: 82%
- Problem: `Ngram(pub u16)` exposes the raw inner value, allowing callers to construct arbitrary `Ngram` values via `Ngram(raw_u16)` and bypassing `from_bytes`. The newtype pattern loses its value when the inner field is public. Currently `Ngram(key)` is used internally at line 198, but making it public invites external misuse and couples consumers to the encoding.
- Fix: Make the field private and add a `from_raw` constructor for internal/crate use:

```rust
pub struct Ngram(u16);  // private field

impl Ngram {
    /// Construct from a pre-encoded u16 key. Intended for internal use
    /// (e.g. re-hydrating from storage).
    #[must_use]
    #[inline]
    pub(crate) fn from_raw(key: u16) -> Self {
        Self(key)
    }
    // ... existing methods unchanged
}
```

Then at line 198: `Ngram::from_raw(key)` instead of `Ngram(key)`.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`is_border_bigram` linear scan per candidate** - `ngram.rs:254` (called from line 254)
**Confidence**: 80%
- Problem: `is_border_bigram` performs a linear scan over `border_ranges` for each of the `q-1` candidates. For typical queries this is fine (few tokens = few ranges), but as the function signature accepts any `&[(usize, usize)]` and has no documented size bound, it could degrade on pathological input with many whitespace-delimited tokens. The combination with the per-candidate call makes total work O(candidates * ranges).
- Fix: For short queries (the common case) this is acceptable. Consider adding a brief doc comment noting the expected small range count, or for future-proofing, sort ranges and use binary search if this becomes a bottleneck. No code change required now -- this is a "should fix while here" documentation gap.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Duplicate dedup-then-sort in query extraction** - `ngram.rs:265,284` (Confidence: 65%) -- The candidates vector is sorted at line 265, then `selected` is re-sorted at line 284. The second sort is documented as necessary for ties, but since the greedy loop already processes candidates in descending weight order, the insertion order of `selected` is already descending except when two candidates share the same weight and the greedy heuristic selects them out of order. Consider whether this second sort is truly needed or whether a `debug_assert!` on the output order would suffice.

- **HashMap capacity heuristic may over-allocate for short text** - `ngram.rs:188` (Confidence: 62%) -- `bytes.len().min(256)` pre-sizes the HashMap. For very short inputs (2-10 bytes), this allocates capacity for up to 256 entries when at most `len-1` unique bigrams exist. A tighter bound like `(bytes.len().saturating_sub(1)).min(256)` would be more precise, though the practical impact is negligible.

- **Performance test is time-based and potentially flaky in CI** - `ngram_tests.rs:369-391` (Confidence: 70%) -- Wall-clock assertions (`as_micros() < 2000` in release, `as_millis() < 500` in debug) can flake on overloaded CI runners. The dual thresholds (debug vs. release) mitigate this, but consider using a relative benchmark or iteration count instead for deterministic pass/fail.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code is well-structured Rust with excellent practices: proper newtype pattern, `#[must_use]` annotations, `#[repr(transparent)]`, `debug_assert!` for preconditions, comprehensive doc comments, zero-copy `&str` input, `total_cmp` for float sorting, f64 intermediate accumulation for numeric stability, and clean separation of tests. The two blocking MEDIUM items (quadratic termination check and public newtype field) are straightforward to address and do not pose correctness risks for current usage -- they are encapsulation and micro-optimization concerns. Clippy is clean with zero warnings. 382 tests cover all code paths including edge cases (empty, single-byte, CJK, whitespace-only). The weights.rs changes are purely cargo fmt whitespace alignment.
