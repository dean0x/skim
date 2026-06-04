# Regression Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31T15:32
**Commits reviewed**: 12 (880765b..a87aa6e)

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

(none)

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none)

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

- [x] **No exports removed without deprecation** -- All existing public APIs (`FileSource`, `GitCloneSource`, `FixtureSource`, `load_corpus_config`, `CorpusConfig`, `RepoEntry`, `load_fixture_files`, `content_hash`, `compute_idf`) remain exported with unchanged signatures.
- [x] **Return types backward compatible** -- No return type changes on existing functions. `walk_and_load` gained an `extensions: Option<&[&str]>` parameter but is `pub(crate)` (not part of public API). All existing callers pass `None` to preserve original behavior.
- [x] **Default values unchanged** -- Existing CLI subcommands (`run`, `codegen`, `validate`) retain identical default values and argument definitions.
- [x] **Side effects preserved** -- The existing `cmd_run` path produces identical output. The refactored `fetch_all_files` delegates to `fetch_files_parallel` with the same `GitCloneSource`, preserving all file-fetching behavior.
- [x] **All consumers of changed code updated** -- External consumer `rskim-bench` imports `GitCloneSource`, `FileSource`, `load_corpus_config`, `CorpusConfig`, and `RepoEntry` -- all unchanged. Verified via `cargo check -p rskim-bench` (passes).
- [x] **Migration complete across codebase** -- `walk_and_load` signature change: all 4 call sites updated (`GitCloneSource::fetch_files`, `walk_and_load_ast`, `FixtureSource::fetch_files`, `load_fixture_files`). All pass `None` for backward compatibility or `Some(AST_TARGET_EXTENSIONS)` for new AST path.
- [x] **CLI options preserved** -- Existing `run`, `codegen`, and `validate` subcommands are unchanged. Three new subcommands added (`ast-run`, `ast-codegen`, `ast-validate`) are purely additive.
- [x] **Commit messages match implementation** -- All 12 commits accurately describe their changes. The initial feature commit (880765b) adds AST n-gram extraction. Subsequent fix commits address review findings correctly.
- [x] **Breaking changes documented** -- No breaking changes exist.

### Refactoring Safety Analysis

Three existing functions were refactored:

1. **`walk_and_load(root) -> walk_and_load(root, extensions)`** -- Internal function (`pub(crate)`). Added optional parameter with `None` preserving original behavior exactly. Exclusion list (`EXCLUDED_EXTENSIONS`) and target list (`TARGET_EXTENSIONS`) are applied only in the `None` branch, matching pre-change behavior. New test (`walk_and_load_explicit_extensions_includes_md`) validates the `Some` branch. Confidence: 95%.

2. **`fetch_all_files` extracted to `fetch_files_parallel`** -- The original inline code in `fetch_all_files` was extracted into a generic `fetch_files_parallel(config, source)`. The original `fetch_all_files` now creates `GitCloneSource` and delegates. The new `fetch_all_ast_files` creates `AstGitCloneSource` and delegates to the same generic. No behavioral change for the existing `cmd_run` code path. Confidence: 95%.

3. **`write_weight_table` extracted to `write_json_table<T: Serialize>`** -- Generic serialization helper. The original `write_weight_table` now calls `write_json_table(table, output_path, "weight table")`. Behavior preserved: same `serde_json::to_string_pretty`, same `std::fs::write`, same `create_dir_all`. Confidence: 95%.

### Dependency Version Compatibility

- `rskim-core` dependency bumped from `2.9.0` to `2.10.0` in both `rskim-research` and `rskim-search`. This matches the actual `rskim-core` crate version (`2.10.0`). No API changes in `rskim-core` are required by this PR.
- `tree-sitter` added as a direct dependency to `rskim-research`. It was already a transitive dependency via `rskim-core`. The workspace version constraint ensures consistency.

### Test Coverage Verification

- All 107 existing tests pass (verified via `cargo test -p rskim-research`).
- `rskim-bench` compiles successfully against the updated `rskim-research` (verified via `cargo check -p rskim-bench`).
- New regression guard test: `existing_config_unchanged_by_ast_additions` explicitly verifies that `load_corpus_config` still rejects languages only valid in the AST pipeline (e.g., `"Cpp"`).
- New test: `walk_and_load_explicit_extensions_includes_md` guards against the exclusion list leaking into the explicit-extensions path.

### Intent vs Reality (PR Description Verification)

PR states: "All existing modules, tests, and CLI subcommands unchanged. No breaking changes."

**Verified**: `extract.rs`, `idf.rs`, `types.rs`, `validate.rs`, `codegen.rs` have zero diff. `clone.rs`, `config.rs`, `main.rs`, and `lib.rs` were modified but only additively (new structs, new functions, new re-exports). Existing function signatures and behavior are preserved. This aligns with ADR-001 (applies ADR-001: all noticed issues fixed in-branch via the 11 fix commits following the initial feature commit).
