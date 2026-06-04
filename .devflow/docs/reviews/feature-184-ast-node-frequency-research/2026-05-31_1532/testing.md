# Testing Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T15:32

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**No test coverage for `main.rs` subcommand handlers (`cmd_ast_run`, `cmd_ast_codegen`, `cmd_ast_validate`)** - `crates/rskim-research/src/main.rs:387-551`
**Confidence**: 85%
- Problem: The three new AST subcommand handlers (`cmd_ast_run`, `cmd_ast_codegen`, `cmd_ast_validate`) and the shared helpers (`log_ast_summary`, `fetch_all_ast_files`, `write_ast_weight_table`) have zero test coverage. These functions orchestrate the entire AST pipeline -- config loading, corpus fetching, vocabulary stabilization, re-keying, IDF computation, JSON serialization, and codegen file writing. While the individual modules they call are well-tested, the integration/orchestration logic (e.g., the correct sequencing of `stabilize()` -> `rekey()` -> IDF, correct default path resolution, correct `write_json_table` label strings) is only validated by manual runs. The existing `cmd_run`, `cmd_codegen`, and `cmd_validate` handlers for the lexical pipeline also lack tests (pre-existing), but the new AST handlers introduce the same gap in fresh code. (applies ADR-001 -- fix noticed issues immediately; avoids PF-002 -- not deferring this as pre-existing)
- Fix: Add at least one integration test per AST subcommand handler that exercises the full pipeline with fixture data (reusing `FixtureSource` and `tempfile`). For example:
  ```rust
  #[test]
  fn ast_pipeline_end_to_end_with_fixtures() {
      // 1. Load fixture files via FixtureSource or walk_and_load_ast
      // 2. extract_ast_ngrams_from_corpus
      // 3. stabilize + rekey
      // 4. compute IDF weights
      // 5. Build AstWeightTable, serialize to JSON, deserialize back
      // 6. Assert vocabulary, weights, and stats are non-empty and consistent
  }
  ```
  Note: `stabilize_rekey_idf_pipeline_resolves_correct_kind_names` in `ast_extract` partially covers this but does not exercise the JSON serialization or codegen output paths.

### MEDIUM

**`top_bigrams` test asserts the sample size rather than the cap behavior** - `crates/rskim-research/src/ast_validate.rs:318-323`
**Confidence**: 82%
- Problem: The test `top_bigrams_capped_at_20` asserts `== 6` (the sample table size) rather than testing the actual capping behavior when input exceeds 20. The test name promises "capped at 20" but never verifies the cap is applied -- it only confirms that inputs smaller than the cap pass through unchanged.
- Fix: Add a test case with >20 bigrams in the sample table and assert the output is truncated to exactly 20:
  ```rust
  #[test]
  fn top_bigrams_actually_caps_at_20() {
      let mut table = sample_table();
      let bigrams: Vec<AstBigramWeight> = (0..30)
          .map(|i| AstBigramWeight {
              parent_kind: format!("p_{i}"),
              child_kind: format!("c_{i}"),
              bigram: encode_ast_bigram(i as u16, (i + 1) as u16),
              idf: 10.0 - (i as f32 * 0.1),
          })
          .collect();
      table.bigram_weights.insert("Rust".to_string(), bigrams);
      let report = run_ast_validation(&table);
      assert_eq!(report.per_language[0].top_bigrams.len(), 20);
  }
  ```

