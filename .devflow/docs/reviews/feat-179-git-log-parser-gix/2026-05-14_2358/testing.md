# Testing Review Report

**Branch**: feat/179-git-log-parser-gix -> main
**Date**: 2026-05-14

## Issues in Your Changes (BLOCKING)

### HIGH

**Silent test skip on missing git CLI -- tests pass vacuously** - `crates/rskim-search/src/temporal/git_parser_tests.rs:107,149,175,208,235,258,299,313,332,349,370,395`
**Confidence**: 85%
- Problem: 12 of the 25 new tests use the pattern `if !git_available() { return; }` followed by `let Some(dir) = init_git_repo() else { return };`. When `git` is not on PATH (possible in sandboxed CI environments or minimal containers), all 12 tests silently pass without executing any assertions. This means the test suite could report 25/25 passing with zero behavioral coverage exercised. There is no mechanism to detect or report this silent skip -- `cargo test` shows them as passed, not skipped.
- Fix: Use `#[ignore]` with a check, or use an explicit skip message that is visible in output:
```rust
#[test]
fn test_empty_repo_returns_ok_empty() {
    if !git_available() {
        eprintln!("SKIPPED: git not available");
        return;
    }
    // ... rest of test
}
```
  Alternatively, consolidate the guard into a macro that prints to stderr, or use a build-time feature flag (`#[cfg_attr(not(feature = "git-tests"), ignore)]`) so CI can explicitly opt in/out and skipped tests appear in results. The current approach risks false confidence in CI pipelines that lack git.

### MEDIUM

**No test for lookback filtering actually excluding old commits** - `crates/rskim-search/src/temporal/git_parser_tests.rs:298-325`
**Confidence**: 82%
- Problem: The lookback filtering tests (`test_lookback_zero_returns_all_history` and `test_lookback_large_value_returns_recent`) only test cases where all commits are included. There is no test that verifies old commits are actually *excluded* when `lookback_days` is small. Since all test commits are created within milliseconds of each other, a `lookback_days=1` call would include them all, making it impossible to verify filtering behavior. This is a missing edge case for a core feature.
- Fix: Add a test that uses `GIT_AUTHOR_DATE` / `GIT_COMMITTER_DATE` environment variables to create commits with timestamps in the past (e.g., 180 days ago), then call `parse_history` with `lookback_days=30` and assert those old commits are excluded:
```rust
#[test]
fn test_lookback_excludes_old_commits() {
    if !git_available() { return; }
    let Some(dir) = init_git_repo() else { return };

    // Commit with a date 180 days ago
    let old_date = "2025-11-15T12:00:00";
    std::fs::write(dir.path().join("old.txt"), "old").unwrap();
    Command::new("git")
        .args(["add", "old.txt"])
        .current_dir(dir.path())
        .output().unwrap();
    Command::new("git")
        .args(["commit", "-m", "old commit"])
        .env("GIT_AUTHOR_DATE", old_date)
        .env("GIT_COMMITTER_DATE", old_date)
        .current_dir(dir.path())
        .output().unwrap();

    // Recent commit
    git_commit_file(dir.path(), "new.txt", "new", "recent commit");

    let src = GixSource;
    let history = src.parse_history(dir.path(), 30).expect("parse");
    assert_eq!(history.commits.len(), 1, "old commit should be excluded");
    assert_eq!(history.commits[0].message, "recent commit");
}
```

