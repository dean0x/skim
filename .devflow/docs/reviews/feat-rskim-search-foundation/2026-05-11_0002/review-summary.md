# Code Review Summary

**Branch**: feat/rskim-search-foundation -> main
**Date**: 2026-05-11 / Timestamp: 2026-05-11_0002

## Merge Recommendation: CHANGES_REQUESTED

This PR introduces a well-architected foundation crate (`rskim-search`) with solid engineering practices. The multiple cross-cutting changes (edition 2024 migration, thiserror upgrade, 52 collapsible_if refactors) are low-risk and verified. However, **5 MEDIUM-severity blocking issues** across Architecture, Testing, Consistency, and Rust domains must be addressed before merge. All are straightforward fixes (add derives, add trait tests, document semantics).

---

## Issue Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW | Total |
|----------|----------|------|--------|-----|-------|
| Blocking | 0 | 2 | 3 | 0 | **5** |
| Should Fix | 0 | 0 | 2 | 0 | **2** |
| Pre-existing | 0 | 0 | 1 | 0 | **1** |

**Overall Scores** (aggregated):
- Security: 9/10 (APPROVED)
- Architecture: 8/10 (APPROVED_WITH_CONDITIONS)
- Performance: 9/10 (APPROVED)
- Complexity: 9/10 (APPROVED)
- Consistency: 8/10 (APPROVED_WITH_CONDITIONS)
- Regression: 9/10 (APPROVED)
- Testing: 8/10 (APPROVED_WITH_CONDITIONS)
- Rust: 9/10 (APPROVED_WITH_CONDITIONS)
- Dependencies: 9/10 (APPROVED)

---

## Blocking Issues (Must Fix Before Merge)

### Category 1: Issues in Your Changes

#### HIGH (2 items)

**1. SearchQuery not Serialize/Deserialize** - `crates/rskim-search/src/types.rs:119-148`
- **Confidence**: 82%
- **Reviewer**: Architecture
- **Problem**: `SearchResult`, `IndexStats`, `SearchField`, `TemporalFlags`, and `FileId` all derive `Serialize + Deserialize`, but `SearchQuery` does not. This is asymmetric and inconsistent given that queries are equally likely to cross serialization boundaries (logging, debug output, potential RPC layers, persisted history).
- **Impact**: HIGH — API inconsistency that limits composability and future extensibility.
- **Fix**: Add `#[derive(Serialize, Deserialize)]` to `SearchQuery`. The `Language` field from `rskim-core` already derives both traits, so there is no blocker.

---

**2. LayerBuilder::build consumes self, preventing incremental indexing** - `crates/rskim-search/src/types.rs:215-228`
- **Confidence**: 80%
- **Reviewer**: Architecture
- **Problem**: The builder pattern moves ownership on `build(self)`, making it impossible to retain a builder for incremental re-indexing (add more files after building). For a code search system with frequent file changes, this forces full rebuilds on every change. The PR description mentions Waves 1-6 building on this foundation — incremental indexing should be expressible.
- **Impact**: HIGH — Architectural constraint that will require workarounds in Waves 1+.
- **Fix**: Either (a) add a `fn snapshot(&self) -> Result<Box<dyn SearchLayer>>` method that borrows instead of consuming, or (b) clearly document in trait doc comments that incremental indexing is intentionally deferred to a separate trait (e.g., `IncrementalBuilder`). Choose based on intended Wave 1 design.

---

### Category 2: Issues in Code You Touched (Should Fix)

#### MEDIUM (3 items; deconflicted from HIGH above; counted as BLOCKING for merge)

**3. Inconsistent thiserror derive style** - `crates/rskim-search/src/types.rs:269`
- **Confidence**: 92%
- **Reviewer**: Consistency
- **Problem**: `rskim-core` uses `use thiserror::Error;` with `#[derive(Debug, Error)]`. The new `rskim-search` crate uses fully-qualified `#[derive(Debug, thiserror::Error)]` without an import. This is an intra-workspace style inconsistency.
- **Impact**: MEDIUM — Style consistency and maintainability.
- **Fix**: Add `use thiserror::Error;` at the top and change to `#[derive(Debug, Error)]` to match `rskim-core`.

---

**4. Missing PartialEq/Eq on TemporalFlags and IndexStats** - `crates/rskim-search/src/types.rs:105`, `crates/rskim-search/src/types.rs:180`
- **Confidence**: 82%
- **Reviewer**: Consistency
- **Problem**: In `rskim-core`, simple data-holding structs without floats consistently derive `PartialEq` and `Eq` (see `Language`, `Mode`). `TemporalFlags` (only `Option<u32>`) and `IndexStats` (only integers and `Option<u64>`) are equality-comparable but lack these derives, limiting testability (callers cannot use `assert_eq!`).
- **Impact**: MEDIUM — Test ergonomics and API consistency.
- **Fix**: Add `PartialEq, Eq` to both types:
  ```rust
  #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
  pub struct TemporalFlags { ... }
  
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct IndexStats { ... }
  ```

---

