---
feature: cmd-search
name: Search CLI (skim search subcommand)
description: "Use when modifying the skim search CLI dispatch layer, adding new search flags or modes, changing how the lexical/AST/temporal indexes are built or queried from the CLI, updating the staleness/auto-refresh logic, changing the manifest sidecar format, or wiring together the rskim-search library features at the orchestration level. Keywords: skim search, cmd/search, mod.rs, index.rs, query.rs, staleness, manifest, ast, temporal, blast-radius, --hot, --cold, --risky, --ast, SearchAction, Flags, QueryConfig, IndexConfig, build_index, execute_query, execute_query_with_manifest, auto_refresh_if_stale, check_staleness, FileId, FileId-alignment, consume loop, CHANNEL_CAPACITY, .skim-build.lock, .skidx, .skfiles, resolve_search_cache_dir, parse_flags, parse_temporal_flag, parse_limit_value, take_flag_value, TemporalSort, TemporalAnnotation, cochange, blast_radius_paths, ast_file_ids, run_ast_standalone, run_temporal_standalone, derive_ast_entry, search_ast, resolve_ast_file_filter, hooks.rs, install_search_hooks, remove_search_hooks, cochange_partner_paths."
category: architecture
directories:
  - crates/rskim/src/cmd/search/
referencedFiles:
  - crates/rskim/src/cmd/search/mod.rs
  - crates/rskim/src/cmd/search/index.rs
  - crates/rskim/src/cmd/search/query.rs
  - crates/rskim/src/cmd/search/staleness.rs
  - crates/rskim/src/cmd/search/types.rs
  - crates/rskim/src/cmd/search/ast.rs
  - crates/rskim/src/cmd/search/temporal.rs
  - crates/rskim/src/cmd/search/manifest.rs
  - crates/rskim/src/cmd/search/walk.rs
  - crates/rskim/src/cmd/search/snippet.rs
  - crates/rskim/src/cmd/search/hooks.rs
created: 2026-06-09
updated: 2026-06-09
version: 3
---

# Search CLI (skim search subcommand)

## Overview

`crates/rskim/src/cmd/search/` is the CLI orchestration layer for `skim search`. It is the only code that performs I/O, parses flags, coordinates the build pipeline, triggers auto-refresh, resolves paths, and formats output. All business logic — n-gram extraction, BM25F scoring, AST index construction, temporal risk scoring, co-change matrices — lives in the `rskim-search` library crate. This layer exists solely to wire those library features together into a cohesive user-facing command.

The module is split into eleven focused files: `mod.rs` (dispatch + hand-rolled flag parsing), `index.rs` (streaming pipeline), `query.rs` (search execution + formatters), `staleness.rs` (git-HEAD-based auto-refresh + AST self-heal), `types.rs` (pure data types), `ast.rs` (AST flag helpers), `temporal.rs` (temporal flag helpers), `manifest.rs` (JSONL sidecar), `walk.rs` (file traversal), `snippet.rs` (context window extraction), and `hooks.rs` (git hook install/remove). The design rule is that I/O lives in `mod.rs`/`index.rs`/`query.rs` and helpers are pulled into the focused sub-modules — never scattered across files.

## System Context

`skim search` is invoked as a subcommand of the main `skim` binary. Its public entry point is `pub(crate) fn run(args: &[String], analytics: &AnalyticsConfig) -> anyhow::Result<ExitCode>` in `mod.rs`. The skim binary passes the slice of arguments that follow `search`.

Cache layout: `~/.cache/skim/search/{sha256(canonical_root)[..16]}/` (per-project hash). Override via `SKIM_CACHE_DIR`. Key files inside:

- `index.skidx` / `index.skfiles` — lexical n-gram index (built by `NgramIndexBuilder`)
- `ast_index.skidx` / `ast_index.skpost` — AST structural index (built by `AstIndexBuilder`)
- `index.skfiles` / `manifest.json` — actually `index.skfiles` is a JSONL sidecar (`FileManifest`)
- `temporal.db` — SQLite temporal DB populated by `skim heatmap`
- `.skim-build.lock` — advisory file-based lock for concurrent build safety

## Component Architecture

### Flag Parsing — hand-rolled, not clap

`mod.rs` uses a manual `parse_flags()` loop rather than clap for the top-level `skim search` flags. This is intentional: `skim search index` (legacy subcommand) does use clap via `IndexCli`, but all other flags are hand-rolled so that positional query arguments are naturally accumulated into `query_parts`.

