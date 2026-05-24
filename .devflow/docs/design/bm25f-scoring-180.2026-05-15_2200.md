---
title: "BM25F Scoring Engine with AST-Derived Field Weights"
issue: "#180"
status: draft
created: 2026-05-15
---

# BM25F Scoring Engine — Issue #180

## Goal & Scope

Add BM25F (fielded BM25) scoring to the skim-search lexical index, replacing flat BM25
with a 7-field weighted scoring engine. Terms in type definitions score higher than terms
in string literals. Boosts and parameters are configurable.

## BM25F Formula

```
score(q,d) = Σ_t IDF(t) * tf_weighted(t,d) / (tf_weighted(t,d) + k1)

where tf_weighted(t,d) = Σ_f boost_f * tf(t,d,f) / (1 + b_f * (dl_f/avdl_f - 1))
```

## Field Classification

| Field | Discriminant | Default Boost | Node Kinds |
|-------|-------------|---------------|-----------|
| TypeDefinition | 0 | 5.0 | type_alias_declaration, interface_declaration, struct_item, trait_item, enum_item |
| FunctionSignature | 1 | 4.0 | function_declaration, function_item, method_declaration |
| SymbolName | 2 | 3.5 | identifier children of priority 4/5 nodes (DEFERRED — needs parent context) |
| ImportExport | 3 | 3.0 | import_statement, use_declaration, export_statement |
| FunctionBody | 4 | 1.0 | block, statement_block content |
| Comment | 5 | 0.8 | comment, line_comment, block_comment |
| StringLiteral | 6 | 0.5 | string_literal, template_string |
| Other | 7 | 1.0 | everything else |

## Architecture Decisions

### A. Format Migration: Clean v1→v2 Bump

Bump FORMAT_VERSION from 1 to 2. Reject v1 indexes with a clear error suggesting rebuild.
No dual-reader — pre-1.0 crate with no production indexes to migrate.

### B. FileMetaEntry Expansion (5→37 bytes)

Add `field_lengths: [u32; 8]` after existing `doc_length`. Each element stores the byte
count classified into that SearchField variant.

### C. SkidxHeader Expansion (30→62 bytes)

Add `avg_field_lengths: [f32; 8]` after existing `avg_doc_length`. Pre-computed at build
time to avoid full metadata scan at reader open.

### D. Cross-Crate Exposure

New `pub fn node_kind_priority(kind: &str) -> u8` in rskim-core/lib.rs, delegating to
the existing `pub(crate) node_kind_info()`. Minimal API surface — returns u8, not internal types.

### E. TreeSitterClassifier in rskim-search

Implements `FieldClassifier` trait by matching node.kind strings to SearchField variants.
Lives in rskim-search (where FieldClassifier is defined), not rskim-core.

### F. BM25FConfig as Plain Struct

```rust
pub struct BM25FConfig {
    pub k1: f32,
    pub field_boosts: [f32; 8],
    pub field_b: [f32; 8],
}
```

`Default` impl provides: k1=1.2, boosts from field table above, b=0.75 for all fields.

### G. Builder Integration via `add_file_classified()`

New method on `NgramIndexBuilder`:
```rust
pub fn add_file_classified(
    &mut self, id: FileId, content: &str, lang: Language,
    field_map: &[(Range<usize>, SearchField)],
) -> Result<()>
```

Builder stays pure (no tree-sitter dep). Caller provides pre-computed field classification.
Existing `add_file` delegates with empty field_map (all Other).

### H. Module Structure (follows temporal/ pattern)

```
src/lexical/
  mod.rs              — re-exports
  config.rs           — BM25FConfig (~80 lines)
  config_tests.rs     — config tests (~60 lines)
  scoring.rs          — bm25f_score() (~200 lines)
  scoring_tests.rs    — scoring tests (~250 lines)
  classifier.rs       — TreeSitterClassifier (~150 lines)
  classifier_tests.rs — classifier tests (~200 lines)
```

## File-Level Change List

### New Files (7)

| File | Purpose | ~Lines |
|------|---------|--------|
| src/lexical/mod.rs | Module re-exports | 15 |
| src/lexical/config.rs | BM25FConfig struct | 80 |
| src/lexical/config_tests.rs | Config tests | 60 |
| src/lexical/scoring.rs | bm25f_score() function | 200 |
| src/lexical/scoring_tests.rs | Scoring formula tests | 250 |
| src/lexical/classifier.rs | TreeSitterClassifier | 150 |
| src/lexical/classifier_tests.rs | Classifier tests | 200 |