**5. Missing trait contract tests for SearchLayer and LayerBuilder** - `crates/rskim-search/src/types.rs`
- **Confidence**: 82%
- **Reviewer**: Testing
- **Problem**: The two primary traits (`SearchLayer`, `LayerBuilder`) define the core API contract for the entire search system, but no test demonstrates their implementation. Only `FieldClassifier` has a concrete-implementation test. These traits are the integration surface for Waves 1-6 — contract tests now catch signature drift before downstream implementors exist.
- **Impact**: MEDIUM — Integration risk; missing test coverage of critical API surface.
- **Fix**: Add two minimal mock/fake implementations within the test module demonstrating both traits can be implemented:
  ```rust
  #[test]
  fn test_search_layer_contract() {
      struct FakeLayer;
      impl SearchLayer for FakeLayer {
          fn search(&self, _query: &SearchQuery) -> Result<Vec<SearchResult>> { Ok(vec![]) }
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
          fn build(self) -> Result<Box<dyn SearchLayer>> { Ok(Box::new(FakeLayer)) }
      }
      let mut builder = FakeBuilder;
      builder.add_file(FileId(0), "fn main() {}", rskim_core::Language::Rust).unwrap();
      let layer = builder.build().unwrap();
      assert_eq!(layer.name(), "fake");
  }
  ```

---

## Additional Issues (Should Fix)

### Category 2: Code You Touched (HIGH Priority, Non-Blocking Suggestions)

