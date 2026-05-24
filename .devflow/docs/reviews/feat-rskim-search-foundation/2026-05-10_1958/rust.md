# Rust Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

### MEDIUM

**SearchField::name() duplicates serde rename_all behavior** - `crates/rskim-search/src/types.rs:67-81`
**Confidence**: 82%
- Problem: `SearchField::name()` manually maps each variant to its `snake_case` string, but `#[serde(rename_all = "snake_case")]` (line 47) already provides this mapping via `serde_json::to_string`. This creates a dual-maintenance burden -- if a new variant is added, both the enum and the `name()` method must be updated, and they could drift out of sync. The test at line 403 explicitly verifies they agree, but the duplication is the root issue.
- Fix: Consider deriving `strum::Display` with `#[strum(serialize_all = "snake_case")]` or using `serde_json::to_value(self)` to extract the string name from serde, eliminating the manual match. Alternatively, if the manual match is intentional for performance (avoiding allocation), add a compile-time test that statically asserts exhaustive coverage, or rely on the existing exhaustive match (which the compiler already enforces). The current approach is defensible but worth documenting the rationale.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **FileId inner field is pub with no validation** - `crates/rskim-search/src/types.rs:30` (Confidence: 65%) -- `FileId(pub u32)` allows construction of arbitrary values. The doc comment explains this is intentional for posting-list efficiency, but a `FileId::new(id: u32)` constructor could coexist with the pub field for callers that want semantic clarity. Low priority given the documented rationale.

- **SearchResult Deserialize may silently accept NaN scores** - `crates/rskim-search/src/types.rs:144` (Confidence: 62%) -- `SearchResult` now derives `Deserialize` (added in this PR). The `score: f64` field will accept JSON `null` or special float values from non-standard JSON parsers. Since the doc comment explicitly notes NaN concerns (line 142-143), consider adding `#[serde(deserialize_with = "...")]` validation if untrusted JSON input is expected in the future. Not blocking since the PR description says this is Wave 0 types-only.

- **Result type alias shadows std::result::Result** - `crates/rskim-search/src/types.rs:255` (Confidence: 70%) -- `pub type Result<T> = std::result::Result<T, SearchError>;` is a common Rust pattern but shadows the std prelude within any file that does `use rskim_search::Result`. The re-export from `lib.rs` (line 15) makes this available to all consumers. This is idiomatic for error-crate libraries (anyhow, thiserror examples do this), but downstream code that uses both `rskim_search::Result` and `std::result::Result` will need to disambiguate. Standard Rust practice -- not a defect.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 9/10
**Recommendation**: APPROVED_WITH_CONDITIONS

## Analysis Notes

### What This PR Does

This PR has two distinct parts:

1. **rskim-search crate (Wave 0)**: Pure types, traits, and error definitions for a 3-layer code search system. The crate contains `FileId` (newtype), `SearchField` (AST-aware field classification), `SearchQuery`, `SearchResult`, `IndexStats`, three traits (`SearchLayer`, `LayerBuilder`, `FieldClassifier`), and a `SearchError` enum with `thiserror` integration. A CLI stub at `crates/rskim/src/cmd/search.rs` registers the subcommand.

2. **Edition 2024 formatting migration**: The vast majority of changed lines (90+ files) are reformatting of `if let ... && let ...` chains from edition 2021 style (body indented inside the last `&&` arm) to edition 2024 style (closing brace on same indent level as `if`). This is a mechanical `cargo fmt` change with zero semantic impact.

### Rust-Specific Strengths

- **Newtype pattern applied correctly**: `FileId(pub u32)` prevents accidental integer misuse, with `Display`, `Ord`, `Hash` derives for use as map keys. Follows C-NEWTYPE.
- **thiserror 2.0 used for library errors**: `SearchError` uses `#[from]` for `rskim_core::SkimError` and `std::io::Error`, with domain-specific variants (`IndexCorrupted`, `InvalidQuery`, `FileNotFound`). Clean error hierarchy.
- **Trait design separates build from query**: `LayerBuilder` (mutable build phase) produces `Box<dyn SearchLayer>` (immutable query phase). `Send + Sync` bounds on `SearchLayer` enable concurrent search. `where Self: Sized` on `build()` prevents calling on `dyn LayerBuilder`.
- **#[must_use] on constructors**: Both `SearchField::name()` and `SearchQuery::new()` are annotated.
- **Clippy lint configuration**: `unwrap_used = "deny"`, `expect_used = "deny"`, `panic = "deny"` in the new crate's `Cargo.toml`. Tests correctly use `#[allow(clippy::unwrap_used)]`.
- **No unsafe code** in the new crate.
- **Result type alias** follows the idiomatic `pub type Result<T> = std::result::Result<T, SearchError>` pattern.
- **Dependency removed from rskim binary**: `rskim-search` was correctly removed from `crates/rskim/Cargo.toml` since the CLI stub doesn't use search types yet. Clean separation.
- **serde alignment**: `#[serde(rename_all = "snake_case")]` on `SearchField` ensures JSON output matches `name()` method output. `SearchResult` gained `Deserialize` for roundtrip capability.
- **Edition 2024 let-chain formatting**: Mechanical change, no semantic impact. All 3,323 tests pass.
