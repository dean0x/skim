# Complexity Review Report

**Branch**: feature/191-cochange-validation-benchmark -> main
**Date**: 2026-05-29

## Issues in Your Changes (BLOCKING)

### HIGH

**`evaluate_at_thresholds` exceeds function length limit (175 lines)** - `crates/rskim-bench/src/cochange/validate.rs:180`
**Confidence**: 92%
- Problem: At 175 lines, this function is well above the 50-line warning threshold and approaching the 200-line critical threshold. It manages 8 accumulator vectors, a jaccard cache, actual sets, and a threshold sweep — three distinct responsibilities in a single function body.
- Fix: Extract into three focused helpers:
  1. `compute_jaccard_cache(commit, known_ids, all_file_ids, reader)` — the inner loop building jaccard pairs (lines 242-261)
  2. `compute_actual_sets(known_ids)` — pre-computing the ground truth sets (lines 265-274)
  3. `sweep_thresholds(jaccard_cache, actual_sets, thresholds, accumulators)` — the threshold loop (lines 278-312)

  This keeps `evaluate_at_thresholds` as an orchestrator under 50 lines that initializes accumulators, loops over commits, and assembles results.

**Deep nesting (6 levels) in jaccard cache computation** - `crates/rskim-bench/src/cochange/validate.rs:243-261`
**Confidence**: 90%
- Problem: The nested structure is: `for commit` > `for query_id` > `for candidate_id` > `if` / `match` > `Ok(j) if j > 0.0` > `pairs.push(...)`. Six levels of indentation make the control flow difficult to follow and test in isolation.
- Fix: Extract the inner two loops into a helper function:
  ```rust
  fn build_jaccard_pairs(
      query_id: FileId,
      all_file_ids: &[FileId],
      reader: &CochangeMatrixReader,
  ) -> anyhow::Result<Vec<(FileId, f64)>> {
      let mut pairs = Vec::new();
      for &candidate_id in all_file_ids {
          if candidate_id == query_id {
              continue;
          }
          match reader.jaccard(query_id, candidate_id) {
              Ok(j) if j > 0.0 => pairs.push((candidate_id, j)),
              Ok(_) => {}
              Err(SearchError::IndexCorrupted(msg)) => {
                  return Err(anyhow::anyhow!("matrix corrupted: {msg}"));
              }
              Err(e) => return Err(anyhow::anyhow!("jaccard error: {e}")),
          }
      }
      Ok(pairs)
  }
  ```
  This reduces the outer loop from 6 levels to 3 levels (commit > query > call helper).

### MEDIUM

**`to_markdown` function length (94 lines) with string-building complexity** - `crates/rskim-bench/src/cochange/report.rs:37`
**Confidence**: 82%
- Problem: The function manually constructs a markdown report through sequential `push_str` calls across 5 distinct sections. While each section is simple, the combined length (94 lines) exceeds the 50-line warning threshold.
- Fix: Extract each of the 5 numbered sections (summary, threshold sweep, per-repo, methodology, reproducibility) into private helper functions that return `String`, then join them in `to_markdown`. The `repo_section` helper already demonstrates this pattern.

**`chrono_now` uses magic numbers for calendar arithmetic** - `crates/rskim-bench/src/bin/cochange_validate.rs:237-246`
**Confidence**: 80%
- Problem: Variables like `719_468`, `146_097`, `146_096`, `153`, and various formulae rely on magic numbers from the Hinnant algorithm. While the algorithm reference URL is provided, the constants are not named, making the code nearly impossible to verify without external reference.
- Fix: Name the key constants:
  ```rust
  const DAYS_FROM_CIVIL_EPOCH: i64 = 719_468; // days from 0000-03-01 to 1970-01-01
  const DAYS_PER_ERA: i64 = 146_097;          // 400 years in days
  ```
  Alternatively, since this is a benchmark binary (not core library), consider using the `time` or `jiff` crate which is already available in the Rust ecosystem for this exact purpose, trading 26 lines of hand-rolled calendar math for a single function call.

**`capture_head_sha` duplicates existing `git_run_with_timeout` pattern** - `crates/rskim-bench/src/cochange/validate.rs:646-693`
**Confidence**: 85%
- Problem: This function re-implements the exact same spawn-thread-for-timeout pattern that exists in `rskim_research::clone::git_run_with_timeout` (including platform-specific SIGKILL/taskkill, channel-based timeout). The duplication means two places to maintain the timeout and kill logic. The only difference is that `capture_head_sha` captures stdout output while `git_run_with_timeout` returns a bool.
- Fix: Either generalize `git_run_with_timeout` in `rskim_research` to optionally capture output (e.g., return `Result<Option<String>>` or accept a flag), or extract the timeout-spawn-kill logic into a shared utility in the workspace. This eliminates 48 lines of duplicated complexity. Applies ADR-001 (fix noticed issues immediately).

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`full_pipeline_synthetic_repo` integration test is 135 lines** - `crates/rskim-bench/tests/cochange_validation.rs:280`
**Confidence**: 80%
- Problem: This test combines repo setup (creating 50+ git commits), pipeline execution, and assertion into a single test body at 135 lines. The setup phase alone is ~60 lines of sequential git operations.
- Fix: Extract the synthetic repo creation into a helper like `create_coupling_repo(dir, num_training, num_solo)` that returns the `TempDir`. This makes the test body focus on pipeline logic and assertions while the setup helper can be reused for additional test scenarios.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`validate.rs` total file length approaches warning zone** - `crates/rskim-bench/src/cochange/validate.rs` (Confidence: 70%) — At 698 non-test lines, this file is above the 500-line critical threshold. The `evaluate_at_thresholds` extraction suggested above would bring it closer to a reasonable size.

- **`aggregate_metrics` accumulates 7 variables in a loop** - `crates/rskim-bench/src/cochange/validate.rs:478-500` (Confidence: 65%) — The closure captures `mp_sum`, `mr_sum`, `mip_sum`, `mir_sum`, `count`, `total_commits`, `total_queries`. An intermediate struct like `AccumulatedMetrics` could make the accumulation self-documenting.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 6/10
**Recommendation**: CHANGES_REQUESTED

The code is well-structured at the module level (5 focused modules with clear responsibilities), but the core evaluation function (`evaluate_at_thresholds`) carries too much weight at 175 lines with 6 levels of nesting. The `capture_head_sha` duplication adds unnecessary maintenance surface. Extracting 2-3 helper functions from `evaluate_at_thresholds` would bring this PR to a comfortable complexity level. Avoids PF-002 by surfacing all findings for resolution.
