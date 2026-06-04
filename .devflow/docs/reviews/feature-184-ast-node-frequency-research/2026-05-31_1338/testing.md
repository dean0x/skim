# Testing Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T13:38

## Issues in Your Changes (BLOCKING)

### MEDIUM

**No test for `walk_and_load_ast` with AST extension list** - `crates/rskim-research/src/clone.rs:375`
**Confidence**: 85%
- Problem: `walk_and_load_ast` delegates to `walk_and_load(root, Some(AST_TARGET_EXTENSIONS))` and is the entry point for all AST file loading, but no test verifies that the `Some(extensions)` code path in `walk_and_load` correctly accepts AST-specific extensions (`.c`, `.h`, `.cpp`, `.rb`, `.sql`, `.kt`, `.swift`, `.md`, etc.) and does NOT apply the `EXCLUDED_EXTENSIONS` filter. The existing `FixtureSource` tests only exercise the `None` (default lexical) code path. A regression where the exclusion list accidentally applies to the AST path would silently drop Markdown and SQL files from the corpus.
- Fix: Add a fixture-based test using `walk_and_load(root, Some(&["rs", "md"]))` on the existing fixtures directory, verifying that `.md` files are included (they would be excluded in the `None` path via `EXCLUDED_EXTENSIONS`). For example:
```rust
#[test]
fn walk_and_load_with_explicit_extensions_skips_exclusion_list() {
    let dir = fixtures_dir();
    // "md" is in EXCLUDED_EXTENSIONS but should be accepted when explicit.
    let files = walk_and_load(&dir, Some(&["rs", "md"])).unwrap();
    // Should find at least the .rs fixture files.
    assert!(!files.is_empty());
}
```

**No test for `AstGitCloneSource` as `FileSource` trait object** - `crates/rskim-research/src/clone.rs:81-98`
**Confidence**: 82%
- Problem: `AstGitCloneSource` is a new `FileSource` implementation but has no trait-object compatibility test or unit test. The existing `fixture_source_is_trait_object_compatible` test covers `FixtureSource` and `GitCloneSource` has one implicit test, but `AstGitCloneSource` is only tested indirectly through the main binary. If the struct fields or trait implementation regress, there is no fast feedback loop.
- Fix: Add a compile-time trait-object test:
```rust
#[test]
fn ast_clone_source_is_trait_object_compatible() {
    let dir = tempfile::tempdir().unwrap();
    let source: Box<dyn FileSource> = Box::new(AstGitCloneSource {
        corpus_dir: dir.path().to_path_buf(),
    });
    let _ = source;
}
```

**`ast_validate` tests do not verify distribution percentile accuracy at boundaries** - `crates/rskim-research/src/ast_validate.rs:136-142`
**Confidence**: 80%
- Problem: The `percentile` function uses `.round()` for index computation, which can produce incorrect results at distribution boundaries (e.g., p99 on small arrays). The `distribution_stats_correct` test checks p50 but not p90 or p99 values. The `distribution_single_value` test checks min/max/mean but skips median/p90/p99 assertions entirely. Missing coverage for the percentile edge cases.
- Fix: Extend the existing tests to assert on p90 and p99 values:
```rust
#[test]
fn distribution_stats_correct() {
    let values = vec![1.0f32, 2.0, 3.0, 4.0, 5.0];
    let dist = compute_distribution("TestLang", values.into_iter());
    assert_eq!(dist.count, 5);
    assert!((dist.min - 1.0).abs() < 0.01);
    assert!((dist.max - 5.0).abs() < 0.01);
    assert!((dist.mean - 3.0).abs() < 0.01);
    assert!((dist.median - 3.0).abs() < 0.01);
    // p90 of [1,2,3,4,5] at index round(0.9*4)=round(3.6)=4 => 5.0
    assert!((dist.p90 - 5.0).abs() < 0.01, "p90 should be 5.0, got {}", dist.p90);
    // p99 of [1,2,3,4,5] at index round(0.99*4)=round(3.96)=4 => 5.0
    assert!((dist.p99 - 5.0).abs() < 0.01, "p99 should be 5.0, got {}", dist.p99);
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`ast_codegen` tests use `contains` for source verification instead of structural assertions** - `crates/rskim-research/src/ast_codegen.rs:429-451`
**Confidence**: 82%
- Problem: Tests `generated_source_contains_vocabulary`, `generated_source_contains_language_arrays`, and `generated_source_contains_lookup_functions` all use `source.contains(...)` to check for substring presence. These weak assertions would pass even if the generated code is syntactically invalid Rust. The existing lexical `codegen::tests::generate_valid_rust_source` test verifies that output contains `pub const`, `fn bigram_weight`, etc. with slightly more structure, but the AST codegen tests are weaker. A codegen regression that produces malformed Rust would not be caught until the downstream crate fails to compile.
- Fix: Add a test that verifies the generated source is syntactically valid by checking for expected structure markers:
```rust
#[test]
fn generated_source_is_syntactically_structured() {
    let table = sample_table();
    let source = build_ast_weights_rs(&table).unwrap();
    // Verify structural markers
    assert!(source.contains("pub const NODE_KIND_VOCABULARY: &[&str]"));
    assert!(source.contains("pub const RUST_AST_BIGRAM_WEIGHTS: &[(u32, f32)]"));
    assert!(source.contains("pub fn ast_bigram_weight(lang: &str, bigram: u32) -> Option<f32>"));
    assert!(source.contains("#[cfg(test)]"));
    // Verify the generated tests module exists
    assert!(source.contains("fn vocabulary_is_non_empty()"));
}
```

**No test for `NaN` IDF value in `validate_ast_table`** - `crates/rskim-research/src/ast_codegen.rs:55-89`
**Confidence**: 80%
- Problem: `validate_ast_table` checks `!w.idf.is_finite() || w.idf <= 0.0`, which correctly rejects NaN, Infinity, and non-positive values. However, only negative IDF is tested (`negative_idf_returns_error`). There is no test for NaN or positive Infinity, which are the other two cases the guard covers. Since NaN comparisons can be subtle (NaN != NaN), this path deserves explicit coverage.
- Fix: Add NaN and Infinity tests:
```rust
#[test]
fn nan_idf_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("ast_weights.json");
    let out_path = dir.path().join("ast_weights.rs");

    let mut table = sample_table();
    table.bigram_weights.get_mut("Rust").unwrap()[0].idf = f32::NAN;

    let json = serde_json::to_string(&table).unwrap();
    std::fs::write(&json_path, json).unwrap();

    let err = generate_ast_weights_rs(&json_path, &out_path).unwrap_err();
    assert!(err.to_string().contains("invalid IDF"));
}