`SearchAction` enum encodes the mutually exclusive modes (`Build`, `Rebuild`, `Update`, `Stats`, `InstallHooks`, `RemoveHooks`, `Query(String)`). The final `match flags.action` is the single dispatch point — adding a new mode means adding a variant and one match arm.

Three private helpers in `parse_flags()`:

- **`take_flag_value(arg, next_arg, flag)`**: handles both space-separated and equals form for flags that require a value (`--limit`, `--root`, `--ast`). Returns `(value, consumed_next)`. Caller advances `i` by one extra when `consumed_next` is `true`.
- **`parse_limit_value(raw)`**: validates a `--limit` string — must be a positive integer ≥ 1. Returns `Err` for non-numeric or zero.
- **`parse_temporal_flag(arg, next_arg, temporal_sort, blast_radius)`**: handles `--hot`, `--cold`, `--risky`, `--blast-radius`, and `--blast-radius=VALUE`. Returns `Ok(true)` when the next token was consumed. Mutually-exclusive sort flag check lives here.

### Dispatch Ordering — validation before dispatch

`run()` performs all validation in a fixed order before any dispatch:

1. `"index"` prefix → immediately delegate to `index::run()` (legacy subcommand path)
2. Empty args or `--help/-h` → print help (checked BEFORE `parse_flags` to avoid spurious errors)
3. `--ast` + temporal sort (`--hot/--cold/--risky`) → `#202` error (not yet composable)
4. `--ast` single-node pattern → `#283` error
5. Unknown `--ast` pattern → library error (lists valid names)

This ordering is tested and must not be changed without updating tests. Note: `--ast` + `--blast-radius` is NOT an error — `--blast-radius` is composable with `--ast` on the standalone path.

### Streaming Build Pipeline — `index.rs`

`build_index(config)` acquires `.skim-build.lock` (advisory file-based lock) with a **bounded poll loop** before delegating to `Pipeline::run()`.

**Lock acquisition**: uses `try_lock()` (not unbounded `.lock()`) in a loop capped at 120 seconds (200 ms poll interval, ~9 iterations before deadline). Prints a one-time waiting notice to stderr on first `WouldBlock`. Returns an error on deadline expiry. Drop-based release is preserved — `lock_file` drops at function end. Never blocks indefinitely. `std::fs::TryLockError::WouldBlock` and `TryLockError::Error` are handled separately.

The pipeline has three stages:

1. **Walk** (`walk_metadata`) — metadata-only directory traversal; returns `Vec<WalkEntry>` sorted for determinism.
2. **Producer thread** — reads content, computes SHA-256, applies 2-tier SHA cache, classifies fields; sends `ProcessedFile` on a `crossbeam_channel::bounded(CHANNEL_CAPACITY=64)` channel.
3. **Consumer loop** (`Pipeline::consume`) — receives files, adds to both lexical and AST builders, inserts manifest entries, drops content immediately.

**Commit ordering** (crash-safety invariant):
```
(1) builder.build()       → writes index.skidx + index.skfiles
(2) ast_builder.build()   → writes ast_index.skpost then ast_index.skidx
(3) new_manifest.save()   → records git HEAD (the commit point)
```
If AST build fails, `manifest.save()` is NEVER reached. The old manifest survives and the next query self-heals. "HEAD recorded ⟹ both indexes coherent" is the invariant.

**Commit-boundary assertion** (before writes): the number of manifest entries (`manifest_count`) is compared to the consume loop's `file_count`. A mismatch aborts BEFORE any `build()` call. This is intentionally defensive — on case-sensitive filesystems the counts must match because each `WalkEntry` has a distinct rel-path key.

**FileId-alignment invariant** (critical): the lexical builder and AST builder must receive exactly the same set of files in the same order. `next_file_id` only advances after a successful `add_file_classified`. A lexical builder error causes `continue` — the file is excluded from BOTH indexes. AST entries are always inserted (empty set on linearization error) so FileIds stay aligned. If `add_file_ngrams` fails after `add_file_classified` succeeded, the build aborts with an error and `manifest.save()` is skipped — preventing a committed-but-corrupt index (ADR-006).

