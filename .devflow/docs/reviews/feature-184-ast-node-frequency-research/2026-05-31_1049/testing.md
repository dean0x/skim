# Testing Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31
**PR**: #263

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**Missing test for remap_trigram correctness** - `crates/rskim-research/src/ast_types.rs`
**Confidence**: 90%
- Problem: Tests cover `remap_bigram` and `rekey_bigram_df_map` with dedicated roundtrip assertions (lines 489-530), but there is no corresponding test for `remap_trigram` or `rekey_trigram_df_map`. The trigram remap function (`remap_trigram`, line 84) has more components (3 IDs vs 2) and is more likely to have an off-by-one or masking bug. The feature knowledge explicitly warns about testing with pre-stabilize IDs (the remap bug fixed in commit 605203a), yet only the bigram side of the fix is regression-tested. `applies ADR-001`
- Fix: Add `remap_trigram_correctness` and `rekey_trigram_df_map_preserves_counts` tests mirroring the bigram equivalents:
```rust
#[test]
fn remap_trigram_correctness() {
    let mut vocab = NodeKindVocabulary::new();
    vocab.get_or_insert("z_kind"); // old ID 0
    vocab.get_or_insert("m_kind"); // old ID 1
    vocab.get_or_insert("a_kind"); // old ID 2

    let old_trigram = encode_ast_trigram(0, 1, 2);
    let remap = vocab.stabilize();
    // After stabilize: a=0, m=1, z=2
    // remap: [2, 1, 0]

    let new_trigram = remap_trigram(old_trigram, &remap).unwrap();
    let (gp, p, c) = decode_ast_trigram(new_trigram);
    assert_eq!(vocab.resolve(gp), Some("z_kind"));
    assert_eq!(vocab.resolve(p), Some("m_kind"));
    assert_eq!(vocab.resolve(c), Some("a_kind"));
}

#[test]
fn rekey_trigram_df_map_preserves_counts() {
    let mut vocab = NodeKindVocabulary::new();
    vocab.get_or_insert("z_kind");
    vocab.get_or_insert("m_kind");
    vocab.get_or_insert("a_kind");

    let old_trigram = encode_ast_trigram(0, 1, 2);
    let mut df_map = HashMap::new();
    df_map.insert(old_trigram, 17u32);

    let remap = vocab.stabilize();
    let rekeyed = rekey_trigram_df_map(&df_map, &remap);

    assert_eq!(rekeyed.len(), 1);
    let new_trigram = remap_trigram(old_trigram, &remap).unwrap();
    assert_eq!(rekeyed[&new_trigram], 17);
}
```

**Missing remap out-of-bounds test** - `crates/rskim-research/src/ast_types.rs:73,84`
**Confidence**: 85%
- Problem: Both `remap_bigram` and `remap_trigram` return `Option` to handle out-of-bounds IDs, but no test verifies that `None` is actually returned when an ID exceeds the remap table length. This is a boundary condition that should be tested, especially since `rekey_bigram_df_map` and `rekey_trigram_df_map` silently drop entries on `None` (line 102, 119) -- if that guard is ever accidentally removed, silent data loss would occur without any test catching it.
- Fix: Add a test confirming the `None` return:
```rust
#[test]
fn remap_bigram_out_of_bounds_returns_none() {
    let remap: Vec<NodeKindId> = vec![1, 0]; // only 2 entries
    let bigram = encode_ast_bigram(5, 0); // parent ID 5 is out of bounds
    assert_eq!(remap_bigram(bigram, &remap), None);
}
```

### MEDIUM

**Codegen output not validated as compilable Rust** - `crates/rskim-research/src/ast_codegen.rs:399-481`
**Confidence**: 82%
- Problem: The codegen tests verify that the generated string contains expected tokens (vocabulary, arrays, lookup functions) and that arrays are sorted, but never attempt to parse the generated source as valid Rust syntax. The feature knowledge states tests should verify "codegen output compilability." A malformed `writeln!` format string or missing semicolon would not be caught by substring assertions.
- Fix: Add a test that uses `syn::parse_file` (already a common Rust dev-dependency) or at minimum assert the output is valid syntax by checking balanced braces and that the file starts/ends correctly. Alternatively, add a `#[test] fn generated_source_compiles()` integration test that writes the source to a temp file and runs `rustc --edition 2021 --crate-type lib` on it.

**`error_nodes_counted_but_not_in_bigrams` test has weak assertions** - `crates/rskim-research/src/ast_extract.rs:397-406`
**Confidence**: 80%
- Problem: The test for error node handling (line 397) only verifies the function does not panic. It does not assert that `error_node_count > 0` (to confirm errors were actually detected) or that no bigrams contain ERROR node IDs. The test name promises "counted but not in bigrams" but the body asserts neither condition. The `let _ = result;` on line 405 discards the result entirely.
- Fix:
```rust
#[test]
fn error_nodes_counted_but_not_in_bigrams() {
    let mut vocab = NodeKindVocabulary::new();
    let source = "fn broken(((( {}";
    let result =
        extract_ast_ngrams_from_file(source, Language::Rust, &mut vocab, false).unwrap();
    // ERROR nodes should be detected
    assert!(result.error_node_count > 0, "should detect error nodes");
    // ERROR should not be in the vocabulary (not turned into bigrams)
    assert!(vocab.get("ERROR").is_none(), "ERROR should not be in vocabulary");
}
```

