# Regression Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29T04:47:00Z

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

### 1. Lost Functionality Check

No exports, CLI options, API endpoints, or public interfaces were removed. Changes to existing code are strictly **additive**:

- `extract_repo_name` visibility widened from `fn` to `pub fn` in `clone.rs:51` -- additive, no consumer breaks.
- `git_run_with_timeout` visibility widened from `fn` to `pub fn` in `clone.rs:74` -- additive, no consumer breaks.
- Thread handle captured in `git_run_with_timeout` (`let handle = std::thread::spawn(...)`) so it can be joined after SIGKILL -- strictly improves cleanup behavior, no behavioral regression.
- New `git_output_with_timeout` function added -- additive, no existing function removed.
- New `clone_with_history` function added -- additive, no existing function removed.

### 2. Broken Behavior Check

- **`RepoEntry` struct field addition** (`deep_clone: bool` in `config.rs:22-27`): Uses `#[serde(default)]` so deserialization of existing TOML without this field defaults to `false`. Backward compatible. Test `dummy_repo()` was updated to include the new field. All 47 rskim-research tests pass.

- **Temporal split semantics**: The `temporal_split` function is new code, not a modification of an existing function. It correctly reverses newest-first input to chronological order before splitting. Tests verify no leakage, total count preservation, and chronological ordering.

- **Evaluate thresholds**: The `evaluate_at_thresholds` function is new code with decomposed helpers (`compute_jaccard_cache`, `compute_actual_sets`, `sweep_thresholds`). Tests verify zero-metric behavior for edge cases (all-unmapped commits, single-file commits).

### 3. Intent vs Reality Check

Each commit message was verified against its actual diff:

| Commit | Claim | Verified |
|--------|-------|----------|
| `2aa42d5` feat: cochange-validate binary | New binary, types, validation pipeline | Matches -- all claimed modules present |
| `be68334` fix: address self-review issues | Range validation, single-commit fix, NaN guard, split_timestamp wiring | Matches -- all four fixes present |
| `5974e52` fix: dedup capture_head_sha, join threads, validate partial clones | Extract git_output_with_timeout, join handles, .git/HEAD check, remove direct libc dep | Matches -- libc removed from Cargo.toml, handle.join() present, .git/HEAD check present |
| `625a131` perf: eliminate unconditional allocations, gate test-utils | test-utils feature flag, scratch set reuse | Matches -- `#[cfg(any(test, feature = "test-utils"))]` present, `predicted_scratch` reused |
| `a9a9c3c` refactor: decompose evaluate_at_thresholds | Extract helpers, add MAX_TEST_COMMITS/MAX_FILES_PER_COMMIT guards | Matches -- helpers present, bounds present |

### 4. Incomplete Migration Check

- `extract_repo_name` made public: previously called only within `clone.rs`. Now also called from `validate.rs:358`. No remaining private callers that would break.
- `git_run_with_timeout` made public: previously called only within `clone.rs`. Now also called from `clone_with_history` (same file) and indirectly used via `git_output_with_timeout`. No migration issues.
- `RepoEntry.deep_clone` field: the only existing corpus config (`crates/rskim-research/corpus.toml`) does not reference this field, and `#[serde(default)]` ensures backward compatibility. The new `cochange-corpus.toml` correctly sets `deep_clone = true` for all entries.

### 5. Test Coverage for Regressions

- 152 lib tests + 36 integration tests pass for rskim-bench (188 total)
- 47 tests pass for rskim-research (confirms config/clone changes are backward-compatible)
- The integration test `full_pipeline_synthetic_repo` creates a real git repo, runs the full pipeline (parse, split, build matrix, evaluate), and asserts `macro_recall > 0.0` at threshold 0.01 -- this is a strong regression guard against silent metric zeroing
- Tests cover edge cases: empty input, single commit, NaN fraction, all-unmapped commits, single-file-only test commits

### 6. Applies ADR-001

Per ADR-001, all noticed issues from prior review cycles were fixed immediately rather than deferred. The commit history shows 5 fix/refactor cycles (batch-2 through batch-5) addressing review findings inline. Avoids PF-002 (no findings were classified as pre-existing to skip resolution).

### Regression Checklist

- [x] No exports removed without deprecation
- [x] Return types backward compatible
- [x] Default values unchanged (deep_clone defaults to false via serde)
- [x] Side effects preserved (existing git timeout/kill behavior preserved and improved with thread join)
- [x] All consumers of changed code updated (validate.rs uses new public APIs)
- [x] Migration complete across codebase
- [x] CLI options preserved (new binary, no changes to existing CLI)
- [x] Commit messages match implementation
- [x] No breaking changes
