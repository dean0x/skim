# Regression Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**SearchField serde serialization format changed from PascalCase to snake_case** - `crates/rskim-search/src/types.rs:47`
**Confidence**: 90%
- Problem: The `SearchField` enum had `#[derive(Serialize, Deserialize)]` without `rename_all`, producing PascalCase JSON values (e.g., `"TypeDefinition"`, `"FunctionSignature"`). This PR adds `#[serde(rename_all = "snake_case")]` which changes the wire format to `"type_definition"`, `"function_signature"`. While this is a new crate (Wave 0) and there are no known external consumers yet, this is a deliberate breaking change to the serialization contract. Any code that was written against the base branch's PascalCase format (e.g., tests from the previous commit at 0181a51) will break. The tests in this PR were updated to match, confirming the intent, but this represents a serialization-incompatible change that must be tracked.
- Fix: This is an intentional design improvement (aligning serde output with `SearchField::name()` which already returns snake_case). Since this is pre-1.0 and Wave 0 with no external consumers, the risk is acceptable. Document this in the PR description or CHANGELOG as a deliberate format change to prevent future confusion if anyone built against the intermediate commit.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`rskim-search` dependency removed from `rskim` CLI crate but doc comment still references it** - `crates/rskim/src/cmd/search.rs:4`
**Confidence**: 85%
- Problem: Line 4 says "The full search implementation lives in `rskim-search` library crate" and lines 8-9 say "Business logic lives in: `rskim-search` crate". However, the `rskim-search` dependency was removed from `crates/rskim/Cargo.toml` in this PR. The CLI search stub currently has zero dependency on `rskim-search`. The doc comments are now misleading -- they describe an integration that does not yet exist in the dependency graph.
- Fix: Either restore the `rskim-search` dependency (if the intent is for the CLI to consume it in a follow-up PR) or update the doc comments to note that the dependency will be re-added when search is implemented. As-is, the comments describe a future state as if it were current, which could confuse developers working in this area.

**`pub use types::*` narrowed to explicit re-exports without deprecation notice** - `crates/rskim-search/src/lib.rs:14-17`
**Confidence**: 82%
- Problem: The glob re-export `pub use types::*` was replaced with an explicit item list: `pub use types::{FieldClassifier, FileId, IndexStats, ...}`. While the explicit list is strictly better engineering (no accidental leaks), the `Result` type alias is now re-exported where it was previously available through the glob. If any items were added to `types.rs` in the future but not added to the explicit list, they would silently become unavailable through `rskim_search::`. This is not a regression today (all items are covered), but it changes the contract for future additions. Since this is a new crate with no consumers, this is informational.
- Fix: No action needed now. This is the correct pattern. Just be aware that new public types in `types.rs` must be added to the re-export list in `lib.rs`.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Rust edition 2024 formatting changes are purely mechanical but span 100+ files** - (Confidence: 65%) -- The bulk of this PR is `cargo fmt` reformatting for Rust edition 2024 style (import alphabetization, `let-chain` brace placement). These are semantically identical transformations but they touch critical code paths in `cmd/git/diff/parse.rs`, `cmd/infra/gh/api.rs`, `cmd/init/install.rs`, and `cmd/test/cargo.rs`. The reformatting changes indentation of `let-chain` conditional blocks (e.g., `if condition && let Some(x) = ...` moving the brace to a new line). All observed changes preserve logic; no behavioral regression detected.

- **`SearchResult` now derives `Deserialize` in addition to `Serialize`** - `crates/rskim-search/src/types.rs:144` (Confidence: 70%) -- Adding `Deserialize` is additive and does not change existing serialization behavior, but it implies `SearchResult` may be deserialized from external input in the future. If so, the `score: f64` field (which deliberately avoids `PartialEq` due to NaN) could accept NaN from untrusted input. This is a future consideration, not a current regression.

- **Compile-only test `test_public_api_accessible` removed from `lib.rs`** - `crates/rskim-search/src/lib.rs` (Confidence: 60%) -- The test that verified all public types, traits, and the `SearchLayer`/`LayerBuilder`/`FieldClassifier` trait bounds were accessible was removed. Its coverage is now spread across the expanded test suite in `types.rs`. The compile-time trait-bound checks (`fn _assert_search_layer<T: SearchLayer>() {}`) are not replicated elsewhere, but since these traits are used in `types.rs` tests via `Result<()>` returns, the coverage is effectively maintained through usage.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 2 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Conditions

1. **Acknowledge the SearchField serialization format change** -- The PascalCase-to-snake_case change is intentional and correct for alignment with `SearchField::name()`, but it should be documented as a deliberate breaking change in the PR description or a commit message. Since this is Wave 0 with no external consumers, it does not block merge.

2. **Update search.rs doc comments** -- The doc header references `rskim-search` as an active dependency, but the dependency was removed from `Cargo.toml`. Update the comments to reflect that the integration is planned for a future wave.

### Regression Assessment

The vast majority of changes (100+ files) are mechanical `cargo fmt` reformatting for Rust edition 2024. These change import ordering (alphabetical) and `let-chain` brace placement but are semantically identical. All tests pass. The `rskim-search` crate changes are additive (new tests, improved derives, serde alignment) with one intentional serialization format change. No lost functionality, no removed exports, no broken signatures. The removed `rskim-search` dependency from the CLI crate is consistent with the current stub-only state of the search command.
