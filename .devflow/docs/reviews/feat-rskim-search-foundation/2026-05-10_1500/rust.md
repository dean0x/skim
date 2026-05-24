# Rust Review Report

**Branch**: feat-rskim-search-foundation -> main
**Date**: 2026-05-10T15:00

## Issues in Your Changes (BLOCKING)

### MEDIUM

**FileId newtype leaks inner representation via `pub` field** - `crates/rskim-search/src/types.rs:25`
**Confidence**: 85%
- Problem: `FileId(pub u32)` exposes the inner `u32` directly, undermining the newtype pattern. Callers can construct arbitrary `FileId` values via `FileId(999)` and directly access `.0`, bypassing any future validation. The Rust API Guidelines (C-NEWTYPE) recommend keeping the inner field private and providing explicit constructors and accessors.
- Fix: Make the inner field private and add a constructor + accessor:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileId(u32);

impl FileId {
    #[must_use]
    pub fn new(id: u32) -> Self { Self(id) }

    #[must_use]
    pub fn as_u32(self) -> u32 { self.0 }
}
```
  This preserves Display, Serialize/Deserialize behavior. Update the 2 test call-sites to use `FileId::new(0)` etc. Since this is a `publish = false` v0.1.0 crate, the API surface can be corrected now before downstream code grows.

**SearchField serde serialization uses PascalCase but `.name()` returns snake_case** - `crates/rskim-search/src/types.rs:41-74`
**Confidence**: 82%
- Problem: `SearchField::TypeDefinition` serializes as `"TypeDefinition"` (serde default for enums), but `SearchField::TypeDefinition.name()` returns `"type_definition"`. Consumers who need a string representation for the same enum variant will get inconsistent results depending on whether they call `.name()` or serialize. This is a consistency bug that will bite any JSON API built on top of this type.
- Fix: Either add `#[serde(rename_all = "snake_case")]` to the enum to align serde output with `.name()`, or rename `.name()` to return PascalCase strings matching serde. The snake_case approach is more conventional for JSON APIs:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchField {
    // ...
}
```
  Then remove the manual `.name()` method (or have it delegate to serde) and update the serialization test expectations.

**Missing `#[must_use]` on `SearchQuery::new` and `SearchField::name`** - `crates/rskim-search/src/types.rs:116,63`
**Confidence**: 80%
- Problem: Both `SearchQuery::new()` and `SearchField::name()` are pure constructors/accessors whose return values should never be silently discarded. The Rust clippy lint `must_use_candidate` flags these. Adding `#[must_use]` prevents accidental `SearchQuery::new("test");` (discarded query) bugs at compile time.
- Fix:
```rust
#[must_use]
pub fn new(text: impl Into<String>) -> Self { ... }

#[must_use]
pub fn name(self) -> &'static str { ... }
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`SearchResult` missing `Deserialize` derive while having `Serialize`** - `crates/rskim-search/src/types.rs:136`
**Confidence**: 80%
- Problem: `SearchResult` derives `Serialize` but not `Deserialize`. While the comment explains the `PartialEq` omission (NaN), `Deserialize` has no such limitation. For a search library, results often need to be round-tripped through JSON (caching, IPC, persistence). Adding `Deserialize` now avoids a breaking change later when consumers need it.
- Fix: Add `Deserialize` to the derive list:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult { ... }
```
  Note: `Range<usize>` implements `Deserialize` via serde, and `Vec<Range<usize>>` does too.

## Pre-existing Issues (Not Blocking)

No critical pre-existing issues found in the reviewed files.

## Suggestions (Lower Confidence)

- **Edition 2024 if-let chaining formatting inconsistency** - `crates/rskim/src/cmd/heatmap/git_source.rs:226-228` (Confidence: 65%) -- The `&& let` chaining in `parse_git_log_output` places the closing brace on the same line as the condition, while other if-let chains in the same PR (e.g., `signatures.rs:155-157`) use a dedicated line. This is stylistic, but the PR should be internally consistent.

- **`LayerBuilder::build` could return `impl SearchLayer` instead of `Box<dyn SearchLayer>`** - `crates/rskim-search/src/types.rs:209` (Confidence: 60%) -- The `where Self: Sized` bound on `build` already prevents calling it on `dyn LayerBuilder`, suggesting the intent is concrete-type usage. Returning `Box<dyn SearchLayer>` imposes a heap allocation. However, this may be intentional for composability (heterogeneous layer collections), so flagging as a suggestion only.

- **`SearchQuery` all-pub fields could benefit from builder methods** - `crates/rskim-search/src/types.rs:99-112` (Confidence: 62%) -- All fields are `pub` and mutable after construction. Builder methods (`.with_lang()`, `.with_limit()`) would provide a more ergonomic, chainable API and allow future validation at construction time.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 3 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Rust Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The new `rskim-search` crate demonstrates strong Rust fundamentals: proper newtype pattern (FileId), trait-based architecture with Send+Sync bounds, thiserror for library errors, comprehensive clippy lint configuration (`unwrap_used = "deny"`, `expect_used = "deny"`, `panic = "deny"`), clean separation of library/CLI, and pure types with no I/O. The edition 2024 migration and collapsible_if cleanup are clean and correct. The `unsafe` wrapping of `set_var`/`remove_var` for edition 2024 is done correctly with SAFETY comments.

The three blocking MEDIUM items are API design polish that should be addressed before the crate's public API stabilizes: tightening the FileId newtype, aligning SearchField serialization formats, and adding `#[must_use]`. None are correctness bugs -- they are all API hygiene items that are cheapest to fix at v0.1.0 before any downstream code depends on them.
