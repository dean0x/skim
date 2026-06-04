# Consistency Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31

## Issues in Your Changes (BLOCKING)

### HIGH

**validate output channel inconsistency: ast_validate uses eprintln while validate uses println** - `crates/rskim-research/src/ast_validate.rs:146-197`, `crates/rskim-research/src/main.rs:513-529`
**Confidence**: 92%
- Problem: The lexical `cmd_validate` function in `main.rs:335-368` outputs its validation report to **stdout** via `println!` (the expected channel for command output), while `print_ast_validation_report` in `ast_validate.rs:146-197` sends all output to **stderr** via `eprintln!`. The doc comment at line 4-5 says "Output goes to stderr (not stdout) so it does not interfere with piped workflows" -- but the lexical validate subcommand does not follow this pattern. Users who pipe `ast-validate` output to a file will get an empty file, while `validate` output works as expected. This is a user-facing behavioral inconsistency between two sibling subcommands.
- Fix: Either change `ast_validate.rs` to use `println!` (matching the existing `validate` convention), or document the divergence explicitly in the CLI help text. Matching `println!` is the more consistent choice since both commands are "report" subcommands.

**validate_ast_repo allows uppercase hex in commit SHA while validate_repo is ambiguous** - `crates/rskim-research/src/config.rs:110-111`
**Confidence**: 82%
- Problem: `validate_repo` (line 144) checks `repo.commit.chars().all(|c| c.is_ascii_hexdigit())` which accepts both uppercase and lowercase hex. `validate_ast_repo` (line 111) uses the identical check. However, git SHAs are conventionally lowercase and the doc comment at line 109 says "40-character lowercase hex SHA" while the code accepts uppercase too. This is consistent between the two validators (both accept uppercase), but the doc comment is misleading. Since this is a pattern shared between old and new code, it is borderline pre-existing, but the AST variant introduced a new doc comment that explicitly says "lowercase" while the code does not enforce it.
- Fix: Either add `.to_ascii_lowercase()` normalization before the check, or correct the doc comment to say "hex SHA" without "lowercase". The latter is simpler and matches actual git behavior (git accepts both cases).

### MEDIUM

**AstGitCloneSource duplicates GitCloneSource body instead of parameterizing** - `crates/rskim-research/src/clone.rs:80-98`
**Confidence**: 85%
- Problem: `AstGitCloneSource::fetch_files` (lines 86-97) is a near-verbatim copy of `GitCloneSource::fetch_files` (lines 66-77), differing only in the final call (`walk_and_load_ast` vs `walk_and_load(_, None)`). Both share the same clone logic, repo name extraction, and idempotency guard. The existing `FileSource` trait and `fetch_files_parallel` abstraction show the codebase values DRY patterns. A parameterized approach (e.g., passing the walker function or extension list) would reduce duplication.
- Fix: Consider a generic `CloneAndWalk` struct parameterized by the extension list, or add an `extensions` field to `GitCloneSource` and remove `AstGitCloneSource` entirely:
```rust
pub struct GitCloneSource {
    pub corpus_dir: PathBuf,
    pub extensions: Option<&'static [&'static str]>,
}
impl FileSource for GitCloneSource {
    fn fetch_files(&self, repo: &RepoEntry) -> anyhow::Result<Vec<SourceFile>> {
        // ... same clone logic ...
        walk_and_load(&dest, self.extensions)
    }
}
```

**cmd_ast_run does not call validation after weight computation, unlike cmd_run** - `crates/rskim-research/src/main.rs:374-451`
**Confidence**: 88%
- Problem: The lexical `cmd_run` (line 195) calls `log_validation_summary(&weights)` to print a border-vs-uniform selectivity summary before writing the table. The AST `cmd_ast_run` skips any analogous validation step and goes straight from IDF computation to serialization. While the AST pipeline has a separate `ast-validate` subcommand, the lexical pipeline also has a separate `validate` subcommand AND still runs inline validation during `run`. The asymmetry means an AST corpus run provides no immediate quality feedback.
- Fix: Add an inline validation call in `cmd_ast_run` that prints per-language stats (e.g., vocabulary size, bigram/trigram counts, error node rates) to stderr before writing. This matches the lexical pattern of giving immediate feedback during the run.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**walk_tree is recursive despite comment saying "iterative"** - `crates/rskim-research/src/ast_extract.rs:108,115-193`
**Confidence**: 95%
- Problem: The doc comment on line 108 says "Iterative tree walk using TreeCursor to avoid recursion depth limits" but the function is actually **recursive** -- it calls itself on line 171. The `MAX_AST_DEPTH` guard (line 128) prevents stack overflow, so the code is correct, but the doc comment is misleading. This is a documentation consistency issue -- the comment claims iterative but the implementation is recursive with depth limiting.
- Fix: Change the doc comment to accurately describe the approach:
```rust
/// Recursive tree walk using `TreeCursor` with depth and node-count guards.
///
/// Collects parent->child bigrams and (when `collect_trigrams` is true)
/// grandparent->parent->child trigrams. ...
```

