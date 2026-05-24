# Architecture Review Report

**Branch**: feat-rskim-search-foundation -> main
**Date**: 2026-05-10T15:00

## Issues in Your Changes (BLOCKING)

### MEDIUM

**Unused dependency: `rskim` binary crate depends on `rskim-search` but never imports it** - `crates/rskim/Cargo.toml:17`
**Confidence**: 95%
- Problem: The `rskim` binary crate declares `rskim-search = { version = "0.1.0", path = "../rskim-search" }` as a dependency, but no source file in `crates/rskim/src/` imports anything from `rskim_search`. The `cmd/search.rs` stub is entirely self-contained. This adds the full `rskim-search` dependency tree (including `rskim-core` transitively again and `tree-sitter` directly) to compile time without any compile-time or runtime benefit.
- Fix: Either remove the dependency from `crates/rskim/Cargo.toml` until the search CLI actually uses library types, or add a minimal import in `cmd/search.rs` to justify the dependency now (e.g., re-export types for future use). The first option is cleaner:
```toml
# Remove this line until search CLI integration actually uses rskim-search types
# rskim-search = { version = "0.1.0", path = "../rskim-search" }
```

**`FileId` newtype has public inner field, undermining encapsulation** - `crates/rskim-search/src/types.rs:25`
**Confidence**: 82%
- Problem: `pub struct FileId(pub u32)` is documented as an "opaque numeric identifier" that "prevents accidental misuse of IDs as raw integers," but the `pub` visibility on the inner `u32` field means callers can freely construct arbitrary `FileId` values and access the raw integer directly (`id.0`). This contradicts the stated purpose of the newtype pattern and reduces it to a transparent wrapper. If the intent is truly opaque, construction should go through a factory or a dedicated `new()` method, and access through an accessor.
- Fix: Either make the inner field private and add explicit constructors/accessors, or update the documentation to reflect that `FileId` is a transparent wrapper (which is acceptable for a foundation crate where index-builders need to create IDs):
```rust
// Option A: Make truly opaque
pub struct FileId(u32);

impl FileId {
    pub fn new(id: u32) -> Self { Self(id) }
    pub fn value(self) -> u32 { self.0 }
}

// Option B: Keep pub but fix the doc comment (lower-effort, acceptable)
/// Numeric identifier for a file in the search index.
///
/// Uses a newtype for type safety (prevents mixing with other u32 values).
/// The inner field is public for construction by index builders.
pub struct FileId(pub u32);
```

**`pub use types::*` glob re-export limits API surface control** - `crates/rskim-search/src/lib.rs:14`
**Confidence**: 80%
- Problem: The library root uses `pub use types::*` to re-export everything from the `types` module. For a foundation crate that will grow over time and be consumed by both `rskim` binary and potentially external users (`publish = false` today, but that could change), wildcard re-exports make it impossible to control what is part of the public API without modifying the `types` module itself. Adding a helper function or internal constant to `types.rs` accidentally makes it public. This violates the Interface Segregation Principle for modules.
- Fix: Replace with explicit re-exports:
```rust
pub use types::{
    FileId, FieldClassifier, IndexStats, LayerBuilder, Result, SearchError,
    SearchField, SearchLayer, SearchQuery, SearchResult, TemporalFlags,
};
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`SearchField` serde representation mismatch with `name()` method** - `crates/rskim-search/src/types.rs:41-74` (Confidence: 68%) -- `SearchField` serializes as PascalCase via default serde derive (`"TypeDefinition"`) but `name()` returns snake_case (`"type_definition"`). If both APIs are used in the same JSON output (e.g., a field name from `name()` alongside a serialized field value), consumers will see inconsistent casing. Consider whether `#[serde(rename_all = "snake_case")]` on the enum or removing `name()` in favor of serde would be more consistent.

- **`SearchResult` missing `Deserialize`** - `crates/rskim-search/src/types.rs:136` (Confidence: 65%) -- `SearchResult` derives `Serialize` but not `Deserialize`, while all other data types (`FileId`, `SearchField`, `TemporalFlags`, `IndexStats`) derive both. If search results will ever be persisted or transmitted across a boundary and read back, the asymmetry will require a breaking change. The omission appears intentional (score: f64 NaN concern noted in the comment), but NaN is a serialization concern, not a deserialization concern.

- **`tree-sitter` as direct dependency of rskim-search** - `crates/rskim-search/Cargo.toml:16` (Confidence: 62%) -- The `FieldClassifier` trait uses `tree_sitter::Node` in its signature, coupling the search crate directly to the tree-sitter version. If the library is meant to be a pure types/traits crate, consider whether `FieldClassifier` belongs here or should live in a separate adapter crate that bridges tree-sitter and search. This is a minor concern for now since tree-sitter is already a workspace dependency.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 3 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Assessment

The architecture of this PR is well-designed overall. Key strengths:

1. **Clean dependency direction**: `rskim-search` depends on `rskim-core` (not the reverse), and the `rskim` binary depends on both. No circular dependencies. Dependencies point inward correctly per Clean Architecture.

2. **Proper separation of concerns**: The library crate is explicitly I/O-free, with traits accepting pre-parsed data. The CLI stub in `cmd/search.rs` correctly owns all I/O. This follows the Hexagonal Architecture port/adapter pattern.

3. **Builder/Query separation**: The `LayerBuilder` (mutable build phase) vs `SearchLayer` (immutable query phase) split is a good application of the Builder pattern, with the `where Self: Sized` bound on `build()` correctly preventing object-safe ambiguity.

4. **Thread safety by design**: `SearchLayer: Send + Sync` and `FieldClassifier: Send + Sync` enforce concurrent access patterns at the trait level, while `LayerBuilder: Send` allows transfer across threads without requiring shared access during construction.

5. **Error type composition**: `SearchError` properly wraps `SkimError` via `#[from]`, maintaining error chain fidelity without leaking implementation details.

6. **Edition 2024 + thiserror 2.0 upgrade**: The if-let chaining refactors across ~30 files are purely mechanical and correct, reducing nesting without changing semantics.

The three MEDIUM issues are all about tightening encapsulation and API hygiene -- important for a foundation crate, but not blocking since the crate is `publish = false` and the API surface is small.