**No test for file rename tracking** - `crates/rskim-search/src/temporal/git_parser.rs:219-227`
**Confidence**: 80%
- Problem: The implementation handles `Change::Rewrite` (renames) by extracting the destination location, but there is no test verifying rename behavior. The heatmap's `git_source.rs` has tests for rename resolution in numstat output (`test_rename_resolution_*`), but the new `GixSource` parser uses a completely different code path (gix tree diff) that is untested for renames. If `Change::Rewrite` changes behavior in a future gix version, there would be no regression guard.
- Fix: Add a test that renames a file and verifies the new path appears in `changed_files`:
```rust
#[test]
fn test_file_rename_appears_in_changed_files() {
    if !git_available() { return; }
    let Some(dir) = init_git_repo() else { return };
    git_commit_file(dir.path(), "old_name.rs", "content", "add file");
    Command::new("git")
        .args(["mv", "old_name.rs", "new_name.rs"])
        .current_dir(dir.path())
        .output().unwrap();
    Command::new("git")
        .args(["commit", "-m", "rename file"])
        .current_dir(dir.path())
        .output().unwrap();

    let src = GixSource;
    let history = src.parse_history(dir.path(), 0).expect("parse");
    let rename_commit = &history.commits[0];
    let paths: Vec<_> = rename_commit.changed_files.iter()
        .map(|f| f.path.to_string_lossy().to_string()).collect();
    assert!(
        paths.iter().any(|p| p.contains("new_name.rs")),
        "expected new_name.rs in rename commit, got: {paths:?}"
    );
}
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Unsafe `timestamp as u64` cast in heatmap metrics** - `crates/rskim/src/cmd/heatmap/metrics.rs:215`
**Confidence**: 82%
- Problem: The shared `CommitInfo.timestamp` type changed from `u64` to `i64` to accommodate pre-epoch commits, but the heatmap metrics code uses `commit.timestamp as u64` which will wrap negative timestamps to very large u64 values. While pre-epoch commits are rare, this is an unchecked assumption that could produce wildly incorrect stability scores. The test helper `make_commit` also does `ts as i64` (line 526), meaning the test setup masks this issue since tests never exercise negative timestamps.
- Fix: Use `u64::try_from(commit.timestamp).unwrap_or(0)` or clamp:
```rust
.push(commit.timestamp.max(0) as u64);
```

**Heatmap coupling uses `unwrap_or("")` for path conversion** - `crates/rskim/src/cmd/heatmap/metrics.rs:99`
**Confidence**: 80%
- Problem: `f.path.to_str().unwrap_or("")` silently converts non-UTF-8 paths to empty strings. An empty string key in the coupling HashMap would accumulate phantom co-occurrence data from all files with non-UTF-8 paths, producing misleading coupling metrics. While rare on most systems, this differs from the `to_string_lossy()` approach used elsewhere in the same file (e.g., lines 36, 211, 268, 338, 450, 462), making the behavior inconsistent.
- Fix: Use `to_string_lossy()` consistently, matching other call sites in the same file:
```rust
.map(|f| f.path.to_string_lossy().into_owned())
// or
.map(|f| f.path.to_string_lossy().as_ref())
```
  Note: this would change the coupling map from `&str` borrows to owned `String` keys, which has a small performance implication. The `&str` borrow approach works because paths borrow from `CommitRecord` which outlives the map -- but `to_string_lossy()` returns a `Cow<str>` that may need `.as_ref()` handling. Consider whether the lifetime borrow optimization is worth the inconsistency.

## Pre-existing Issues (Not Blocking)

(none -- no critical pre-existing testing issues observed in unchanged code)

## Suggestions (Lower Confidence)

- **Missing test for `first_line_of` helper** - `crates/rskim-search/src/temporal/git_parser.rs:273` (Confidence: 70%) -- The `first_line_of` helper is used to extract the first line of commit messages but has no direct unit test. Edge cases like empty strings, strings with only whitespace, or strings with leading blank lines are untested. This is a pure function that would benefit from a small focused test.

- **Serialization roundtrip test could use `assert_eq!` on the whole struct** - `crates/rskim-search/src/temporal/git_parser_tests.rs:491-518` (Confidence: 62%) -- The `CommitInfo` type derives `PartialEq`, so the roundtrip test could use `assert_eq!(restored, original)` instead of comparing each field individually. This would be more concise and would automatically catch any future field additions that are missed in the test.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 2 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Testing Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The test suite is well-structured overall: 25 tests covering repository opening, commit parsing, file tracking, lookback filtering, metadata, fix classification, trait safety, and serialization. Tests follow clear AAA structure with descriptive names and good error messages. The test infrastructure (init_git_repo, git_commit_file, git_delete_file helpers) is clean and reusable. The key gaps are: (1) silent test skipping when git is unavailable could mask zero-coverage runs in CI, (2) no test exercises lookback filtering actually *excluding* commits, and (3) no test covers file renames through the gix tree-diff code path. Addressing the HIGH and MEDIUM blocking issues would bring this to a solid 8-9/10.