#[test]
fn infinity_idf_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let json_path = dir.path().join("ast_weights.json");
    let out_path = dir.path().join("ast_weights.rs");

    let mut table = sample_table();
    table.bigram_weights.get_mut("Rust").unwrap()[0].idf = f32::INFINITY;

    let json = serde_json::to_string(&table).unwrap();
    std::fs::write(&json_path, json).unwrap();

    let err = generate_ast_weights_rs(&json_path, &out_path).unwrap_err();
    assert!(err.to_string().contains("invalid IDF"));
}
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Missing integration test for full AST pipeline end-to-end** - `crates/rskim-research/src/main.rs:374-481` (Confidence: 70%) -- The `cmd_ast_run` function orchestrates clone -> extract -> stabilize -> rekey -> IDF -> serialize, but only the extract-stabilize-rekey-IDF sub-pipeline has a dedicated integration test (`stabilize_rekey_idf_pipeline_resolves_correct_kind_names`). The full pipeline including serialization -> deserialization roundtrip is not tested. However, the existing sub-pipeline test is thorough and main.rs is a thin orchestrator, reducing the risk.

- **`ast_codegen` does not verify generated trigram array sort order** - `crates/rskim-research/src/ast_codegen.rs:229-261` (Confidence: 65%) -- `bigram_arrays_are_sorted_ascending` verifies bigram sort order but there is no analogous test for trigram arrays. The code is structurally identical, but a copy-paste error in `write_language_trigram_arrays` could go undetected.

- **`AST_TARGET_EXTENSIONS` and `AST_VALID_LANGUAGES` have no cross-validation test** - `crates/rskim-research/src/clone.rs:31` / `crates/rskim-research/src/config.rs:38` (Confidence: 62%) -- If a new language is added to `AST_VALID_LANGUAGES` but its file extensions are not added to `AST_TARGET_EXTENSIONS` (or vice versa), the corpus would silently produce no files for that language. A cross-validation test could assert that every AST language has at least one matching extension.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 3 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The test suite is solid for a new module with 51 tests covering the new AST infrastructure -- encoding roundtrips, vocabulary stabilization, remap correctness, IDF formula matching, corpus deduplication, error node handling, and a critical integration test guarding the stabilize-rekey-IDF pipeline. The main gaps are: (1) no coverage for the `walk_and_load_ast` extension-filter code path that governs which files enter the corpus, (2) missing edge-case tests for NaN/Infinity IDF validation, and (3) `contains`-based assertions in codegen tests that are weaker than ideal. All findings apply ADR-001. Avoids PF-002 -- all findings surfaced for resolution regardless of blocking status.
