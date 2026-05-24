# Testing Review Report

**Branch**: feat-176-empirical-sparse-ngram-weights -> main
**Date**: 2026-05-13

## Issues in Your Changes (BLOCKING)

### HIGH

**`is_border_bigram` has no dedicated unit tests despite complex branching logic** - `crates/rskim-research/src/validate.rs:76-98`
**Confidence**: 88%
- Problem: The `is_border_bigram` function contains 3 distinct branches (token >= 2 bytes with exact match, token >= 2 bytes with partial first-byte match, token == 1 byte) but has zero direct tests. It is only exercised indirectly through `border_weighted_selectivity`. The partial first-byte match at line 87 (`window[0] == first2[0] || window[0] == last2[0]`) is particularly suspicious -- it means almost every bigram will be classified as a "border bigram" if any of its bytes match the first byte of any token's first or last pair. This makes the border multiplier apply extremely broadly, potentially making the uniform vs border-weighted comparison meaningless. The single integration test `border_selectivity_exceeds_uniform_for_code_queries` passes trivially because the over-broad matching ensures border always exceeds uniform.
- Fix: Add focused unit tests for `is_border_bigram` that verify the precise boundary conditions. Consider whether the line 87 check is intentional or a logic bug -- if `window[0] == first2[0]` is sufficient to classify any bigram as a border bigram, most bigrams in "fn parse" will be border bigrams (since `f` matches `first2[0]` of "fn", and `p` matches `first2[0]` of "parse"), defeating the purpose of the distinction.

```rust
#[test]
fn interior_bigram_is_not_border() {
    // "ar" in "parse" is interior -- not first2 or last2
    let tokens: Vec<&[u8]> = vec![b"parse"];
    // Depending on intent, this should NOT be a border bigram
    let result = is_border_bigram(b"ar", &tokens);
    // Assert expected behavior once the spec is clear
}

#[test]
fn first_two_bytes_is_border() {
    let tokens: Vec<&[u8]> = vec![b"parse"];
    assert!(is_border_bigram(b"pa", &tokens));
}

#[test]
fn last_two_bytes_is_border() {
    let tokens: Vec<&[u8]> = vec![b"parse"];
    assert!(is_border_bigram(b"se", &tokens));
}
```

**`higher_idf_bigrams_preferred` test does not verify the ordering claim** - `crates/rskim-research/src/validate.rs:246-259`
**Confidence**: 85%
- Problem: The test is named "higher_idf_bigrams_preferred" but only checks that selected bigrams exist in the weight table. It does not verify that higher-IDF bigrams are actually preferred over lower-IDF ones. The comment says "We just verify the result is non-empty and selections are valid bigrams" -- this is an assertion on existence, not on the stated ordering property. This means the greedy heuristic's core invariant (selecting highest-IDF bigrams first) is untested.
- Fix: Assert that for any two selected bigrams, the one selected earlier has IDF >= the one selected later, or at minimum assert that the highest-IDF bigrams in the query ("fn" at 8.0, "im" at 6.0) appear in the selected set before lower-IDF ones.

```rust
#[test]
fn higher_idf_bigrams_preferred() {
    let query = "fn impl";
    let w = synthetic_weights();
    let selected = covering_set_heuristic(query, &w);

    // "fn" (IDF 8.0) and "im" (IDF 6.0) should be in the selected set
    let fn_key = encode_bigram(b'f', b'n');
    let im_key = encode_bigram(b'i', b'm');
    assert!(selected.contains(&fn_key), "high-IDF 'fn' should be selected");
    assert!(selected.contains(&im_key), "high-IDF 'im' should be selected");
}
```

**`run_validation_returns_nonzero_for_nonempty_table` uses `>=` assertions that accept zero** - `crates/rskim-research/src/validate.rs:278-285`
**Confidence**: 82%
- Problem: The test asserts `result.uniform_selectivity >= 0.0` and `result.improvement_pct >= 0.0`. These assertions trivially pass even if all values are exactly `0.0`, which would indicate a broken implementation. Since the test uses `synthetic_weights()` containing bigrams that overlap with `TEST_QUERIES` (e.g., "fn" matches "fn parse"), uniform selectivity must be strictly positive. The improvement assertion is particularly weak -- if border weighting produced no improvement, that could indicate a bug.
- Fix: Use `> 0.0` for uniform_selectivity (guaranteed positive with matching bigrams), and verify border selectivity is strictly greater than uniform (the border multiplier is 3.5x, so improvement must be positive when any bigrams match token borders).

```rust
#[test]
fn run_validation_returns_nonzero_for_nonempty_table() {
    let w = synthetic_weights();
    let result = run_validation(&w, TEST_QUERIES);
    assert!(result.uniform_selectivity > 0.0, "should be positive with matching bigrams");
    assert!(result.border_weighted_selectivity > result.uniform_selectivity,
            "border should exceed uniform");
    assert!(result.improvement_pct > 0.0, "improvement should be positive");
}
```

### MEDIUM

**`codegen::generate_weights_rs` missing test for negative IDF values** - `crates/rskim-research/src/codegen.rs:57-64`
**Confidence**: 85%
- Problem: The function validates that IDF values are positive (line 58: `if w.idf <= 0.0`), including rejecting zero and negative values. The `empty_weights_returns_error` test covers the empty-table case and `invalid_json_returns_error` covers parse failures, but there is no test for the negative-IDF validation path. This is a distinct error path with a distinct error message that is untested.
- Fix: Add a test that constructs a `WeightTable` with a negative IDF entry, writes it to JSON, and verifies `generate_weights_rs` returns an error containing "positive".

