# Testing Review Report

**Branch**: main (commit 353ef87)
**Date**: 2026-05-24
**Scope**: cochange module -- 46 tests across 3 test files

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Missing truncated-input tests for `decode_file_commit` and `decode_pair`** - `format_tests.rs`
**Confidence**: 90%
- Problem: `format.rs` lines 199-203 and 231-234 have explicit truncation guards returning `SearchError::IndexCorrupted` for short input buffers. These error paths have zero test coverage. The header truncation test exists (`test_header_truncated`), but the analogous tests for the two entry decoders do not. These decoders are called during mmap reads where corrupt files could produce short slices.
- Fix: Add two tests:
```rust
#[test]
fn test_file_commit_entry_truncated() {
    let result = decode_file_commit(&[0u8; 4]); // < FILE_COMMIT_ENTRY_SIZE (8)
    assert!(result.is_err());
}

#[test]
fn test_pair_entry_truncated() {
    let result = decode_pair(&[0u8; 8]); // < PAIR_ENTRY_SIZE (12)
    assert!(result.is_err());
}
```

**Missing reader size-mismatch test** - `reader_tests.rs`
**Confidence**: 92%
- Problem: `reader.rs` lines 98-103 contain a size-mismatch guard that verifies `mmap.len() == pairs_end`. This branch is never exercised by any test. The closest test (`test_open_corrupt_file_fails`) uses garbage data that fails on magic validation, never reaching the size check. A crafted file with valid header but truncated body would exercise this distinct validation path.
- Fix: Add a test that writes a valid header claiming N file entries and M pair entries, but with a body shorter than expected:
```rust
#[test]
fn test_reader_size_mismatch_detected() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("cochange.skcc");
    // Valid header claiming 1 pair + 1 file, but no body bytes
    let header = SkccHeader {
        magic: *SKCC_MAGIC,
        version: FORMAT_VERSION,
        pair_count: 1,
        file_count: 1,
        checksum: 0,
    };
    std::fs::write(&path, encode_header(&header)).unwrap();
    let result = CochangeMatrixReader::open(tmp.path());
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("mismatch"), "error should mention size mismatch: {msg}");
}
```

### LOW

**Misleading test name: `test_jaccard_zero_denominator_returns_zero`** - `reader_tests.rs:150`
**Confidence**: 85%
- Problem: The test name claims to test the zero-denominator guard (reader.rs:166-168), but the test uses an empty history where `count_ab == 0`, so it always exits at the `count_ab == 0` early return (reader.rs:160-161) and never reaches the denominator check. The zero-denominator branch is mathematically unreachable (if `count_ab > 0`, then both `count_a` and `count_b` are `>= count_ab`, so `count_a + count_b - count_ab >= count_ab > 0`). The defensive guard is fine, but the test name is misleading about what it actually exercises.
- Fix: Rename to `test_jaccard_no_shared_commits_returns_zero` to accurately describe the tested path. The denominator==0 guard is a defensive impossibility -- acknowledge that in a comment if desired, but do not claim a test covers it.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Conditional guard weakens CRC corruption test** - `reader_tests.rs:261`
**Confidence**: 82%
- Problem: `if data.len() > 20` silently skips the byte-flip corruption if the generated file happens to be 20 bytes or fewer. For the current test data (2 files, 1 pair = 46 bytes), the condition is always true, so the test works today. But if the test inputs change to produce a smaller file (e.g., empty history), the test would silently pass without corrupting anything. Replace with an unconditional assertion.
- Fix:
```rust
// Replace:
if data.len() > 20 {
    data[18] ^= 0xFF;
}
// With:
assert!(data.len() > HEADER_SIZE, "test requires non-empty data section");
data[HEADER_SIZE] ^= 0xFF;
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Missing `max_pairs = 0` boundary test** - `builder_tests.rs` (Confidence: 70%) -- The safety cap is tested with `max_pairs = 2`, but `max_pairs = 0` (where any pair insertion should immediately fail) is not tested. This is a classic boundary value.

- **Missing `lookup_pair` single-element binary search test** - `format_tests.rs` (Confidence: 65%) -- Binary search tests use 2-3 elements. A single-element array is a classic edge case for binary search correctness.

- **No rebuild/overwrite test** - `builder_tests.rs` (Confidence: 62%) -- Building twice to the same directory (verifying atomic overwrite correctness) is untested. The `atomic_write` function uses `NamedTempFile::persist` which should overwrite, but this is unverified.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 1 |
| Should Fix | - | 0 | 1 | - |
| Pre-existing | - | - | 0 | 0 |

**Testing Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Passed Checks

- **Test isolation**: All tests use `TempDir` which auto-cleans on drop. No shared state between tests. No ordering dependencies.
- **No flaky patterns**: No timing, sleep, or non-deterministic assertions. All tests are deterministic.
- **Behavior-focused**: Tests assert on observable outputs (stats, counts, Jaccard values), not on internal implementation details.
- **AAA structure**: Clear Arrange-Act-Assert structure throughout.
- **Test naming**: Descriptive names that describe expected behavior (with one exception noted above).
- **Shared test helpers**: Well-designed `test_helpers.rs` with `make_history` and `make_path_map` -- clean, reusable, minimal.
- **Safety cap boundary testing**: COUPLING_MAX_FILES tested at all three boundaries (below, at, above). MAX_PAIRS tested via `build_with_max_pairs` with testable limit injection.
- **Binary format roundtrip**: All three struct types (Header, FileCommitEntry, PairEntry) have encode/decode roundtrip tests.
- **Corrupt input testing**: Bad magic, bad version, truncated header, garbage file, CRC32 mismatch, misaligned pair data all tested.
- **Edge cases**: Empty history, single-file commit (no pairs), duplicate paths in commit (dedup), unknown paths, self-pairs, canonical ordering, absent pairs.
- **Jaccard correctness**: Known-value test (2/6), perfect coupling (1.0), self-pair (0.0), absent pair (0.0).
- **pairs_for_file**: Both lower-ID and higher-ID branches exercised; sort-descending verified.
- **Send + Sync**: Compile-time trait check for `CochangeMatrixReader`.
- **Test helper `#[cfg(test)]` gating**: `test_helpers` module correctly gated behind `#[cfg(test)]`, `build_with_max_pairs` uses `#[cfg(test)]` to avoid leaking test-only API.
- **46 tests total**: Strong coverage for ~900 lines of production code across 3 modules.

### Overall Assessment

This is a well-structured test suite with strong coverage of happy paths, error cases, and boundary conditions. The tests are behavior-focused, isolated, and deterministic. The main gaps are: (1) two untested error paths in format decoders (truncated `FileCommitEntry`/`PairEntry`), (2) the reader's size-mismatch validation path has no test, and (3) a misleadingly-named test. These are all MEDIUM/LOW severity -- the test suite provides solid confidence in the module's correctness.
