# Architecture Review Report

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-11

## Issues in Your Changes (BLOCKING)

### HIGH

**SearchQuery not Serialize/Deserialize despite being a search API boundary type** - `crates/rskim-search/src/types.rs:119-148`
**Confidence**: 82%
- Problem: `SearchResult`, `IndexStats`, `SearchField`, `TemporalFlags`, and `FileId` all derive `Serialize + Deserialize`, but `SearchQuery` does not. The module comment explains serde is needed for `--json` CLI output, yet queries are equally likely to cross serialization boundaries (e.g., query logging, debug output, RPC if search becomes client-server, persisted query history). The asymmetry is architecturally inconsistent within a single module that uses serde for all other public types.
- Fix: Add `#[derive(Serialize, Deserialize)]` to `SearchQuery`. The `Language` field from `rskim-core` already derives `Serialize + Deserialize`, so there is no blocker.

---

**`LayerBuilder::build` consumes `self` but `add_file` takes `&mut self` — prevents builder reuse for incremental indexing** - `crates/rskim-search/src/types.rs:215-228`
**Confidence**: 80%
- Problem: The builder pattern moves ownership on `build(self)`, making it impossible to retain a builder for incremental re-indexing (add more files after building a read-only snapshot). For a code search system where files change frequently, this forces a full rebuild on every change. The PR description mentions Waves 1-6 building on this foundation — an incremental update path should at least be expressible via the trait.
- Fix: Consider either (a) adding a `fn snapshot(&self) -> Result<Box<dyn SearchLayer>>` that borrows instead of consuming, or (b) documenting in the trait doc that incremental indexing is intentionally deferred to a separate trait (e.g., `IncrementalBuilder`). If the intent is "build once, query many times" this is fine — but the intent should be explicit in the trait documentation.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`rskim-search` depends on `rskim-core` for only 2 types (`Language`, `SkimError`) — tight coupling to a large crate** - `crates/rskim-search/Cargo.toml:12`, `crates/rskim-search/src/types.rs:124,220,273`
**Confidence**: 85%
- Problem: `rskim-search` pulls in the entire `rskim-core` crate (which transitively brings in all tree-sitter grammars) for only two items: `rskim_core::Language` (an enum) and `rskim_core::SkimError` (for `From` conversion). This couples a search-foundation library to the full AST parsing infrastructure. Any breaking change in `rskim-core` forces an `rskim-search` release even if search functionality is unaffected. The PR description explicitly calls this "a pure library crate" — the dependency graph contradicts that characterization.
- Fix: Extract `Language` and `SkimError` into a shared `rskim-types` crate (or use a feature-gated re-export), so `rskim-search` doesn't transitively depend on tree-sitter. Alternatively, accept this coupling as intentional (since search will eventually need `rskim-core` for AST classification) and document the rationale in `Cargo.toml` comments.

---

**CLI search stub does not use `rskim-search` types at all** - `crates/rskim/src/cmd/search.rs`
**Confidence**: 83%
- Problem: The search CLI stub is entirely self-contained with no import of any `rskim-search` type. The `rskim-search` dependency is wired only as a dev-dependency for "compile-time canary" purposes. While the stub is intentionally minimal, having the foundation crate and its CLI consumer share zero types means the API contract is tested only by "does it compile" — not by "does the CLI actually use the public API." This leaves the integration unvalidated architecturally.
- Fix: Import at least one type from `rskim-search` in the search CLI module (even if unused in the stub body, as a `use rskim_search::SearchQuery;` at the top) to establish a real integration point. Alternatively, the dev-dep canary approach works if a test in `crates/rskim/tests/` actually exercises the search API — which currently none do.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**Workspace edition 2024 migration applies `if let` chain syntax that is unstable-looking but valid** - multiple files in `crates/rskim/src/cmd/`
**Confidence**: 60% (below threshold — moved to Suggestions)

## Suggestions (Lower Confidence)

- **TemporalFlags has only one field** - `crates/rskim-search/src/types.rs:106` (Confidence: 65%) — A struct with a single `Option<u32>` field may be premature abstraction; a plain `Option<u32>` in `SearchQuery` could suffice until the temporal model requires multiple fields. However, this may be intentional forward-design for Wave 2+.

- **NodeInfo uses `&'static str` for `kind` field** - `crates/rskim-search/src/types.rs:244` (Confidence: 70%) — This requires callers to provide string literals or leaked strings. For non-tree-sitter languages that compute node kinds dynamically, this could force allocation. An `Arc<str>` or `Cow<'static, str>` would be more flexible, though the `&'static str` choice is idiomatic for tree-sitter node kinds which are always compile-time constants.

- **`FileId(pub u32)` limits index to ~4 billion files** - `crates/rskim-search/src/types.rs:30` (Confidence: 62%) — For a code search tool operating within a single repository this is fine; for a multi-repo global index it could be limiting. The PR description scopes this to single-project use, making u32 appropriate.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 0 | - |
| Should Fix | - | 0 | 2 | - |
| Pre-existing | - | - | 0 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The architecture is fundamentally sound. The crate introduces clean trait-based abstractions (`SearchLayer`, `LayerBuilder`, `FieldClassifier`) with correct dependency direction (search depends on core, not vice versa). The `NodeInfo` abstraction successfully decouples `FieldClassifier` from tree-sitter. The separation of I/O (CLI) from logic (library) follows the existing project pattern. The builder/layer split correctly separates mutation (indexing) from immutable querying.

Conditions for approval:
1. Address the `SearchQuery` serde inconsistency (HIGH) — either add derives or document why it is intentionally excluded.
2. Document the `LayerBuilder::build` ownership semantics (HIGH) — clarify whether incremental re-indexing is deferred to a future trait or out of scope.
