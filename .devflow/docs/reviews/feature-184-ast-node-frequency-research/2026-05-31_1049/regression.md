# Regression Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31

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

## Detailed Regression Analysis

### 1. Lost Functionality Check

**Removed Exports**: None. All 7 existing public modules (`clone`, `codegen`, `config`, `extract`, `idf`, `types`, `validate`) remain exported in `lib.rs`. Five new modules were added (`ast_codegen`, `ast_extract`, `ast_idf`, `ast_types`, `ast_validate`) -- purely additive.

**Removed CLI Options**: None. The existing three subcommands (`Run`, `Codegen`, `Validate`) are unchanged in the `Commands` enum. Three new subcommands were added (`AstRun`, `AstCodegen`, `AstValidate`) -- purely additive.

**Removed Files**: None (`git diff main...HEAD --name-status | grep "^D"` returned empty).

**Removed Functions**: No public functions were removed. `walk_and_load` had its signature changed from `fn walk_and_load(root: &Path)` to `fn walk_and_load(root: &Path, extensions: Option<&[&str]>)`, but all four existing call sites were updated to pass `None`, preserving identical behavior.

### 2. Broken Behavior Check

**`walk_and_load` Signature Change (clone.rs:287)**: The added `Option<&[&str]>` parameter defaults to `None` at all existing call sites (`GitCloneSource`, `FixtureSource`, `load_fixture_files`). When `None`, the `match` arm applies the exact same `EXCLUDED_EXTENSIONS` + `TARGET_EXTENSIONS` filtering as the original code. This is a backward-compatible extension -- confirmed by 97 passing tests.

**`fetch_all_files` Refactoring (main.rs:265-272)**: The function was factored into a generic `fetch_files_parallel` that accepts `&impl FileSource`, with `fetch_all_files` now a thin wrapper that creates `GitCloneSource` and delegates. The external behavior is identical -- same progress bar, same error handling, same parallel execution. No call site changes were needed for the existing `cmd_run` path.

**`load_corpus_config` Unchanged (config.rs:61-73)**: The existing validation function and its `VALID_LANGUAGES` constant are completely untouched. A new parallel function `load_ast_corpus_config` was added with its own `AST_VALID_LANGUAGES` list. A regression test (`existing_config_unchanged_by_ast_additions`, config.rs:297-308) explicitly verifies that `load_corpus_config` still rejects languages that are only valid for the AST corpus (like "Cpp").

### 3. Intent vs Reality Mismatch

**Commit Messages vs Code**: The 3 commits (`880765b`, `605203a`, `9452810`) align with the implementation. The PR description states "AST-level n-gram analysis for issue #184" -- the code delivers exactly this: AST extraction, IDF computation, code generation, and validation, all properly scoped to the `rskim-research` crate. No intent-reality gaps found.

### 4. Incomplete Migration Check

**`walk_and_load` Migration**: All 4 call sites updated (GitCloneSource:76, FixtureSource:383, load_fixture_files:389, walk_and_load_ast:373). No stale callers remain. (`grep` across the entire codebase confirms no external consumers.)

**No External Consumers**: `walk_and_load` and `fetch_all_files` are only used within `rskim-research` (publish=false). No other crate depends on these functions.

### 5. Dependency Impact

**tree-sitter added to rskim-research**: This is a workspace dependency already used by `rskim-core`. Adding it to `rskim-research` introduces no new transitive dependency -- it was already in the dependency graph. The `Cargo.lock` change is a single line addition confirming this.

### 6. Regression Checklist

- [x] No exports removed without deprecation
- [x] Return types backward compatible (walk_and_load extension parameter is additive)
- [x] Default values unchanged (all existing callers pass None)
- [x] Side effects preserved (progress bars, eprintln messages unchanged)
- [x] All consumers of changed code updated (4/4 call sites migrated)
- [x] Migration complete across codebase (grep confirms no stale callers)
- [x] CLI options preserved (Run/Codegen/Validate unchanged)
- [x] Commit messages match implementation
- [x] All 97 package tests pass (applies ADR-001 -- issues found are fixed immediately)

### 7. Test Verification

`cargo test --package rskim-research` passes 97 tests (0 failures, 0 skipped). The test suite includes:
- Existing config validation tests (unchanged)
- A new regression test `existing_config_unchanged_by_ast_additions` that explicitly guards the boundary between lexical and AST config validation
- New AST-specific tests covering extraction, IDF computation, codegen, and validation
