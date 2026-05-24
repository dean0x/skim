# Regression Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### CRITICAL

(none)

### HIGH

**`skim search <query>` behavior change: FAILURE -> SUCCESS with side effects** - `crates/rskim/src/cmd/search/mod.rs:84`
**Confidence**: 95%
- Problem: On main, `skim search "fn parse"` returned `ExitCode::FAILURE` with a "not yet implemented" message. After this PR, passing any unrecognized positional argument triggers a full query execution pipeline including auto-index build, disk I/O, and network-free git HEAD reads. While this is the intended feature, the old test `test_search_unimplemented_returns_failure` was deleted and replaced only with a comment (line 471-473). No replacement test validates that query execution on a non-indexed, non-git directory gracefully returns `SUCCESS` (or a sensible exit code) rather than erroring. The query tests use `create_test_project` which sets up `.git` and source files -- there is no test for the degraded case of running `skim search "foo"` in a bare temp directory with no `.git`.
- Fix: Add a test for query execution in a directory with no `.git` and no index to confirm the exit code and error handling path. This ensures the behavior change from FAILURE to "query execution" does not panic or produce confusing errors in edge cases.

```rust
#[test]
fn test_query_in_non_git_dir_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    // No .git, no source files, no index
    let config = QueryConfig {
        text: "anything".to_string(),
        limit: 5,
        json: false,
        root: dir.path().to_path_buf(),
        cache_dir: dir.path().join("cache"),
    };
    std::fs::create_dir_all(&config.cache_dir).unwrap();
    // Should not panic; either Ok with 0 results or a graceful Err
    let _ = execute_query(&config, &TEST_ANALYTICS);
}
```

### MEDIUM

**`NoStoredHead` triggers unconditional rebuild on every query** - `crates/rskim/src/cmd/search/staleness.rs:243-248`
**Confidence**: 82%
- Problem: When `check_staleness` returns `NoStoredHead` (which includes manifests written by older skim versions without the `git_head` field), `auto_refresh_if_stale` triggers a full index rebuild. Since old manifests are common during version upgrades, the first query after upgrading skim will always rebuild -- expected. However, the rebuild stores git HEAD in the new manifest, so this only happens once. The risk is for non-git projects: `read_git_head` returns `None`, `set_git_head(None)` is stored, and on the next query `stored_git_head()` returns `None` again, meaning `check_staleness` returns `NoStoredHead` and triggers another rebuild. This creates a rebuild-every-query regression for non-git projects.
- Fix: Add a `StalenessCheck::NonGitProject` variant (or treat `stored: None, current: None` as `Current`) so non-git projects do not rebuild on every query invocation.

```rust
// In check_staleness, after loading stored head:
let stored = match manifest.stored_git_head() {
    Some(h) => h.to_string(),
    None => {
        // If there's no current HEAD either, the project is non-git.
        // Treat as current to avoid rebuild loops.
        if read_git_head(project_root).is_none() {
            return StalenessCheck::Current;
        }
        return StalenessCheck::NoStoredHead;
    }
};
```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**Spawned background process is never waited on or reaped** - `crates/rskim/src/cmd/init/install.rs:314-323`
**Confidence**: 85%
- Problem: The background `skim search --build` process is spawned with `Command::new(...).spawn()` but the returned `Child` handle is immediately dropped. On Unix, dropping a `Child` without calling `wait()` leaves a zombie process until the parent exits. For a CLI tool that exits quickly this is benign, but if `skim init` is called from a long-running agent session, the zombie persists. The `child.id()` is printed to stderr but never used again.
- Fix: Since this is intentionally fire-and-forget, explicitly call `std::mem::forget(child)` or `.wait()` in a detached thread. Or use the common pattern of double-forking on Unix. The current behavior is not a regression (new code), but is a latent resource leak.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`sorted_paths()` invariant depends on walk ordering** - `crates/rskim/src/cmd/search/manifest.rs:263-272` (Confidence: 70%) -- The `sorted_paths()` doc comment asserts that `sorted_paths()[n]` corresponds to `FileId(n)` because the index build pipeline walks files in sorted order. This invariant holds today because `walk_metadata` sorts by `rel_path` and the consumer processes entries sequentially. However, the invariant is implicit and fragile -- if the walk order or the consumer's insert order ever changes, query results will silently map to wrong files. Consider adding a debug assertion in the build pipeline that verifies the invariant at build time.

- **`is_hex_sha` does not reject uppercase hex** - `crates/rskim/src/cmd/search/staleness.rs:135-137` (Confidence: 65%) -- `is_hex_sha` uses `c.is_ascii_hexdigit()` which accepts both `a-f` and `A-F`. Git typically uses lowercase SHAs, but some tools may produce uppercase. This is not a bug per se (comparison still works if both sides use the same casing), but comparing a lowercase stored SHA against an uppercase current SHA would incorrectly trigger a rebuild.

- **Help text change: `skim search index` described as "legacy"** - `crates/rskim/src/cmd/search/mod.rs:359` (Confidence: 60%) -- The help text labels the `index` subcommand as "(legacy)" which may confuse existing users who have scripts using `skim search index`. The subcommand still works, but the wording suggests deprecation without a formal deprecation notice.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 1 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Regression Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The PR cleanly extends existing behavior without breaking public APIs. The `ManifestHeader` backward compatibility is correctly handled via `#[serde(default)]` and tested. The `skim search index` subcommand path is preserved. The `FORMAT_VERSION` remains at 1, which is correct since the schema change is additive (not breaking). The visibility widening of `resolve_search_cache_dir` from `fn` to `pub(super) fn` is scoped appropriately.

The two blocking issues are: (1) a missing edge-case test for the behavior change from "not implemented" to query execution, and (2) a rebuild-every-query loop for non-git projects caused by the staleness logic treating `stored: None, current: None` the same as `NoStoredHead`. The second issue is a functional regression for non-git project users.
