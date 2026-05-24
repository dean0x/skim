# Reliability Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19
**Scope**: Incremental (459d0af...HEAD, 2 commits)

## Issues in Your Changes (BLOCKING)

### HIGH

**String slicing on StalenessCheck::Display may panic on non-ASCII git SHAs** - `crates/rskim/src/cmd/search/staleness.rs:43-44`
**Confidence**: 82%
- Problem: The `Display` impl for `StalenessCheck::HeadChanged` uses byte-index slicing (`&stored[..8.min(stored.len())]`). If `stored` or `current` contained multi-byte UTF-8 characters (e.g. from a corrupted git HEAD read), slicing at byte position 8 could split a multi-byte codepoint and panic. The same pattern appears at line 290-291 in `auto_refresh_if_stale`. While SHA hex strings are always ASCII, the `HeadChanged` variant's `String` fields are populated from `read_git_head` which reads arbitrary file content -- a corrupted `.git/HEAD` could theoretically produce non-ASCII that passes `is_hex_sha` due to an upstream validation gap (unlikely but defensively worth hardening).
- Impact: Runtime panic in display formatting path, crashing the CLI for corrupted repos.
- Fix: Use `stored.get(..8).unwrap_or(stored)` or `stored.chars().take(8).collect::<String>()` instead of direct byte-index slicing, which returns the whole string on out-of-bounds rather than panicking. The same fix applies to line 290-291.

### MEDIUM

**`--limit 0` accepted silently -- unbounded result set** - `crates/rskim/src/cmd/search/mod.rs:142`
**Confidence**: 85%
- Problem: `parse_flags` parses `--limit` as `usize` and accepts `0`. The error message says "must be a positive integer" but `0` parses successfully as a `usize`. Depending on how downstream `run_query` handles `limit=0`, this could either produce no results (benign) or no upper bound on results (reliability concern for large indexes). The error message is misleading at minimum.
- Impact: User confusion or potentially unbounded memory allocation if `limit=0` means "no limit" downstream.
- Fix: Add a `limit == 0` rejection after parsing:
```rust
if limit == 0 {
    anyhow::bail!("--limit must be >= 1 (got 0)");
}
```

**Last action flag silently wins on conflicting flags** - `crates/rskim/src/cmd/search/mod.rs:130-135`
**Confidence**: 80%
- Problem: When a user passes `--build --rebuild`, the last flag wins because `action_flag` is overwritten each time. This is not necessarily wrong, but violates the "fail loud" principle from the project's error handling philosophy -- the user may not realize one flag was ignored.
- Impact: Silent misbehavior when conflicting flags are passed (e.g. `--build --stats` runs stats, not build).
- Fix: Detect the conflict explicitly:
```rust
"--build" => {
    if action_flag.is_some() {
        anyhow::bail!("conflicting action flags: only one of --build, --rebuild, --update, --stats, --install-hooks, --remove-hooks is allowed");
    }
    action_flag = Some(SearchAction::Build);
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none -- incremental review scope limited to diff)

## Suggestions (Lower Confidence)

- **Advisory lock blocks indefinitely** - `crates/rskim/src/cmd/search/index.rs:175-177` (Confidence: 70%) -- `lock_file.lock()` blocks without a timeout. If another skim process hangs while holding the lock, all subsequent builds block forever. Consider using `try_lock()` with a timeout loop, or at minimum document this behavior. The bounded-iteration principle suggests an upper bound on wait time.

- **`content.lines().count()` iterates twice** - `crates/rskim/src/cmd/search/snippet.rs:66-84` (Confidence: 65%) -- `extract_context_window` calls `content.lines().count()` (full scan) then `content.lines().enumerate().skip().take()` (partial scan). For large files, the first full scan is unnecessary if you restructure to just iterate with early termination. The 5 MB size guard in `extract_snippet` bounds the practical impact, but the double-iteration could be avoided.

- **`query_parts` collected when action flag is present** - `crates/rskim/src/cmd/search/mod.rs:176,181` (Confidence: 62%) -- If a user passes `--build somefile`, `somefile` is collected into `query_parts` and then silently discarded because `action_flag` takes precedence. This is a minor reliability concern (no error on unexpected positional args with action flags).

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 2 | 0 |
| Should Fix | 0 | 0 | 0 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 8/10
**Recommendation**: CHANGES_REQUESTED

The incremental changes are strong reliability improvements overall -- the infinite rebuild loop fix (4-way staleness matrix), the `SearchAction` enum replacing boolean flag cascade, `Result`-returning `parse_flags`, advisory build locking, and the `NamedTempFile` TOCTOU fix are all excellent hardening. The blocking HIGH issue (string slicing) is a defensive hardening concern rather than a likely runtime failure, and the MEDIUM issues around `--limit 0` acceptance and silent flag conflicts are straightforward to address.
