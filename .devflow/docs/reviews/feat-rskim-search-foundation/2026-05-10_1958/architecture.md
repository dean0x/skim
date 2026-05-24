# Architecture Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-10T19:58
**PR**: #213

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**rskim-search library depends on tree-sitter concrete type in FieldClassifier trait** - `crates/rskim-search/src/types.rs:223`
**Confidence**: 85%
- Problem: The `FieldClassifier` trait takes `&tree_sitter::Node<'_>` as a parameter, coupling the search library's public API directly to the tree-sitter concrete type. This violates DIP — the search layer's public trait boundary now depends on a specific parser implementation. Any future parser backend (e.g., for data formats like JSON/YAML that already use serde-based parsing in rskim-core) cannot implement `FieldClassifier` without importing tree-sitter. The existing project architecture explicitly routes non-tree-sitter languages through a Strategy Pattern in `Language::transform_source()`, so the search layer should follow the same principle and not assume tree-sitter is the only parser.
- Fix: Introduce an abstraction layer. Either:
  (a) Define a `NodeInfo` struct in rskim-search that captures the fields `FieldClassifier` actually needs (node kind, byte range, child count, etc.), and have the caller convert `tree_sitter::Node` to `NodeInfo` before calling `classify`.
  (b) Move `FieldClassifier` to rskim-core where tree-sitter is already an implementation detail, and re-export it from rskim-search.

  ```rust
  // Option (a): Abstract node representation
  pub struct NodeInfo<'a> {
      pub kind: &'a str,
      pub byte_range: std::ops::Range<usize>,
      pub named_child_count: usize,
  }

  pub trait FieldClassifier: Send + Sync {
      fn classify(&self, node: &NodeInfo<'_>, source: &str) -> SearchField;
  }
  ```

### MEDIUM

**SearchField::name() duplicates serde rename_all serialization** - `crates/rskim-search/src/types.rs:67-81`
**Confidence**: 82%
- Problem: The `SearchField::name()` method manually maps each variant to its snake_case string, which is identical to what `#[serde(rename_all = "snake_case")]` already provides for serialization. This creates a maintenance burden: adding a new variant requires updating both the enum (with correct serde behavior) and the `name()` match arm. If they diverge, the serialized form and the programmatic name will silently disagree. This violates the DRY principle and is a coupling maintenance risk.
- Fix: Either remove `name()` entirely and use serde serialization where the name is needed, or derive the name from a single source of truth. If `name()` must exist for non-serde contexts (e.g., display without serde_json), consider using `strum` or a macro to derive it from the variant names, ensuring it stays in sync with `rename_all`.

**rskim binary removed rskim-search dependency but keeps search stub** - `crates/rskim/Cargo.toml` (line 17 removed) and `crates/rskim/src/cmd/search.rs`
**Confidence**: 80%
- Problem: The `rskim` binary's `Cargo.toml` had the `rskim-search` dependency removed in this PR, but the `search.rs` CLI stub remains and does not import anything from the search crate. This means the types, traits, and error handling defined in rskim-search are currently orphaned — no binary consumes them, and the library has no downstream integration test via the CLI. The PR description says "Wave 0" which implies this is intentional staging, but architecturally this creates a gap: the library's public API surface has no compile-time consumer outside its own unit tests. API design problems (missing derives, wrong trait bounds, lifetime issues) may only surface when the binary re-adds the dependency in a future wave.
- Fix: Consider adding a minimal integration test in the workspace that imports rskim-search from the binary crate's test scope, or keep the dependency in `[dev-dependencies]` of `rskim` so the binary's tests can validate the search API surface. This would be a lightweight compile-time canary.

  ```toml
  # crates/rskim/Cargo.toml
  [dev-dependencies]
  rskim-search = { version = "0.1.0", path = "../rskim-search" }
  ```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

### MEDIUM

**Dual Result type aliases across crates risk confusion** - `crates/rskim-search/src/types.rs:255` and `crates/rskim-core/src/types.rs:780`
**Confidence**: 82%
- Problem: Both `rskim-core` and `rskim-search` define `pub type Result<T>` aliases with different error types (`SkimError` vs `SearchError`). When a downstream consumer imports from both crates, unqualified `Result` becomes ambiguous. The `SearchError` already wraps `SkimError` via `#[from]`, so the error hierarchy is correct, but the naming collision will create friction when the binary re-integrates rskim-search. This is a known Rust ecosystem pattern (e.g., `std::io::Result`), but since both crates are in the same workspace and will be used together, it benefits from explicit disambiguation.
- Fix: This is informational — the standard Rust pattern is to qualify as `rskim_core::Result` or `rskim_search::Result` at call sites. No action needed now, but consider whether the binary should use `anyhow::Result` (which it already does) at the integration seam and reserve crate-specific `Result` aliases for internal use only.

## Suggestions (Lower Confidence)

- **SearchQuery lacks Builder pattern** - `crates/rskim-search/src/types.rs:106` (Confidence: 65%) -- SearchQuery has 6 optional fields that must be set by direct field access. A builder pattern (`SearchQuery::new("text").lang(Language::Rust).limit(10)`) would provide a cleaner API and allow validation at build time, consistent with the project's "validate at boundaries" principle.

- **TemporalFlags may be too narrow for future temporal queries** - `crates/rskim-search/src/types.rs:91-95` (Confidence: 62%) -- Currently only `modified_within_days: Option<u32>` is defined. If the search layer needs richer temporal queries (modified after date, created before, etc.), this struct will need breaking changes. Consider whether an enum-based temporal filter would be more extensible.

- **Edition 2024 let-chain reformatting is mechanical but invasive** - multiple files (Confidence: 70%) -- The ~90 files of formatting changes from edition 2024 let-chain syntax are mixed into the same PR as the rskim-search architectural work. This makes the architectural diff harder to review. Consider splitting edition migration into a separate PR for cleaner git history.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Architecture Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

### Rationale

The overall architecture is sound: clean separation of a pure-types library crate (`rskim-search`) from the CLI layer (`rskim`), proper use of `thiserror` for typed error hierarchies, correct `#[from]` chains for error propagation, and the builder/layer trait split (`LayerBuilder` -> `SearchLayer`) follows the immutable-after-construction pattern well. The edition 2024 migration and `thiserror` 2.0 upgrade are clean.

The primary architectural concern is the `FieldClassifier` trait coupling the search library's public API to tree-sitter's concrete `Node` type, which contradicts the project's existing Strategy Pattern for parser dispatch and will prevent non-tree-sitter languages from participating in field classification. This should be addressed before the trait boundary solidifies, as changing trait signatures after downstream implementations exist is a breaking change.
