# Testing Review Report

**Branch**: feat/178-two-file-mmap-index -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**Missing offset pagination test** - `reader_tests.rs:233-251`
**Confidence**: 90%
- Problem: The "search -- offset / limit" section header at line 233 promises coverage of both offset and limit, but only `test_limit_restricts_result_count` exists. The `SearchQuery.offset` field is exercised in production code (`reader.rs:247-248`) with skip logic (`reader.rs:263-266`), but no test verifies that offset actually skips the correct number of top-ranked results. This is a real behavioral gap: a regression in offset handling would go undetected.
- Fix: Add a test that builds an index with multiple files containing a common term, searches with `offset = 2`, and asserts that the first result returned is NOT the same as the first result returned without offset. For example:
```rust
#[test]
fn test_offset_skips_top_results() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    for i in 0..5u32 {
        builder.add_file(FileId(i), "fn main() {}", rskim_core::Language::Rust).unwrap();
    }
    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();

    let all = reader.search(&SearchQuery::new("main")).unwrap();
    let mut query = SearchQuery::new("main");
    query.offset = Some(2);
    let offset_results = reader.search(&query).unwrap();

    assert!(!offset_results.is_empty(), "offset results should not be empty");
    // The first result with offset=2 should match the 3rd result without offset
    assert_eq!(offset_results[0].file_id, all[2].file_id);
}
```

### MEDIUM

**Two tests directly access builder internal fields** - `builder_tests.rs:96,185`
**Confidence**: 85%
- Problem: `test_add_file_increments_file_count` asserts `builder.file_count` (line 96) and `test_build_file_metadata_correctness` accesses `builder.file_meta[0]` (line 185). These tests couple to internal struct fields rather than observable behavior through the `LayerBuilder` trait. If the internal representation changes (e.g., `file_count` becomes a method, or `file_meta` uses a different data structure), these tests break even though the behavior is unchanged. The file count behavior is already indirectly covered by `test_build_multiple_files`, and metadata correctness could be verified through the reader's `stats()` or search results.
- Fix: Replace `test_add_file_increments_file_count` with an assertion through the built layer:
```rust
#[test]
fn test_add_file_increments_file_count() {
    let dir = tmp_dir();
    let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder.add_file(FileId(0), "fn main() {}", rskim_core::Language::Rust).unwrap();
    builder.add_file(FileId(1), "def hello(): pass", rskim_core::Language::Python).unwrap();
    builder.build().unwrap();
    let reader = NgramIndexReader::open(dir.path()).unwrap();
    assert_eq!(reader.stats().file_count, 2);
}
```
For `test_build_file_metadata_correctness`, verify through the lang filter: build a Rust file, search with `lang = Some(Rust)`, assert it's found; search with `lang = Some(Python)`, assert it's empty.

**No test for corrupted .skpost file** - `reader_tests.rs`
**Confidence**: 85%
- Problem: `test_corrupted_skidx_detected` verifies corruption detection for `.skidx`, but there is no corresponding test for a corrupted `.skpost` file. The reader validates `.skpost` size at open time (`reader.rs:94-99`), but a `.skpost` with valid size but corrupted posting data would silently produce wrong results. While `.skpost` is not checksummed (only `.skidx` entries + metadata are), a size mismatch test would confirm the validation at `reader.rs:94-99` works.
- Fix: Add a test that writes a `.skpost` file with incorrect size:
```rust
#[test]
fn test_corrupted_skpost_size_mismatch_detected() {
    let dir = tmp_dir();
    {
        let mut builder = NgramIndexBuilder::new(dir.path().to_path_buf()).unwrap();
        builder.add_file(FileId(0), "hello world", rskim_core::Language::Rust).unwrap();
        builder.build().unwrap();
    }
    // Append garbage to .skpost to change its size
    let post_path = dir.path().join("index.skpost");
    let mut bytes = std::fs::read(&post_path).unwrap();
    bytes.extend_from_slice(b"GARBAGE");
    std::fs::write(&post_path, bytes).unwrap();
    let result = NgramIndexReader::open(dir.path());
    assert!(result.is_err(), "skpost size mismatch should fail to open");
}
```

