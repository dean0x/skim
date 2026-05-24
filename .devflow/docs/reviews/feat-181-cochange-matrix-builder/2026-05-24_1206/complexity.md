# Complexity Review Report

**Branch**: feat/181-cochange-matrix-builder -> main
**Date**: 2026-05-24

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`accumulate_pairs` function has high cyclomatic complexity (4 nested levels, ~70 lines)** - `builder.rs:102-174`
**Confidence**: 85%
- Problem: `accumulate_pairs` has 4 levels of nesting (`for commit` > `if > COUPLING_MAX_FILES` / `for fc` > `match` / `for i` > `for j` > `if !contains_key && len >= MAX_PAIRS`). The function handles three distinct responsibilities: (1) resolving file IDs with dedup, (2) tracking per-file commit counts, and (3) generating and counting canonical pairs with safety limits. At ~70 lines of logic (excluding blanks/comments), it sits at the upper end of the "warning" threshold. The nested double loop at lines 146-163 with the `contains_key` + `len` guard adds meaningful cognitive load.
- Fix: Extract two helper functions to flatten nesting and separate concerns:
  ```rust
  /// Resolve commit paths to deduplicated, sorted file IDs.
  fn resolve_ids(
      changed_files: &[FileChangeInfo],
      path_map: &HashMap<PathBuf, FileId>,
  ) -> (Vec<u32>, u32 /* unknown_skipped */) { ... }

  /// Generate canonical (min,max) pairs and merge into accumulator.
  fn insert_pairs(
      ids: &[u32],
      pair_counts: &mut HashMap<(u32, u32), u32>,
  ) -> Result<()> { ... }
  ```
  This would reduce `accumulate_pairs` to a single-level loop with early-continue for oversized commits, bringing nesting depth to 2 and cyclomatic complexity from ~8 to ~3.

**`pairs_for_file` uses O(N) linear scan over all pairs** - `reader.rs:163-182`
**Confidence**: 82%
- Problem: This function performs a linear scan over every pair entry in the matrix (O(pair_count)) to find partners for a single file. With `MAX_PAIRS = 2M`, this means scanning up to 24MB of data per query. The method itself is simple (low cyclomatic complexity), but the algorithmic complexity creates a latent scalability concern. The PR description does document this as O(pair_count), which shows awareness. However, for a module explicitly designed with 2M-pair safety caps, the linear scan deserves a performance note or a TODO for future optimization (e.g., a secondary index by file_id, or using the sorted order to binary-search for the first entry containing `file_id` in file_a and then scanning forward).
- Fix: At minimum, add a doc comment noting the performance characteristic and when callers should be cautious:
  ```rust
  /// NOTE: With MAX_PAIRS=2M this scans up to 24MB. If called in a hot
  /// loop, consider caching results or building a secondary index.
  ```
  For a structural improvement, since pairs are sorted by `(file_a, file_b)`, the file_a matches can be found via binary search. The file_b matches still require a full scan, but this halves the work for the common case.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Duplicated test helper functions across builder_tests.rs and reader_tests.rs** - `builder_tests.rs:17-51`, `reader_tests.rs:19-53`
**Confidence**: 88%
- Problem: `make_history` and `make_path_map` are defined identically in both test files (35 lines each, character-for-character duplicates). When the `CommitInfo` or `FileChangeInfo` struct changes, both copies must be updated in lockstep. This is a maintainability complexity issue -- the reader_tests.rs version already adds a third helper (`build_matrix`) that composes the other two, showing the pattern wants to be shared.
- Fix: Create a `cochange/test_helpers.rs` module gated behind `#[cfg(test)]` and import it from both test files:
  ```rust
  // cochange/test_helpers.rs
  #![cfg(test)]
  pub(super) fn make_history(commits: Vec<Vec<&str>>) -> HistoryResult { ... }
  pub(super) fn make_path_map(paths: &[&str]) -> HashMap<PathBuf, FileId> { ... }
  ```

## Pre-existing Issues (Not Blocking)

(none -- all files are new)

## Suggestions (Lower Confidence)

- **`serialize` allocates three separate Vec buffers** - `builder.rs:178-250` (Confidence: 65%) -- `fc_buf`, `pair_buf`, and `buf` are allocated separately then concatenated. A single pre-sized `Vec<u8>` written sequentially would halve allocations, though the current approach is clearer and correctness-focused (CRC is computed over fc+pair before header assembly). The current design is arguably more readable.

- **`file_commit_slice` and `pairs_slice` repeat offset arithmetic** - `reader.rs:217-228` (Confidence: 70%) -- Both methods compute `HEADER_SIZE + file_count * FILE_COMMIT_ENTRY_SIZE` independently. Caching `fc_end` as a field on `CochangeMatrixReader` (set once in `open`) would eliminate the repeated multiplication and make the struct's memory layout more explicit. Minor concern given the values are small.

- **Type alias `AccumulatedPairs` is a 3-tuple** - `builder.rs:96` (Confidence: 62%) -- `(HashMap<(u32, u32), u32>, HashMap<u32, u32>, CochangeStats)` as a type alias hides the meaning of each position. A named struct would be self-documenting. However, the alias is private and used in exactly one place, so the cost is low.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 0 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

### Rationale

The module is well-decomposed across four files with clear single-responsibility separation (format codec, builder, reader, public API). Function lengths are generally reasonable -- `format.rs` is a model of simplicity with small, focused encode/decode functions. The binary search implementations in `format.rs:lookup_pair` and `reader.rs:file_commits` are clean and correct.

The two HIGH findings are actionable but not merge-blocking individually:
1. `accumulate_pairs` is the densest function and would benefit from extraction to keep nesting at 2 levels.
2. `pairs_for_file` has documented O(N) behavior that is acceptable for v1 but deserves explicit callout for future optimization.

The test helpers duplication (MEDIUM) is a maintenance quality issue that should be addressed before the codebase accumulates more cochange test files.

Overall, this is a cleanly structured module with good separation of concerns. The conditions for approval are: acknowledge the `accumulate_pairs` complexity and the `pairs_for_file` linear scan as known technical debt, even if not refactored in this PR.