## Issues in Code You Touched (Should Fix)

### HIGH

**No integration test for the full stabilize-rekey-IDF pipeline** - `crates/rskim-research/src/main.rs:374-451`
**Confidence**: 88%
- Problem: The `cmd_ast_run` function (line 374) implements the critical pipeline: extract -> stabilize -> rekey -> IDF compute. This is exactly where the remap bug (commit 605203a) lived. While each component is unit-tested individually, there is no integration test that exercises the full sequence with real source files through `extract_ast_ngrams_from_corpus` -> `vocab.stabilize()` -> `rekey_bigram_df_map` -> `compute_ast_bigram_weights` and verifies the resulting weight table resolves kind strings correctly. A regression in the ordering of these calls would not be caught by any existing test. `avoids PF-002`
- Fix: Add an integration test in `ast_extract` or a new `tests/` integration file:
```rust
#[test]
fn full_pipeline_stabilize_rekey_idf_produces_valid_weights() {
    let files = vec![
        make_file("fn a() { let x = 1; }", Language::Rust),
        make_file("fn b() { let y = 2; }", Language::Rust),
    ];
    let mut vocab = NodeKindVocabulary::new();
    let (bigram_dfs, _, stats) = extract_ast_ngrams_from_corpus(&files, &mut vocab, false);

    let remap = vocab.stabilize();

    for (lang, df_map) in &bigram_dfs {
        let rekeyed = rekey_bigram_df_map(df_map, &remap);
        let weights = compute_ast_bigram_weights(&rekeyed, stats.total_files, 0.0, &vocab);
        // All weights should resolve to valid kind strings
        for w in &weights {
            assert!(!w.parent_kind.is_empty(), "parent_kind should resolve");
            assert!(!w.child_kind.is_empty(), "child_kind should resolve");
            assert!(w.idf > 0.0, "IDF should be positive");
        }
    }
}
```

### MEDIUM

**`NodeKindVocabulary::get_or_insert` overflow uses `debug_assert` only** - `crates/rskim-research/src/ast_types.rs:157-162`
**Confidence**: 80%
- Problem: The u16 overflow guard is a `debug_assert!` which is stripped in release builds. In release mode, if more than 65,535 node kinds are inserted, the `as NodeKindId` cast silently wraps, producing duplicate IDs and corrupting the vocabulary. While the comment notes tree-sitter grammars have ~O(100) kinds, the vocabulary spans 14 languages and their union grows across the corpus. This is not a test issue per se, but the existing tests do not exercise this boundary. There is no test that verifies the `debug_assert` triggers (which would confirm the guard exists).
- Fix: Add a test that runs in debug mode to confirm the assert fires, or upgrade to a runtime check (`anyhow::bail!` / `Result`) to protect release builds as well.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Property-based test for encode/decode roundtrips** - `crates/rskim-research/src/ast_types.rs:324-356` (Confidence: 70%) -- The current roundtrip tests use hand-picked values including 0, 1, 100, 300, and u16::MAX. A property-based test (via `proptest` or `quickcheck`) would exercise the full u16 space and provide higher confidence that all bit patterns roundtrip correctly.

- **`all_14_ts_languages_produce_output` uses weak disjunctive assertion** - `crates/rskim-research/src/ast_extract.rs:492-497` (Confidence: 65%) -- The assertion `!result.bigrams.is_empty() || result.node_count > 0` is disjunctive; for Markdown (the last entry), `bigrams` may be empty if Parser::new returns Err (since Markdown might not have a tree-sitter grammar in the project's Parser), and `node_count > 0` would still be 0. The test would pass but not actually verify AST extraction works for Markdown.

- **No test for `AstGitCloneSource` / `walk_and_load_ast` extension filter** - `crates/rskim-research/src/clone.rs:80-97,372-374` (Confidence: 60%) -- The `AST_TARGET_EXTENSIONS` list (21 extensions) is tested only indirectly through integration with `walk_and_load`. A unit test confirming that `.md`, `.sql`, `.swift` etc. are actually accepted when the AST walker is used would prevent regressions if extensions are modified.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 1 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The test suite is solid with 45 new AST-specific tests plus 12 tests added to modified modules (config, clone), for a total of 57 tests covering the new feature. Encode/decode roundtrips, vocabulary stabilization, IDF computation, serde roundtrips, deduplication, and multi-language extraction are all well-covered. The main gaps are: (1) missing regression tests for the trigram side of the remap fix, (2) no integration test for the full stabilize-rekey-IDF pipeline that was the site of the bug in commit 605203a, and (3) a test with a misleading name that asserts nothing (`error_nodes_counted_but_not_in_bigrams`).
