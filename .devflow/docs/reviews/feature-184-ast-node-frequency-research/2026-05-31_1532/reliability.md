# Reliability Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T15:32

## Issues in Your Changes (BLOCKING)

No CRITICAL or HIGH reliability issues found.

### MEDIUM

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none -- no findings met the 60% threshold)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 9/10
**Recommendation**: APPROVED

## Rationale

This PR demonstrates strong reliability engineering throughout. A systematic review of all five new modules (`ast_extract.rs`, `ast_types.rs`, `ast_idf.rs`, `ast_codegen.rs`, `ast_validate.rs`) and modified modules (`clone.rs`, `config.rs`, `main.rs`) found no reliability violations above the 80% confidence threshold.

### Bounded Iteration

All loops and recursive traversals are properly bounded:

- **`walk_tree` recursion** (`ast_extract.rs:142-204`): Bounded by `MAX_AST_DEPTH = 500` (depth guard at line 150) and `MAX_AST_NODES = 100_000` (node count guard at lines 150, 195). The sibling iteration loop at line 192 terminates via tree-sitter's finite sibling list (`goto_next_sibling()` returns false) or the `MAX_AST_NODES` early-exit. Stack frame size at ~64 bytes per frame means 500 frames = ~32KB, well within the default 8MB thread stack.
- **`extract_ast_ngrams_from_corpus`** (`ast_extract.rs:307-363`): Iterates over a finite, pre-grouped HashMap of languages. No unbounded loops.
- **`process_language_files`** (`ast_extract.rs:218-290`): Iterates over a finite slice of files. No retries.

### Assertion Density

Preconditions and invariants are well-asserted (applies ADR-001):

- `NodeKindVocabulary::get_or_insert` (line 158): Production `assert!` guards against `u16::MAX` overflow -- prevents silent truncation that would corrupt all DF maps.
- `percentile` (line 137): `debug_assert!` on input range `[0, 100]` -- appropriate for a hot-path internal helper (avoids PF-002 -- the `.min()` clamp at line 145 makes the function safe even without the assertion firing in release).
- File-size guard: `MAX_FILE_SIZE = 100 KiB` at line 69 prevents memory spikes from pathological inputs.
- Trigram cap: `MAX_TRIGRAMS_PER_FILE = 50_000` at line 184 bounds HashSet growth.

### Allocation Discipline

- `Vec::with_capacity(df_map.len())` in `rekey_bigram_df_map` and `rekey_trigram_df_map` (lines 100, 117) pre-sizes output maps.
- `Vec::with_capacity(256 * 1024)` in `build_ast_weights_rs` (line 94) pre-sizes the output buffer.
- Counter aggregation in `process_language_files` uses `saturating_add` throughout (lines 239, 243, 261, 262, 266, 270) -- confirmed by prior resolution cycle.

### Overflow Safety

All `as` casts in production code were verified:

- `as NodeKindId` (u16) in `get_or_insert`, `stabilize`: Guarded by the `assert!(len < u16::MAX)` precondition.
- `as u64` for ProgressBar (line 324): Widening cast, always safe.
- `as f32` for error rate (line 74) and mean (line 118): Values bounded by `MAX_AST_NODES` (100K) and language count, within f32 precision.
- `as usize` in `percentile` (line 144): Result clamped by `.min(sorted.len() - 1)`. NaN/negative saturate to 0 in Rust 1.45+.
- Decode casts `(bigram >> 16) as NodeKindId` etc.: Masked by `& 0xFFFF`, always fits u16.

### Error Tolerance

- Non-tree-sitter languages (JSON, YAML, TOML) return empty result instead of error (line 79-81).
- Parse failures return empty result with error node counting (lines 84-87, 160-165).
- File-level extraction failures are logged and skipped (line 252-258) rather than aborting the corpus.
- `total_files_seen` uses `u32::try_from(...).unwrap_or(u32::MAX)` (line 323) rather than an `as` cast.

### Indirection and Metaprogramming

No excessive indirection (`Box<Box<T>>`, deep pointer chains) or complex generics. The `WalkContext` struct bundles mutable traversal state cleanly. No metaprogramming beyond standard derive macros.
