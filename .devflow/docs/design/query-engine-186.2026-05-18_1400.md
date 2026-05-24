---
issue: "#186"
title: "Wave 1e: Query Engine"
status: implemented
created: 2026-05-18
wave: 1e
depends_on: ["#177", "#178", "#180"]
---

# Query Engine Design — Issue #186

## Architecture Decision

**Option B: Stateless SearchLayer decorator**

`QueryEngine` wraps `Box<dyn SearchLayer>` (typically `NgramIndexReader`) and adds
boundary validation before delegating search. The original `SearchQuery` is passed
unchanged to the inner layer. Stepping stone toward Wave 4 compound engine.

```
SearchQuery
    ↓
QueryEngine::search()
    ├── empty check → Ok(vec![])
    ├── length guard → Err(InvalidQuery) if > MAX_QUERY_BYTES
    ├── BM25F config validation → Err(InvalidQuery) if invalid
    └── delegate (original query unchanged) → inner.search(query) → Vec<SearchResult>
```

### Options Evaluated

| Option | Verdict | Rationale |
|--------|---------|-----------|
| A: Wrap with PreparedQuery | Deferred (Wave 4) | PreparedQuery adds ngram extraction complexity premature for Wave 1e |
| **B: Stateless decorator** | **Chosen** | Lean, composable, no double ngram extraction |
| C: Use format primitives directly | Rejected | Tight coupling, duplicates reader's I/O and validation |
| D: Higher-level orchestrator | Future (Wave 4) | Correct for multi-layer, premature for Wave 1e |

## Scope

### In Scope

1. SearchQuery validation at trust boundary (max length, empty, BM25F config)
2. `QueryEngine` struct implementing `SearchLayer`
3. Query length guard (`MAX_QUERY_BYTES = 4096`)
4. Thread-safety (Send + Sync, `&self` methods only)
5. 15 TDD tests (Send+Sync removed — compiler enforces via trait bound)

### Out of Scope (Deferred)

1. `PreparedQuery` intermediate type — deferred to Wave 4
2. `MAX_NGRAM_BUDGET` — deferred to Wave 4 (MAX_QUERY_BYTES indirectly bounds ngrams)
3. Snippet/line_range extraction — index format v2 lacks line offset table
4. FileId-to-path translation — CLI layer responsibility
5. Search optimizations: cheapest-first, lazy posting list iteration, early termination
6. Corrupt index handling beyond pass-through
7. Multi-layer composition (Wave 4)
8. Performance benchmarks

## File Changes

| File | Action |
|------|--------|
| `crates/rskim-search/src/lexical/query.rs` | Created (~90 lines) |
| `crates/rskim-search/src/lexical/query_tests.rs` | Created (~230 lines, 16 tests) |
| `crates/rskim-search/src/lexical/mod.rs` | Modified (+2 lines: pub mod + pub use) |
| `crates/rskim-search/src/lib.rs` | Modified (+2 re-exports: QueryEngine, MAX_QUERY_BYTES) |

## Type Definitions

```rust
pub const MAX_QUERY_BYTES: usize = 4096;

pub struct QueryEngine {
    inner: Box<dyn SearchLayer>,
}

impl QueryEngine {
    pub fn new(inner: Box<dyn SearchLayer>) -> Self;
}

impl SearchLayer for QueryEngine {
    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>>;
    fn name(&self) -> &str; // "query-engine"
}
```

## Test Coverage (15 tests — all pass)

### Phase 1 — Validation
1. `test_empty_query_returns_empty_vec`
2. `test_oversized_query_returns_invalid_query_error`
3. `test_query_at_exact_max_length_succeeds`
4. `test_invalid_bm25f_config_rejected_before_search`
5. `test_nan_bm25f_config_rejected`
6. `test_name_returns_query_engine`

### Phase 2 — Integration
7. `test_happy_path_finds_matching_file`
8. `test_search_delegates_to_inner_layer`
9. `test_deterministic_results`
10. `test_unicode_query_works`

### Phase 3 — Edge cases
11. `test_whitespace_only_query_returns_empty`
12. `test_single_char_query_returns_empty`
13. `test_no_matching_ngrams_returns_empty`
14. `test_lang_filter_passes_through`
15. `test_pagination_passes_through`

## Design Notes

- **No PreparedQuery**: inline validation is sufficient for Wave 1e; deferred to Wave 4.
- **No MAX_NGRAM_BUDGET**: MAX_QUERY_BYTES at 4096 bytes indirectly bounds ngrams. Explicit
  budget tracking adds complexity without benefit at this stage.
- **Original query passed through**: inner layer retains full control over scoring, filtering,
  and pagination — QueryEngine does not transform the query.
- **BM25F validation is fail-fast**: config validation runs before any index I/O.

## PR Description Guidance

- **Problem:** No boundary validation between callers and NgramIndexReader. Need composable
  SearchLayer decorator for Wave 4 compound engine.
- **Key Changes:** QueryEngine (SearchLayer decorator), MAX_QUERY_BYTES
- **Breaking Changes:** None
- **Reviewer Focus:** Validation logic, decision to pass original SearchQuery to inner layer,
  test coverage completeness
