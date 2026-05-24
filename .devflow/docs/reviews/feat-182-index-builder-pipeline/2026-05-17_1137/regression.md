# Regression Review Report

**Branch**: feat/182-index-builder-pipeline -> main
**Date**: 2026-05-17

## Issues in Your Changes (BLOCKING)

### HIGH

**`skim search index --help` prints parent help instead of subcommand help** - `crates/rskim/src/cmd/search/mod.rs:34`
**Confidence**: 95%
- Problem: The `run()` function checks `args.iter().any(|a| matches!(a.as_str(), "--help" | "-h"))` before dispatching to the `index` subcommand. When a user runs `skim search index --help`, `args` is `["index", "--help"]`, and the `any()` scan finds `"--help"` at position 1, printing the parent `search` help text instead of delegating to `index::run(&["--help"])` which has its own help. The user never sees the index-specific help (with `--root`, `--force`, `--max-files` documentation).
- Fix: Only check for `--help`/`-h` when it is the first argument, or when `args[0]` does not match a known subcommand. A clean approach:
```rust
pub(crate) fn run(
    args: &[String],
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.is_empty() {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Dispatch subcommands first — let them handle their own --help
    if args.first().is_some_and(|a| a == "index") {
        let rest = &args[1..];
        return index::run(rest);
    }

    // Top-level help (only when not a subcommand)
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    // Has a query arg -> not yet implemented
    eprintln!("skim search: not yet implemented");
    Ok(ExitCode::FAILURE)
}
```

## Issues in Code You Touched (Should Fix)

(none)

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

(none -- all findings met the 80% threshold)

## Regression Checklist

- [x] No exports removed without deprecation -- `pub(crate) fn run()` signature preserved exactly
- [x] Return types backward compatible -- still `anyhow::Result<ExitCode>`
- [x] Default values unchanged -- help and unimplemented paths preserved
- [x] All consumers of changed code updated -- `cmd/mod.rs` dispatch unchanged (`search::run(args, analytics)`)
- [x] CLI options preserved -- `--help`, `-h`, no-args all behave identically
- [x] Commit messages match implementation -- all 7 commits accurately describe their changes
- [x] File rename (search.rs -> search/mod.rs) preserves module path -- `mod search;` in cmd/mod.rs still resolves correctly
- [x] `tempfile` promotion from dev-dependencies to dependencies is justified -- `manifest.rs` uses `NamedTempFile` in production code for atomic writes
- [x] No deleted files -- rename only (R070 status)
- [x] All 4006 workspace tests pass (0 failures, 2 skipped)

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 1 | 0 | - |
| Should Fix | - | 0 | 0 | - |
| Pre-existing | - | - | 0 | 0 |

**Regression Score**: 9/10
**Recommendation**: CHANGES_REQUESTED

The single blocking issue is that `skim search index --help` is intercepted by the parent dispatcher and shows the wrong help text. The fix is a straightforward reordering of the dispatch logic (check subcommands before scanning all args for `--help`). Once fixed, this PR introduces no regressions -- the file rename, dependency promotion, help text updates, and new subcommand dispatch are all clean.