**`derive_ast_entry` helper** (index.rs private function): encapsulates per-file AST linearization and extraction. Returns `(AstNgramSet, StructuralMetrics, node_count)`. Deliberately infallible — on any error (grammar failure, linearization error, empty language) returns an empty-but-valid triple so the consume loop can always insert an aligned empty AST entry.

**Producer join on abort**: the producer thread is joined BEFORE propagating any consume error. This surfaces worker-thread panics on both the success and abort paths, and ensures the producer's tx.send() has already returned Err before the lock is released (applies ADR-006, happens-before guarantee).

**Happens-before note**: `producer_skips.load(Ordering::Relaxed)` is only valid after `producer_handle.join()` returns. Moving that load before `join()` would be a data race.

### Staleness and Auto-Refresh — `staleness.rs`

`check_staleness(cache_dir, project_root)` compares the git HEAD stored in the manifest against the current HEAD read from `.git/HEAD` (no git subprocess — pure file I/O). Handles ordinary repos, worktrees (`.git` file with `gitdir:` pointer), detached HEADs, and packed-refs.

`StalenessCheck` outcomes:
- `Current` — no action needed
- `HeadChanged` — rebuild triggered
- `NoStoredHead` — rebuild triggered (old manifest or new git repo)
- `NoIndex` — cold build triggered

**AST self-heal**: `check_staleness` runs an additional check before comparing HEADs. If `ast_index.skidx` is absent OR its format version (6-byte probe via `AstIndexReader::index_version`) is below `AST_INDEX_FORMAT_VERSION`, it returns `NoStoredHead` to force a full rebuild. This handles: post-format-upgrade (v1→v2), crash between `lexical.build()` and `ast.build()`, and first run after adding `--ast` to an existing install.

`auto_refresh_if_stale(root, cache_dir, analytics)` is called at the start of every query path. It returns `(refreshed: bool, manifest: FileManifest)` so the caller never loads the manifest a second time.

### Query Execution — `query.rs`

**Two entry points**:

- `execute_query(config, analytics)` — test-only entry point. Calls `execute_query_with_manifest(config, None, analytics)`. Annotated `#[cfg_attr(not(test), allow(dead_code))]`.
- `execute_query_with_manifest(config, pre_loaded_manifest, analytics)` — production path. When `pre_loaded_manifest` is `Some`, skips `auto_refresh_if_stale` entirely (used by the combined text+`--ast` path in `run_query`, which already refreshed before opening the AST engine). When `None`, refreshes itself — this is the pure-lexical path.

Steps in `execute_query_with_manifest`:
1. Empty text → short-circuit immediately (no I/O).
2. Refresh or use pre-loaded manifest.
3. Open `NgramIndexReader` → wrap in `QueryEngine`.
4. Build `SearchQuery`: set `limit` and `file_filter`.
5. **File filter construction** — intersection of blast-radius FileIds ∩ AST FileIds. Applied before `LIMIT` so the limit applies to the filtered set. Uses `u32::try_from(idx)` (applies PF-004: safe widening, never `as u32`).
6. `engine.search(&sq)` → raw `Vec<SearchResult>` with FileIds.
7. `resolve_paths_and_snippets` — maps FileId → path via `manifest.sorted_paths()`, extracts snippets.
8. Return `QueryOutput`.

After `execute_query_with_manifest`, `mod.rs` applies `apply_temporal_enrichment` (per-file DB lookups) if temporal sort flags are present.

### AST Flag Helpers — `ast.rs`

Four responsibilities:

- `open_ast_engine(cache_dir)` — fails loud (Err) when `ast_index.skidx` is absent; gives build guidance in error message.
- `validate_ast_pattern(raw)` — called at dispatch time BEFORE opening the index. Rejects `SingleNode` queries (`#283`) and unknown patterns.
- `resolve_ast_file_filter(engine, raw)` — **calls `search_ast` directly** (not through `SearchLayer`). Parses the pattern, calls `engine.search_ast(&query)`, returns `HashSet<FileId>` from the `Vec<(FileId, f64)>` result. No `SearchResult` construction, no `usize::MAX` sort, no `SearchLayer` overhead. The caller's `--limit` applies at intersection time inside the lexical engine.
- `run_ast_standalone` — standalone `--ast` dispatch (no text query, no temporal sort flags). Also calls `search_ast` directly. **Now supports `--blast-radius`**: resolves co-change peers via the temporal DB, converts paths → FileIds via `manifest.sorted_paths()`, and intersects with the AST result set BEFORE applying `--limit` (avoids PF-006 silent-drop). Then warns on out-of-range FileIds rather than silently dropping them.