**NodeInfo.kind uses &'static str, potentially constraining non-tree-sitter implementors** - `crates/rskim-search/src/types.rs:244`
- **Confidence**: 82%
- **Reviewers**: Rust (MEDIUM blocking for that domain)
- **Problem**: `NodeInfo` uses `pub kind: &'static str` for the grammar rule name. Tree-sitter nodes naturally provide `&'static str`, but non-tree-sitter languages (JSON, YAML, TOML) referenced in doc comments would need to use string literals or `Box::leak()` to produce `&'static str`, which is ergonomically awkward and leaks memory.
- **Assessment**: This is a **MEDIUM-level issue in the Rust review domain** but doesn't block the PR if the constraint is documented. Either switch to `Cow<'static, str>` or explicitly document that all node kinds must be compile-time constants (which is likely true for JSON, YAML, TOML).
- **Recommendation**: Document the constraint clearly in trait docs: "All `NodeInfo.kind` values must be `&'static str` (compile-time constants). Tree-sitter provides these naturally; other parser implementations must use string literals or static references." Or switch to `Cow<'static, str>` for full flexibility.

---

**rskim-search depends on rskim-core only for 2 types** - `crates/rskim-search/Cargo.toml:12`
- **Confidence**: 85%
- **Reviewer**: Architecture (MEDIUM but categorized as "Should Fix")
- **Problem**: `rskim-search` pulls in the entire `rskim-core` crate (14 tree-sitter grammars transitively) for only two items: `Language` (an enum) and `SkimError` (error type). This couples the pure-library crate to the full AST parsing infrastructure.
- **Assessment**: This is acknowledged as intentional given this is a foundation crate, but the tight coupling contradicts the "pure library" characterization. In Wave 1, when search layers need language-aware logic, this coupling will be justified. For now, document the decision.
- **Recommendation**: Add a comment in `rskim-search/Cargo.toml`:
  ```toml
  [dependencies]
  rskim-core = { path = "../rskim-core", version = "0.1" }
  # NOTE: rskim-core provides Language enum for type safety and SkimError for
  # error interop. Full tree-sitter dependency is acceptable here because Wave 1
  # search layers will require language-aware parsing. Future refactoring could
  # extract Language + SkimError into a shared rskim-types crate if the coupling
  # becomes prohibitive.
  ```

---

## Pre-existing Issues (Informational Only, Not Blocking)

**Heavy transitive dependency via rskim-core** - `crates/rskim-search/Cargo.toml:12`
- **Confidence**: 82%
- **Reviewer**: Dependencies
- **Problem**: As noted above; acknowledged as pre-existing architectural pattern.
- **Note**: This does not block the PR since `publish = false` limits impact.

---

## Action Plan

**Before Merge:**
1. Add `#[derive(Serialize, Deserialize)]` to `SearchQuery` (HIGH #1) — 1 line
2. Resolve `LayerBuilder::build` ownership semantics (HIGH #2) — Either add `snapshot()` method or document in trait docs (5-10 lines)
3. Fix thiserror import style (MEDIUM #3) — 2 lines
4. Add `PartialEq, Eq` to `TemporalFlags` and `IndexStats` (MEDIUM #4) — 2 lines
5. Add mock implementations of `SearchLayer` and `LayerBuilder` traits (MEDIUM #5) — ~40 lines of test code
6. Document the `NodeInfo.kind: &'static str` constraint or switch to `Cow<'static, str>` (Rust domain MEDIUM) — 5-10 lines or refactor

**After Merge (Wave 1 Planning):**
- Consider extracting `Language` and `SkimError` into a shared `rskim-types` crate to decouple `rskim-search` from tree-sitter
- Confirm incremental indexing design with Wave 1 implementors

---

## Detailed Assessment by Domain

### Security (9/10 — APPROVED)
- Pure library architecture with no I/O, network, filesystem, or crypto exposure
- Strict clippy lints enforced (no `unwrap_used`, `expect_used`, or `panic` in production)
- Existing security controls preserved through edition 2024 refactors
- thiserror 2.0 upgrade is a drop-in replacement with no security implications
- Only low-confidence suggestions (query size bounds, limit defaults) are future-proofing, not blocking

### Architecture (8/10 — APPROVED_WITH_CONDITIONS)
- Strategy Pattern correctly routes languages to appropriate parsers
- Clean trait-based abstractions (`SearchLayer`, `LayerBuilder`, `FieldClassifier`) with correct dependency direction
- `NodeInfo` decouples `FieldClassifier` from tree-sitter — good for non-tree-sitter languages
- **Blocking issues**: Missing serde derives on `SearchQuery`, unclear `LayerBuilder::build` ownership semantics
- **Should-fix**: rskim-core coupling acknowledged but intentional for Wave 1

### Performance (9/10 — APPROVED)
- Foundation crate contains only types/traits — no algorithmic hot paths
- Well-designed type choices: `FileId(u32)` is Copy-friendly, `SearchField` is Copy enum, `SearchField::name()` returns `&'static str` for zero-allocation lookups
- Trait design supports parallelism: `SearchLayer: Send + Sync`
- Edition 2024 if-let chaining maintains identical performance to nested-if patterns
- No regression risk to existing code paths

### Complexity (9/10 — APPROVED)
- New `rskim-search` crate has zero control-flow complexity
- Production code in `types.rs` is only ~298 lines (661 total with 362 test lines)
- 52 collapsible_if refactors uniformly reduce nesting depth — net complexity improvement
- All functions are trivial (complexity 1-2), max nesting depth 1
- CLI stub is intentionally minimal

### Consistency (8/10 — APPROVED_WITH_CONDITIONS)
- Edition 2024 matches all workspace crates post-upgrade
- Clippy lint configuration matches `rskim-core` exactly
- Section separators, test annotations, doc style all consistent
- **Blocking issues**: thiserror import style inconsistency, missing `PartialEq`/`Eq` derives

### Regression (9/10 — APPROVED)
- thiserror 2.0 is a drop-in replacement (no breaking changes in any used patterns)
- Edition 2024 migration complete across all 3 crates with correct semantics
- All 52 collapsible_if changes verified to preserve semantics
- New `rskim-search` crate is purely additive
- New `search` CLI subcommand is additive with no conflicts
- All 3333 tests pass

### Testing (8/10 — APPROVED_WITH_CONDITIONS)
- Test suite well-structured with Arrange-Act-Assert pattern
- Tests target observable behavior (serialization contracts, roundtrips)
- `test_search_field_serde_agrees_with_name` is standout pattern — prevents drift between sources of truth
- **Blocking issue**: Missing trait contract tests for `SearchLayer` and `LayerBuilder` — these are the integration surface for all Waves 1-6
- Existing tests avoid implementation coupling (good)

### Rust (9/10 — APPROVED_WITH_CONDITIONS)
- Proper `thiserror` usage with `#[from]` conversions
- Newtype pattern (`FileId`) for type-safe identifiers (C-NEWTYPE idiomatic)
- `#[must_use]` on critical functions
- Clean separation: pure library, CLI at boundary
- Correct `pub(crate)` boundaries
- **MEDIUM issue**: `NodeInfo.kind: &'static str` may constrain non-tree-sitter implementors — document or use `Cow<'static, str>`
- Edition 2024 upgrade applied correctly
- Dev-dependency canary pattern (rskim importing rskim-search) validates API at compile time

### Dependencies (9/10 — APPROVED)
- thiserror 2.0.17, serde 1.0.228, serde_json 1.0.145 — all well-maintained, no known CVEs
- License audit: MIT/Apache-2.0 compatible
- Supply chain: verified publishers (dtolnay, serde-rs org)
- Pre-existing coupling to rskim-core is acknowledged but intentional
- No MSRV declared after edition 2024 upgrade (Rust 1.85+ required) — low-confidence suggestion to add to Cargo.toml

---

## Summary

This PR introduces a well-engineered foundation for the rskim-search system with solid architecture, extensive tests, and low regression risk. The multiple cross-cutting changes (edition 2024, thiserror 2.0, 52 collapsible_if fixes) are all verified as safe. However, **5 MEDIUM-severity blocking items** must be addressed:

1. Add serde derives to `SearchQuery` for API consistency
2. Document or refactor `LayerBuilder::build` ownership semantics
3. Fix thiserror import style inconsistency
4. Add `PartialEq`/`Eq` to `TemporalFlags` and `IndexStats`
5. Add trait contract tests for `SearchLayer` and `LayerBuilder`

All are straightforward fixes (< 50 lines total). Once completed, this PR is **APPROVED** for merge.

**Merge Status**: ⏳ **CHANGES_REQUESTED** — Address 5 items above, then request re-review.
