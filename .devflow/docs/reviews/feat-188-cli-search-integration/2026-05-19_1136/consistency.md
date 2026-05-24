# Consistency Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### HIGH

**`parse_flags` returns bare `Flags` instead of `anyhow::Result<Flags>`** - `crates/rskim/src/cmd/search/mod.rs:116`
**Confidence**: 85%
- Problem: Both `init::flags::parse_flags` and `heatmap::args::parse_args` return `anyhow::Result<T>` for their parsed config structs. The new `search::parse_flags` returns bare `Flags`, silently ignoring invalid `--limit` values (e.g. `--limit abc` silently keeps the default 20) and missing `--root` values. This is a pattern mismatch with how the rest of the codebase validates CLI input.
- Fix: Return `anyhow::Result<Flags>` and propagate validation errors for `--limit` and `--root`:
  ```rust
  fn parse_flags(args: &[String]) -> anyhow::Result<Flags> {
      // ...
      "--limit" | "-n" => {
          i += 1;
          let n = args.get(i)
              .ok_or_else(|| anyhow::anyhow!("--limit requires a value"))?
              .parse::<usize>()
              .map_err(|_| anyhow::anyhow!("--limit requires a positive integer"))?;
          limit = n;
      }
      // ...
      Ok(Flags { ... })
  }
  ```

**`-j` alias for `--json` not used by any other subcommand** - `crates/rskim/src/cmd/search/mod.rs:137`
**Confidence**: 82%
- Problem: The search module introduces `-j` as a short alias for `--json`. No other subcommand in the codebase uses `-j` (heatmap uses `--json` only, git subcommands use `--json` only). Introducing an undocumented short alias inconsistent with other subcommands creates confusion.
- Fix: Remove the `-j` alias to match the existing convention, or document it in the help text if intentionally adding it. If adding `-j`, it should be added consistently across all subcommands, not just search.

### MEDIUM

**Duplicate git-root discovery logic** - `crates/rskim/src/cmd/init/install.rs:332`
**Confidence**: 85%
- Problem: `find_git_root_from_cwd()` in install.rs duplicates the same logic as `walk::discover_project_root()`. Both walk up from a directory looking for `.git` with a 256-ancestor cap. The only difference is return type (`Option<PathBuf>` vs `anyhow::Result<PathBuf>`) and that `discover_project_root` canonicalizes the start path. Having two implementations of the same concept violates DRY and the single-source-of-truth principle used throughout the codebase.
- Fix: Extract a shared utility or have `find_git_root_from_cwd` delegate to `discover_project_root`:
  ```rust
  fn find_git_root_from_cwd() -> Option<std::path::PathBuf> {
      let cwd = std::env::current_dir().ok()?;
      crate::cmd::search::walk::discover_project_root(&cwd).ok()
  }
  ```
  Note: this requires making `discover_project_root` visible as `pub(crate)` rather than `pub(super)`.

**`StalenessCheck` uses `Debug` formatting for user-facing output** - `crates/rskim/src/cmd/search/mod.rs:271,287`
**Confidence**: 80%
- Problem: The `--stats` output uses `{staleness_status:?}` (Debug format) for both JSON and human-readable output. This exposes Rust enum internals like `HeadChanged { stored: "abc...", current: "def..." }` directly to users. Other subcommands use Display-style formatting or explicit string mapping for user-facing output.
- Fix: Implement `Display` for `StalenessCheck` or use explicit string mapping:
  ```rust
  let staleness_label = match &staleness_status {
      StalenessCheck::Current => "current".to_string(),
      StalenessCheck::HeadChanged { .. } => "stale (HEAD changed)".to_string(),
      StalenessCheck::NoStoredHead => "unknown (no HEAD recorded)".to_string(),
      StalenessCheck::NoIndex => "no index".to_string(),
  };
  ```

**Module doc comment in types.rs is outdated** - `crates/rskim/src/cmd/search/types.rs:1`
**Confidence**: 90%
- Problem: The module doc says `//! Shared types for the skim search index pipeline.` but the file now contains `SnippetLine`, `SnippetContext`, `QueryConfig`, `QueryOutput`, `ResolvedResult` -- types for the query/snippet/display pipeline, not just the index pipeline. This is a documentation drift.
- Fix: Update to:
  ```rust
  //! Shared types for the `skim search` subcommand — indexing, querying, and display.
  //!
  //! All types here are pure data — no I/O, no side effects.
  ```

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`run_build` uses `_analytics` prefix but `run_query` passes analytics through** - `crates/rskim/src/cmd/search/mod.rs:201,320`
**Confidence**: 82%
- Problem: `run_build` takes `_analytics: &crate::analytics::AnalyticsConfig` (unused, prefixed with underscore) while `run_query` passes `analytics` to `execute_query` which passes it to `auto_refresh_if_stale` which also prefixes it as `_analytics`. The analytics parameter is threaded through four function calls but never used anywhere in the search module. The inconsistency is that `run_build` honestly marks it unused, but `run_query` and `execute_query` accept it without underscore as if they use it.
- Fix: Either add analytics recording (consistent with other subcommands like heatmap which record analytics) or consistently use the `_` prefix at all call sites.

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **Hand-rolled flag parser instead of clap** - `crates/rskim/src/cmd/search/mod.rs:116-176` (Confidence: 65%) -- The sibling `index.rs` uses `clap::Parser` derive API for argument parsing. The new search top-level uses a hand-rolled `parse_flags` instead. Both `heatmap` and `init` also use hand-rolled parsing, so this is not strictly wrong, but within the `search/` module itself the inconsistency is notable. The hand-rolled approach lacks automatic `--help` generation for invalid flags and validation that clap provides for free.

- **`#[allow(dead_code)]` on `QueryConfig` and `ResolvedResult`** - `crates/rskim/src/cmd/search/types.rs:40,56` (Confidence: 70%) -- These types are actively used by query.rs. The `#[allow(dead_code)]` annotations suggest they were added preemptively before the query module existed. Now that the types are used, these annotations should be cleaned up.

- **Mixed output destinations: `eprintln!` vs `println!` in install.rs hooks section** - `crates/rskim/src/cmd/init/install.rs:309,311` (Confidence: 60%) -- Error output goes to stderr (`eprintln!`), success output goes to stdout (`println!`). The rest of `execute_install` uses `println!` for success messages (consistent with the check-mark pattern), but mixing stderr/stdout in the same code block is unusual for this codebase.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Consistency Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The new modules follow the established search/ directory conventions well: co-located `_tests.rs` files, `pub(super)` visibility, section separators with `// ===` comment blocks, comprehensive module-level documentation, and consistent error handling via `anyhow::Result`. The main consistency gaps are the `parse_flags` return type deviating from the `anyhow::Result<T>` pattern used by sibling subcommands, the `-j` alias not present anywhere else, and the Debug-formatted enum in user-facing output.