**`resolve_ast_file_filter` signature** (no `lang` parameter): language filtering at the AST layer was removed — the intersection with lexical results handles language narrowing implicitly.

**`--ast + --blast-radius` on standalone path**: `run_ast_standalone` accepts `blast_radius: Option<&str>`, `temporal_db_path: &Path`, and `root: &Path`. When `blast_radius` is set, it opens `temporal.db` (via `temporal::open_temporal_db`), resolves the path to repo-relative form, looks up co-change partners, includes the target file itself (mirrors `resolve_blast_radius_filter`), converts paths → FileIds via `manifest.sorted_paths()`, and intersects with the AST result set. Limit is applied AFTER intersection.

**FileId warning in `run_ast_standalone`**: when `fid.0 as usize >= sorted.len()`, a warning is emitted to stderr. This is the fail-loud-ish counterpart on the read side (not silent drop).

**Output-level only — no `:line` suffix**: standalone AST output is file-level. Results are formatted as `path  score: N.NNN` with no line number suffix.

### Temporal Flag Helpers — `temporal.rs`

Mirrors `ast.rs` in structure. Key functions:

- **`normalize_blast_radius_path(raw, project_root)`**: resolves user path to repo-relative. Algorithm: if absolute, check existence; if relative, try project-root-relative first, then CWD-relative fallback. Canonicalizes, strips root prefix, replaces `\\` with `/`.
- **`cochange_partner_paths(partners, target)`**: extracts partner paths from co-change rows, handling both `file_a`/`file_b` directions via `cochange_partner` helper. Does NOT include the target file itself — callers add it separately.
- **`open_temporal_db(db_path)`**: returns `None` when absent or corrupt (graceful degradation).
- **`check_temporal_staleness(db, project_root)`**: spawns `git rev-parse HEAD` with 5-second timeout (distinct from the pure-file-I/O `staleness.rs::read_git_head`). Advisory only.
- **`query_standalone`**: dispatches to `top_hotspots`, `top_coldspots`, `top_risks`, or `cochanges_for_file` based on the sort/blast-radius flags.
- **`apply_temporal_enrichment`**: annotates `Vec<ResolvedResult>` with hotspot/risk scores and re-sorts. O(N) DB queries at default `--limit 20`.
- **`resort_partners_by_temporal`**: pre-truncates to `limit*5` (clamped at 100) before per-file DB lookups for blast-radius + sort combination.

### Git Hook Management — `hooks.rs`

New dedicated module (split out from `mod.rs`) for `--install-hooks` / `--remove-hooks`.

- `install_search_hooks(project_root)` — for each of `post-commit`, `post-merge`, `post-checkout`: creates the hook with `#!/bin/sh` + skim block if absent, appends the skim block if the hook exists without markers. Idempotent — running twice is a no-op if markers already present.
- `remove_search_hooks(project_root)` — strips the `# skim-search-start … # skim-search-end` block from each hook. Non-fatal: missing hooks silently skipped.
- Hook block format: `# skim-search-start\nskim search --update 2>/dev/null &\n# skim-search-end`
- Writes use atomic temp-file + rename pattern (via `tempfile::NamedTempFile`).
- Unix: sets hook file permissions to executable (`chmod +x` equivalent).

### Manifest Sidecar — `manifest.rs`

JSONL file at `{cache_dir}/index.skfiles`. First line is a `ManifestHeader` (version, root path, optional `git_head`). Subsequent lines are `ManifestEntry` records (path, sha256, lang, field_map triples, mtime).

The manifest serves three purposes:
1. SHA-256 cache for incremental builds (avoids re-classifying unchanged files)
2. FileId → path mapping for query resolution (via `sorted_paths()`)
3. git HEAD storage for staleness detection

Writes are atomic (temp file + rename). Wrong-root detection: if the stored root path in the header doesn't match the current project root, the entire manifest is discarded.

## Combined text+`--ast` Path — Single Refresh Guarantee

The combined text+`--ast` query path (`run_query` → `execute_query_with_manifest`) calls `auto_refresh_if_stale` **exactly once**, before opening the AST engine:

