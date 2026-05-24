# Consistency Review Report

**Branch**: feat/193-custom-field-mapping-json-yaml-toml -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Stale module-level doc comment in classifier.rs** - `crates/rskim-search/src/lexical/classifier.rs:13-17`
**Confidence**: 95%
- Problem: The module doc comment under "# Non-tree-sitter languages" still reads: "For languages where `Language::to_tree_sitter` returns `None` (JSON, YAML, TOML), the entire source is classified as `SearchField::Other` -- a single range `0..source.len()`." This is now factually wrong -- JSON, YAML, and TOML are dispatched to format-specific classifiers before the tree-sitter path and produce rich field classifications.
- Fix: Update lines 13-17 to describe the new dispatch behavior. For example:
  ```rust
  //! # Format-specific languages
  //!
  //! JSON, YAML, and TOML are dispatched to dedicated scanners in
  //! [`crate::fields::serde_fields`] before the tree-sitter path. Markdown
  //! uses a custom tree-sitter classifier in [`crate::fields::markdown`].
  //! These produce format-appropriate field classifications (TypeDefinition,
  //! SymbolName, StringLiteral, etc.) instead of a single `Other` range.
  ```

**Stale comments in boundary test** - `crates/rskim-search/src/lexical/classifier_tests.rs:87,90-92`
**Confidence**: 90%
- Problem: The `test_source_at_limit_boundary_does_not_error` test has two stale comments: (1) line 87 says "it returns a single Other range without touching the parser" -- JSON now goes through the format-specific scanner, not a single Other range. (2) lines 90-92 say "Json parser returns an error (unsupported for tree-sitter)" -- JSON no longer errors; it returns a properly classified Vec. The test logic itself still works correctly (it verifies `FileTooLarge` is not triggered), but the comments mislead future readers.
- Fix: Update the comments to reflect the new behavior:
  ```rust
  // We use JSON so this stays fast even at 100 MiB;
  // the format-specific scanner classifies without tree-sitter parsing.
  let at_limit = " ".repeat(MAX_SOURCE_BYTES);
  let result = classify_source(&at_limit, rskim_core::Language::Json);
  // The size guard must not fire at exactly MAX_SOURCE_BYTES.
  ```

**Test naming convention inconsistency** - `crates/rskim-search/src/fields/fields_tests.rs`
**Confidence**: 82%
- Problem: The existing test naming convention in this codebase uses `test_` prefix (e.g., `test_empty_source_returns_empty`, `test_json_field_mapping_non_trivial`, `test_rust_struct_contains_type_definition`). The new `fields_tests.rs` uses a different convention with short prefixes like `f_json_01_`, `f_yaml_02_`, `f_toml_03_`, `f_md_04_`, `c_01_`, `i_01_`. While this prefixed/numbered scheme is internally consistent and arguably more organized, it departs from the established `test_` prefix pattern used in all other test files (`classifier_tests.rs`, `manifest_tests.rs`, `scoring_tests.rs`, `format_tests.rs`, etc.).
- Fix: This is a style choice that has internal consistency (all tests within `fields_tests.rs` follow the same numbering scheme). Since the new file is a separate module with its own scope, the deviation is tolerable but worth noting. No change required unless project style mandates uniform `test_` prefixing across all modules.

### LOW

**Section separator style inconsistency** - `crates/rskim-search/src/fields/fields_tests.rs`
**Confidence**: 80%
- Problem: The new `fields_tests.rs` uses `// ============` (equals) separators, while the existing `classifier_tests.rs` uses `// --------` (dashes) separators. Other production files in the codebase (`ngram.rs`, `types.rs`, `manifest_tests.rs`) also use `// ============`, so both styles exist. The new file matches production code style, but not the sibling test file it most closely relates to.
- Fix: Not blocking. Both separator styles coexist in the codebase. The new file is internally consistent.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Return type inconsistency between sibling classifiers** - `crates/rskim-search/src/fields/serde_fields.rs` vs `crates/rskim-search/src/fields/markdown.rs`
**Confidence**: 85%
- Problem: The three serde classifiers (`classify_json`, `classify_yaml`, `classify_toml`) return `Vec<(Range<usize>, SearchField)>` (infallible), while `classify_markdown` returns `crate::Result<Vec<(Range<usize>, SearchField)>>` (fallible). This is an intentional design decision documented in the module doc (`mod.rs:6-8`), and the PR description explicitly calls out "infallible, return Vec" for serde scanners. However, `classify_markdown` never actually returns `Err` in practice -- both error branches (`Parser::new` failure, `parse` failure) return `Ok(vec![(0..len, SearchField::Other)])` as fallback. The `Result` wrapping only exists because it calls `crate::fields::markdown::classify_markdown` which in turn passes through `crate::Result`, but the function itself is effectively infallible.
- Fix: This is an intentional and documented design choice (Markdown uses tree-sitter which could theoretically fail, while serde scanners are pure byte-level). The inconsistency is justified. Consider adding a brief note to `classify_markdown`'s doc comment clarifying that while the return type is `Result`, the function is fault-tolerant and never returns `Err` in practice. Alternatively, if guaranteed infallibility is desired, the error branches could be changed to not propagate via `?` and the return type could be unified to `Vec`.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**Duplicated test helper functions** - `crates/rskim-search/src/fields/fields_tests.rs:20-54` and `crates/rskim-search/src/lexical/classifier_tests.rs:13-44`
**Confidence**: 85%
- Problem: `assert_contiguous` and `assert_field_lengths_sum` are copy-pasted between two test files. Both test modules need these helpers to validate classifier output invariants. The implementations are nearly identical (the only difference is parameter naming: `len` vs `source_len`). If the contract invariants change, both copies must be updated.
- Fix: Consider extracting shared test helpers into a `#[cfg(test)]` utility module (e.g., `src/test_helpers.rs` or a `testutil` submodule) that both test files can import. This is not blocking -- duplicate test helpers are common in Rust projects -- but it would reduce maintenance burden as the number of classifier test files grows.

## Suggestions (Lower Confidence)

- **fields_tests.rs imports `classify_source` but uses it only in integration tests** - `crates/rskim-search/src/fields/fields_tests.rs:11` (Confidence: 65%) -- The integration tests (I-01 through I-06) test through the `classify_source` dispatch point. These could arguably live in `classifier_tests.rs` since they test the dispatch behavior of `classify_source`, not the field classifiers directly. However, grouping them with the field-level tests keeps all format-specific test coverage in one file, which has its own merits.

- **`in_key_stack` unbounded growth** - `crates/rskim-search/src/fields/serde_fields.rs:68` (Confidence: 60%) -- The `in_key_stack` Vec grows proportionally to JSON nesting depth with no cap. While deeply nested JSON files are uncommon in practice, the project's reliability rules require "every loop, retry, and resource has an explicit bound." Consider adding a `MAX_JSON_DEPTH` constant (e.g., 256) after which nested objects are treated as Other.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 3 | 1 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Consistency Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new code is well-structured and internally consistent. The serde scanners follow a uniform pattern (infallible, byte-level, same return type), the test file has thorough coverage with clear naming, and the module organization is clean. The main consistency issues are stale documentation comments in the existing `classifier.rs` module that now describe behavior that has been replaced by this PR. These should be updated before merge to prevent confusion for future readers. The test naming convention deviation is notable but acceptable given the file's internal consistency.
