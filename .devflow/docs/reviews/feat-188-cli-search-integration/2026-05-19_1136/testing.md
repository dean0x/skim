# Testing Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### HIGH

**Missing test for `auto_refresh_if_stale` — the glue function between staleness detection and index rebuild** - `staleness.rs:199`
**Confidence**: 85%
- Problem: `auto_refresh_if_stale` is the central orchestrator that ties `check_staleness` to `build_index`, with four match arms (Current, NoIndex, HeadChanged, NoStoredHead) and debug output. It is called by both `execute_query` and `run_update` but has no direct unit test. The `check_staleness` function is well-tested and `execute_query` integration tests exercise `auto_refresh_if_stale` indirectly (cold-start path), but the `HeadChanged` and `NoStoredHead` refresh branches are never tested. A regression in any match arm would silently break auto-refresh without a test failure.
- Fix: Add tests in `staleness_tests.rs` that:
  1. Set up a project with a built index, then change HEAD, and verify `auto_refresh_if_stale` returns `Ok(true)` (refresh happened).
  2. Set up a project with no stored HEAD in the manifest, verify rebuild is triggered.
  3. Set up a project with current HEAD matching, verify `Ok(false)` (no refresh).

**Missing error-path test for `execute_query` with corrupt/invalid index** - `query.rs:58`
**Confidence**: 82%
- Problem: `execute_query` calls `NgramIndexReader::open(cache_dir)` which can fail on a corrupt `index.skidx`. There is no test verifying that `execute_query` propagates errors from a corrupt index file gracefully. All existing `execute_query` tests use the happy path (cold start auto-build). If `NgramIndexReader::open` returns an `Err` that doesn't map cleanly to anyhow, or if a regression causes a panic instead of `Err`, no test would catch it.
- Fix: Write a test that creates a fake `index.skidx` with garbage bytes and a valid manifest, then calls `execute_query` and asserts the result is `Err`.

### MEDIUM

**`parse_flags` missing tests for `--root`, `-n`, and combined flag scenarios** - `mod.rs:116-176`
**Confidence**: 85%
- Problem: `parse_flags` handles `--root PATH`, `--root=PATH`, and `-n` (short for `--limit`), but none of these are tested. The `--root` override is used by every subcommand (build, rebuild, update, stats, hooks, query). A regression in `--root` parsing (e.g., off-by-one in the `i += 1` for the value arg) would break all subcommands when used with `--root`. Combined flags (e.g., `--json --limit 5 authenticate`) are also untested.
- Fix: Add tests:
  ```rust
  #[test]
  fn test_parse_flags_root() {
      let flags = parse_flags(&["--root".into(), "/tmp/proj".into()]);
      assert_eq!(flags.root_override, Some(PathBuf::from("/tmp/proj")));
  }

  #[test]
  fn test_parse_flags_root_equals() {
      let flags = parse_flags(&["--root=/tmp/proj".into()]);
      assert_eq!(flags.root_override, Some(PathBuf::from("/tmp/proj")));
  }

  #[test]
  fn test_parse_flags_short_limit() {
      let flags = parse_flags(&["-n".into(), "3".into()]);
      assert_eq!(flags.limit, 3);
  }

  #[test]
  fn test_parse_flags_combined() {
      let flags = parse_flags(&["--json".into(), "--limit".into(), "5".into(), "authenticate".into()]);
      assert!(flags.json);
      assert_eq!(flags.limit, 5);
      assert_eq!(flags.query_text, "authenticate");
  }
  ```

**`byte_offset_to_line` lacks out-of-bounds offset test** - `snippet.rs:42-49`
**Confidence**: 82%
- Problem: `byte_offset_to_line` uses `offset.min(content.len())` to clamp, which is a safety mechanism against panics from `content[..offset]` out-of-bounds. There is no test that passes `offset > content.len()` to verify this clamping works. The clamping is correct in the implementation, but without a test this invariant is unprotected against refactoring.
- Fix: Add a test:
  ```rust
  #[test]
  fn test_byte_offset_to_line_beyond_end() {
      let content = b"line1\nline2\n";
      // offset=100 exceeds content length — must not panic
      let result = byte_offset_to_line(content, 100);
      assert_eq!(result, 2, "clamped to end of content");
  }
  ```

