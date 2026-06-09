---
feature: cmd-search
name: Search CLI (skim search subcommand)
description: "Use when modifying the skim search CLI dispatch layer, adding new search flags or modes, changing how the lexical/AST/temporal indexes are built or queried from the CLI, updating the staleness/auto-refresh logic, changing the manifest sidecar format, or wiring together the rskim-search library features at the orchestration level. Keywords: skim search, cmd/search, mod.rs, index.rs, query.rs, staleness, manifest, ast, temporal, blast-radius, --hot, --cold, --risky, --ast, SearchAction, Flags, QueryConfig, IndexConfig, build_index, execute_query, auto_refresh_if_stale, check_staleness, FileId, FileId-alignment, consume loop, CHANNEL_CAPACITY, .skim-build.lock, .skidx, .skfiles, resolve_search_cache_dir, parse_flags, TemporalSort, TemporalAnnotation, cochange, blast_radius_paths, ast_file_ids, run_ast_standalone, run_temporal_standalone."
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
created: 2026-06-09
updated: 2026-06-09
---

# Search CLI (skim search subcommand)

## Overview

`crates/rskim/src/cmd/search/` is the CLI orchestration layer for `skim search`. It is the only code that performs I/O, parses flags, coordinates the build pipeline, triggers auto-refresh, resolves paths, and formats output. All business logic — n-gram extraction, BM25F scoring, AST index construction, temporal risk scoring, co-change matrices — lives in the `rskim-search` library crate. This layer exists solely to wire those library features together into a cohesive user-facing command.

The module is split into ten focused files: `mod.rs` (dispatch + hand-rolled flag parsing), `index.rs` (streaming pipeline), `query.rs` (search execution + formatters), `staleness.rs` (git-HEAD-based auto-refresh + AST self-heal), `types.rs` (pure data types), `ast.rs` (AST flag helpers), `temporal.rs` (temporal flag helpers), `manifest.rs` (JSONL sidecar), `walk.rs` (file traversal), and `snippet.rs` (context window extraction). The design rule is that I/O lives in `mod.rs`/`index.rs`/`query.rs` and helpers are pulled into the focused sub-modules — never scattered across files.

## System Context

`skim search` is invoked as a subcommand of the main `skim` binary. Its public entry point is `pub(crate) fn run(args: &[String], analytics: &AnalyticsConfig) -> anyhow::Result<ExitCode>` in `mod.rs`. The skim binary passes the slice of arguments that follow `search`.

Cache layout: `~/.cache/skim/search/{sha256(canonical_root)[..16]}/` (per-project hash). Override via `SKIM_CACHE_DIR`. Key files inside:

- `index.skidx` / `index.skfiles` — lexical n-gram index (built by `NgramIndexBuilder`)
- `ast_index.skidx` / `ast_index.skpost` — AST structural index (built by `AstIndexBuilder`)
- `index.skfiles` / `manifest.json` — actually `index.skfiles` is a JSONL sidecar (`FileManifest`)
- `temporal.db` — SQLite temporal DB populated by `skim heatmap`
- `.skim-build.lock` — advisory `flock`-based lock for concurrent build safety

## Component Architecture

### Flag Parsing — hand-rolled, not clap

`mod.rs` uses a manual `parse_flags()` loop rather than clap for the top-level `skim search` flags. This is intentional: `skim search index` (legacy subcommand) does use clap via `IndexCli`, but all other flags are hand-rolled so that positional query arguments are naturally accumulated into `query_parts`.

`SearchAction` enum encodes the mutually exclusive modes (`Build`, `Rebuild`, `Update`, `Stats`, `InstallHooks`, `RemoveHooks`, `Query(String)`). The final `match flags.action` is the single dispatch point — adding a new mode means adding a variant and one match arm.

Flags that require a following token (`--limit N`, `--root PATH`, `--ast PATTERN`, `--blast-radius FILE`) support both space-separated and equals form (`--limit=10`). The parser advances `i` by one extra when it consumes the next token.

### Dispatch Ordering — validation before dispatch

`run()` performs all validation in a fixed order before any dispatch:

1. `"index"` prefix → immediately delegate to `index::run()` (legacy subcommand path)
2. Empty args or `--help/-h` → print help (checked BEFORE `parse_flags` to avoid spurious errors)
3. `--ast` + temporal sort (`--hot/--cold/--risky`) → `#202` error (not yet composable)
4. `--ast` single-node pattern → `#283` error
5. Unknown `--ast` pattern → library error (lists valid names)

