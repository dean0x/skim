---
feature: cmd-search
name: Search CLI (skim search subcommand)
description: "Use when modifying the skim search CLI dispatch layer, adding new search flags or modes, changing how the lexical/AST/temporal indexes are built or queried from the CLI, updating the staleness/auto-refresh logic, changing the manifest sidecar format, or wiring together the rskim-search library features at the orchestration level. Keywords: skim search, cmd/search, mod.rs, index.rs, query.rs, staleness, manifest, ast, temporal, blast-radius, --hot, --cold, --risky, --ast, SearchAction, Flags, QueryConfig, IndexConfig, build_index, execute_query, execute_query_with_manifest, auto_refresh_if_stale, check_staleness, FileId, FileId-alignment, consume loop, CHANNEL_CAPACITY, .skim-build.lock, .skidx, .skfiles, binary manifest, SKFM, MANIFEST_FORMAT_VERSION, version_matches, manifest_stale, total-on-disk size, resolve_search_cache_dir, parse_flags, parse_temporal_flag, parse_limit_value, take_flag_value, TemporalSort, TemporalAnnotation, cochange, blast_radius_paths, ast_file_ids, run_ast_standalone, run_temporal_standalone, derive_ast_entry, search_ast, resolve_ast_file_filter, hooks.rs, install_search_hooks, remove_search_hooks, resolve_blast_radius_paths, resolve_blast_radius_file_ids, paths_to_file_ids, cochange_partner_paths, temporal_build, build_lock, AstNgramCache, CachedAstEntry, CompositeWeights6, --weights, layers_matched, AstResult, format_ast_json, format_ast_text, recover_line, read_line_at, resolve_git_dir, is_hex_sha, warn_skip, MIN_COCHANGE_JACCARD, LOCK_POLL_MS, LOCK_DEADLINE_SECS, stderr prefix, skim search:, skim search [debug]:, skim_bin_path, query_substring_present, run_compound_query, filter_set, disjoint blast radius, accumulate_posting_tfs, collect_scored_results, temporal_annotation_tag, shrink_to_fit, postings_buf, WorkingTreeDelta, WorkingTreeChanged, scan_working_tree, freshness_entries, temporal_db_is_stale, try_rebuild_temporal_nonfatal, ValidityMarker, validity, weights_inert_notice, wilson_lower_bound, risk_score_wilson_decay, AD-378, AD-379, AD-376, AD-377."
category: architecture
directories: [crates/rskim/src/cmd/search/]
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
  - crates/rskim/src/cmd/search/temporal_build.rs
  - crates/rskim/src/cmd/search/build_lock.rs
created: 2026-06-21
updated: 2026-07-01
version: 5
---

# Search CLI (skim search subcommand)

## Overview

`crates/rskim/src/cmd/search/` is the **CLI orchestration layer** for `skim
search`. All I/O lives here. Business logic is in `rskim-search` (the library
crate). This module owns:

- Flag parsing and dispatch (`mod.rs`)
- Index build pipeline — streaming producer/consumer with crossbeam channel,
  lexical + AST index building, manifest caching (`index.rs`)
- Temporal index build — git history parsing, hotspot/risk scoring, co-change
  matrix, SQLite persistence (`temporal_build.rs`)
- Query execution — lexical BM25F, AST filter, composite RRF ranking (`query.rs`)
- Staleness detection and auto-refresh (`staleness.rs`)
- Blast-radius, temporal sort, AST result enrichment (`temporal.rs`)
- AST structural query interface (`ast.rs`)
- Manifest sidecar — file content hashes for incremental build (`manifest.rs`)
- File walk and project root discovery (`walk.rs`)
- Snippet extraction (`snippet.rs`)
- Git hook installation (`hooks.rs`)
- Build mutex (`build_lock.rs`)

## Module Structure (complete)

```
cmd/search/
  mod.rs                — public entry: run(); parse_flags; SearchAction; Flags;
                          run_query, run_temporal_standalone; help text; inline tests;
                          skim_bin_path() test helper (single source for CARGO_BIN_EXE_skim
                          fallback logic, used by all subprocess-spawning tests)
  index.rs              — build_index; IndexPipeline; streaming producer+consumer;
                          derive_ast_entry; resolve_search_cache_dir; Pipeline struct
  query.rs              — execute_query_with_manifest; format_json_output; format_text_output;
                          temporal_annotation_tag (array-of-options+flatten idiom, no mut Vec);
                          weights_inert_notice; candidate_pool
  staleness.rs          — StalenessCheck; check_staleness; auto_refresh_if_stale;
                          read_git_head; resolve_git_dir; is_hex_sha;
                          WorkingTreeDelta; scan_working_tree; temporal_db_is_stale;
                          try_rebuild_temporal_nonfatal; create_real_git_repo (#[cfg(test)])
  types.rs              — SearchAction, Flags, QueryConfig, IndexConfig, IndexResult,
                          ResolvedResult, QueryOutput, WalkEntry, ProcessedFile, SkipReason,
                          TemporalSort, TemporalAnnotation, SnippetLine, SnippetContext
  ast.rs                — open_ast_engine; validate_ast_pattern; resolve_ast_scored;
                          run_ast_standalone; read_line_at; pattern_description;
                          re-exports: AstResult, format_ast_json, format_ast_text (from rskim_search)
  temporal.rs           — open_temporal_db; resort_window; resolve_blast_radius_paths;
                          resolve_blast_radius_file_ids; paths_to_file_ids;
                          cochange_partner_paths; query_standalone; apply_temporal_enrichment;
                          enrich_ast_results; format_temporal_text; format_temporal_json;
                          normalize_blast_radius_path; check_temporal_staleness (#[cfg(test)] only)
  temporal_build.rs     — rebuild_temporal; build_hotspot_rows; build_risk_rows;
                          build_cochange_rows; current_epoch_secs; union_paths; warn_skip! macro
  manifest.rs           — FileManifest: binary (v4, SKFM) sidecar caching sha256+field_map+mtime+size
  walk.rs               — walk_metadata; discover_project_root; walk_and_read (test-only);
                          normalize_rel_path (now pub(super) for scan_working_tree)
  snippet.rs            — extract_snippet context window from file content;
                          query_substring_present (thin delegate → rskim_search::query_substring_present)
  hooks.rs              — install_search_hooks; remove_search_hooks (git post-commit/merge)
  build_lock.rs         — acquire; acquire_bounded (inner testable impl); LOCK_POLL_MS=200;
                          LOCK_DEADLINE_SECS=120; advisory .skim-build.lock mutex
  *_tests.rs            — co-located test files included via #[path] for each module
```

## Stderr Prefix Convention

All stderr output from this module follows exactly two forms:

- `"skim search:"` — always-on diagnostic (errors, warnings, non-debug info)
- `"skim search [debug]:"` — debug-gated only (emitted when `SKIM_DEBUG=1`)

