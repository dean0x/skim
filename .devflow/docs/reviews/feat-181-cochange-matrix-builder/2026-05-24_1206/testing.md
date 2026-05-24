# Testing Review Report

**Branch**: feat/181-cochange-matrix-builder -> main
**Date**: 2026-05-24T12:06

## Issues in Your Changes (BLOCKING)

### HIGH

**Missing test for MAX_PAIRS safety limit breach** - `builder.rs:155`, `builder_tests.rs`
**Confidence**: 95%
- Problem: The `MAX_PAIRS` safety limit (2M pairs) is a critical safety boundary documented in the PR description as a reviewer focus area. The code at `builder.rs:155` checks `pair_counts.len() >= MAX_PAIRS` before inserting a new key and returns `SearchError::IndexCorrupted`. However, there is no test that exercises this error path. The PR description specifically calls out verifying that the check happens "before inserting a new key (not after)". Without a test, this invariant is unverified and could regress.
- Fix: Add a test that overrides or works with a smaller `MAX_PAIRS` value, or directly tests the `accumulate_pairs` function behavior when the pair limit is about to be exceeded. Since `MAX_PAIRS` is a constant, one approach is to test at a smaller scale by constructing enough distinct files to generate pairs that exceed a testable threshold. A practical approach:

```rust
#[test]
fn test_max_pairs_breach_returns_error() {
    // To exercise MAX_PAIRS without generating 2M pairs, test the
    // accumulate_pairs logic by constructing commits with enough
    // distinct files to approach the limit. Since MAX_PAIRS = 2_000_000
    // this is impractical to test directly at full scale.
    //
    // Consider either:
    // 1. Extract accumulate_pairs as a testable unit with a configurable limit
    // 2. Add a #[cfg(test)] const TEST_MAX_PAIRS for test-time override
    // 3. Test the error variant shape matches expectations (integration-level)
}
```

The most architecturally sound approach is to make the limit injectable (dependency injection on the cap), which would allow testing without generating millions of pairs.

**Duplicated test helper functions across builder_tests.rs and reader_tests.rs** - `builder_tests.rs:17-51`, `reader_tests.rs:19-53`
**Confidence**: 85%
- Problem: `make_history()` and `make_path_map()` are duplicated verbatim across `builder_tests.rs` and `reader_tests.rs` (34 identical lines). This violates DRY and means any fix to the helper logic must be applied in two places. Since both test modules are `#[path = "..."]` includes under the same parent module, a shared test utility would be straightforward.
- Fix: Create a `test_helpers.rs` file in the `cochange/` directory and include it as a shared `#[cfg(test)]` module, or define the helpers in a `#[cfg(test)]` submodule of `mod.rs` that both test files can import.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**No test for Jaccard with perfect coupling (1.0)** - `reader_tests.rs`
**Confidence**: 85%
- Problem: The Jaccard tests cover known values (0.333...), self-pairs (0.0), absent pairs (0.0), and zero-denominator (0.0), but never test the case where two files always co-change together (Jaccard = 1.0). This is an important boundary value for Jaccard similarity: `count_ab / (count_a + count_b - count_ab) = n / (n + n - n) = 1.0`. Testing this validates the denominator arithmetic at the other extreme.
- Fix: Add a test:
```rust
#[test]
fn test_jaccard_perfect_coupling_returns_one() {
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(
        &tmp,
        vec![vec!["a.rs", "b.rs"], vec!["a.rs", "b.rs"], vec!["a.rs", "b.rs"]],
        &["a.rs", "b.rs"],
    );
    let j = reader.jaccard(FileId(0), FileId(1)).unwrap();
    assert!((j - 1.0).abs() < 1e-9, "perfect coupling Jaccard should be 1.0, got {j}");
}
```