**`extract_ast_ngrams_from_corpus` does not use rayon for parallel extraction, unlike `extract_bigrams_from_corpus`** - `crates/rskim-research/src/ast_extract.rs:204-307`
**Confidence**: 80%
- Problem: The existing `extract_bigrams_from_corpus` in `extract.rs` processes files sequentially (it is a single-threaded loop), so the AST extraction being sequential is technically consistent with that. However, the `fetch_files_parallel` function in `main.rs` (line 230) uses `par_iter` for parallel fetching, and the crate depends on `rayon`. The AST extraction shares a mutable `vocab`, which prevents trivial parallelization, but this is worth noting as a pattern that could diverge as the codebase evolves. This is informational -- not a consistency violation since the lexical version is also sequential.
- Fix: No immediate fix needed. The shared mutable `NodeKindVocabulary` makes parallelization non-trivial. Consider documenting this as a known limitation in the module doc comment.

## Pre-existing Issues (Not Blocking)

### MEDIUM

**Lexical `extract_bigrams_from_corpus` does not use ProgressBar while AST version does** - `crates/rskim-research/src/extract.rs:65` vs `crates/rskim-research/src/ast_extract.rs:221-226`
**Confidence**: 85%
- Problem: The AST `extract_ast_ngrams_from_corpus` adds a progress bar (lines 221-226), but the lexical `extract_bigrams_from_corpus` in `extract.rs` has no progress bar. This is a pre-existing gap that the AST pipeline actually improves upon -- the new code is more user-friendly. No action needed on the AST side; the lexical side could be updated in a separate PR.

## Suggestions (Lower Confidence)

- **Duplicate MAX_FILE_SIZE constants** - `crates/rskim-research/src/ast_extract.rs:27` and `crates/rskim-research/src/clone.rs:15` (Confidence: 70%) -- Both define `MAX_FILE_SIZE = 100 * 1024` but using different types (`usize` vs `u64`). The values are consistent but not shared. Could be unified into a shared constant if the crate's policy is one canonical limit.

- **Missing `#[must_use]` on `extract_ast_ngrams_from_corpus`** - `crates/rskim-research/src/ast_extract.rs:204` (Confidence: 65%) -- The lexical `extract_bigrams_from_corpus` has `#[must_use]` (line 64 of extract.rs). The AST equivalent does not. Project convention (and CLAUDE.md Rust rules) calls for `#[must_use]` on functions with important return values.

- **`lang_to_ident` underscore collapsing is fragile** - `crates/rskim-research/src/ast_codegen.rs:161-163` (Confidence: 62%) -- The `.split("__").collect::<Vec<_>>().join("_")` approach only collapses double underscores, not triple or more. A language like `C++` maps to `C__` which collapses to `C_`, but `C+++` (hypothetical) would leave `C___` uncollapsed. This is unlikely to matter in practice since all current language names are well-known, but a regex or loop-based approach would be more robust.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 1 | 0 |

**Consistency Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The AST pipeline demonstrates strong consistency with the existing lexical pipeline in most areas: it reuses `compute_idf`, follows the same error-handling patterns (anyhow + `with_context`), uses `#[must_use]` annotations, and mirrors the subcommand structure (`run`/`codegen`/`validate` -> `ast-run`/`ast-codegen`/`ast-validate`). The IDF formula is correctly shared via `crate::idf::compute_idf` (applies ADR-001 by fixing the vocabulary rekey bug in the same PR). The main consistency gaps are the stdout-vs-stderr output channel for the validate subcommand and the duplicated clone source struct. These are straightforward to address.