```
run_query():
  auto_refresh_if_stale() → (refreshed, manifest)   // self-heal AST index if needed
  open_ast_engine()                                   // safe: index guaranteed fresh
  resolve_ast_file_filter()                           // AST FileIds
  execute_query_with_manifest(pre_loaded=Some(manifest))  // skip redundant refresh
```

Previously, a bug caused `open_ast_engine` to be called before `auto_refresh_if_stale`, breaking self-heal on the combined path. The fix: always refresh before any index open, same as `run_ast_standalone`. The `pre_loaded_manifest` parameter threads the already-loaded manifest into `execute_query_with_manifest` so no second `auto_refresh_if_stale` occurs.

## Component Interactions

```
skim binary
    └── cmd/search/mod.rs   ← parse_flags, SearchAction dispatch
            ├── index.rs    ← build_index(config) [streaming pipeline]
            │     ├── walk.rs          ← walk_metadata, open_and_read, sha256_hex
            │     ├── manifest.rs      ← FileManifest (SHA cache + path map + HEAD)
            │     ├── derive_ast_entry ← infallible per-file AST helper (index.rs private)
            │     └── rskim-search     ← NgramIndexBuilder, AstIndexBuilder,
            │                             classify_source, linearize_source,
            │                             extract_ast_ngrams_with_metrics
            ├── query.rs    ← execute_query_with_manifest(config, manifest_opt), formatters
            │     ├── staleness.rs     ← auto_refresh_if_stale, check_staleness
            │     ├── manifest.rs      ← sorted_paths() for FileId→path
            │     ├── snippet.rs       ← extract_snippet
            │     └── rskim-search     ← NgramIndexReader, QueryEngine, SearchQuery
            ├── ast.rs      ← open_ast_engine, validate_ast_pattern,
            │                  resolve_ast_file_filter (direct search_ast),
            │                  run_ast_standalone (direct search_ast + blast-radius + FileId warn)
            │     └── rskim-search     ← AstQueryEngine, AstIndexReader,
            │                             parse_ast_query, search_ast
            ├── temporal.rs ← normalize_blast_radius_path, cochange_partner_paths,
            │                  apply_temporal_enrichment, query_standalone,
            │                  format_temporal_*, resort_partners_by_temporal
            │     └── rskim-search     ← TemporalDb, HotspotRow, RiskRow, CochangeRow
            └── hooks.rs    ← install_search_hooks, remove_search_hooks
                              (marker-delimited blocks in .git/hooks/)
```

## Constraints

**Concurrent build safety**: all callers that write index files acquire `.skim-build.lock` (via bounded `try_lock()` poll loop with 120s deadline) before touching index files. Never write to `index.skidx` or `ast_index.skidx` without holding this lock.

**FileId contract**: FileId is a 0-based integer assigned to files in the order they appear in `manifest.sorted_paths()`. It must be stable across the entire build cycle. Never break the consumer loop's `continue`-on-lexical-error + always-insert-AST-entry pattern.

**Graceful degradation**: missing `temporal.db` → warning + exit 0 (not error). Missing AST index → loud error (the user explicitly asked for `--ast`). Stale temporal data → warning on stderr, query proceeds.

**Commit ordering**: manifest is always the last thing written. If any index build fails, `manifest.save()` must not be called. The presence of the manifest is the "both indexes are coherent" signal.

**Validation order in `run()`**: the exact order (legacy subcommand, help, `--ast`+temporal, single-node, unknown pattern) is tested and must not change without updating the tests in `mod.rs`. `--ast` + `--blast-radius` (without temporal sort) is NOT in this error list — it is a valid combination.

**`auto_refresh_if_stale` before any index open**: both `run_ast_standalone` and `run_query` (the combined text+`--ast` path) must call `auto_refresh_if_stale` BEFORE opening the AST engine. Opening the engine first breaks self-heal for the combined path.

**`execute_query` is test-only**: production dispatch calls `execute_query_with_manifest` directly. Tests use `execute_query` (which delegates to `execute_query_with_manifest(config, None, analytics)`).

## Anti-Patterns

**Adding I/O to `ast.rs` or `temporal.rs` beyond what they already have.** New queries, formatters, or DB operations belong there, but filesystem operations (cache dir resolution, lock acquisition) belong in `mod.rs` or `index.rs`.

