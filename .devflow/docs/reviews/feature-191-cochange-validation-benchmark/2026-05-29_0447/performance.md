# Performance Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29T04:47Z

## Issues in Your Changes (BLOCKING)

### HIGH

**O(Q x F) jaccard cache per commit creates large intermediate allocations for repos with many files** - `crates/rskim-bench/src/cochange/validate.rs:279`
**Confidence**: 85%
- Problem: `compute_jaccard_cache` materializes a `Vec<Vec<(FileId, f64)>>` containing up to `Q x F` entries (where Q = files in commit, F = all files in path_map). For a commit touching 500 files against a repo with 20,000 files, this is 500 x 20,000 = 10M entries x 12 bytes each = ~120 MB per commit. Although `MAX_FILES_PER_COMMIT` caps Q at 500 and `MAX_FILES_FOR_EVALUATION` caps F at 20,000, the product is still substantial. The cache is rebuilt from scratch for every test commit, so the allocator is churning through these large vectors on every iteration.
- Fix: Pre-allocate the outer Vec once outside the commit loop and reuse it via `clear()` + refill, similar to the `predicted_scratch` pattern already used for the threshold sweep. This avoids repeated allocation/deallocation of the cache across commits:
  ```rust
  // Outside the commit loop:
  let mut jaccard_cache: Vec<Vec<(FileId, f64)>> = Vec::new();

  // Inside the commit loop, instead of:
  //   let jaccard_cache = compute_jaccard_cache(...)?;
  // Do:
  jaccard_cache.resize_with(known_ids.len(), Vec::new);
  for (i, &query_id) in known_ids.iter().enumerate() {
      jaccard_cache[i].clear();
      for &cid in &all_file_ids {
          if cid == query_id { continue; }
          match reader.jaccard(query_id, cid) {
              Ok(j) if j > 0.0 => jaccard_cache[i].push((cid, j)),
              Ok(_) => {},
              Err(SearchError::IndexCorrupted(msg)) => return Err(...),
              Err(e) => return Err(...),
          }
      }
  }
  jaccard_cache.truncate(known_ids.len());
  ```

**`compute_actual_sets` allocates N HashSets per commit, each with N-1 elements** - `crates/rskim-bench/src/cochange/validate.rs:281-283`
**Confidence**: 85%
- Problem: For each commit with Q mapped files, `compute_actual_sets` builds Q `HashSet<FileId>` values, each containing Q-1 entries. For a commit with 500 mapped files, this is 500 HashSets x 499 entries = ~250K FileId insertions per commit. Since the "actual" set for query `i` is simply "all other known files in this commit," this information is already implicitly available from `known_ids` itself. The precision/recall computations could be done inline without materializing separate sets.
- Fix: Instead of pre-computing actual sets, compute precision and recall inline using the known_ids slice directly. For intersection counting, iterate `predicted_scratch` and check membership in `known_ids` (which is small enough that a linear scan or a single `HashSet<FileId>` built once per commit suffices):
  ```rust
  // Build ONE actual set per commit (all known_ids), then for each query,
  // the actual set is actual_all minus the query itself.
  let actual_all: HashSet<FileId> = known_ids.iter().copied().collect();
  // In sweep: actual_count = actual_all.len() - 1 (exclude self)
  // intersection = predicted_scratch.intersection(&actual_all).count()
  //   minus (1 if query_id is in predicted_scratch, which it never is
  //   because self-pairs are excluded from jaccard_cache)
  ```
  This replaces Q HashSet allocations per commit with one.

### MEDIUM

**Redundant intersection computation in sweep_thresholds: precision/recall helpers walk intersection, then explicit intersection count repeats the walk** - `crates/rskim-bench/src/cochange/validate.rs:730-738`
**Confidence**: 82%
- Problem: For each (threshold, query) pair, `compute_precision` internally calls `predicted.intersection(actual).count()`, `compute_recall` calls the same `predicted.intersection(actual).count()`, and then line 738 calls `predicted_scratch.intersection(actual).count()` a third time for micro-TP accumulation. Three intersection walks over the same two sets per query. At scale (20K candidates x 6 thresholds x 500 queries per commit), this triples the hash lookups.
- Fix: Compute the intersection count once and derive precision, recall, and TP from it:
  ```rust
  let intersection_count = predicted_scratch.intersection(actual).count();
  let p = if predicted_scratch.is_empty() { 0.0 }
          else { intersection_count as f64 / predicted_scratch.len() as f64 };
  let r = if actual.is_empty() { 0.0 }
          else { intersection_count as f64 / actual.len() as f64 };
  commit_precision_sum += p;
  commit_recall_sum += r;
  micro_tp[ti] += intersection_count;
  ```

