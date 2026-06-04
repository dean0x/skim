# Architecture Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T15:32

## Issues in Your Changes (BLOCKING)

### MEDIUM

**`cmd_ast_run` orchestration lives in `main.rs` (50+ lines of sequencing logic)** - `crates/rskim-research/src/main.rs:387-465`
**Confidence**: 82%
- Problem: `cmd_ast_run` directly orchestrates the full extract-stabilize-rekey-IDF-serialize pipeline in ~80 lines of inline procedural code inside `main.rs`. The lexical pipeline (`cmd_run`) has the same pattern at ~50 lines. Both embed domain logic (stabilize, rekey, weight computation) in the CLI binary rather than exposing a single composable entry point from the library. This means any future consumer (benchmark harness, test, or second binary) must duplicate the stabilize-then-rekey-then-compute-IDF sequencing, which is error-prone (the remap bug in commit 605203a already demonstrated this).
- Fix: Extract a `pub fn build_ast_weight_table(files: &[SourceFile], collect_trigrams: bool, threshold: f32) -> AstWeightTable` function in the library (e.g., in `ast_extract.rs` or a new `ast_pipeline.rs`) that encapsulates the extract -> stabilize -> rekey -> IDF -> assemble sequence. `cmd_ast_run` would then just call this after cloning. This parallels how well-structured CLI tools keep `main.rs` as a thin dispatch layer.

**Parallel lexical/AST pipelines share structure but no abstraction** - `crates/rskim-research/src/main.rs:157-210,387-465`
**Confidence**: 80%
- Problem: `cmd_run` and `cmd_ast_run` follow the same 5-step pattern (load config -> resolve corpus dir -> fetch files -> compute weights -> write JSON) but share no common abstraction. Similarly, `cmd_codegen`/`cmd_ast_codegen` and `cmd_validate`/`cmd_ast_validate` are near-identical with different type parameters. This is not yet a SOLID violation since there are only two pipelines, but each new n-gram type (e.g., quadgrams, syntax-path n-grams) would require copy-pasting another ~80 lines of orchestration. The `write_json_table` and `fetch_files_parallel` extractions (already done in this PR) show the right instinct -- the remaining pipeline body is the next candidate.
- Fix: Consider a trait or generic pipeline struct that parameterizes the extraction and weight computation steps while sharing the clone-fetch-write skeleton. Not blocking because the current duplication is manageable at two instances, but flag for next iteration.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`AstWeightTable.bigram_weights` sorted inconsistently across modules** - `crates/rskim-research/src/ast_idf.rs:54`, `crates/rskim-research/src/ast_codegen.rs:204`, `crates/rskim-research/src/ast_validate.rs:60-61`
**Confidence**: 83%
- Problem: `compute_ast_bigram_weights` in `ast_idf.rs` sorts weights by IDF descending. `write_language_bigram_arrays` in `ast_codegen.rs` re-sorts by bigram key ascending. `run_ast_validation` in `ast_validate.rs` assumes IDF-descending order to pick top-20 via `.take(20)`. The weight data flows through multiple modules with implicit ordering assumptions. If a future change moves the sort or inserts a step between IDF computation and codegen, the codegen binary-search tables could silently break, or the validation report could show wrong "top" entries.
- Fix: Document the sort contract explicitly at the type level. Either: (a) Always store unsorted and sort at each use site (current approach but with a doc comment on `AstBigramWeight` stating "no guaranteed order"), or (b) Sort once in the pipeline and document the invariant with `debug_assert!(is_sorted_by(...))` at consumption sites. Option (a) is simpler and matches what is effectively happening now -- just add the doc comment.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`NodeKindVocabulary` could enforce a "frozen after stabilize" invariant** - `crates/rskim-research/src/ast_types.rs:151` (Confidence: 70%) -- After calling `stabilize()`, calling `get_or_insert()` would corrupt the alphabetical ordering and invalidate all encoded bigram/trigram keys. A typestate pattern (`Vocabulary<Building>` vs `Vocabulary<Stable>`) or a `frozen: bool` flag with a runtime check would prevent this misuse at the API level. Currently safe because the only caller (`cmd_ast_run`) sequences correctly, but the API permits misuse.

- **`WalkContext` uses mutable references to individual counters instead of owning them** - `crates/rskim-research/src/ast_extract.rs:109-122` (Confidence: 65%) -- `WalkContext` borrows `&mut u32` for `error_count` and `node_count` and borrows `&mut HashSet` for bigrams/trigrams. This works but creates a lifetime dependency that prevents `WalkContext` from being stored or returned. Owning the counters and sets directly in `WalkContext` and extracting them into `AstFileResult` after the walk would simplify lifetimes and make the API more self-contained.

- **`ast-corpus.toml` accepts `"HEAD"` but the lexical `corpus.toml` validator does not** - `crates/rskim-research/src/config.rs:110` (Confidence: 72%) -- The AST validator accepts `"HEAD"` as a commit reference for convenience, but this means builds are not reproducible across runs (HEAD moves). The prior resolution cycle pinned all 16 entries to SHA hashes. Consider either: removing `"HEAD"` support entirely (since all entries are now pinned), or adding a `--allow-head` flag so HEAD is opt-in rather than silently accepted.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 8/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

The new AST n-gram pipeline is architecturally sound. Key strengths:

1. **Clean module decomposition** -- Five new modules (`ast_types`, `ast_extract`, `ast_idf`, `ast_codegen`, `ast_validate`) mirror the existing lexical pipeline's module structure exactly, each with a single responsibility. This is well-aligned with SRP.

2. **Good use of the Strategy Pattern** -- `FileSource` trait with `AstGitCloneSource` vs `GitCloneSource` cleanly separates the extension-filtering concern. The `walk_and_load(root, extensions)` parameterization avoids branching on "which pipeline" inside the walker.

3. **Two-pass design is sound** -- The insert-with-temporary-IDs then stabilize-and-rekey pattern is a well-known approach for building deterministic vocabularies. The remap table design (`Vec<NodeKindId>` indexed by old ID) is O(1) per lookup and the re-keying functions are correct.

4. **Appropriate reuse** -- `compute_idf` from the lexical module, `content_hash` from `extract`, `find_workspace_root` from `codegen`, and the generified `write_json_table` and `fetch_files_parallel` show the right level of code sharing without over-abstracting.

5. **Bounded resource usage** -- `MAX_AST_DEPTH`, `MAX_AST_NODES`, `MAX_FILE_SIZE`, `MAX_TRIGRAMS_PER_FILE` constants prevent unbounded work (applies ADR-001 indirectly via the reliability principle).

The two MEDIUM blocking issues are about preventing the orchestration logic from becoming a maintenance burden as the project grows, not about correctness bugs. The conditions for approval: add doc comments clarifying sort-order contracts on the weight types (avoids PF-002 by not deferring noticed issues).