### Modified Files (8)

| File | Changes |
|------|---------|
| rskim-core/src/lib.rs | +3 lines: expose node_kind_priority() |
| rskim-search/src/lib.rs | Add pub mod lexical; + re-exports |
| rskim-search/src/types.rs | Add SearchField::ALL, count() |
| rskim-search/src/index/format.rs | FORMAT_VERSION→2, header 30→62, meta 5→37 |
| rskim-search/src/index/builder.rs | add_file_classified(), per-field length tracking |
| rskim-search/src/index/reader.rs | Per-field TF accumulation, BM25F scoring, stable sort |
| rskim-search/src/index/format_tests.rs | v2 roundtrip tests |
| rskim-search/src/index/reader_tests.rs | Acceptance + BM25F tests |

## Implementation Steps

### Phase 1: Cross-Crate Prerequisite

1. **Expose `node_kind_priority()` from rskim-core** — Add `pub fn node_kind_priority(kind: &str) -> u8` to rskim-core/lib.rs. Test: verify struct_item→5, unknown→1.

### Phase 2: BM25F Config

2. **Create BM25FConfig with tests** — RED: test defaults match spec. GREEN: implement struct + Default.
3. **Add SearchField::ALL and count()** — Compile-time-checked constant array and count method.

### Phase 3: Classifier

4. **Write failing classifier tests** — Tests for all 7+1 field types.
5. **Implement TreeSitterClassifier** — Match node.kind strings to SearchField variants.

### Phase 4: BM25F Scoring Function

6. **Write failing scoring tests** — Single-field-matches-BM25, boost comparison, zero guards, determinism.
7. **Implement bm25f_score()** — Pure function implementing the BM25F formula with edge case guards.

### Phase 5: Format v2

8. **Write failing format v2 tests** — Header/meta roundtrip for 62/37-byte layouts.
9. **Implement format v2 codec** — Bump version, expand structures, update encode/decode.
10. **Update existing format tests** — Fix SkidxHeader/FileMetaEntry constructors for new fields.

### Phase 6: Builder Integration

11. **Write failing builder tests** — Verify field_id in postings, per-field doc lengths.
12. **Implement add_file_classified()** — Accept field_map, classify bigram positions, track per-field lengths.

### Phase 7: Reader Integration

13. **Write failing reader BM25F tests** — Acceptance criteria #1-4.
14. **Refactor reader scoring loop** — Per-field TF in [f32; 8] arrays, call bm25f_score(), stable sort, populate SearchResult.field.

## Test Strategy

| Criterion | Test | Method |
|-----------|------|--------|
| #1: struct > string | test_bm25f_type_def_outranks_string_literal | Index with field_map, verify ranking |
| #2: Configurable | test_bm25f_configurable_boosts | Override boost, verify ranking reverses |
| #3: Tunable | test_bm25f_tunable_params | Change k1, verify different score |
| #4: Deterministic | test_bm25f_deterministic_ordering | 100 searches, identical ordering |

## Edge Cases

- `avdl_f == 0` → guard to 1.0 (prevents division by zero)
- `boost_f == 0` → skip field entirely (zero contribution)
- Empty files → all field_tfs are 0, score is 0
- Single-field documents → only that field contributes
- Serde languages (JSON/YAML/TOML) → all Other (no tree-sitter AST)

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Builder perf with field_map lookup | Binary search over sorted ranges: O(log n) per byte |
| Format v2 breaks indexes | Pre-1.0, clear error, documented in PR |
| SymbolName underclassified | Falls back to Other; follow-up for parent context |
| Serde language classification | All Other; acceptable for data formats |

## PR Description Guidance

**Problem:** Flat BM25 ignores where terms appear — struct definitions rank same as string literals.

**Key Changes:** New lexical/ module (BM25FConfig, TreeSitterClassifier, bm25f_score); format v2 with per-field doc lengths; builder accepts field classification; reader uses per-field TF.

**Breaking Changes:** Format v1→v2. Existing indexes must be rebuilt.

**Reviewer Focus:** Format v2 codec correctness, BM25F formula accuracy, builder performance.
