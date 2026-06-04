# Regression Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T13:38

## Issues in Your Changes (BLOCKING)

_No blocking regression issues found._

## Issues in Code You Touched (Should Fix)

_No should-fix regression issues found._

## Pre-existing Issues (Not Blocking)

_No pre-existing regression issues found._

## Suggestions (Lower Confidence)

- **PR description states 44 repos but ast-corpus.toml contains 40** - `crates/rskim-research/ast-corpus.toml` (Confidence: 65%) -- The PR description claims "New corpus config ast-corpus.toml with 44 repos" but `grep -c '^\[\[repos\]\]'` returns 40. This is a documentation/intent mismatch, not a code regression. The corpus itself is valid and complete (40 repos covering 12 languages; SQL and Markdown are extracted from polyglot repos per the file comment).

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 0 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: APPROVED

## Detailed Analysis

### Regression Checklist

- [x] **No exports removed** -- All public exports are preserved. `walk_and_load` changed from private `fn` to `pub(crate)` with a new `extensions` parameter, but all 4 call sites are updated. No external crates used this function (it was never `pub`).
- [x] **Return types backward compatible** -- No return type changes on existing functions. `fetch_all_files` retains its signature; internal refactoring to `fetch_files_parallel` is private.
- [x] **Default values unchanged** -- The `Commands::Run` CLI subcommand retains all its defaults (`threshold=1.5`, `corpus_config=corpus.toml`). New `AstRun` subcommand defaults (`threshold=1.5`, `corpus_config=ast-corpus.toml`, `trigrams=true`) do not shadow existing ones.
- [x] **Side effects preserved** -- Progress bars, stderr logging, and error reporting in the lexical pipeline (`cmd_run`) are unchanged.
- [x] **All consumers of changed code updated** -- `GitCloneSource`, `FixtureSource`, and `load_fixture_files` all pass `None` to the updated `walk_and_load`, preserving original behavior. `rskim-bench` uses `GitCloneSource` and compiles without issues.
- [x] **Migration complete across codebase** -- `cargo check --workspace` passes. All 4 callers of `walk_and_load` updated. `rskim-bench` (downstream consumer of `rskim-research`) compiles cleanly.
- [x] **CLI options preserved** -- The three existing subcommands (`run`, `codegen`, `validate`) retain identical option definitions. Three new subcommands (`ast-run`, `ast-codegen`, `ast-validate`) are additive.
- [x] **Commit messages match implementation** -- Commits accurately describe: initial feature (880765b), stabilize/rekey bug fix (605203a), and five incremental review-fix batches. No intent/reality mismatch.
- [x] **Tests pass** -- `rskim-research`: 103 pass. `rskim-search`: 464 pass (3 skip). `rskim-core`: 470 pass. Full workspace compiles.
- [x] **Dependency version alignment** -- `rskim-core` version bumped from 2.9.0 to 2.10.0 in both `rskim-research/Cargo.toml` and `rskim-search/Cargo.toml`, matching the actual `rskim-core/Cargo.toml` version.

### Key Refactoring Safety Analysis

1. **`walk_and_load` signature change**: The function was `fn walk_and_load(root: &Path)` (private) and became `pub(crate) fn walk_and_load(root: &Path, extensions: Option<&[&str]>)`. The `None` branch preserves exact original behavior (exclusion list + target list filtering). The `Some` branch is only used by the new `walk_and_load_ast` wrapper. No behavioral regression.

2. **`fetch_all_files` refactoring**: The body was extracted into a generic `fetch_files_parallel(config, source)` helper, and `fetch_all_files` becomes a thin wrapper that creates `GitCloneSource` and delegates. The call site in `cmd_run` is unchanged. No behavioral regression.

3. **New tree-sitter dependency in rskim-research**: Added `tree-sitter = { workspace = true }`. This is additive -- the workspace already depended on tree-sitter through rskim-core. No new transitive dependencies introduced.

### Applies/Avoids

- applies ADR-001: All 20 prior-cycle issues were fixed (per PRIOR_RESOLUTIONS). This cycle found no new regression issues to fix.
- avoids PF-002: No findings were classified as "deferred" or "pre-existing to skip." All categories were evaluated with equal scrutiny.