This ordering is tested and must not be changed without updating tests.

### Streaming Build Pipeline — `index.rs`

`build_index(config)` acquires `.skim-build.lock` (exclusive `flock`) then delegates to `Pipeline::run()`. The pipeline has three stages:

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

**FileId-alignment invariant** (critical): the lexical builder and AST builder must receive exactly the same set of files in the same order. `next_file_id` only advances after a successful `add_file_classified`. A lexical builder error causes `continue` — the file is excluded from BOTH indexes. AST entries are always inserted (empty set on linearization error) so FileIds stay aligned. If `add_file_ngrams` fails after `add_file_classified` succeeded, the build aborts with an error and `manifest.save()` is skipped — preventing a committed-but-corrupt index.

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

`execute_query(config, analytics)` is the main query path. Steps:

1. Empty text → short-circuit immediately (no I/O).
2. `auto_refresh_if_stale` — ensures fresh index, returns manifest.
3. Open `NgramIndexReader` → wrap in `QueryEngine`.
4. Build `SearchQuery`: set `limit` and `file_filter`.
5. **File filter construction** — intersection of blast-radius FileIds ∩ AST FileIds. Applied before `LIMIT` so the limit applies to the filtered set. Uses `u32::try_from(idx)` (applies PF-004: safe widening, never `as u32`).
6. `engine.search(&sq)` → raw `Vec<SearchResult>` with FileIds.
7. `resolve_paths_and_snippets` — maps FileId → path via `manifest.sorted_paths()`, extracts snippets.
8. Return `QueryOutput`.

After `execute_query`, `mod.rs` applies `apply_temporal_enrichment` (per-file DB lookups) if temporal sort flags are present.

### AST Flag Helpers — `ast.rs`

Three responsibilities:

- `open_ast_engine(cache_dir)` — fails loud (Err) when `ast_index.skidx` is absent; gives build guidance in error message.
- `validate_ast_pattern(raw)` — called at dispatch time BEFORE opening the index. Rejects `SingleNode` queries (`#283`) and unknown patterns.
- `resolve_ast_file_filter(engine, raw, lang)` — runs `SearchQuery` with `ast_pattern` set and `limit = usize::MAX` to get the full unfiltered FileId set for intersection.

Standalone `--ast` dispatch (no text query, no temporal flags): `run_ast_standalone` in `ast.rs`. Output is file-level only — no `:line` suffix (intentional, per spec).

### Temporal Flag Helpers — `temporal.rs`

Mirrors `ast.rs` in structure. Two independent data flows:

- **Blast-radius**: resolves a user path to co-change partners via `normalize_blast_radius_path` → `db.cochanges_for_file`. Returns `HashSet<String>` of repo-relative paths (including the target file itself). Converted to FileIds in `query.rs`.
- **Sort/annotation**: `apply_temporal_enrichment` annotates `Vec<ResolvedResult>` with hotspot/risk scores (per-file DB lookups) and re-sorts. O(N) DB queries — acceptable at default `--limit 20`, noted for large limits.

`check_temporal_staleness` (in `temporal.rs`) uses `git rev-parse HEAD` via subprocess with a 5-second timeout — distinct from the pure-file-I/O staleness check in `staleness.rs`. The subprocess path is used only for the temporal DB stale check, not for rebuilding.

### Manifest Sidecar — `manifest.rs`

JSONL file at `{cache_dir}/index.skfiles`. First line is a `ManifestHeader` (version, root path, optional `git_head`). Subsequent lines are `ManifestEntry` records (path, sha256, lang, field_map triples, mtime).

The manifest serves three purposes:
1. SHA-256 cache for incremental builds (avoids re-classifying unchanged files)
2. FileId → path mapping for query resolution (via `sorted_paths()`)
3. git HEAD storage for staleness detection

Writes are atomic (temp file + rename). Wrong-root detection: if the stored root path in the header doesn't match the current project root, the entire manifest is discarded.

## Component Interactions