**No sub-qualifiers** like `"skim search index:"` or `"skim search temporal:"`.
Message bodies are self-describing. This was unified in the Wave-4 hardening pass
(commit `f526729`). The `warn_skip!` macro in `temporal_build.rs` emits
`"skim search [debug]:"` form. `ast.rs` uses `"skim search: AST result warning:"` for
always-on AST file-ID out-of-range warnings. All four forms are consistent with this rule.

## CLI Flag Surface

Accepted flags for `skim search`:
```
--build            Build lexical+AST+temporal index incrementally
--rebuild          Force full rebuild from scratch
--update           Auto-refresh if stale (git HEAD changed)
--stats [--json]   Show index statistics (now shows true on-disk sizes, #380)
--install-hooks    Install git post-commit/merge hooks for auto-refresh
--remove-hooks     Remove skim git hooks
--json / -j        Output results as JSON
--limit N / -n N   Max results (default: 20; equals form --limit=N supported)
--root PATH        Override project root
--ast PATTERN      AST structural filter (named pattern or containment query)
--hot              Sort by hotspot score DESC
--cold             Sort by hotspot score ASC
--risky            Sort by Wilson+decay risk score DESC (#378)
--blast-radius FILE Restrict to co-change partners of FILE
--weights l,a,t    Composite RRF weights (N-signal UNION path, #200/#377)
```

Mutual exclusions: `--hot`, `--cold`, `--risky` are mutually exclusive (enforced
in `parse_temporal_flag`).