**No test for `rekey_bigram_df_map` when multiple old keys map to the same new key (merge-on-collision)** - `crates/rskim-research/src/ast_types.rs:96-107`
**Confidence**: 80%
- Problem: `rekey_bigram_df_map` uses `*new_map.entry(new_key).or_default() += count` which accumulates counts when two old bigrams remap to the same new bigram. This collision-merge behavior is critical for correctness but has no test. The existing `rekey_bigram_df_map_preserves_counts` test uses a 1:1 remap and never triggers a collision.
- Fix: Add a test where two distinct old bigrams remap to the same new key and verify counts are summed:
  ```rust
  #[test]
  fn rekey_bigram_df_map_merges_colliding_keys() {
      // Create a remap where old IDs 0 and 1 both map to new ID 0.
      let remap: Vec<NodeKindId> = vec![0, 0];
      // Two bigrams: (0,0) and (1,1), both become (0,0) after remap.
      let mut df_map = HashMap::new();
      df_map.insert(encode_ast_bigram(0, 0), 10u32);
      df_map.insert(encode_ast_bigram(1, 1), 20u32);
      let rekeyed = rekey_bigram_df_map(&df_map, &remap);
      let merged_key = encode_ast_bigram(0, 0);
      assert_eq!(rekeyed[&merged_key], 30, "counts should sum on collision");
  }
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`percentile` function has no test for boundary percentiles (0th and 100th)** - `crates/rskim-research/src/ast_validate.rs:136-146`
**Confidence**: 82%
- Problem: The `percentile` function handles 0.0 and 100.0 as valid inputs (enforced by `debug_assert!`), but no test exercises `percentile(sorted, 0.0)` or `percentile(sorted, 100.0)`. The `distribution_stats_correct` test only exercises p50, p90, and p99. Edge behavior at 0th and 100th percentile is untested -- especially relevant because `round()` on `0.0 * (n-1)` and `100.0 * (n-1)` exercises both ends of the index range and the `min()` clamp.
- Fix: Add boundary cases to the existing distribution test or as a separate test:
  ```rust
  #[test]
  fn percentile_boundary_values() {
      let sorted = vec![1.0f32, 2.0, 3.0, 4.0, 5.0];
      assert!((percentile(&sorted, 0.0) - 1.0).abs() < 0.01);
      assert!((percentile(&sorted, 100.0) - 5.0).abs() < 0.01);
  }
  ```

**`empty_source_returns_empty_result` test does not verify `node_count == 0`** - `crates/rskim-research/src/ast_extract.rs:384-390`
**Confidence**: 80%
- Problem: The test checks `bigrams.is_empty()`, `trigrams.is_empty()`, and `error_node_count == 0`, but does not assert `node_count == 0`. For consistency with other tests (e.g., `non_tree_sitter_language_returns_empty` which does assert `node_count == 0`), and to fully validate the empty-input contract, `node_count` should also be asserted.
- Fix: Add `assert_eq!(result.node_count, 0);` to the test.

## Pre-existing Issues (Not Blocking)

(none -- pre-existing lexical pipeline handlers also lack integration tests, but those were not modified in this PR)

## Suggestions (Lower Confidence)

- **No property-based test for encode/decode roundtrips** - `crates/rskim-research/src/ast_types.rs:340-371` (Confidence: 70%) -- The roundtrip tests use a fixed set of boundary values. A property-based test using `proptest` or similar would strengthen confidence that the bit-packing is correct for all `u16` combinations, though the current boundary-value selection is reasonable.

- **`all_14_ts_languages_produce_output` uses a weak disjunctive assertion** - `crates/rskim-research/src/ast_extract.rs:548-576` (Confidence: 65%) -- The assertion `!result.bigrams.is_empty() || result.node_count > 0` passes if either condition holds. For most languages, both should hold. The disjunction may mask a language where bigrams are unexpectedly empty but nodes are counted.

- **Generated Rust source from `build_ast_weights_rs` is not compilation-tested** - `crates/rskim-research/src/ast_codegen.rs:428-479` (Confidence: 65%) -- Tests verify substring presence in the generated source but do not compile it. A `trybuild` or `syn::parse_file` test would catch generated code that is syntactically invalid.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

### Rationale

The test suite is solid overall -- 107 tests pass, all new modules have at least 7 tests each, and critical behaviors (encode/decode roundtrips, IDF formula correctness, deduplication, stabilize+rekey pipeline, error-node exclusion, boundary inputs) are well-covered. The main gap is the absence of integration tests for the three new AST subcommand orchestration paths in `main.rs`, which chain multiple modules together in a specific sequence. The `stabilize_rekey_idf_pipeline_resolves_correct_kind_names` integration test in `ast_extract` is excellent and covers the core pipeline correctness concern, but the full orchestration through JSON serialization, codegen output, and validation reporting remains untested. The remaining findings are lower-severity test precision issues.