```
skim binary
    └── cmd/search/mod.rs   ← parse_flags, SearchAction dispatch
            ├── index.rs    ← build_index(config) [streaming pipeline]
            │     ├── walk.rs          ← walk_metadata, open_and_read, sha256_hex
            │     ├── manifest.rs      ← FileManifest (SHA cache + path map + HEAD)
            │     └── rskim-search     ← NgramIndexBuilder, AstIndexBuilder,
            │                             classify_source, linearize_source,
            │                             extract_ast_ngrams_with_metrics
            ├── query.rs    ← execute_query(config), format_text/json_output
            │     ├── staleness.rs     ← auto_refresh_if_stale, check_staleness
            │     ├── manifest.rs      ← sorted_paths() for FileId→path
            │     ├── snippet.rs       ← extract_snippet
            │     └── rskim-search     ← NgramIndexReader, QueryEngine, SearchQuery
            ├── ast.rs      ← open_ast_engine, validate_ast_pattern,
            │                  resolve_ast_file_filter, run_ast_standalone
            │     └── rskim-search     ← AstQueryEngine, AstIndexReader, parse_ast_query
            └── temporal.rs ← normalize_blast_radius_path, apply_temporal_enrichment,
                               query_standalone, format_temporal_*
                  └── rskim-search     ← TemporalDb, HotspotRow, RiskRow, CochangeRow
```

## Constraints

**Concurrent build safety**: all callers that write index files (direct `--build`/`--rebuild`, git hook `--update`, `auto_refresh_if_stale`) acquire `.skim-build.lock` before touching index files. Never write to `index.skidx` or `ast_index.skidx` without holding this lock.

**FileId contract**: FileId is a 0-based integer assigned to files in the order they appear in `manifest.sorted_paths()`. It must be stable across the entire build cycle — the lexical and AST indexes must agree. Never break the consumer loop's `continue`-on-lexical-error + always-insert-AST-entry pattern.

**Graceful degradation**: missing `temporal.db` → warning + exit 0 (not error). Missing AST index → loud error (the user explicitly asked for `--ast`). Stale temporal data → warning on stderr, query proceeds.

**Commit ordering**: manifest is always the last thing written. If any index build fails, `manifest.save()` must not be called. The presence of the manifest is the "both indexes are coherent" signal.

**Validation order in `run()`**: the exact order (legacy subcommand, help, `--ast`+temporal, single-node, unknown pattern) is tested and must not change without updating the tests in `mod.rs`.

## Anti-Patterns

**Adding I/O to `ast.rs` or `temporal.rs` beyond what they already have.** These are focused helper modules. New queries, formatters, or DB operations belong there, but filesystem operations (cache dir resolution, lock acquisition) belong in `mod.rs` or `index.rs`.

**Calling `manifest.save()` after a failed index build.** Any code path that calls `builder.build()` or `ast_builder.build()` and then calls `manifest.save()` regardless of success breaks the crash-safety invariant. The manifest must only be saved when ALL index writes succeed.

**Adding temporal sort + `--ast` compound queries without resolving #202.** The current code explicitly errors on this combination. Do not silently degrade or ignore one of the flags — either implement the intersection (with a tracking ticket) or keep the error.

**Constructing FileIds from `idx as u32` (applies PF-004).** The file cap (50,000) makes overflow impossible in practice, but use `u32::try_from(idx)` everywhere FileId values are constructed from positional indexes. This is already the pattern in `query.rs` and must be followed consistently.

**Breaking the `index.rs` consume loop's fail-soft / fail-loud contract.** Lexical errors (from `add_file_classified`) are fail-soft: `continue` and skip the file from both indexes. AST errors (from `add_file_ngrams`) after a successful lexical insert are fail-loud: `return Err(...)` to abort the build. Do not swap these — fail-soft AST errors after a lexical success would advance `next_file_id` and corrupt the index.

## Gotchas

**`skim search index` vs `skim search --build`**: the `"index"` prefix check in `run()` is a legacy subcommand path. It dispatches to `index.rs::run()` which uses clap for its own flag parsing. The parent `run()` does not call `parse_flags()` for this path. `--help` for `skim search index` goes to the clap-generated help, not `print_help()`.

**Help is checked before `parse_flags`**: `--help` / `-h` in the top-level handler is caught BEFORE calling `parse_flags`. If help appears anywhere in `args` alongside a subcommand, `print_help()` is called. This is a regression risk — a test covers `skim search index --help` dispatching to index help not parent help.