**`build_path_map` clones every PathBuf when building the final HashMap** - `crates/rskim-bench/src/cochange/validate.rs:94-97`
**Confidence**: 80%
- Problem: `build_path_map` first collects all unique paths into a `BTreeSet<&PathBuf>` (good -- zero-copy dedup), then clones every `PathBuf` when building the `HashMap` via `(p.clone(), FileId(...))`. For a repo with 20,000 files, this is 20K `PathBuf` heap allocations. The `BTreeSet` borrows from the input `commits` slice, so the clones are necessary to produce an owned map. However, the function is called once per repo, so this is amortized.
- Fix: This is acceptable for a benchmark binary that runs once per repo. Noting it for awareness -- if this function were called in a hot loop, the clones would need to be avoided (e.g., via arena allocation or returning borrowed keys). No action needed for current usage.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`clone_and_parse` calls `GixSource.parse_history(dest, 0)` with lookback_days=0, parsing entire repo history into memory** - `crates/rskim-bench/src/cochange/validate.rs:558-560`
**Confidence**: 82%
- Problem: For large repositories (e.g., ripgrep with 10,000+ commits), parsing the entire git history into a `Vec<CommitInfo>` loads all commit metadata and file-change lists into memory at once. With 500 files per commit average and 10K commits, this could be 5M `FileChangeInfo` structs. This is inherent to the benchmark's design (it needs full history for temporal split), but worth noting that peak memory per-repo could be significant.
- Fix: This is a design trade-off acknowledged in the pipeline doc comment. The bounded constants (`MAX_FILES_FOR_EVALUATION`, `MAX_TEST_COMMITS`, `MAX_FILES_PER_COMMIT`) prevent the evaluation phase from exploding, but the parse phase is unbounded. Consider adding a `MAX_COMMITS` constant (e.g., 500K) to bail early if a repo's history is unexpectedly large, preventing OOM on repos like the Linux kernel:
  ```rust
  const MAX_COMMITS_FOR_PARSE: usize = 500_000;
  if all_commits.len() > MAX_COMMITS_FOR_PARSE {
      anyhow::bail!("commit count {} exceeds safety limit {}", all_commits.len(), MAX_COMMITS_FOR_PARSE);
  }
  ```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Consider pre-filtering jaccard_cache by the minimum threshold before the sweep** - `crates/rskim-bench/src/cochange/validate.rs:279` (Confidence: 70%) -- Since thresholds are sorted ascending, pairs below the minimum threshold will never contribute at any threshold. Filtering them out during cache construction would shrink the cache and speed up all threshold sweeps.

- **`filter_denied` calls `to_string_lossy()` for every file in every commit** - `crates/rskim-bench/src/cochange/deny_list.rs:126-128` (Confidence: 65%) -- For non-UTF8 paths this allocates a `Cow::Owned`. On typical repos all paths are UTF-8 so it returns `Cow::Borrowed` and costs nothing beyond the method call, but a `path.as_os_str().to_str()` approach could skip the lossy conversion entirely.

- **`all_file_ids` Vec is re-sorted per call to `evaluate_at_thresholds` but only used for iteration** - `crates/rskim-bench/src/cochange/validate.rs:208-211` (Confidence: 60%) -- The sort is O(F log F) but happens once per repo, so it is negligible. Noting it because the sorted order is not exploited by any subsequent binary search -- it is iterated linearly.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Performance Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The benchmark pipeline demonstrates good performance awareness overall: bounded constants prevent runaway computation (`MAX_FILES_FOR_EVALUATION`, `MAX_TEST_COMMITS`, `MAX_FILES_PER_COMMIT`), the temporal split uses zero-copy `split_off`, and the `predicted_scratch` HashSet is reused across iterations to avoid per-query allocation. The parallel processing is sensibly capped at 3 threads.

The two HIGH findings address per-commit allocation churn in the evaluation hot loop (`compute_jaccard_cache` and `compute_actual_sets`), which are the dominant cost center for repos with many files. The redundant intersection computation in `sweep_thresholds` is a straightforward optimization that eliminates 2 of 3 hash-set walks per query. All three findings follow applies ADR-001 -- fix noticed issues now rather than deferring. Avoids PF-002 -- all findings are surfaced for resolution rather than classified as deferrable.
