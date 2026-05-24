# Security Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### HIGH

**BM25FConfig validation does not reject NaN/Infinity float values** - `crates/rskim-search/src/lexical/config.rs:64-84`
**Confidence**: 92%
- Problem: The `validate()` method checks `k1 < 0.0`, `boost < 0.0`, and `!(0.0..=1.0).contains(&b)`. However, IEEE 754 NaN comparisons always return `false`: `NaN < 0.0` is `false`, `NaN >= 0.0` is `false`, and `(0.0..=1.0).contains(&NaN)` is `false`. This means:
  - `k1 = NaN` passes validation (since `NaN < 0.0` is `false`)
  - `field_boosts[i] = NaN` passes validation (since `NaN < 0.0` is `false`)
  - `field_b[i] = NaN` is correctly rejected (since `contains` returns `false` for NaN)
  - `k1 = f32::INFINITY` passes validation (since `INFINITY < 0.0` is `false`)
  - `field_boosts[i] = f32::INFINITY` passes validation
  
  NaN values propagate through the BM25F scoring formula, causing all scores to become NaN. Since `NaN.partial_cmp(NaN)` returns `None`, the sort falls back to `Ordering::Equal`, producing non-deterministic result ordering. `f32::INFINITY` in `k1` would cause `tf_weighted / (tf_weighted + INFINITY)` to collapse to 0.0, silently zeroing all scores. While `BM25FConfig` is deserialized from JSON via serde (which does not produce NaN/Infinity from valid JSON), the struct has all-public fields, so any Rust caller can construct invalid configs programmatically.
- Fix: Add NaN and Infinity checks to `validate()`:
```rust
pub fn validate(&self) -> Result<()> {
    if self.k1 < 0.0 || self.k1.is_nan() || self.k1.is_infinite() {
        return Err(SearchError::InvalidQuery(format!(
            "BM25FConfig: k1 must be a finite value >= 0.0, got {}",
            self.k1
        )));
    }
    for (i, &boost) in self.field_boosts.iter().enumerate() {
        if boost < 0.0 || boost.is_nan() || boost.is_infinite() {
            return Err(SearchError::InvalidQuery(format!(
                "BM25FConfig: field_boosts[{i}] must be a finite value >= 0.0, got {boost}"
            )));
        }
    }
    for (i, &b) in self.field_b.iter().enumerate() {
        if !(0.0..=1.0).contains(&b) || b.is_nan() {
            return Err(SearchError::InvalidQuery(format!(
                "BM25FConfig: field_b[{i}] must be in [0.0, 1.0], got {b}"
            )));
        }
    }
    Ok(())
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

### MEDIUM

**Mmap safety comment acknowledges undefined behavior on concurrent file modification** - `crates/rskim-search/src/index/reader.rs:82-86`
**Confidence**: 80%
- Problem: The SAFETY comment explicitly states "If another process truncates or overwrites them concurrently, behaviour is undefined." While this is a pre-existing inherent constraint of mmap-based indexes (not introduced by this PR), the new format with larger headers (62 bytes) and per-file metadata (37 bytes) increases the surface area for corrupt reads if a concurrent writer is present. This is informational only since the constraint is inherent to the mmap design and already documented.
- Fix: No action needed for this PR. A future enhancement could use file locking (`flock`) or read-only file permissions as defense-in-depth.

## Suggestions (Lower Confidence)

- **`add_file_classified` does not validate field_map ranges against content length** - `crates/rskim-search/src/index/builder.rs:113-188` (Confidence: 65%) -- The `field_map` parameter is trusted to contain ranges within `[0..content.len())`. A malformed field_map with out-of-bounds ranges would not cause memory unsafety (the linear scan in the posting loop clamps naturally), but could produce incorrect field_length totals. In practice, field_map is produced internally by `classify_source` which enforces the contiguous invariant, so external misuse is unlikely.

- **CRC32 is not a cryptographic integrity check** - `crates/rskim-search/src/index/format.rs` (Confidence: 62%) -- The CRC32 checksum guards against accidental corruption but not deliberate tampering with index files. Since these are local files on the user's filesystem, cryptographic integrity is not required for this threat model, but worth noting for any future network-served index scenario.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 1 | 0 |

**Security Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The PR demonstrates strong security awareness overall: input size bounds on `classify_source` (`MAX_SOURCE_BYTES`), `BM25FConfig::validate()` at trust boundaries (both `open_with_config` and `search`), checked arithmetic throughout (`checked_add`, `checked_mul`, `saturating_add`), atomic writes for index files, explicit format version rejection for v1 indexes, and out-of-range `doc_id` guards in the search loop. The one blocking finding (NaN/Infinity bypass in `validate()`) is a focused fix that does not require architectural changes.