**Blast-radius includes the target file itself**: `resolve_blast_radius_filter` adds the normalized path for the target file to the `HashSet` of partners. This is intentional — text queries like `skim search auth --blast-radius src/auth.rs` should surface matches within `src/auth.rs` itself, not just its co-change partners.

**`sorted_paths()` order = FileId**: the manifest stores entries in a `BTreeMap<String, ManifestEntry>` keyed by path. `sorted_paths()` returns keys in sorted order. The NgramIndexBuilder assigns FileId 0 to the first file sent to `add_file_classified`, 1 to the second, and so on — matching the consumer loop's `next_file_id` counter. These must stay in sync. If the manifest's sorted order ever differs from the build order, FileId → path resolution will silently mis-map results.

**AST self-heal and `NoStoredHead`**: `check_staleness` returns `NoStoredHead` (not a dedicated `AstStale` variant) when the AST index needs rebuilding. This means callers that pattern-match on `NoStoredHead` to detect "no git HEAD" will also be triggered by an AST-stale condition. Do not rely on `NoStoredHead` meaning exclusively "no git HEAD".

**`temporal.rs::read_git_head` is subprocess-based**: unlike `staleness.rs::read_git_head` (pure file I/O), the temporal staleness check in `temporal.rs` spawns `git rev-parse HEAD` with a 5-second timeout. These are two separate implementations for different callers. Do not unify them without understanding the divergent requirements (the staleness.rs version must work without any subprocess for speed; the temporal.rs version needs robustness in edge cases where packed-refs format may differ).

**`#289` temporal rebuild hook point**: `auto_refresh_if_stale` has a `TODO(#289)` comment immediately after the manifest load. When the temporal populate path is implemented, the call should go here (under the same `.skim-build.lock`, reusing the already-read HEAD).

## Key Files

- `crates/rskim/src/cmd/search/mod.rs` — entry point, flag parsing, dispatch, all action handlers, `resolve_blast_radius_filter`
- `crates/rskim/src/cmd/search/index.rs` — `build_index`, `Pipeline::run/consume`, `resolve_search_cache_dir`, FileId-alignment invariant
- `crates/rskim/src/cmd/search/query.rs` — `execute_query`, FileId filter construction, `format_text_output`, `format_json_output`
- `crates/rskim/src/cmd/search/staleness.rs` — `check_staleness`, `auto_refresh_if_stale`, AST self-heal, git HEAD file I/O
- `crates/rskim/src/cmd/search/types.rs` — `QueryConfig`, `IndexConfig`, `ResolvedResult`, `QueryOutput`, `TemporalSort`, `TemporalAnnotation`, `WalkEntry`, `ProcessedFile`
- `crates/rskim/src/cmd/search/ast.rs` — `open_ast_engine`, `validate_ast_pattern`, `resolve_ast_file_filter`, `run_ast_standalone`
- `crates/rskim/src/cmd/search/temporal.rs` — `normalize_blast_radius_path`, `apply_temporal_enrichment`, `query_standalone`, `format_temporal_*`
- `crates/rskim/src/cmd/search/manifest.rs` — `FileManifest`, `ManifestEntry`, `ManifestHeader`, atomic write, wrong-root detection

## Related

- Feature knowledge: `ast-index` — the `AstIndexBuilder`, `AstIndexReader`, `AstQueryEngine`, `AstNgramSet`, `StructuralMetrics`, `AST_INDEX_FORMAT_VERSION`, and `FORMAT_VERSION` probe used by `staleness.rs::check_staleness` and `index.rs::consume`
- Feature knowledge: `temporal-scoring` — the `TemporalDb`, `HotspotRow`, `RiskRow`, `META_GIT_HEAD` used by `temporal.rs`; the `top_hotspots`, `top_risks`, `hotspot_for_file`, `risk_for_file`, `cochanges_for_file` queries called from the CLI layer
- Feature knowledge: `cochange` — the `CochangeRow` and co-change data that backs `--blast-radius`
- ADR-004: follow-up tickets filed before implementation — tracked deferrals in this feature: `#283` (single-node/unigram), `#202` (--ast+temporal compound), `#289` (temporal rebuild hook), `#290` (AST incremental build cache)
- PF-004: u16→u32 widening before arithmetic — applied in `query.rs` when constructing `FileId(u32::try_from(idx)?)` from positional indexes
