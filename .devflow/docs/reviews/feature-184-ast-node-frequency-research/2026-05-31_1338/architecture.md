# Architecture Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T13:38

## Issues in Your Changes (BLOCKING)

### HIGH

**AstGitCloneSource duplicates GitCloneSource body (DRY / SRP violation)** - `crates/rskim-research/src/clone.rs:85-97`
**Confidence**: 90%
- Problem: `AstGitCloneSource::fetch_files` is an exact copy of `GitCloneSource::fetch_files` with only the final call changed from `walk_and_load(&dest, None)` to `walk_and_load_ast(&dest)`. The clone-and-checkout logic (repo name extraction, existence check, `clone_repo` call) is duplicated verbatim. This violates DRY and creates a maintenance risk: any change to the clone logic (e.g., handling of shallow-clone fallback, credential passing) must be updated in both implementations.
- Fix: Extract the clone-and-checkout logic into a shared helper, and let each `FileSource` impl call it then dispatch to its walker:

```rust
/// Common clone-or-reuse logic shared by all clone-based FileSource impls.
fn ensure_cloned(corpus_dir: &Path, repo: &RepoEntry) -> anyhow::Result<PathBuf> {
    let repo_name = extract_repo_name(&repo.url)?;
    let dest = corpus_dir.join(&repo_name);
    if !dest.exists() {
        clone_repo(&repo.url, &repo.commit, &dest)
            .with_context(|| format!("cloning {}", repo.url))?;
    }
    Ok(dest)
}

impl FileSource for GitCloneSource {
    fn fetch_files(&self, repo: &RepoEntry) -> anyhow::Result<Vec<SourceFile>> {
        let dest = ensure_cloned(&self.corpus_dir, repo)?;
        walk_and_load(&dest, None)
    }
}

impl FileSource for AstGitCloneSource {
    fn fetch_files(&self, repo: &RepoEntry) -> anyhow::Result<Vec<SourceFile>> {
        let dest = ensure_cloned(&self.corpus_dir, repo)?;
        walk_and_load_ast(&dest)
    }
}
```

Alternatively, since `AstGitCloneSource` differs from `GitCloneSource` only in which extensions to walk, a single struct parameterized by extension list would eliminate both the duplication and the second struct entirely. (applies ADR-001)

---

**`cmd_ast_run` duplicates summary-printing logic inline instead of reusing `ast_validate::run_ast_validation`** - `crates/rskim-research/src/main.rs:450-478`
**Confidence**: 85%
- Problem: After writing the weight table, `cmd_ast_run` manually iterates `table.bigram_weights`, looks up `language_stats`, computes error rates, and prints a summary. This is structurally identical to what `ast_validate::run_ast_validation` + `print_ast_validation_report` already computes (error node rate, per-language bigram/trigram counts). The inline code re-implements the same computation (error_node_count / total_node_count), creating divergence risk if the formula changes. The existing lexical pipeline has the same pattern (`log_validation_summary` calls into `validate::run_validation`), showing the project convention is to delegate summary computation to the validation module.
- Fix: Extract a short `log_ast_summary` function that calls `ast_validate::run_ast_validation(&table)` and prints the compact stderr summary from the returned report, mirroring the pattern established by `log_validation_summary`. (applies ADR-001)

### MEDIUM

**`write_ast_weight_table` duplicates `write_weight_table`** - `crates/rskim-research/src/main.rs:495-515`
**Confidence**: 82%
- Problem: `write_ast_weight_table` is structurally identical to `write_weight_table` (lines 296-310): create parent dir, serialize to pretty JSON, write, log. The only difference is the type parameter (`AstWeightTable` vs `WeightTable`) and the default path. Both types implement `Serialize`, so a single generic helper would eliminate the duplication.
- Fix: Create a generic write helper:

```rust
fn write_json_table<T: serde::Serialize>(
    table: &T,
    output: Option<PathBuf>,
    default_path: impl FnOnce() -> PathBuf,
    label: &str,
) -> anyhow::Result<()> {
    let output_path = output.unwrap_or_else(default_path);
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating output directory {}", parent.display()))?;
    }
    let json = serde_json::to_string_pretty(table)
        .with_context(|| format!("serializing {label}"))?;
    std::fs::write(&output_path, json)
        .with_context(|| format!("writing {}", output_path.display()))?;
    eprintln!("Written: {}", output_path.display());
    Ok(())
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Consider parameterizing clone source by extension list rather than maintaining two struct types** - `crates/rskim-research/src/clone.rs:61-98` (Confidence: 70%) -- `GitCloneSource` and `AstGitCloneSource` are identical in everything except which extension set they pass to `walk_and_load`. A single `GitCloneSource { corpus_dir, extensions: Option<&'static [&'static str]> }` would collapse both structs into one.

- **AST extraction uses a single shared mutable `NodeKindVocabulary` across all languages** - `crates/rskim-research/src/ast_extract.rs:205-209` (Confidence: 65%) -- The shared vocabulary means node kinds from different languages share the same ID namespace. This is intentional and documented, but could create cross-language interference if a language-specific analysis is added later. A per-language vocabulary with a merge step would be more modular, though the current design is simpler and sufficient for IDF computation.

- **`AstWeightTable.vocabulary` duplicates data available from `NodeKindVocabulary`** - `crates/rskim-research/src/main.rs:442` (Confidence: 62%) -- `vocab.kinds().into_iter().map(str::to_string).collect()` copies the entire vocabulary into the table as `Vec<String>`, creating a second copy of data that already lives in the vocabulary. For serialization this is necessary, but the intermediate copy could be avoided by implementing `Serialize` on `NodeKindVocabulary` directly.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 1 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Architecture Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The new AST n-gram pipeline follows the same architectural pattern as the existing lexical bigram pipeline (extract -> IDF -> codegen -> validate), which is the correct approach for consistency. Module boundaries are clean: `ast_types` for data structures, `ast_extract` for tree walking, `ast_idf` for weight computation, `ast_codegen` for Rust source generation, `ast_validate` for reporting. The `WalkContext` struct properly bundles traversal state to keep function signatures manageable.

The main concerns are code duplication in the clone source implementations and the inline summary logic in `cmd_ast_run`. The `AstGitCloneSource` is nearly identical to `GitCloneSource`, which is a straightforward DRY violation that should be addressed before merge. The inline summary printing in `cmd_ast_run` re-implements computation already available in the validation module, diverging from the project's own convention established by `log_validation_summary`. Both are fixable with small refactoring.