**Calling `manifest.save()` after a failed index build.** Any code path that calls `builder.build()` or `ast_builder.build()` and then calls `manifest.save()` regardless of success breaks the crash-safety invariant. The manifest must only be saved when ALL index writes succeed.

**Adding temporal sort + `--ast` compound queries without resolving #202.** The current code explicitly errors on this combination. Do not silently degrade or ignore one of the flags.

**Constructing FileIds from `idx as u32` (applies PF-004).** Use `u32::try_from(idx)` everywhere FileId values are constructed from positional indexes.

**Breaking the `index.rs` consume loop's fail-soft / fail-loud contract.** Lexical errors are fail-soft: `continue` and skip the file from both indexes. AST errors after a successful lexical insert are fail-loud: `return Err(...)` to abort the build.

**Routing CLI AST queries through `SearchLayer::search` instead of `search_ast`.** `resolve_ast_file_filter` and `run_ast_standalone` call `engine.search_ast(&query)` directly. Using `SearchLayer::search` adds overhead and the `lang` filter is not needed.

**Calling `open_ast_engine` before `auto_refresh_if_stale` in the combined text+`--ast` path.** This was the root cause of regression #10. Always refresh before opening any index.

**Silently dropping out-of-range FileIds in AST result resolution.** `run_ast_standalone` warns on stderr when a FileId is beyond the manifest range. Do not revert to silent `filter_map`.

**Using `execute_query` in production dispatch (it is test-only).** Use `execute_query_with_manifest` with `pre_loaded_manifest = Some(manifest)` when you've already refreshed, or `None` for the pure-lexical path.

**Using unbounded lock acquisition on `.skim-build.lock`.** The build lock must use the bounded `try_lock()` poll loop with a 120-second deadline. Never use `.lock()` (blocks indefinitely).

**Calling `producer_skips.load()` before `producer_handle.join()`.** The atomic load is only valid after join() returns — join() is the happens-before edge.

## Gotchas

**`skim search index` vs `skim search --build`**: the `"index"` prefix check in `run()` is a legacy subcommand path. It dispatches to `index.rs::run()` which uses clap for its own flag parsing. The parent `run()` does not call `parse_flags()` for this path.

**Help is checked before `parse_flags`**: `--help` / `-h` in the top-level handler is caught BEFORE calling `parse_flags`. If help appears anywhere in `args` alongside a subcommand, `print_help()` is called.

**Blast-radius includes the target file itself**: `resolve_blast_radius_filter` adds the normalized path for the target file to the `HashSet` of partners. Same behavior in `run_ast_standalone`. This is intentional.

**`sorted_paths()` order = FileId**: the manifest stores entries in a `BTreeMap<String, ManifestEntry>` keyed by path. `sorted_paths()` returns keys in sorted order. The NgramIndexBuilder assigns FileId 0 to the first file sent to `add_file_classified`, 1 to the second, and so on. These must stay in sync.

**AST self-heal and `NoStoredHead`**: `check_staleness` returns `NoStoredHead` (not a dedicated `AstStale` variant) when the AST index needs rebuilding. Callers that pattern-match on `NoStoredHead` to detect "no git HEAD" will also be triggered by an AST-stale condition.

**`temporal.rs::read_git_head` is subprocess-based**: unlike `staleness.rs::read_git_head` (pure file I/O), the temporal staleness check spawns `git rev-parse HEAD` with a 5-second timeout. These are two separate implementations for different callers. Do not unify them.

**`#289` temporal rebuild hook point**: `auto_refresh_if_stale` has a `TODO(#289)` comment immediately after the manifest load. When the temporal populate path is implemented, the call should go here (under the same `.skim-build.lock`, reusing the already-read HEAD).

**`--stats HEAD` field**: the `run_stats` handler reads the git HEAD from the manifest (not from the git repo), so it reflects the HEAD at the time of the last build, not the current HEAD. This is intentional — it shows the state the index was built from.

**`derive_ast_entry` is infallible by design**: any error in linearization or extraction returns an empty triple. The consume loop then inserts an empty AST entry for that FileId. The only fail-loud path is when `add_file_ngrams` itself rejects the entry after a lexical success.

**`cochange_partner_paths` does NOT include the target**: unlike `resolve_blast_radius_filter` and the blast-radius arm of `run_ast_standalone`, `cochange_partner_paths` itself does not include the target file. Callers that need the target included must call `paths.insert(normalized)` explicitly after.

