# Consistency Review Report

**Branch**: feat-rskim-search-foundation -> main
**Date**: 2026-05-10T15:00

## Issues in Your Changes (BLOCKING)

### HIGH

**Glob re-export deviates from rskim-core explicit re-export pattern** - `crates/rskim-search/src/lib.rs:14`
**Confidence**: 90%
- Problem: rskim-search uses `pub use types::*` while rskim-core explicitly names every re-exported symbol: `pub use types::{Language, Mode, Parser, Result, SkimError, TransformConfig, TransformResult}`. The PR description states "should follow rskim-core conventions." Glob re-exports hide the public API surface, making it harder to track breaking changes and understand what is exported without reading the types module.
- Fix:
```rust
pub use types::{
    FieldClassifier, FileId, IndexStats, LayerBuilder, Result, SearchError, SearchField,
    SearchLayer, SearchQuery, SearchResult, TemporalFlags,
};
```

### MEDIUM

**SearchField.name() returns snake_case but serde serializes as PascalCase** - `crates/rskim-search/src/types.rs:63-74`
**Confidence**: 82%
- Problem: `SearchField::TypeDefinition.name()` returns `"type_definition"` but `serde_json::to_string(&SearchField::TypeDefinition)` produces `"TypeDefinition"`. This dual-representation inconsistency means the same enum variant has two different string forms depending on the access path. In rskim-core, `Language.name()` and `Mode.name()` do not have a competing serde representation because those types do not derive Serialize/Deserialize. If the `name()` method is intended for JSON output (e.g., search result fields), it will disagree with serde-derived JSON.
- Fix: Either add `#[serde(rename_all = "snake_case")]` to `SearchField` so serde matches `name()`, or change `name()` to return PascalCase matching the serde default. The former is preferable for API consistency with the explicit snake_case naming:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchField {
    // ...
}
```

**Serde derives on types diverge from rskim-core convention** - `crates/rskim-search/src/types.rs:24,41,84,98,136,157`
**Confidence**: 80%
- Problem: rskim-core types (Language, Mode, TransformConfig, TransformResult) do not derive Serialize/Deserialize -- they are pure library types with serialization handled externally. rskim-search derives Serialize/Deserialize on nearly every type (FileId, SearchField, TemporalFlags, IndexStats, SearchResult). While this may be intentional for a search library that needs JSON output, it couples the core domain types to a serialization format and pulls `serde` as a direct dependency. The PR description explicitly says "should follow rskim-core conventions: types.rs structure, derive conventions." This is a deliberate deviation that should be justified.
- Fix: If serde derives are intentional (likely for JSON search output), add a brief architecture comment explaining why this deviates from rskim-core:
```rust
// ARCHITECTURE: Search types derive Serialize/Deserialize because search results
// are serialized to JSON for CLI output (--json flag). This differs from rskim-core
// where serialization is handled externally.
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Inconsistent let-chain brace style across refactored files** - multiple files
**Confidence**: 85%
- Locations: `crates/rskim/src/cmd/agents/util.rs:8`, `crates/rskim/src/cmd/git/diff/parse.rs:88`, `crates/rskim/src/cmd/git/diff/source.rs:18`, `crates/rskim/src/cmd/git/diff/render.rs:110-111`, `crates/rskim/src/cmd/heatmap/git_source.rs:227`
- Problem: The Rust 2024 let-chain refactoring applies two different brace placement styles. Some instances place the opening brace on the same line as the last `let` condition (K&R style): `&& let Ok(p) = rskim_core::Parser::new(lang) {`. Others place it on a new line after the condition chain (Allman-like): `&& let Some(sig) = extract_signature(...)?\n{`. The codebase should pick one style and apply it uniformly. The K&R style (brace on same line) is more common in this PR.
- Fix: Adopt one brace placement style for all let-chains. The majority of instances in this PR use the opening brace on the same line as the last condition -- standardize the outliers (`structure.rs:300`, `signatures.rs:157`) to match.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Missing Cargo.toml metadata fields** - `crates/rskim-search/Cargo.toml` (Confidence: 65%) -- rskim-core includes `readme`, `keywords`, and `categories` fields. rskim-search omits them. Since `publish = false`, these are not required, but including them maintains structural parity for when publishing is eventually enabled.

- **Test allow annotation inconsistency** - `crates/rskim-search/src/lib.rs:17`, `crates/rskim-search/src/types.rs:259` (Confidence: 70%) -- Both test modules use `#[allow(clippy::unwrap_used)]`, which is consistent with rskim-core/src/types.rs. However, rskim-core/src/lib.rs uses `#[allow(clippy::expect_used)]` instead, with a comment explaining the rationale. This pre-existing inconsistency in rskim-core makes it unclear which convention to follow. Not blocking, but worth noting if the project wants to standardize.

- **SearchQuery missing Serialize/Deserialize while peers have it** - `crates/rskim-search/src/types.rs:98` (Confidence: 62%) -- SearchQuery derives only `Debug, Clone` while SearchResult, SearchField, FileId, TemporalFlags, and IndexStats all derive Serialize and/or Deserialize. If the serde-derive approach is intentional for this crate, SearchQuery is inconsistently excluded. This may be deliberate (queries are input-only, not serialized), but the asymmetry is worth a brief comment.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 7/10
**Recommendation**: CHANGES_REQUESTED