`--ast` composes freely with every temporal flag and with text queries (Wave 4/#202 complete).

**`--weights` and `weights_inert_notice` (#377)**: when `--weights` is supplied,
`query.rs::weights_inert_notice` checks whether the supplied signal weights are
inert on the active path and, if so, emits a stderr notice. For example, supplying
a non-zero temporal weight on a text+`--ast` path (where temporal ranking is not
active) triggers: `"skim search: --weights temporal component is inert on --ast
compound path (temporal not ranked here)"`. The notice function:

```rust
pub(super) fn weights_inert_notice(
    weights: Option<CompositeWeights6>,
    has_text: bool,
    has_ast: bool,
    has_blast: bool,
) -> Option<&'static str>
```

Returns `None` when no inert notice applies. Called before dispatch in `mod.rs`;
callers emit the notice to stderr if `Some`.

**`--weights` ast component**: the `ast` weight (0.3 default) is **inert on the
pure `--blast-radius` path**. That path fuses lexical + co-change (temporal)
signals only — no AST layer is built there. The weight is reserved for the full
text+AST+temporal compound dispatch (tracked in #339). It is NOT zero in the
profile; it simply has no layer to apply to on this path.

## Dispatch Flow (mod.rs)

```
run(args) → parse_flags(args) → validate --ast (validate_ast_pattern)
                              → weights_inert_notice (emit to stderr if Some)
         ↓
SearchAction::Build     → run_build  (force=false)
SearchAction::Rebuild   → run_build  (force=true)
SearchAction::Update    → run_update
SearchAction::Stats     → run_stats
SearchAction::InstallHooks → run_install_hooks
SearchAction::RemoveHooks  → run_remove_hooks
SearchAction::Query(text) with text non-empty → run_query
SearchAction::Query(_) with --ast but no text → run_ast_standalone arm
  (ordered BEFORE temporal-only arm; --ast --hot lands here, not run_temporal_standalone)
SearchAction::Query(_) with temporal flags only → run_temporal_standalone
SearchAction::Query(_) empty otherwise → print_help
```

**Validation ordering is load-bearing**: `--ast` patterns are validated before
dispatch regardless of flag combination. Single-node queries (`#283` error) and
unknown pattern names return errors before any I/O.

**--ast ordering**: the `if let Some(ref raw) = flags.ast` arm in the `match` is
placed BEFORE the `flags.temporal_sort.is_some() || flags.blast_radius.is_some()`
arm so that `--ast --hot` is handled by `run_ast_standalone` (which honours the
AST filter), never silently by `run_temporal_standalone` (R1/GAP-6 invariant).

## Index Build Pipeline (index.rs)

`build_index(config: &IndexConfig) -> anyhow::Result<IndexResult>`

The public entry acquires `build_lock::acquire("skim search index", &pipeline.cache_dir)`
BEFORE calling `pipeline.run()`. The lock is released when the returned `IndexResult`
(or `Err`) drops.

Pipeline stages:
1. `walk_metadata` — enumerate supported files under `root`; skip ignored paths
2. `load_manifest` — load binary (v4) sidecar for incremental caching
3. Load `AstNgramCache` from `ast_index.skcache` (or `with_dir` on `--force`)
4. Spawn producer thread: for each `WalkEntry`, call `read_and_classify` →
   emit `ProcessedFile` into crossbeam channel (capacity: 64)
5. Consumer loop (main thread):
   - `resolve_ast_entry` — cache hit or `derive_ast_entry` (calls `linearize_source`
     + `extract_ast_ngrams_with_metrics`)
   - `NgramIndexBuilder::add_file_classified` (lexical index)
   - `AstIndexBuilder::add_file_ngrams` (AST index)
   - `FileManifest::insert` (sidecar update including mtime AND size, AD-379-2)
6. After channel drain: write lexical index (`.skidx`), AST index, manifest, AST cache

**File cap determinism fix (#379 / c1b8830)**: the walk now processes files in a
deterministic order even when the file cap (`DEFAULT_MAX_FILES = 50,000`) is reached.
Walk entries are sorted before being sent to the producer so the set of indexed files
is stable across runs on the same tree.

**FileId alignment**: both builders receive files in the exact same order from the
consumer loop, ensuring `FileId` values are consistent between the lexical and AST
indexes. This is the CLI layer's responsibility — neither builder enforces alignment
independently.

**`resolve_ast_entry`**: checks the `AstNgramCache` by content SHA. Cache hit →
returns cached `CachedAstEntry` (skips extraction). Miss → calls `derive_ast_entry`
and inserts the new entry into the cache.

`IndexResult` tracks: `file_count`, `skipped`, `cache_hits` (lexical), `ast_cache_hits`,
`ast_reextracted`, `duration`.

**`resolve_search_cache_dir`**: default path is
`~/.cache/skim/search/<sha256_of_canonical_root>/`. The hash makes different project
roots use separate cache dirs without conflicts. Now surfaces the resolved path in
`--stats` output (#381).

**`Pipeline` struct**: `Pipeline<'cfg>` holds `config`, `cache_dir`, and `start` (timer).
`Pipeline::new(config)` resolves the cache dir and creates it. `Pipeline::run(self)` runs
all three stages. `Pipeline::flush_empty(skip_count)` handles the early return when
`walk_entries.is_empty()` (writes empty index, returns `IndexResult` with zeros).

## Build Lock (build_lock.rs)

Two-function design: `acquire` (production, uses hardcoded consts) delegates to
`acquire_bounded` (testable inner impl with injectable `poll` and `deadline_after`).

Constants:
- `LOCK_POLL_MS = 200` — sleep between `try_lock` attempts
- `LOCK_DEADLINE_SECS = 120` — maximum wait time before `Err`

`acquire` returns `std::fs::File` holding the exclusive advisory lock via
`lock_file.try_lock()`. The lock is released when the file is dropped.

The same lock file (`.skim-build.lock` in `cache_dir`) is used by both
`build_index` (lexical/AST build) and `rebuild_temporal` (temporal build), so
concurrent skim processes serialise against ALL write operations.

## Staleness and Auto-Refresh (staleness.rs)

`check_staleness(cache_dir, root) -> (StalenessCheck, Option<FileManifest>)`

Checks whether the index needs refreshing. Returns:
- `StalenessCheck::Current` — HEAD matches (not `Fresh`)
- `StalenessCheck::HeadChanged { stored, current }` — HEAD changed
- `StalenessCheck::NoStoredHead` — manifest has no HEAD, or AST index absent/old
- `StalenessCheck::NoIndex` — cold start, no `index.skidx`
- `StalenessCheck::WorkingTreeChanged { changed, added, removed }` — HEAD unchanged
  but working-tree metadata scan found edits/additions/deletions (#379)

**Working-tree staleness scan (#379, AD-379-1/AD-379-2)**:

`WorkingTreeChanged` is a new `StalenessCheck` variant. When the HEAD compare
would yield `Current`, `check_staleness` runs `scan_working_tree(root, manifest,
max_files)` to detect uncommitted edits, additions, and deletions.

`scan_working_tree` uses `walk_metadata` (the same ignore-config walk the rebuild
uses, so the scanned file set is exactly what a rebuild would index). For each walked
file the normalized rel-path is the manifest key (`walk::normalize_rel_path`); the
comparison checks:
- **added** — path not present in manifest
- **changed** — path present but mtime OR size differs

Manifest paths not seen during the walk are counted as **removed**.

This is a metadata-only scan (zero file content reads, zero SHA). A pre-#379
manifest entry with `mtime: None` or `size: None` is treated as changed so the
field is repopulated on the rebuild (AC10).

`WorkingTreeDelta`:
```rust
pub(super) struct WorkingTreeDelta {
    pub changed: usize,   // mtime or size differs
    pub added: usize,     // on disk but not in manifest
    pub removed: usize,   // in manifest but not on disk
}
impl WorkingTreeDelta {
    pub fn is_dirty(self) -> bool { ... }
}
```

AD-379-9: only aggregate counts are retained in `WorkingTreeDelta`, never a
per-file path-set diff. Detailed per-path logging is a separate `--verbose`
follow-up ticket.

**Temporal staleness (AD-TMP-2/AD-TMP-3)**:

`temporal_db_is_stale(cache_dir, current_head) -> bool` checks whether
`temporal.db` is missing or its stored `META_GIT_HEAD` does not match
`current_head`. Uses a minimal read-only SQLite open (no WAL pragma, no
migrations) to read just the one `meta` row.

**AD-TMP-2**: temporal.db staleness is INDEPENDENT of lexical staleness (#357
BUG B). The old code's `Current` early-return in `auto_refresh_if_stale` skipped
the temporal hook, leaving temporal.db stale forever when the lexical index was
current. `temporal_db_is_stale` fixes this.

**AD-TMP-3**: production temporal staleness uses file-I/O HEAD comparison here,
NOT `check_temporal_staleness` from `temporal.rs` — that helper is
`#[cfg(test)]`-only.

**Non-fatal temporal rebuild (`try_rebuild_temporal_nonfatal`)**:

The single implementation of the D5 non-fatal-swallow contract that was
previously duplicated in three structurally-divergent copies. Takes `root`,
`cache_dir`, `head: Option<&str>`, and `debug_label`. Swallows `Err` from
`rebuild_temporal` (per ADR-006/D5), emitting a debug-gated warning only.
`None` head skips the rebuild (non-git dir).

**Shared test helper (`create_real_git_repo`)**:

A `#[cfg(test)] pub(super)` helper that creates a real git repository with
commits. Canonical shared helper used by `staleness_tests.rs`,
`temporal_build_tests.rs`, and `mod.rs` test modules — eliminates three
near-verbatim copies that would otherwise drift. Returns the full 40-hex SHA
of HEAD.

**AST self-heal**: when the lexical index exists but `ast_index.skidx` is absent
or has a format version below `rskim_search::AST_INDEX_FORMAT_VERSION`, returns
`NoStoredHead` (triggering a full rebuild). The manifest is still returned so
display consumers (e.g. `--stats`) show the real HEAD.

**`resolve_git_dir(project_root) -> Option<PathBuf>`**: resolves `.git` directory.
- If `.git` is a **directory**: returns it directly.
- If `.git` is a **file** (linked worktree): parses `gitdir: <path>` pointer, returns resolved path.
- Returns `None` when `.git` doesn't exist (bare repo, non-git dir). AD-413-11: bare repos and reftable repos are out of scope.
- **Never walks up**: this function resolves ONE directory; ancestor discovery is in `git_head_state` and `walk::discover_project_root`.

**`git_head_state(project_root) -> HeadState`** (#413): three-state HEAD classifier.
- `resolved` — HEAD resolves to a commit SHA; includes linked worktrees (via `commondir` ladder: probe 1 = worktree-local loose ref, probe 2 = commondir loose ref, probe 3 = commondir `packed-refs`, probe 4 = commondir `packed-refs` via symref) and subdirectory roots (adopted via nearest enclosing repo, AD-413-14).
- `unresolved` — git dir found but HEAD could not be resolved (unborn branch, unsupported ref backend, corrupt HEAD).
- `not_a_repo` — no `.git` entry at `project_root` or within `MAX_ANCESTORS`.
Exposed in `--stats --json` as `git_head_state` key (AD-413-13). Per-worktree ref namespaces (`refs/bisect/`, `refs/worktree/`, `refs/rewritten/`) are never redirected to the commondir.

**`read_git_head(project_root) -> Option<String>`**: reads current git HEAD SHA.
Resolution order: `resolve_git_dir` OR `resolve_repo_toplevel` → read `HEAD` → if symbolic ref, 4-probe ladder (AD-413-4/5, including `commondir`); path traversal guard (AD-413-6, `is_repo_relative_safe`); 40/64-hex raw SHA (detached HEAD). Returns `None` for unborn branch, reftable, corrupt HEAD.

**`is_hex_sha(s) -> bool`**: accepts both 40-char (SHA-1) and 64-char (SHA-256)
hex strings (for `extensions.objectFormat = sha256` repos).

`auto_refresh_if_stale(root, cache_dir, analytics) -> anyhow::Result<(bool, FileManifest)>`

The primary entry point used by all query paths. Returns `(refreshed, manifest)`.

**HEAD threading (O-A / #289)**: `read_git_head(root)` is called ONCE at function
entry; the result is threaded into `rebuild_temporal` and `temporal_db_is_stale`.

**Self-heal ordering (applies ADR-006)**: in combined text+`--ast` queries and
standalone `--ast` queries, `auto_refresh_if_stale` is called BEFORE opening the
AST engine. A stale or absent AST index is rebuilt before the first query attempt.
The manifest returned from `auto_refresh_if_stale` is threaded into
`execute_query_with_manifest` so the combined path calls it exactly once.

**`WorkingTreeChanged` rebuild**: the `auto_refresh_if_stale` logic rebuilds on
`WorkingTreeChanged` (same path as `HeadChanged`), EXCEPT when the rebuild is
triggered by the working-tree scan alone and `--update` was specified — in that
case the rebuild proceeds. Callers should not distinguish `WorkingTreeChanged` from
`HeadChanged` for rebuild purposes.

**Decision O-B**: `check_temporal_staleness` is `#[cfg(test)]` only — it is NOT
called from any production query path.

## Temporal Index Build (temporal_build.rs)

`rebuild_temporal(root, cache_dir, head, now_epoch) -> anyhow::Result<()>`

Called from `try_rebuild_temporal_nonfatal` when the lexical index was refreshed:
1. Single full-history walk: `GixSource::parse_history(root, 0)` → `HistoryResult`
2. `compute_file_risk_scores` + `compute_file_temporal_stats` → per-file scores
3. `build_cochange_rows(history)` → `Vec<CochangeRow>` (inline Jaccard, no external builder)
4. `build_hotspot_rows` + `build_risk_rows` → `Vec<HotspotRow>` / `Vec<RiskRow>`
5. `build_lock::acquire("skim search", cache_dir)` — acquire lock AFTER pure compute
6. `TemporalDb::open(db_path)` + `db.sync(hotspots, risks, cochanges, head)`

**`build_risk_rows` uses `risk_score_wilson_decay` (#378)**: computes
`RiskRow.risk_score = risk_score_wilson_decay(scores[path].fix_density,
stats[path].fix_commits, stats[path].total_commits)`. The Wilson lower bound
suppresses small-sample files that previously saturated at 1.0 with the bare
ratio.

**`warn_skip!` macro**: all recoverable early-return arms use this macro to degrade
gracefully (non-git dir, gix error, `CapacityExceeded`, sync failure). It emits a
`"skim search [debug]:"` prefixed warning (debug-gated). This enforces D5 isolation
("temporal failure MUST NOT fail lexical") from a single, auditable location.

**`build_cochange_rows`**: pure function (no I/O). Accumulates per-file commit counts
and canonical `(file_a < file_b)` pair counts directly from `HistoryResult`, computes
Jaccard, and filters by `MIN_COCHANGE_JACCARD` (imported from `rskim_search`, not
redeclared). Uses `union_paths` helper for the row-join pattern shared with hotspot/risk.

**`MIN_COCHANGE_JACCARD`** and **`COUPLING_MAX_FILES`**: imported from `rskim_search`,
not redeclared in this file (Decision O-D — single source of truth in rskim-search).

**`current_epoch_secs()`**: returns current Unix timestamp in seconds; returns `0`
on pre-epoch clocks (safe fallback, `#[must_use]`).

## Temporal CLI Layer (temporal.rs)

Functions called at query time from `mod.rs`:

```rust
pub(super) fn open_temporal_db(db_path: &Path) -> Option<TemporalDb>
    // Returns None if file absent; never errors — graceful degradation

pub(super) fn resort_window(limit: usize) -> usize
    // Returns max(limit * 5, 100) for over-fetching before temporal re-sort (GAP-1)

pub(super) fn normalize_blast_radius_path(raw: &str, root: &Path) -> anyhow::Result<String>
    // Normalizes user-provided path: absolute as-is; relative tries root-first then CWD.
    // Validates existence before canonicalize to get actionable "not found" errors.
    // Strips root prefix; replaces \\ with / for Windows cross-platform consistency.

pub(super) fn resolve_blast_radius_paths(
    blast_radius: Option<&str>, root, db_path, json
) -> anyhow::Result<Option<HashSet<String>>>
    // Normalizes path, opens db, queries cochanges_for_file, returns partner paths
    // Returns Ok(None) when temporal.db is absent (graceful degradation)

pub(super) fn resolve_blast_radius_file_ids(
    blast_radius, root, db_path, sorted_paths: &[&str], json
) -> anyhow::Result<Option<HashSet<FileId>>>
    // Single resolver for the standalone --ast path's blast-radius
    // uses paths_to_file_ids(sorted_paths, partner_paths_set) for O(n log n) lookup

pub(super) fn apply_temporal_enrichment(
    results: &mut [ResolvedResult], sort: TemporalSort, db
) -> anyhow::Result<()>
    // Annotates results with hotspot/risk scores then re-sorts by temporal signal

pub(super) fn enrich_ast_results(
    results: &mut [AstResult], sort: TemporalSort, db
)
    // Same as apply_temporal_enrichment but for run_ast_standalone's AstResult slice

pub(super) fn query_standalone(
    sort, blast_radius, limit, db, root
) -> anyhow::Result<TemporalQueryOutput>
    // Dispatches to top_hotspots / top_risks / top_coldspots / cochanges_for_file

// TEST-ONLY — not part of production dispatch:
#[cfg(test)]
pub(super) fn check_temporal_staleness(db: &TemporalDb, project_root: &Path) -> Option<String>
    // Used in temporal_build_tests.rs AC6 to verify stored HEAD matches current HEAD
    // #[cfg(test)] on both this function and its read_git_head helper (spawns git process)
```

**GAP-1**: when a temporal sort is active, `resort_window(limit)` is used as the
query limit (`max(limit*5, 100)`) so that temporally-hot files ranked beyond
`--limit` in raw lexical/composite order can be promoted. After re-sort, results are
truncated to `--limit`.

**Blast-radius resolver (`resolve_blast_radius_file_ids`)**: the single resolver for
the `--ast --blast-radius` path. It resolves co-change partners to `HashSet<FileId>`
(using `sorted_paths` from the manifest) so `run_ast_standalone` can intersect AST
results with the blast-radius set before truncation — avoiding PF-006 (silent
feature-drop from post-truncation filter).

## AST CLI Layer (ast.rs)

```rust
pub(super) fn open_ast_engine(cache_dir) -> anyhow::Result<AstQueryEngine<AstIndexReader>>
    // Opens the AST index; fails loud if absent (fail-loud counterpart to temporal's graceful degrade)

pub(super) fn validate_ast_pattern(raw: &str) -> anyhow::Result<AstQuery>
    // Returns the parsed AstQuery so callers can reuse it (avoids second parse)
    // Called in mod.rs before dispatch; also used inside run_ast_standalone
    // SingleNode → error (#283); unknown pattern → lists available names

pub(super) fn resolve_ast_scored(
    engine: &AstQueryEngine<AstIndexReader>, raw: &str
) -> anyhow::Result<Vec<(FileId, f64)>>
    // Calls parse_ast_query → search_ast; returns FileId-ASC sorted scored vec
    // Used by run_query text+--ast path

pub(super) fn run_ast_standalone(
    raw_pattern, limit, json, cache_dir, manifest,
    blast_file_ids: Option<HashSet<FileId>>,  // pre-resolved by mod.rs
    temporal_sort, temporal_db, root, w
) -> anyhow::Result<ExitCode>
    // Pure AST (no text query) standalone dispatch
    // Truncation order:
    //   1. blast-radius filter (FileId intersection) — BEFORE truncation
    //   2. take bounded window (limit without sort; limit*5>=100 with sort)
    //   3. temporal enrichment + re-sort (when both sort and db are Some)
    //   4. truncate to limit — AFTER re-sort (AC-F4)
    //   5. recover_line re-parse for each surviving file (line-span, AC-API3)
```

**Re-exports from `rskim_search`** (pub(super) in ast.rs):
- `AstResult` — enriched row type for AST-only results
- `format_ast_json` — JSON formatter for AST results
- `format_ast_text` — text formatter for AST results
- `recover_line` — line-span re-parse (bounded, fail-soft)

**`run_ast_standalone` accepts `blast_file_ids: Option<HashSet<FileId>>`** (not raw
path strings) — the pre-resolution happens in `mod.rs` via
`temporal::resolve_blast_radius_file_ids`. The function is DB-free by design.

**`read_line_at(abs_path, line_1indexed, max_bytes) -> Option<String>`**: reads a
specific 1-indexed line from a file. Returns `None` on I/O error, non-UTF8,
file exceeds `max_bytes`, or out-of-range line. Uses `rskim_search::MAX_REPARSE_FILE_BYTES`
as the size guard in production calls.

The CLI calls `search_ast` directly on `AstQueryEngine` (not through
`SearchLayer::search`) to avoid `SearchResult` construction and `SearchLayer` overhead.

## Query Execution (query.rs)

`execute_query_with_manifest(config, pre_loaded_manifest, analytics) -> anyhow::Result<QueryOutput>`

Accepts an optional pre-loaded manifest so the combined text+`--ast` path
(which already called `auto_refresh_if_stale`) can skip a redundant refresh.
When `pre_loaded_manifest` is `None`, the function calls `auto_refresh_if_stale`
itself exactly once.

**`execute_query`**: test-facing wrapper that calls `execute_query_with_manifest(config, None, analytics)`.
Marked `#[cfg_attr(not(test), allow(dead_code))]`.

The `ast_scored` field in `QueryConfig` carries `Vec<(FileId, f64)>` for RRF
fusion. When `blast_radius_paths` and `ast_scored` are both set, the ranking is
a 6-signal composite weighted-RRF (`CompositeWeights6`). When only text is set,
it's pure BM25F lexical.

**`candidate_pool(limit, k) -> usize`**: computes the over-fetch window for queries
with offset (k = limit + offset). Returns `max(limit * 5, 100)` when a temporal
sort is active; returns `limit + offset` otherwise. Centralises the window-sizing
logic previously scattered across dispatch arms.

**`weights_inert_notice` (#377)**: called before dispatch; returns a static string
message when `--weights` supplies a non-zero weight for a signal that has no
active layer on the current query path (e.g. temporal weight on the `--ast`
compound path). Callers emit the message to stderr.

**`run_compound_query` disjoint-set early-out**: when blast-radius and AST sets
are both non-empty but share no files, `filter_set.is_empty()` triggers an
explicit early return of an empty `QueryOutput` (total=0, results=[]). This
replaces the previous approach of relying on the reader's `file_filter = Some(empty)`
side-effect semantics. `sq.limit = Some(filter_set.len())` (no `.max(1)`) is safe
because the early-out guarantees `filter_set.len() >= 1` at that point (ADR-003,
#356).

`ResolvedResult.layers_matched` tracks which signals contributed to a result:
`["lexical"]`, `["lexical","ast"]`, etc. Absent on pure-lexical rows.

**`score` field semantics**:
- Single-token exact-symbol path (≥3 bytes, no whitespace, `is_single_token=true`): raw
  occurrence count (length-norm-free, AD-372-6, #372). Large-file definers rank by how
  many times the token appears, NOT by BM25F field-length normalization.
- Multi-word / UNION lexical path (multi-token query): BM25F magnitude.
- Composite UNION blast-radius path (#200): fused weighted-RRF score (small positive,
  NOT BM25F magnitude). `field = "co_change_partner"` indicates temporal-only results.

**Formatter content contract (Wave-4 F2 hardening)**:
- Text output renders `path:line`, a 3-decimal score, and the snippet for matched rows.
- JSON carries `mode`/`total`/`path`/`score` and **omits** `line`/`snippet` on
  degraded (no-line) rows (additive-key contract — consumers must not assume these keys).

## Manifest Sidecar (manifest.rs)

`FileManifest`: a binary file (`index.skfiles`, format v4 with a `SKFM` magic
header) storing content SHA-256, pre-classified `field_map`, mtime, AND size
per indexed file. Used for **incremental builds** — if a file's SHA matches the
cached entry, `field_map` is reused without re-classifying. (#380 binarized the
former JSONL sidecar to shrink the on-disk footprint; v2/v3 JSONL manifests
cold-start.)

**New in #379**: `ManifestEntry` now stores `mtime: Option<u64>` AND `size: Option<u64>`
(AD-379-2). Both fields are used by `scan_working_tree` for the working-tree
staleness check. A `mtime: None` or `size: None` from a pre-#379 manifest entry is
treated as changed so the fields are repopulated on the next rebuild (AC10).

`stored_git_head()`: returns the git HEAD SHA stored in the manifest, used by
staleness checking. This is the single source of truth for "what HEAD was the
index built at?".

`FileManifest::sorted_paths()`: returns a sorted `Vec<&str>` of all manifest
paths, used by `paths_to_file_ids` for O(n log n) FileId resolution.

`FileManifest::lookup(&str) -> Option<&ManifestEntry>`: used by `run_ast_standalone`
to retrieve stored mtime for the stale guard in `recover_line`.

`FileManifest::freshness_entries()`: new in #379. Returns an iterator of
`(path: &str, mtime: Option<u64>, size: Option<u64>)` tuples for every indexed
file. Used by `scan_working_tree` to compare on-disk metadata against the manifest.

**`--stats` on-disk size (#380)**: `run_stats` now reads the actual on-disk sizes
of `index.skidx`, `index.skpost`, `ast_index.skidx`, `ast_index.skpost`,
`temporal.db`, etc. instead of deriving from entry counts. The resolved `cache_dir`
path is also surfaced in the stats output (#381).

**Binary v4 format (`SKFM` magic, #380)**: the manifest is now a compact binary
format. v2/v3 JSONL manifests are detected by the lack of a valid `SKFM` header
and cause a cold-start (manifest ignored, all files re-classified). Format has a
12-byte fixed header (`SKFM` magic + format version + declared entry count) followed
by length-prefixed binary entries. Security bounds:
- `MAX_MANIFEST_ENTRIES = 60_000`
- `MAX_MANIFEST_FILE_SIZE` reject oversized files up front
- `MAX_FIELD_BYTES = 64 KiB` per field_map
- `MAX_FIELD_MAP_TRIPLES = 1_000_000`
All length reads use `AD-380-3`: `saturating try_from`, never `as u32`.

## Validity Marker (rskim-search/src/validity.rs, #376)

A new crate-private module (`pub(crate) mod validity`) in `rskim-search` provides
`ValidityMarker` — a compact sidecar that caches a prior successful CRC32
verification, skipping the per-query full-payload CRC32 on the hot path.

Every `NgramIndexReader::open` / `AstIndexReader::open` re-hashes the entire
posting blob before any query. On large corpora this dominated query latency
(median 57 ms / p90 77 ms scaling with `.skpost` size). The marker moves that
one-shot integrity check off the hot path.

**Trust boundary (AD-376-2)**: The signature is `(idx_len, idx_mtime_ns,
post_len, post_mtime_ns)` PLUS the header's already-stored `checksum` field.
mtime + len detect any rewrite of either file; carrying the checksum means a
marker minted for one index can never validate a file whose header advertises
a different checksum.

**Not a corruption detector**: the marker caches a prior successful CRC32 —
it does NOT replace it. The full CRC32 remains the desync/mis-rank integrity
guard. KNOWN LIMIT (AC1 of #376, PF-007): a content byte-flip that simultaneously
preserves `len` AND `mtime` AND the header `checksum` field is served unverified
(silent mis-rank). This is accepted and pinned in tests.

**On-disk format** (52-byte fixed LE binary):
```
[0..8]   idx_len        u64 LE
[8..24]  idx_mtime_ns   i128 LE  (nanoseconds from UNIX_EPOCH; signed for pre-epoch)
[24..32] post_len       u64 LE
[32..48] post_mtime_ns  i128 LE
[48..52] checksum       u32 LE   (header.checksum field, NOT recomputed)
```

**Robustness (AC6)**: every read is best-effort — truncated, garbage, zero-length,
or unreadable marker yields `None` and the caller falls through to the full CRC32.
A failed marker write is swallowed; the next open re-verifies.

The marker files are `index.skverify` (lexical) and `ast_index.skverify` (AST)
in the cache directory. They are NOT listed in the `Cache Directory Layout` below
but should be expected there.

## Key Types (types.rs)

```rust
struct Flags {
    action: SearchAction,
    json: bool,
    limit: usize,               // default 20
    root_override: Option<PathBuf>,
    temporal_sort: Option<TemporalSort>,  // Hot | Cold | Risky
    blast_radius: Option<String>,
    ast: Option<String>,
    weights: Option<CompositeWeights6>,
}

struct QueryConfig {
    text, limit, offset: Option<usize>, json, root, cache_dir,
    blast_radius_paths: Option<HashSet<String>>,
    ast_scored: Option<Vec<(FileId, f64)>>,   // FileId-ASC sorted
    composite_weights: Option<CompositeWeights6>,
}

struct ResolvedResult {
    path, score, field, line_number,
    line_range: Option<Range<usize>>,
    snippet: Option<SnippetContext>,
    stale: bool,
    match_positions: Vec<Range<usize>>,
    temporal: Option<TemporalAnnotation>,
    layers_matched: Vec<&'static str>,  // skip_serializing_if: empty
}

struct IndexResult {
    file_count, skipped, cache_hits,
    ast_cache_hits, ast_reextracted,
    duration,
}

struct IndexConfig {
    root, max_files: Option<usize>,
    force: bool,
    cache_dir_override: Option<PathBuf>,
    // DEFAULT_MAX_FILES = 50_000 (associated const on IndexConfig)
}

// NEW in #379 (WalkEntry gains size field):
struct WalkEntry {
    abs_path, rel_path, lang,
    mtime: Option<u64>,
    size: Option<u64>,  // NEW: AD-379-2 working-tree staleness
}

// NEW in #379 (ProcessedFile gains size field):
struct ProcessedFile {
    rel_path, lang, content, sha256,
    mtime: Option<u64>,
    size: Option<u64>,  // NEW: AD-379-2
    field_map, cache_hit, ast_cached,
}
```

`TemporalAnnotation` in `types.rs` is `pub(super)` — has fields: `hotspot_score`,
`risk_score`, `fix_density`, `cochange_jaccard`, `changes_30d`, `changes_90d`.

`QueryConfig.offset: Option<usize>` is new (#372-AD-372-3): used on the pure-lexical
exact-symbol path and threaded through to `resolve_paths_and_snippets_verified`.

## Cache Directory Layout

Default: `~/.cache/skim/search/<sha256_of_canonical_root>/`

Files:
- `index.skidx` — lexical n-gram index (NgramIndexBuilder output)
- `index.skpost` — lexical posting lists
- `index.skverify` — lexical validity marker (ValidityMarker sidecar, #376)
- `index.skfiles` — incremental build manifest sidecar (binary v4, SKFM magic): SHA + field_map + mtime + size per file
- `ast_index.skidx` — AST n-gram index header + metadata
- `ast_index.skpost` — AST posting lists
- `ast_index.skverify` — AST validity marker (ValidityMarker sidecar, #376)
- `ast_index.skcache` — AST n-gram extraction cache by content SHA
- `temporal.db` — SQLite temporal data (hotspots, risks, co-changes, meta)
- `.skim-build.lock` — build mutex file (`build_lock.rs`)

## Anti-Patterns

- **Refreshing the index more than once per query path**: each dispatch arm
  (`run_query`, `run_ast_standalone`) calls `auto_refresh_if_stale` exactly once.
  The combined text+`--ast` path pre-loads the manifest and passes it to
  `execute_query_with_manifest` to avoid a second refresh call.

- **Calling `auto_refresh_if_stale` AFTER opening the AST engine**: the self-heal
  ordering in ADR-006 requires the refresh to happen BEFORE `open_ast_engine`.
  A stale AST index must be rebuilt before the query engine is opened.

- **Using `paths_to_file_ids` without a sorted `sorted_paths` input**: the function
  uses binary search and requires the input to be lexicographically sorted.
  `manifest.sorted_paths()` guarantees this; building the slice manually does not.

- **Applying the blast-radius FileId filter AFTER truncating to `--limit`**: the
  blast-radius intersection must happen before truncation (PF-006: silent
  feature-drop). `run_ast_standalone` receives pre-resolved `blast_file_ids`
  and intersects before truncation. `resolve_blast_radius_paths` in `run_query`
  feeds `blast_radius_paths` into `QueryConfig` so it is applied inside the
  search engine before LIMIT.

- **Routing through `SearchLayer::search` for AST queries from the CLI**: the CLI
  calls `search_ast` directly on `AstQueryEngine`. `SearchLayer::search` adds
  overhead and is not the primary dispatch path.

- **Treating `score` in `ResolvedResult` as always BM25F**: on the composite
  UNION blast-radius path (`--blast-radius` with `CompositeWeights6`), `score`
  is a weighted-RRF value (small positive, not a BM25F magnitude).

- **Redeclaring `MIN_COCHANGE_JACCARD` or `COUPLING_MAX_FILES` in CLI code**:
  these constants are imported from `rskim_search` (Decision O-D). `temporal_build.rs`
  explicitly comments that it does NOT redeclare them.

- **Placing the `--ast` dispatch arm AFTER the temporal-only arm**: the `--ast`
  arm must come first in the `match` so `--ast --hot` is handled by
  `run_ast_standalone` (R1/GAP-6), not silently dropped to `run_temporal_standalone`.

- **Calling `check_temporal_staleness` from production code**: it is `#[cfg(test)]`
  only. Adding a production call would break with a compile error.

- **Using non-standard stderr prefixes**: all new stderr output must use exactly
  `"skim search:"` or `"skim search [debug]:"`. No sub-qualifiers (e.g.
  `"skim search index:"`) — message bodies are self-describing.

- **Reimplementing `query_substring_present` locally**: the canonical definition
  lives in `rskim-search/src/types.rs` as `pub fn query_substring_present`. Both
  the CLI verify gate (`snippet.rs`) and the bench harness (`rskim-bench`) delegate
  to `rskim_search::query_substring_present`. Do not add a local copy.

- **Duplicating the `CARGO_BIN_EXE_skim` fallback in new tests**: use the shared
  `skim_bin_path()` helper defined in `mod.rs`'s test module.

- **Duplicating the `create_real_git_repo` test helper**: use
  `super::staleness::create_real_git_repo` from `staleness_tests.rs`,
  `temporal_build_tests.rs`, and `mod.rs` test modules. Do not add another copy.

- **Using bare `fix_density` ratio for `--risky` ranking in `build_risk_rows`**: always
  call `risk_score_wilson_decay(decay_fix_factor, fix_commits, total_commits)` to
  produce `RiskRow.risk_score`. The bare ratio saturates on tiny samples (#378).

- **Reading `temporal.db` staleness by opening `TemporalDb::open` on the hot path**:
  use `temporal_db_is_stale(cache_dir, current_head)` which opens a minimal
  read-only SQLite connection to read just the one `meta` row. The full
  `TemporalDb::open` runs WAL pragma + two metadata syscalls + migration check.

## Gotchas

- **`StalenessCheck` now has a `WorkingTreeChanged` variant** (#379): `is_dirty()`
  is NOT a method on `StalenessCheck` — it's on `WorkingTreeDelta`. The
  `WorkingTreeChanged` case carries aggregate counts `{ changed, added, removed }`,
  not a per-file diff. Matching exhaustively requires handling this new arm.

- **`StalenessCheck` variant is `Current`, not `Fresh`**: the "up to date" variant
  is `StalenessCheck::Current`. Code using `.is_stale()` or matching `Fresh` will
  fail to compile.

- **`build_lock.rs` has two public-to-module functions**: `acquire` (production path,
  uses `LOCK_POLL_MS` and `LOCK_DEADLINE_SECS` consts) and `acquire_bounded` (inner
  impl exposed for `build_lock_tests.rs` with injectable `poll`/`deadline_after`).
  Both `build_index` (index.rs) and `rebuild_temporal` (temporal_build.rs) call
  `acquire`, not `acquire_bounded`.

- **`--weights` ast signal is inert on the `--blast-radius` path**: `CompositeWeights6`
  has six signals; on the blast-radius composite path only lexical and temporal
  (co-change) signals are active. The ast weight (0.3 default) has no AST posting
  list to score against on that path.

- **`validate_ast_pattern` returns `anyhow::Result<AstQuery>`** (not `Result<()>`):
  the return value is the parsed query, enabling callers that need both validation
  and the parsed query to avoid a second `parse_ast_query` call.

- **`--ast` with temporal flags does NOT error** (Wave 4/#202 complete):
  `--ast --hot`, `--ast --blast-radius`, etc. all work. The interim guard was removed.

- **`run_ast_standalone` truncation order is strict**:
  1. blast-radius filter; 2. take window; 3. temporal enrich+sort; 4. truncate to limit;
  5. re-parse (recover_line). Re-parse runs AFTER truncation (at most `limit` files, AC-API3).

- **`temporal_annotation_tag` uses array-of-options + flatten**: the mutable
  `Vec::new()` + push pattern was replaced with `[t.hotspot_score.map(...), t.risk_score.map(...)]
  .into_iter().flatten().collect()`. When adding new annotation fields, extend the
  array — do not revert to the mutable-Vec pattern.

- **`NgramIndexReader` scoring loop is split into helper methods** (`accumulate_posting_tfs`,
  `collect_scored_results`): `accumulate_posting_tfs` handles the first sub-pass (TF
  accumulation + blast-radius early-skip); `collect_scored_results` handles
  defense-in-depth filter, sort, skip, take.

- **`postings_buf.shrink_to_fit()` is called after the encode loop in the lexical
  index builder**: initial `Vec::with_capacity` uses `VARINT_UPPER_BOUND_PER_ENTRY = 9`
  bytes/entry (~2.5× the measured ~3.5 byte v4 average). After encoding,
  `shrink_to_fit()` releases the excess.

- **`AstNgramCache` (`ast_index.skcache`)**: separate from the lexical manifest.
  A file can have a manifest cache hit (field_map reused) but an AST cache miss
  (new extraction) or vice versa.

- **`resolve_git_dir` follows worktree `.git` files** (#413): if `.git` is a file
  (linked worktree), parses `gitdir: <path>` pointer and returns the resolved worktree
  gitdir. Relative paths are resolved relative to the directory containing the `.git` file.
  `resolve_git_dir` resolves ONE directory and never walks up — ancestor discovery lives in
  `git_head_state` (via `resolve_repo_toplevel` + `walk::discover_project_root`). The
  `commondir` pointer (inside the worktree gitdir) further routes HEAD resolution to the
  shared primary `.git` via a 4-probe ladder (AD-413-4/5); per-worktree namespaces
  (`refs/bisect/`, `refs/worktree/`, `refs/rewritten/`) are NOT redirected. Bare repos and
  reftable repos return `None` (AD-413-11; out of scope).

- **The `*_tests.rs` files are co-located test modules** included via `#[path]`.
  All `cmd/search/` modules follow this pattern.

- **`skim search index`** is the legacy subcommand path removed in #375. The
  modern path is `skim search --build` / `--rebuild`. The string "index" is now
  treated as a text query.

- **`run_build` stderr format** (mod.rs:908): `"skim search: indexed N files (M skipped, K cache hits) in Z.Zs"`.
  (Not `K field-map hits` or `X AST reused, Y AST re-extracted` — those are internal counters not surfaced in this line.)

- **Validity marker files (`index.skverify`, `ast_index.skverify`)**: generated by
  `rskim-search::validity` on a successful verified open. Absent on the first open
  (or after a rebuild that calls `unlink_marker_best_effort`). A bad marker yields
  `None` and falls through to the full CRC32 — never a failure.

- **Walk determinism at file cap (#379 / c1b8830)**: `walk_metadata` now returns
  entries in a deterministic sorted order so the set of indexed files is stable
  when the cap is reached. Pre-#379 the walk order was OS-dependent.

## Key Files

- `crates/rskim/src/cmd/search/mod.rs` — dispatch hub; `parse_flags`;
  `run_query`, `run_temporal_standalone`; help text
- `crates/rskim/src/cmd/search/index.rs` — `build_index`; `Pipeline`; streaming producer/
  consumer; `derive_ast_entry`; `resolve_search_cache_dir`; `Pipeline::flush_empty`
- `crates/rskim/src/cmd/search/build_lock.rs` — `acquire`; `acquire_bounded`; advisory mutex
- `crates/rskim/src/cmd/search/temporal_build.rs` — `rebuild_temporal`;
  hotspot/risk/cochange row builders; `current_epoch_secs`; `warn_skip!` macro
- `crates/rskim/src/cmd/search/temporal.rs` — `normalize_blast_radius_path`;
  `resolve_blast_radius_paths`; `resolve_blast_radius_file_ids`;
  `apply_temporal_enrichment`; `enrich_ast_results`; `query_standalone`; formatters
- `crates/rskim/src/cmd/search/ast.rs` — `open_ast_engine`;
  `validate_ast_pattern`; `resolve_ast_scored`; `run_ast_standalone`;
  `read_line_at`; re-exports `AstResult`/`format_ast_json`/`format_ast_text`
- `crates/rskim/src/cmd/search/staleness.rs` — `auto_refresh_if_stale`;
  `check_staleness`; `read_git_head`; `resolve_git_dir`; `is_hex_sha`;
  `WorkingTreeDelta`; `scan_working_tree`; `temporal_db_is_stale`;
  `try_rebuild_temporal_nonfatal`; `create_real_git_repo` (#[cfg(test)])
- `crates/rskim/src/cmd/search/query.rs` — `execute_query_with_manifest`;
  `execute_query` (test-only); JSON/text formatters; `weights_inert_notice`;
  `candidate_pool`
- `crates/rskim/src/cmd/search/types.rs` — all shared data types (WalkEntry.size
  and ProcessedFile.size added in #379; QueryConfig.offset added)
- `crates/rskim/src/cmd/search/manifest.rs` — `FileManifest` binary (v4) sidecar;
  `lookup`; `stored_git_head`; `sorted_paths`; `freshness_entries` (#379);
  `encode_field_map`; `decode_field_map`
- `crates/rskim/src/cmd/search/walk.rs` — `walk_metadata` (deterministic order at
  cap, #379/c1b8830); `normalize_rel_path` (now pub(super))
- `crates/rskim-search/src/validity.rs` — `ValidityMarker`; `MARKER_SIZE=52`;
  `current_signature`; `read_marker`; `write_marker_best_effort`;
  `unlink_marker_best_effort`

## Related

- Feature: `ast-index` — `AstQueryEngine`, `AstIndexReader`, `parse_ast_query`,
  `search_ast`, `AstNgramCache`, `CachedAstEntry`; the AST query engine that `ast.rs` wraps.
- Feature: `temporal-scoring` — `TemporalDb`, `HotspotRow`, `RiskRow`,
  `CochangeRow`, scoring functions including `wilson_lower_bound` and
  `risk_score_wilson_decay` (#378); used by `temporal.rs` and `temporal_build.rs`.
- Feature: `cochange` — `CochangeMatrixBuilder`, `CochangeMatrixReader`,
  `COUPLING_MAX_FILES`; historic binary format path (now `build_cochange_rows` in
  `temporal_build.rs` computes rows directly without the binary matrix step).
- ADR-006: self-heal ordering — `auto_refresh_if_stale` BEFORE `open_ast_engine`.
- PF-006: blast-radius filter must be applied BEFORE truncation to `--limit`.
- PF-007: AD-376-1 — validity marker is NOT a corruption detector (known limit AC1).
- Decision O-B: `check_temporal_staleness` is `#[cfg(test)]` only — intentionally
  absent from all production query paths.
- Decision O-D: `MIN_COCHANGE_JACCARD` and `COUPLING_MAX_FILES` are single-sourced
  in rskim-search, never redeclared in CLI code.
- ADR-003: resource bounds — skcache size limit, build lock deadline, CHANNEL_CAPACITY.
- Issue #202: `--ast` composing with temporal flags — complete as of Wave 4.
- Issue #200: `--weights` / `CompositeWeights6` / N-signal UNION blast-radius path.
- Issue #201: `layers_matched` field; `AstResult` enriched type; `recover_line`
  re-parse; `format_ast_json`/`format_ast_text` in rskim-search.
- Issue #290: AST incremental build cache (`ast_index.skcache`); content-SHA-keyed.
- Issue #283: single-node AST queries not yet supported.
- Issue #339: `--weights` ast signal on the `--blast-radius` path (currently inert).
- Issue #376: validity marker caching (`index.skverify`, `ast_index.skverify`).
- Issue #377: `weights_inert_notice` for inert `--weights` signal detection.
- Issue #378: Wilson+decay volume-weighted risk scoring (`risk_score_wilson_decay`).
- Issue #379: working-tree staleness scan (`WorkingTreeChanged`, `scan_working_tree`,
  `WalkEntry.size`, `ManifestEntry.size`, `freshness_entries`, walk determinism at cap).
- Issue #380: binary v4 manifest (`SKFM`), true on-disk `--stats`, grounded size guard.
- Issue #381: correct index location surfaced in `--stats`; canonicalize-fallback.