```rust
#[test]
fn negative_idf_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("weights.json");
    let out_path = dir.path().join("weights.rs");

    let table = WeightTable {
        version: 1,
        generated_at: "unix:0".to_string(),
        corpus_stats: CorpusStats { total_files: 1, total_bigrams: 1, unique_bigrams: 1, deduplicated_files: 0, language_breakdown: vec![] },
        weights: vec![BigramWeight { bigram: 0x666E, idf: -1.0 }],
    };

    let json = serde_json::to_string(&table).unwrap();
    std::fs::write(&json_path, json).unwrap();

    let err = generate_weights_rs(&json_path, &out_path).unwrap_err();
    assert!(err.to_string().contains("positive"));
}
```

**`codegen::generate_weights_rs` missing test for version == 0 rejection** - `crates/rskim-research/src/codegen.rs:51-53`
**Confidence**: 85%
- Problem: The function rejects `version == 0` as invalid (line 51-53), but no test exercises this validation path.
- Fix: Add a test with `version: 0` that asserts the error message.

**No tests for `selectivity` function in `idf.rs`** - `crates/rskim-research/src/idf.rs:47-57`
**Confidence**: 82%
- Problem: The `selectivity` function is public and used by `validate.rs`, but has no direct unit tests in `idf::tests`. It is only tested indirectly through `validate::uniform_selectivity` (which delegates to it). Edge cases like empty query, single-character query, or query with no matching bigrams are untested at this level.
- Fix: Add direct tests for `selectivity` covering empty input, no-match input, and a known-value input.

**`gen_synthetic.rs` binary has zero tests** - `crates/rskim-research/src/bin/gen_synthetic.rs`
**Confidence**: 80%
- Problem: The `gen_synthetic` binary (230 lines) has no integration test verifying its end-to-end behavior. While it delegates to tested library functions, the orchestration logic (combining fixture files with synthetic code samples, adding all printable ASCII pairs, computing the final table) is untested. Hardcoded language breakdown (lines 188-201) could drift from actual fixture content without detection.
- Fix: Add an integration test that runs `gen_synthetic` to a temp directory and verifies the output is valid JSON with a non-empty weight table. Alternatively, extract the orchestration logic into a library function with unit tests.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`covering_set_covers_all_positions` test has a weak assertion** - `crates/rskim-research/src/validate.rs:219-243`
**Confidence**: 82%
- Problem: The test starts with `if !selected.is_empty()` (line 225), meaning it silently passes if `covering_set_heuristic` returns an empty vec. With the synthetic weights and "fn parse" query, the result should never be empty, so this guard hides potential regressions. The test should assert the selected set is non-empty first.
- Fix: Replace `if !selected.is_empty()` with `assert!(!selected.is_empty(), "covering set should not be empty for query with matching bigrams")` before proceeding to coverage verification.

**`main.rs` command handlers have zero test coverage** - `crates/rskim-research/src/main.rs:92-286`
**Confidence**: 80%
- Problem: The three command handlers (`cmd_run`, `cmd_codegen`, `cmd_validate`) compose library functions but have no integration tests. `cmd_run` is understandably hard to test (requires network), but `cmd_codegen` and `cmd_validate` could be tested with the fixture JSON file that already exists at `crates/rskim-search/data/bigram_weights.json`. The `chrono_now()` function (lines 278-286) uses `.unwrap_or(0)` but has no test.
- Fix: Add integration tests (in a `tests/` directory) that run `cmd_codegen` and `cmd_validate` against the checked-in JSON file, verifying they produce expected output without error.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`is_border_bigram` likely has a logic bug at line 87** - `crates/rskim-research/src/validate.rs:87` (Confidence: 70%) -- The check `window[0] == first2[0]` classifies any bigram whose first byte matches the first byte of any token as a border bigram, which is far broader than "overlaps the first or last 2 bytes" as documented in the function's comment. This may cause the border-weighted strategy to produce inflated scores.

- **Property-based test opportunity for `encode_bigram`/`decode_bigram` roundtrip** - `crates/rskim-research/src/extract.rs:126-134` (Confidence: 65%) -- The exhaustive 256x256 roundtrip test at line 126 is thorough but could be expressed more concisely with proptest/quickcheck. Not a deficiency, just an observation.

- **Hardcoded language breakdown in `gen_synthetic.rs` may drift** - `crates/rskim-research/src/bin/gen_synthetic.rs:188-201` (Confidence: 62%) -- The language breakdown vec is hardcoded to "Rust: 2, TypeScript: 1, Python: 1" rather than computed from actual fixture files. If fixtures change, this metadata will be stale.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 3 | 2 | 0 |
| Should Fix | - | 0 | 2 | 0 |
| Pre-existing | - | - | 0 | 0 |

**Testing Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The test suite covers the core library functions well (extract, IDF computation, codegen, config validation, clone/fixture abstraction), with 34 tests in rskim-research and 4 generated tests in rskim-search/weights.rs. The `FileSource` trait with `FixtureSource` enables clean testing without network access, which is a solid pattern. However, the `is_border_bigram` function -- central to the border-weighted scoring strategy -- lacks direct tests and may contain a logic error. Several validation-focused assertions use `>= 0.0` thresholds that cannot catch regressions, and the greedy covering-set heuristic's ordering property is claimed in a test name but not actually verified. The codegen error paths (negative IDF, version 0) are specified but not tested. Fixing the HIGH items will meaningfully improve confidence in the border-weighting logic and the greedy heuristic.