**No test for `format_text_output` with stale results** - `query.rs:146`
**Confidence**: 80%
- Problem: `format_text_output` renders a `[stale]` tag when `r.stale == true` (line 146). The existing `test_format_text_output_includes_path_and_score` test constructs a result with `stale: false`. There is no test verifying the stale tag appears in output. This is a user-visible feature added in this PR (commit 459d0af).
- Fix: Add a test with `stale: true` in the `ResolvedResult` and assert the output contains `[stale]`.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`test_format_text_output_empty_results` uses weak assertion** - `query_tests.rs:146-149`
**Confidence**: 83%
- Problem: The assertion `s.contains("no results") || s.is_empty() || s.contains("nothing")` is a disjunction of three conditions. The actual behavior (line 140 of query.rs) always writes `no results for "nothing"`, so the `s.is_empty()` branch can never be true. The `s.contains("nothing")` branch matches the query text, not the "no results" message. This means a regression that changes the output to just echo the query text would still pass.
- Fix: Tighten the assertion to match the actual format:
  ```rust
  assert!(
      s.contains("no results"),
      "empty result message should say 'no results', got: {s:?}"
  );
  ```

**`test_check_staleness_non_git_project_current_with_no_stored` comment/name mismatch** - `staleness_tests.rs:242-255`
**Confidence**: 81%
- Problem: The test name says `non_git_project_current` but the assertion checks for `StalenessCheck::NoStoredHead`, not `Current`. The comment at line 250 says "non-git project" but the manifest was written with `git_head: None`, which means the staleness check returns `NoStoredHead` (because the manifest has no stored HEAD). The test is technically correct but the name is misleading — it reads as if the non-git project should be considered "current", when it actually returns `NoStoredHead`.
- Fix: Rename to `test_check_staleness_non_git_project_returns_no_stored_head` to accurately reflect the assertion.

## Pre-existing Issues (Not Blocking)

(No pre-existing CRITICAL issues found in reviewed files.)

## Suggestions (Lower Confidence)

- **Missing integration test for `find_git_root_from_cwd`** - `install.rs:332` (Confidence: 70%) — The new `find_git_root_from_cwd` function is added to `install.rs` with a 256-iteration bound on ancestor traversal. It is untested. However, it is a thin utility and the `install.rs` module has its own test coverage scope that predates this PR.

- **No test verifying `resolve_paths_and_snippets` handles FileId out-of-range** - `query.rs:98` (Confidence: 65%) — `sorted_paths.get(r.file_id.0 as usize)?` uses `filter_map` to skip invalid FileIds, but no test verifies this behavior. If the index has more files than the manifest, results with orphaned FileIds should be silently dropped.

- **`test_execute_query_auto_builds_index_on_cold_start` has weak assertion** - `query_tests.rs:73-75` (Confidence: 65%) — The assertion `output.duration_ms < 60_000` is a 60-second timeout check, not a meaningful behavioral assertion. It would pass even if the function did nothing for 59 seconds. The test does also assert `output.query == "authenticate"` which validates the return, but the timing assertion adds no value.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

### Rationale

The test suite is well-structured with clear AAA patterns, proper isolation via `tempdir()`, and good coverage of happy paths. Test naming is descriptive and the co-located `_tests.rs` file pattern keeps tests organized. The 56 new tests across 5 test files represent solid coverage for a feature of this scope.

The two HIGH issues are the most important: `auto_refresh_if_stale` is the linchpin of the staleness-to-rebuild flow and has no direct test (only indirect coverage via `execute_query` cold-start), and `execute_query` has no error-path testing. Both are critical code paths for the search feature's reliability. The `parse_flags` gaps for `--root` and `-n` are notable because `--root` is threaded through every subcommand.

Overall test quality is good — tests verify behavior (not implementation), use fakes/temp dirs properly, and cover edge cases in the lower-level functions (`byte_offset_to_line`, `extract_context_window`, `strip_block`). The recommendations are additive (more coverage) rather than corrective (fixing bad tests).
