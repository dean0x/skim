# Testing Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-11

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Missing trait contract tests for `SearchLayer` and `LayerBuilder`** - `crates/rskim-search/src/types.rs`
**Confidence**: 82%
- Problem: The two primary traits (`SearchLayer`, `LayerBuilder`) define the core API contract for the entire search system, but only `FieldClassifier` has a concrete-implementation test (`test_field_classifier_concrete_impl`). There is no equivalent test demonstrating that a `SearchLayer` impl can be constructed and invoked, or that a `LayerBuilder` can add files and produce a layer. Since these traits will be the integration point for Waves 1-6, a contract test now catches signature drift before downstream implementors exist.
- Fix: Add a minimal mock/fake implementation of both traits within the test module:

```rust
#[test]
fn test_search_layer_contract() {
    struct FakeLayer;
    impl SearchLayer for FakeLayer {
        fn search(&self, _query: &SearchQuery) -> Result<Vec<SearchResult>> {
            Ok(vec![])
        }
        fn name(&self) -> &str { "fake" }
    }
    let layer = FakeLayer;
    assert_eq!(layer.name(), "fake");
    let results = layer.search(&SearchQuery::new("x")).unwrap();
    assert!(results.is_empty());
}

#[test]
fn test_layer_builder_contract() {
    struct FakeBuilder;
    struct FakeLayer;
    impl SearchLayer for FakeLayer {
        fn search(&self, _: &SearchQuery) -> Result<Vec<SearchResult>> { Ok(vec![]) }
        fn name(&self) -> &str { "fake" }
    }
    impl LayerBuilder for FakeBuilder {
        fn add_file(&mut self, _id: FileId, _content: &str, _lang: rskim_core::Language) -> Result<()> { Ok(()) }
        fn build(self) -> Result<Box<dyn SearchLayer>> where Self: Sized { Ok(Box::new(FakeLayer)) }
    }
    let mut builder = FakeBuilder;
    builder.add_file(FileId(0), "fn main() {}", rskim_core::Language::Rust).unwrap();
    let layer = builder.build().unwrap();
    assert_eq!(layer.name(), "fake");
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Missing edge case: SearchResult with NaN score serialization** - `crates/rskim-search/src/types.rs:341` (Confidence: 65%) -- The doc comment on `SearchResult` explicitly notes that `score: f64` cannot implement `PartialEq` because `NaN != NaN`, yet no test verifies that NaN scores roundtrip correctly through JSON (serde serializes NaN as `null` by default, which will fail deserialization back to `f64`). A test verifying the behavior (or documenting that NaN scores are invalid input) would protect future consumers.

- **Missing error path test for `SearchLayer::search` returning `Err`** - `crates/rskim-search/src/types.rs` (Confidence: 62%) -- All existing tests exercise the happy path or test error Display. There is no test demonstrating a `SearchLayer` impl returning an error variant from `search()`, which would exercise that the `Result` type alias and `SearchError` work correctly through the trait boundary.

- **CLI stub does not verify stderr output message** - `crates/rskim/src/cmd/search.rs:106` (Confidence: 70%) -- `test_search_unimplemented_returns_failure` only asserts the exit code. It does not verify the "not yet implemented" message appears on stderr. If the stub message changes or disappears, the test still passes silently. Consider capturing stderr in an integration test.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The test suite for a foundation/types crate is well-structured. Tests follow Arrange-Act-Assert, target observable behavior (serialization contracts, Display output, roundtrip correctness), and avoid implementation coupling. The `test_search_field_serde_agrees_with_name` test is a standout pattern — it structurally verifies that two sources of truth cannot drift. The single blocking item (MEDIUM severity) is the absence of trait contract tests for the two primary API traits, which are the integration surface for all future waves.