**No test for SearchResult.match_positions correctness** - `reader_tests.rs`
**Confidence**: 82%
- Problem: Several reader tests assert `!results.is_empty()` and check `score > 0.0` and `file_id` correctness, but none assert that `match_positions` contains valid byte ranges. The production code at `reader.rs:232-238` populates `match_positions` with `pos..pos+2` ranges from posting entries. If this logic regressed (e.g., off-by-one, empty positions), no test would catch it.
- Fix: Add an assertion in `test_single_file_roundtrip_finds_term` (or a new test) that checks `match_positions` is non-empty and contains plausible byte ranges:
```rust
assert!(!results[0].match_positions.is_empty(), "should have match positions");
for pos in &results[0].match_positions {
    assert_eq!(pos.end - pos.start, 2, "bigram positions should be 2 bytes wide");
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Duplicate test: `test_duplicate_file_id_rejected` in reader_tests.rs duplicates `test_add_file_duplicate_id_returns_error` in builder_tests.rs** - `reader_tests.rs:308-317`, `builder_tests.rs:39-53`
**Confidence**: 85%
- Problem: Both tests exercise the exact same code path -- `NgramIndexBuilder::add_file` rejecting a duplicate `FileId`. The reader test adds nothing beyond what the builder test already covers, since no reader is involved. This duplication increases maintenance burden without adding coverage.
- Fix: Remove `test_duplicate_file_id_rejected` from `reader_tests.rs`. The builder test already covers this behavior with a more thorough assertion (checking the error message).

**Lang filter test only verifies single-language restriction** - `reader_tests.rs:166-197`
**Confidence**: 80%
- Problem: `test_lang_filter_restricts_results` indexes three languages (Rust, Python, JavaScript) and filters for Rust, asserting only file 0 appears. However, it does not verify that filtering for Python or JavaScript also works correctly, nor does it test the negative case where the filtered language has no matching files. This leaves potential for a bug where, say, the lang filter only works for the first language indexed.
- Fix: Either add a second filter assertion within the same test, or add a small companion test:
```rust
let mut query_py = SearchQuery::new("main");
query_py.lang = Some(rskim_core::Language::Python);
let results_py = reader.search(&query_py).unwrap();
assert!(!results_py.is_empty());
for r in &results_py {
    assert_eq!(r.file_id.0, 1, "Python filter should only return FileId(1)");
}
```

## Pre-existing Issues (Not Blocking)

_None identified._

## Suggestions (Lower Confidence)

- **No test for `bm25_score` with negative IDF** - `format_tests.rs` (Confidence: 70%) -- If `idf_for_key` ever returns a negative value (theoretically impossible with the current weight table but not enforced by the type system), `bm25_score` would return a negative contribution. A defensive test asserting behavior for `idf = 0.0` and `idf = -1.0` would document the expected contract.

- **No test for combined offset + limit** - `reader_tests.rs` (Confidence: 65%) -- The offset and limit logic interact (offset skips, then limit caps). A test with both `offset = 2` and `limit = 1` would verify they compose correctly, but this is somewhat covered implicitly if individual offset and limit tests are added.

- **Benchmark test uses wall-clock timing assertions** - `reader_tests.rs:323-362` (Confidence: 65%) -- `test_1000_file_benchmark` asserts `write_elapsed.as_millis() < 100` and `read_elapsed.as_millis() < 100`. Wall-clock benchmarks in test suites can be flaky on loaded CI machines. The `#[cfg(not(debug_assertions))]` guard mitigates this for debug mode, but it could still fail on a slow CI runner. Consider using Criterion benchmarks separately and removing the timing assertion, or using a more generous threshold.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 3 | 0 |
| Should Fix | - | 0 | 2 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The test suite is well-structured with good coverage of the happy path, codec roundtrips, error conditions, corruption detection, and BM25 scoring properties. Test organization is clean with clear section headers and the `build_index_with` helper reduces boilerplate. However, the missing `offset` pagination test represents a genuine behavioral gap for code that has non-trivial skip logic. The `.skpost` corruption path and `match_positions` output validation are secondary gaps. Two builder tests that inspect internal struct fields should be refactored to assert through the public API (reader stats or search results) to improve resilience to implementation changes.
