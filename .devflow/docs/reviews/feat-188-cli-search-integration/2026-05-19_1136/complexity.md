# Complexity Review Report

**Branch**: feat/188-cli-search-integration -> main
**Date**: 2026-05-19

## Issues in Your Changes (BLOCKING)

### HIGH

**`run_stats` function has excessive nesting depth and mixed concerns** - `mod.rs:243-291`
**Confidence**: 85%
- Problem: `run_stats` (48 lines) sits at the warning threshold for function length and mixes three distinct concerns: index existence check, data gathering (reader + manifest + staleness), and formatting (JSON vs text). The JSON/text branching creates two parallel output paths each with nested `writeln!` calls, making the function harder to follow than its siblings.
- Fix: Extract the formatting into two small helpers (`format_stats_json` / `format_stats_text`) called from `run_stats`, mirroring the query module's `format_json_output` / `format_text_output` split. This brings `run_stats` under 20 lines and isolates the formatting logic:

```rust
fn run_stats(json: bool, root_override: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;
    let index_path = cache_dir.join("index.skidx");
    if !index_path.exists() {
        return print_no_index_error(json);
    }
    let reader = rskim_search::NgramIndexReader::open(&cache_dir)?;
    let stats = reader.stats();
    let manifest = manifest::FileManifest::load(root.clone(), cache_dir.clone())?;
    let git_head = manifest.stored_git_head().map(str::to_string);
    let staleness_status = staleness::check_staleness(&cache_dir, &root);

    let mut out = BufWriter::new(std::io::stdout());
    if json {
        format_stats_json(&stats, &git_head, &staleness_status, &mut out)?;
    } else {
        format_stats_text(&stats, &git_head, &staleness_status, &mut out)?;
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}
```

**`auto_refresh_if_stale` matches on `StalenessCheck::Current` after guarding it out** - `staleness.rs:199-252`
**Confidence**: 88%
- Problem: The function (53 lines) checks `matches!(staleness, StalenessCheck::Current)` on line 208 and returns early, then the `match` on line 220 includes a `StalenessCheck::Current => unreachable!()` arm. This is dead code that artificially inflates the match's cyclomatic complexity by one arm and adds cognitive overhead for readers who must reason about why it is unreachable rather than simply absent. The function overall is at the warning threshold (53 lines including docs) with 4 match arms.
- Fix: Remove the `Current` arm from the match block entirely. The early return on line 208 ensures the match is exhaustive over the remaining three variants. If the compiler requires the arm for exhaustiveness, restructure to use `if let` chains or convert the early return into an `else` block:

```rust
match staleness {
    StalenessCheck::NoIndex => { /* ... */ }
    StalenessCheck::HeadChanged { stored, current } => { /* ... */ }
    StalenessCheck::NoStoredHead => { /* ... */ }
    StalenessCheck::Current => return Ok(false),
}
Ok(true)
```

### MEDIUM

**`parse_flags` uses manual index-based iteration with mutable counters** - `mod.rs:116-176`
**Confidence**: 82%
- Problem: The function (60 lines) uses a manual `while i < args.len()` loop with interior `i += 1` increments for value-taking flags (`--limit`, `--root`). This is a known complexity pattern: manual index management is error-prone (forgetting `i += 1`, off-by-one on value consumption) and harder to reason about than iterator-based approaches. The function also has 10 distinct match arms, putting cyclomatic complexity around 12. For this codebase, which intentionally uses manual argument parsing rather than clap for subcommands, this is a reasonable trade-off, but the length is at the warning threshold.
- Fix: Consider splitting the value-consuming flags (`--limit`, `--root` and their `=` variants) into a helper, or using `args.windows(2)` pre-processing for flags that consume the next argument. Alternatively, accept the current shape but add a brief inline comment noting the intentional `i += 1` semantics for value-consuming flags. The function is well-structured within its constraints but would benefit from being ~10 lines shorter.

**`Flags` struct has 10 fields, 6 of which are mutually exclusive booleans** - `mod.rs:102-114`
**Confidence**: 80%
- Problem: The `Flags` struct has 6 boolean fields (`build`, `rebuild`, `update`, `stats`, `install_hooks`, `remove_hooks`) that are mutually exclusive at the semantic level -- only one command mode can be active at a time -- but the type system does not enforce this. A caller could construct `Flags { build: true, rebuild: true, ... }` and the dispatch in `run()` would silently pick `build` due to if-chain ordering. This is a maintainability concern: adding a new mode requires updating both the struct and the if-chain, and the priority ordering is implicit.
- Fix: Replace the 6 boolean fields with an enum:

```rust
enum SearchMode {
    Build,
    Rebuild,
    Update,
    Stats,
    InstallHooks,
    RemoveHooks,
    Query(String),
    Help,
}

struct Flags {
    mode: SearchMode,
    json: bool,
    limit: usize,
    root_override: Option<PathBuf>,
}
```

This makes the mutual exclusion explicit and simplifies `run()` to a single `match flags.mode { ... }`.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`FileManifest::load` is 84 lines with nested early returns** - `manifest.rs:152-236`
**Confidence**: 82%
- Problem: While this function existed before this PR, the PR added the `git_head` field handling (lines 234, 48-54) and now the function spans 84 lines with 6 early-return points (file not found, too large, empty, parse error, version mismatch, root mismatch). The sequential-validation pattern is acceptable but the function is beyond the 50-line warning threshold. Each guard clause returns `Self::new(project_root, cache_dir)` -- the same fallback repeated 5 times.
- Fix: Extract a `try_load_inner` helper that returns `Option<(ManifestHeader, HashMap<...>)>`, making the fallback explicit in the outer `load`:

```rust
pub(super) fn load(project_root: PathBuf, cache_dir: PathBuf) -> anyhow::Result<Self> {
    match Self::try_parse(&project_root, &cache_dir)? {
        Some((header, entries)) => Ok(Self { project_root, cache_dir, entries, git_head: header.git_head }),
        None => Ok(Self::new(project_root, cache_dir)),
    }
}
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`execute_install` growing toward multi-concern function** - `install.rs:272-327` (Confidence: 65%) -- The function now handles 4 distinct concerns (hook script, settings patching, guidance injection, search hooks + background build). At 55 lines it is past the warning threshold. Consider extracting the search setup block (lines 303-324) into `setup_search_integration()`.

- **`resolve_paths_and_snippets` uses `filter_map` with a 20-line closure** - `query.rs:89-120` (Confidence: 70%) -- The inline closure in `filter_map` contains pattern matching on `SnippetOutcome` and constructs a `ResolvedResult`. While the iterator chain is idiomatic, the closure length makes it harder to scan. A named helper function would improve readability.

- **`strip_block` in hooks.rs has 4 early-return paths for edge cases** - `hooks.rs:144-170` (Confidence: 62%) -- The function handles corrupted markers, missing markers, and newline trimming with multiple return points. Acceptable for defensive code but worth noting as a future simplification target.

## Summary

| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Complexity Score**: 7/10
**Recommendation**: APPROVED_WITH_CONDITIONS

The code is well-decomposed across 7 focused modules with clear single responsibilities. Function lengths are mostly within bounds, nesting depth is controlled through early returns, and the module structure follows the existing codebase conventions. The two HIGH findings (`run_stats` mixed concerns and `auto_refresh_if_stale` dead match arm) are straightforward to address. The MEDIUM finding about `Flags` booleans is a maintainability improvement worth considering before the command surface grows further. Overall, complexity is well-managed for a 2,341-line addition.