**No test for `pairs_for_file` with file appearing on both sides of pairs** - `reader_tests.rs`
**Confidence**: 80%
- Problem: The `pairs_for_file` test (`test_pairs_for_file_sorted_by_count_desc`) only tests with `FileId(0)`, which is always `file_a` (the lower ID) in canonical pairs. There is no test verifying that `pairs_for_file` correctly finds partners when the queried file appears as `file_b` (the higher ID in the pair). The implementation at `reader.rs:172-175` handles both branches (`entry.file_a == id` and `entry.file_b == id`), but only the first branch is exercised in tests.
- Fix: Add a test querying `FileId(2)` (the highest ID), which would only appear as `file_b` in canonical pairs:
```rust
#[test]
fn test_pairs_for_file_finds_partners_when_file_is_higher_id() {
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(
        &tmp,
        vec![vec!["a.rs", "b.rs", "c.rs"]],
        &["a.rs", "b.rs", "c.rs"],
    );
    // c.rs (FileId(2)) is always file_b in canonical pairs
    let pairs = reader.pairs_for_file(FileId(2)).unwrap();
    assert_eq!(pairs.len(), 2, "c.rs should have 2 co-change partners");
}
```

**No test for `file_commits` via binary search edge cases** - `reader_tests.rs`
**Confidence**: 80%
- Problem: The `file_commits` method uses a hand-rolled binary search (`reader.rs:196-209`). Tests only cover the happy path (file found) and unknown-ID case (file not found). Missing: searching for the first element, the last element, and a middle element in a 3+ entry dataset, to validate the binary search logic at boundary positions.
- Fix: Add a test with 3+ files that queries each one:
```rust
#[test]
fn test_file_commits_binary_search_all_positions() {
    let tmp = TempDir::new().unwrap();
    let reader = build_matrix(
        &tmp,
        vec![vec!["a.rs", "b.rs", "c.rs"], vec!["a.rs", "c.rs"]],
        &["a.rs", "b.rs", "c.rs"],
    );
    assert_eq!(reader.file_commits(FileId(0)).unwrap(), 2); // first entry
    assert_eq!(reader.file_commits(FileId(1)).unwrap(), 1); // middle entry
    assert_eq!(reader.file_commits(FileId(2)).unwrap(), 2); // last entry
}
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Consider property-based testing for encode/decode roundtrips** - `format_tests.rs` (Confidence: 70%) -- The codec roundtrip tests use hand-picked values. A property-based test with `proptest` or `quickcheck` (e.g., `forall (file_a, file_b, count): decode(encode(PairEntry{...})) == original`) would catch edge cases around `u32::MAX` values, zero values, and endianness issues automatically.

- **`format_tests.rs` uses glob import (`use super::*`)** - `format_tests.rs:6` (Confidence: 65%) -- Unlike `builder_tests.rs` and `reader_tests.rs` which use explicit imports, `format_tests.rs` uses `use super::*`. This is a minor consistency issue but can make test dependencies less obvious. However, for `#[cfg(test)]` modules within the same file this is a common Rust convention, so it may be intentional.

- **No negative test for `decode_pair` / `decode_file_commit` with truncated input in isolation** - `format_tests.rs` (Confidence: 62%) -- The `decode_pair` and `decode_file_commit` functions have truncation guards, but the tests only exercise roundtrips. While `test_header_truncated` tests the header case, there are no analogous truncation tests for `decode_pair` and `decode_file_commit`. The header test sets a precedent that suggests these should also be covered.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 0 | 0 |
| Should Fix | 0 | 0 | 3 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The test suite is well-structured with good coverage of happy paths, error paths, and important behavioral invariants (canonical ordering, dedup, CRC32 integrity). The 43 tests follow clear Arrange-Act-Assert structure with descriptive names. The main gap is the missing `MAX_PAIRS` breach test -- a critical safety boundary highlighted in the PR description that has zero test coverage. The duplicated test helpers are a maintainability concern. Adding Jaccard boundary tests (perfect coupling = 1.0) and exercising both sides of `pairs_for_file` lookup would round out the coverage nicely.
