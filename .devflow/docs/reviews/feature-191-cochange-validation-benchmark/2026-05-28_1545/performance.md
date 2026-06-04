# Performance Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-28T15:45

## Issues in Your Changes (BLOCKING)

### HIGH

**O(C * T * F) jaccard evaluations with redundant inner-loop work** - `crates/rskim-bench/src/cochange/validate.rs:215-268`
**Confidence**: 92%
- Problem: The `evaluate_at_thresholds` function iterates thresholds in the **outer** position relative to per-query jaccard computation. For each test commit with K known files, the inner loop computes `jaccard(query, candidate)` for all F files in `all_file_ids` -- but these jaccard values are **threshold-independent**. The current structure recomputes the identical jaccard lookups T times (once per threshold, default 6). Additionally, the `actual` HashSet (lines 243-247) is rebuilt identically for every threshold despite depending only on the commit's file set.

  Total cost: O(C * T * K * F * log P) where C=test commits, T=thresholds(6), K=files per commit, F=total files, P=pair count. For a repo with 1000 files, 200 test commits, 5 files/commit: ~6 million jaccard lookups. Moving the threshold loop inside or caching jaccard values per (query, candidate) pair would reduce this to O(C * K * F * log P) -- a 6x reduction.

- Fix: Compute jaccard values once per (query, candidate) pair, then sweep thresholds over the cached values:
  ```rust
  for &query_id in &known_ids {
      // Compute jaccard once per candidate.
      let jaccard_scores: Vec<(FileId, f64)> = all_file_ids
          .iter()
          .filter(|&&c| c != query_id)
          .filter_map(|&c| reader.jaccard(query_id, c).ok().map(|j| (c, j)))
          .filter(|&(_, j)| j > 0.0)
          .collect();

      let actual: HashSet<FileId> = known_ids.iter().copied()
          .filter(|&id| id != query_id).collect();

      for (ti, &threshold) in thresholds.iter().enumerate() {
          let predicted: HashSet<FileId> = jaccard_scores.iter()
              .filter(|&&(_, j)| j >= threshold)
              .map(|&(id, _)| id)
              .collect();
          // ... accumulate metrics as before
      }
  }
  ```

**Triple deep-clone of full commit history in validate_repo** - `crates/rskim-bench/src/cochange/temporal_split.rs:88,103-104` and `crates/rskim-bench/src/cochange/validate.rs:419`
**Confidence**: 90%
- Problem: The commit history flows through three unnecessary full clones:
  1. `temporal_split` clones the entire input to reverse it (line 88: `commits.to_vec()`).
  2. `temporal_split` clones both the train and test slices into the result (lines 103-104: `train_slice.to_vec()`, `test_slice.to_vec()`).
  3. `validate_repo` clones `split.train` again to construct `history_for_builder` (line 419: `split.train.clone()`).

  Each `CommitInfo` contains a `String` hash (40 chars), a `String` author, a `String` message, and a `Vec<FileChangeInfo>` (each with a `PathBuf`). For a repo with 10,000 commits averaging 5 files each, that is 4 full deep-copies of ~10K heap-allocated structs. This is the dominant memory allocation cost in the pipeline.

- Fix: Have `temporal_split` take ownership (`Vec<CommitInfo>` instead of `&[CommitInfo]`) and split in-place using `Vec::split_off`. For the builder clone on line 419, check if `builder.build` can take `&[CommitInfo]` or if `HistoryResult` can be constructed with a borrow:
  ```rust
  pub fn temporal_split(mut commits: Vec<CommitInfo>, train_fraction: f64) -> TemporalSplit {
      commits.reverse(); // in-place, no allocation
      let split_index = /* ... */;
      let test = commits.split_off(split_index); // zero-copy split
      TemporalSplit {
          train: commits,
          test,
          split_timestamp,
      }
  }
  ```

### MEDIUM

**build_path_map clones every PathBuf before dedup** - `crates/rskim-bench/src/cochange/validate.rs:52-55`
**Confidence**: 85%
- Problem: `build_path_map` clones every file path from every commit into a `Vec<PathBuf>`, sorts it, then deduplicates. For repos with high file overlap across commits (common -- the same files appear in many commits), the vast majority of cloned paths are discarded by `dedup`. A `HashSet` or `BTreeSet` would avoid allocating duplicates.
- Fix: Use a `BTreeSet` to collect unique paths directly (maintains sort order, no dedup step, no intermediate allocations for duplicates):
  ```rust
  pub fn build_path_map(commits: &[CommitInfo]) -> HashMap<PathBuf, FileId> {
      let paths: BTreeSet<&PathBuf> = commits
          .iter()
          .flat_map(|c| c.changed_files.iter().map(|f| &f.path))
          .collect();
      paths
          .into_iter()
          .enumerate()
          .map(|(i, p)| (p.clone(), FileId(i as u32)))
          .collect()
  }
  ```

**HashSet allocation per query per threshold in evaluate_at_thresholds** - `crates/rskim-bench/src/cochange/validate.rs:224,243`
**Confidence**: 82%
- Problem: Inside the innermost loop, `predicted` (line 224) is allocated as a new `HashSet` for every (query, threshold) combination, and `actual` (line 243) is allocated for every query within every threshold. With the restructuring from the HIGH finding above, `actual` moves outside the threshold loop. For `predicted`, consider reusing the allocation by clearing instead of reallocating.
- Fix: Declare `predicted` before the threshold loop and use `predicted.clear()` between iterations, or (better) adopt the cached-jaccard approach from the HIGH fix above which makes `predicted` a filtered view of the cache.

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **String allocation in deny_list per-file normalization** - `crates/rskim-bench/src/cochange/deny_list.rs:60` (Confidence: 65%) -- `path.replace('\\', "/")` allocates a new String for every file checked. On Unix, backslashes are virtually never present, so this allocation is wasted. Consider checking for backslash presence first or using a cow pattern. Minor impact since deny-list checking is not on the hot path.

- **Unbounded par_iter thread pool lifetime** - `crates/rskim-bench/src/bin/cochange_validate.rs:112-127` (Confidence: 62%) -- The custom rayon ThreadPool is created with `num_threads(3)` which is appropriate for bounding concurrency. However, each repo validation inside the parallel iterator does blocking I/O (git clone, history parsing) which will tie up rayon worker threads. This is acceptable for a benchmark binary but worth noting if this pattern is reused in the library.

- **chrono_now year calculation is approximate** - `crates/rskim-bench/src/bin/cochange_validate.rs:215` (Confidence: 75%) -- `secs / 86400 / 365` does not account for leap years, so the year can be off by 1. Not a performance issue but the timestamp is used in report filenames and reproducibility metadata.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The core evaluation hot loop (`evaluate_at_thresholds`) performs O(T) redundant jaccard computations by sweeping thresholds in the outer position. For the default 6 thresholds, this is a 6x unnecessary cost multiplier on the most expensive operation in the pipeline. The commit history is also deep-cloned 3 times through the pipeline when ownership transfer and in-place mutation would eliminate all intermediate copies. Neither issue blocks correctness, but for a benchmark binary that processes 7 repositories with full git histories, the cumulative wall-clock and memory impact is material -- applies ADR-001 (fix noticed issues immediately).
