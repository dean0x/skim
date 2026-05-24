# Security Review Report

**Branch**: feat/180-bm25f-scoring -> main
**Date**: 2026-05-16

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Uncontrolled memory allocation in classify_source — potential DoS via large input** - `crates/rskim-search/src/lexical/classifier.rs:116`
**Confidence**: 82%
- Problem: `classify_source` allocates a `Vec<SearchField>` of length `source.len()` (line 116: `vec![SearchField::Other; len]`). There is no upper bound check on input size. If `source` is a multi-gigabyte file, this creates a proportional allocation (1 byte per source byte for an enum that is `repr(u8)`). The project CLAUDE.md notes that very large files (>100MB) should be rejected, but this function does not enforce any limit.
- Fix: Add a size guard at the top of `classify_source` to reject inputs exceeding a reasonable threshold (e.g., `u32::MAX` bytes which is already enforced downstream in the builder, or a tighter limit like 100MB matching project conventions):
  ```rust
  const MAX_CLASSIFY_SIZE: usize = 100 * 1024 * 1024; // 100 MB
  if source.len() > MAX_CLASSIFY_SIZE {
      return Err(SearchError::InvalidQuery(format!(
          "source too large for classification: {} bytes exceeds {}",
          source.len(), MAX_CLASSIFY_SIZE
      )));
  }
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**No validation that field_lengths sum equals doc_length in decode_file_meta** - `crates/rskim-search/src/index/format.rs:330-347`
**Confidence**: 80%
- Problem: The `FileMetaEntry` doc comment (line 145-146) states an invariant: `field_lengths[0..8].iter().sum::<u32>() == doc_length`, and notes "upheld by the builder; validated by the reader." However, `decode_file_meta` does NOT actually validate this invariant. A corrupted or maliciously crafted index file could supply field_lengths that do not sum to doc_length. This could cause the BM25F scoring to produce nonsensical scores, though it would not cause memory unsafety.
- Fix: Add an integrity check in `decode_file_meta`:
  ```rust
  let sum: u32 = field_lengths.iter().sum();
  if sum != doc_length {
      return Err(SearchError::IndexCorrupted(format!(
          "file_meta: field_lengths sum {sum} != doc_length {doc_length}"
      )));
  }
  ```
  Alternatively, if backward compatibility matters, validate in the reader at query time rather than at decode time.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**Unsafe mmap usage without SIGBUS/SIGILL handling** - `crates/rskim-search/src/index/reader.rs:85-86`
**Confidence**: 65% (moved to Suggestions as below threshold)

## Suggestions (Lower Confidence)

- **Mmap concurrent modification risk** - `crates/rskim-search/src/index/reader.rs:82-86` (Confidence: 65%) — The SAFETY comment acknowledges that concurrent file modification causes undefined behavior. While this is an inherent constraint of mmap-based designs and the existing code already acknowledges it, a robust deployment would benefit from advisory file locking or signal handling for SIGBUS.

- **NaN/Inf propagation from crafted avg_field_lengths** - `crates/rskim-search/src/lexical/scoring.rs:63-67` (Confidence: 70%) — If a crafted index provides `avg_field_lengths` containing NaN or negative values, the scoring path guards against zero but not NaN. While `f32::from_le_bytes` cannot produce NaN from normal operation and the builder always produces valid values, a defense-in-depth check (e.g., `is_finite() && v >= 0.0`) in the decode path would harden against crafted indexes.

- **BM25FConfig deserialization from untrusted sources** - `crates/rskim-search/src/types.rs:295-296` (Confidence: 62%) — `SearchQuery.bm25f_config` is deserializable from JSON. If the search API is exposed over a network, an attacker could supply extreme boost values (e.g., `f32::MAX`) to skew rankings. The `validate()` method exists but is not called automatically during deserialization. Consider calling `validate()` in the search path before using a deserialized config.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Security Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code is well-written from a security perspective. All buffer accesses use bounds-checked `read_array` helpers, the format decoder validates magic, version, and CRC32 checksums, and `field_id` values are validated against known discriminants. The primary concern is the unbounded allocation in `classify_source` for very large inputs (HIGH), and the missing invariant validation documented in the code but not enforced (MEDIUM). Neither is immediately exploitable in the library's current context (local index builder), but they should be addressed before any network-facing usage.
