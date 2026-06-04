# Complexity Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T15:32

## Issues in Your Changes (BLOCKING)

### HIGH

**`cmd_ast_run` exceeds function length guideline (79 lines, 6 parameters)** - `crates/rskim-research/src/main.rs:387`
**Confidence**: 85%
- Problem: `cmd_ast_run` spans 79 lines with 5 parameters plus the function body orchestrating config loading, file fetching, vocabulary creation, extraction, stabilization, re-keying, IDF computation for two n-gram types, table assembly, serialization, and logging. This exceeds the 50-line critical threshold and makes the sequential pipeline harder to modify. The function handles too many concerns: I/O, extraction orchestration, vocabulary stabilization, per-language weight computation, table construction, and output. It also has 6 conceptual responsibilities (load config, fetch files, extract, stabilize+rekey, compute weights, write output).
- Fix: Extract the extract-stabilize-rekey-compute-weights pipeline into a dedicated function (e.g., `build_ast_weight_table`) that takes `&[SourceFile]`, `threshold`, and `collect_trigrams` and returns `AstWeightTable`. This mirrors how `cmd_run` delegates to `extract::extract_bigrams_from_corpus` + `idf::compute_weight_table`. The command handler would then be: load config, fetch files, build table, write output, log summary -- about 30 lines.

```rust
// Suggested extraction:
fn build_ast_weight_table(
    files: &[types::SourceFile],
    threshold: f32,
    collect_trigrams: bool,
) -> (ast_types::AstWeightTable, ast_types::NodeKindVocabulary) {
    let mut vocab = ast_types::NodeKindVocabulary::new();
    let (raw_bigram_df, raw_trigram_df, corpus_stats) =
        ast_extract::extract_ast_ngrams_from_corpus(files, &mut vocab, collect_trigrams);
    let remap = vocab.stabilize();
    // ... re-key and compute weights ...
    // ... assemble AstWeightTable ...
    (table, vocab)
}
```

### MEDIUM

**Structural duplication across bigram/trigram code generation functions (4 pairs)** - `crates/rskim-research/src/ast_codegen.rs:195,229,263,297`
**Confidence**: 82%
- Problem: Four pairs of functions differ only in type parameters and string literals: `write_language_bigram_arrays` / `write_language_trigram_arrays`, `write_bigram_lookup_fn` / `write_trigram_lookup_fn`. The diff between each pair shows ~90% identical structure with only type names, format string widths, and field accesses differing. This is not mechanical copy-paste (the types genuinely differ between u32 bigrams and u64 trigrams), but it creates a maintenance burden: any codegen format change must be made in two places. `applies ADR-001`
- Fix: Consider a helper trait or a generic helper closure that accepts the type-specific parts (key type name, format width, weight-table accessor, field extractor) and generates the shared structure. For the lookup functions, a single generic that takes the match arms and key type string would halve the code. However, given that this is code-generation (writing strings, not runtime logic), the duplication is more tolerable than in business logic -- a medium-priority item.

**Structural duplication between `validate_repo` and `validate_ast_repo`** - `crates/rskim-research/src/config.rs:99,133`
**Confidence**: 80%
- Problem: `validate_repo` and `validate_ast_repo` share identical URL validation logic and nearly identical commit validation (the AST variant adds `"HEAD"` acceptance). The language validation differs only in which `VALID_LANGUAGES` list is checked. Two of three validation steps are identical, and the third differs by one condition. `applies ADR-001`
- Fix: Extract common validation into a shared helper that takes the valid-languages list and a commit validator function/flag:

```rust
fn validate_repo_common(
    index: usize,
    repo: &RepoEntry,
    valid_languages: &[&str],
    allow_head_commit: bool,
) -> anyhow::Result<()> { ... }
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`main.rs` approaches file length warning zone (562 lines, all production code)** - `crates/rskim-research/src/main.rs`
**Confidence**: 80%
- Problem: `main.rs` has grown to 562 lines with no `#[cfg(test)]` section -- all lines are production code. It now contains 16 functions handling two distinct pipelines (lexical bigrams and AST n-grams). The file is past the 500-line warning threshold. Each new subcommand (ast-run, ast-codegen, ast-validate) added ~170 lines that structurally mirror the existing lexical commands.
- Fix: Extract the AST subcommand handlers (`cmd_ast_run`, `cmd_ast_codegen`, `cmd_ast_validate`, `fetch_all_ast_files`, `write_ast_weight_table`, `log_ast_summary`) into a separate `ast_commands.rs` module. The `main.rs` dispatch would just call into both modules. This separates the two pipelines while keeping the CLI entry point thin.

## Pre-existing Issues (Not Blocking)

_No critical pre-existing complexity issues found in the reviewed files._

## Suggestions (Lower Confidence)

- **Structural duplication between `compute_ast_bigram_weights` and `compute_ast_trigram_weights`** - `crates/rskim-research/src/ast_idf.rs:22,68` (Confidence: 70%) -- The two functions share identical filter-map-sort structure, differing only in decode function and weight struct fields. A generic helper parameterized by a decode+build closure could eliminate this, but the functions are short (38 lines each) and the type differences are genuine, making this a marginal gain.

- **`process_language_files` has 5 parameters** - `crates/rskim-research/src/ast_extract.rs:218` (Confidence: 65%) -- Five parameters is at the warning threshold. However, the prior review cycle (cycle 2) explicitly extracted this function to reduce `extract_ast_ngrams_from_corpus` complexity, and the parameters are all distinct concerns (language name, files, vocabulary, trigram flag, progress bar). Bundling into an options struct would add boilerplate without clarity gain.

- **`walk_tree` recursive function with 5 parameters** - `crates/rskim-research/src/ast_extract.rs:142` (Confidence: 62%) -- The function takes 5 parameters (cursor, context, depth, parent_id, grandparent_id). The doc comment at line 133 explicitly explains why depth/parent/grandparent remain separate from `WalkContext` (stack-local values that shift per recursive call). The `WalkContext` struct already bundles the shared mutable state. This is a justified design choice, not an oversight.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The new AST pipeline modules (`ast_types`, `ast_extract`, `ast_idf`, `ast_validate`, `ast_codegen`) are individually well-structured with good separation of concerns, bounded recursion, clear naming, and comprehensive test coverage. The `walk_tree` function uses a `WalkContext` struct to bundle mutable state (addressing the prior review cycle's feedback), and all loops/recursion have explicit bounds (`MAX_AST_DEPTH`, `MAX_AST_NODES`, `MAX_TRIGRAMS_PER_FILE`). The main complexity concern is the 79-line `cmd_ast_run` orchestrator function and the growing `main.rs` file size, both of which can be addressed by extracting the pipeline core into a reusable function and splitting AST commands into a separate module. The bigram/trigram code generation duplication is a moderate concern given it is string-generation code, but `applies ADR-001` -- noticed issues should be fixed now rather than deferred. `avoids PF-002`