**`parse_temporal_flag` handles blast-radius equals form**: `--blast-radius=FILE` (equals form) is handled by a `starts_with` arm in `parse_temporal_flag`, not by `take_flag_value`. This is distinct from `--limit` and `--root` which use `take_flag_value` for both forms.

## Test Coverage Summary

Test files are co-located alongside each source file:
- `ast_tests.rs` — validate_ast_pattern, parse_flags AST variants, standalone AST with real index, text+AST intersection, self-heal for below-FORMAT_VERSION, combined text+AST path self-heals when AST index absent, `--ast + --blast-radius` intersection
- `query_tests.rs` — execute_query against real index
- `staleness_tests.rs` — check_staleness variants, auto_refresh_if_stale
- `index_tests.rs` — Pipeline::consume, derive_ast_entry, FileId-alignment invariant, ADR-006 abort path
- `temporal_tests.rs` — normalize_blast_radius_path, apply_temporal_enrichment, query_standalone
- `hooks_tests.rs` — install/remove idempotency, hook block format
- `manifest_tests.rs`, `walk_tests.rs`, `snippet_tests.rs` — focused unit tests

## Key Files

- `crates/rskim/src/cmd/search/mod.rs` — entry point, flag parsing (`parse_flags`, `parse_limit_value`, `parse_temporal_flag`, `take_flag_value`), dispatch, all action handlers, `resolve_blast_radius_filter`
- `crates/rskim/src/cmd/search/index.rs` — `build_index`, `Pipeline::run/consume`, `derive_ast_entry`, `resolve_search_cache_dir`, FileId-alignment invariant, bounded lock acquisition
- `crates/rskim/src/cmd/search/query.rs` — `execute_query_with_manifest`, `execute_query` (test-only), FileId filter construction, `format_text_output`, `format_json_output`
- `crates/rskim/src/cmd/search/staleness.rs` — `check_staleness`, `auto_refresh_if_stale`, AST self-heal, git HEAD file I/O
- `crates/rskim/src/cmd/search/types.rs` — `QueryConfig`, `IndexConfig`, `ResolvedResult`, `QueryOutput`, `TemporalSort`, `TemporalAnnotation`, `WalkEntry`, `ProcessedFile`
- `crates/rskim/src/cmd/search/ast.rs` — `open_ast_engine`, `validate_ast_pattern`, `resolve_ast_file_filter`, `run_ast_standalone` (blast-radius support + FileId warn)
- `crates/rskim/src/cmd/search/temporal.rs` — `normalize_blast_radius_path`, `cochange_partner_paths`, `apply_temporal_enrichment`, `query_standalone`, `resort_partners_by_temporal`
- `crates/rskim/src/cmd/search/manifest.rs` — `FileManifest`, `ManifestEntry`, `ManifestHeader`, atomic write, wrong-root detection
- `crates/rskim/src/cmd/search/hooks.rs` — `install_search_hooks`, `remove_search_hooks`, marker-delimited hook blocks

## Related

- Feature knowledge: `ast-index` — the `AstIndexBuilder`, `AstIndexReader`, `AstQueryEngine`, `AstNgramSet`, `StructuralMetrics`, `AST_INDEX_FORMAT_VERSION`, `FORMAT_VERSION` probe, `search_ast` called directly by `ast.rs`
- Feature knowledge: `temporal-scoring` — the `TemporalDb`, `HotspotRow`, `RiskRow`, `META_GIT_HEAD` used by `temporal.rs`; `top_hotspots`, `top_coldspots`, `top_risks`, `hotspot_for_file`, `risk_for_file`, `cochanges_for_file`
- Feature knowledge: `cochange` — the `CochangeRow` and co-change data that backs `--blast-radius`
- ADR-004: follow-up tickets filed before implementation — tracked deferrals: `#283` (single-node/unigram), `#202` (--ast+temporal compound), `#289` (temporal rebuild hook), `#290` (AST incremental build cache)
- ADR-006: dual-index per-file desync aborts build before commit — the `consume` loop fail-loud-on-post-lexical-AST-error pattern and the `derive_ast_entry` infallible helper implement this decision
- PF-004: u16→u32 widening before arithmetic — applied in `query.rs` and `ast.rs` when constructing `FileId(u32::try_from(idx)?)` from positional indexes
