---
feature: temporal-scoring
name: Temporal Risk Scoring
description: "Use when adding per-file risk metrics from git history, tuning decay parameters, integrating hotspot scores into search ranking, extending the temporal module with new scoring signals, or working with the SQLite temporal persistence layer. Keywords: temporal, hotspot, fix density, exponential decay, git history, risk scores, half-life, FileRiskScores, FileTemporalStats, compute_file_risk_scores, compute_file_temporal_stats, decay_weight, TemporalDb, storage, HotspotRow, RiskRow, CochangeRow, file_filter, SearchQuery, per-file lookup, top-N, hotspot_for_file, risk_for_file, cochanges_for_file, top_hotspots, top_risks, top_coldspots."
category: domain-knowledge
directories: [crates/rskim-search/src/temporal/]
referencedFiles:
  - crates/rskim-search/src/temporal/scoring.rs
  - crates/rskim-search/src/temporal/scoring_tests.rs
  - crates/rskim-search/src/temporal/mod.rs
  - crates/rskim-search/src/temporal/git_parser.rs
  - crates/rskim-search/src/temporal/storage.rs
  - crates/rskim-search/src/temporal/storage_ops.rs
  - crates/rskim-search/src/temporal/storage_types.rs
  - crates/rskim-search/src/types.rs
  - crates/rskim-search/src/lib.rs
created: 2026-05-25
updated: 2026-06-01
---

# Temporal Risk Scoring

## Overview

The temporal scoring subsystem computes two per-file risk metrics from a repository's git commit
history: a **hotspot score** (decay-weighted commit frequency) and a **fix density** (fraction of
recent touches that were bug fixes). Both are `f64` values in `[0.0, 1.0]` and are returned as
`FileRiskScores` in a `HashMap<String, FileRiskScores>`.

A companion function, `compute_file_temporal_stats`, computes raw (non-decay-weighted) commit counts
per file within 30-day and 90-day windows, producing `FileTemporalStats` values intended for
persistence. The `temporal::storage` sub-module persists these computed values — along with
co-change pairs — to a WAL-mode SQLite database via `TemporalDb`.

The module is deliberately split into three independent layers: `git_parser.rs` handles all I/O
(gix-based history traversal), `scoring.rs` is pure (no I/O, no side effects), and `storage.rs`
owns the SQLite connection and schema lifecycle. The I/O boundary in `git_parser.rs` converts gix
types into `CommitInfo`/`FileChangeInfo` structs. Scoring code never touches gix. Storage code
never touches gix or scoring internals.

## Business Context

Risk scores feed downstream ranking signals for the search layer. A file with a high hotspot score
changed frequently and recently — strong candidate for code-smell review or stale-index detection.
A high fix density means a large fraction of recent touches were bug fixes — correlates with
latent defects. Neither metric is a hard filter; both are signals to be blended with BM25F scores
or co-change coupling data.

`FileTemporalStats` (raw counts) complements `FileRiskScores` (decay-weighted) by providing data
suitable for incremental refresh and persistence: raw counts are additive across time windows and
do not depend on a `now_epoch` at read time.

## Core Business Rules

**Hotspot is max-normalized.** After accumulating decay-weighted totals, the algorithm divides
every file's total by the global maximum. This means the most-active file always scores exactly
`1.0` regardless of absolute commit volume.

**Fix density is ratio-based, not count-based.** Fix density is `weighted_fix_total /
weighted_total` for each file independently. A file touched by 10 fix commits and 10 feature
commits scores `0.5`.

**Fix classification is commit-wide, not per-file.** `is_fix_commit` classifies the whole commit
message. If commit A changes `a.rs` and `b.rs` and is a fix commit, both files receive the fix
weight. There is no per-file keyword extraction.

**The half-life parameter is in days, not seconds.** `decay_weight(elapsed_days, half_life_days)`
uses the formula `exp(-elapsed_days / half_life_days)`. At exactly one half-life elapsed, the
weight is `1/e ≈ 0.368`, not `0.5`. This is exponential decay, not a half-life in the biological
sense.

**Future-dated commits are treated as elapsed = 0.** Commits with a timestamp ahead of `now_epoch`
contribute weight `1.0`. Negative timestamps (pre-epoch) are clamped to `0` before conversion to
avoid unsigned underflow, then produce a very large elapsed-days value, yielding near-zero weight.

**`compute_file_temporal_stats` deduplicates per commit.** A file listed twice in one commit's
`changed_files` is counted once in that commit's contribution. The deduplication uses a
`HashSet<String>` that is cleared and reused across commits to avoid repeated allocation.

**Window boundary is inclusive.** A commit at exactly `30.0` or `90.0` elapsed days is included
in the respective window (`<=` comparison).

## Fix Commit Keywords

The regex lives in `temporal/mod.rs` compiled once via `std::sync::LazyLock`:

```
(?i)\b(fix|bug|hotfix|patch|revert)\b
```

Word-boundary anchors (`\b`) prevent false positives from words like `prefix` or `buggy`. All
five keywords are case-insensitive. The `LazyLock` ensures the regex compiles exactly once across
all threads — important because `is_fix_commit` is called once per commit (pre-classified before
the per-file hot loop in `compute_file_risk_scores`; called inline in `compute_file_temporal_stats`
where per-commit overhead is identical).

## Algorithm Walk-Through

`compute_file_risk_scores(commits, now_epoch, half_life_days)` runs in three logical passes:

1. **Pre-classify** — build a `Vec<bool>` of fix flags by calling `is_fix_commit` once per
   commit. This is outside the per-file loop, avoiding repeated regex evaluation.

2. **Accumulate** — single pass over `(commit, is_fix)` pairs. For each commit, compute elapsed
   time in days and call `decay_weight`. Then for each file path in the commit, use the Entry API
   to update `(weighted_total, weighted_fix_total)`. The map is pre-allocated with
   `(commits.len() / 4).clamp(64, 50_000)` to avoid rehashing on large repos.

3. **Normalize and emit** — find `max_total` via a fold over map values. Map each entry to
   `FileRiskScores { hotspot: total / max_total, fix_density: fix_total / total }`.

`compute_file_temporal_stats(commits, now_epoch)` runs a single pass:

1. **Per commit** — call `is_fix_commit` inline, compute elapsed days, determine `in_30d`/`in_90d`
   membership, then collect unique paths via a cleared `HashSet<String>`.

2. **Accumulate** — for each unique path in the commit, increment `total_commits`, `fix_commits`,
   `changes_30d`, and `changes_90d` on the `FileTemporalStats` entry (`or_default()` initialises
   zeroed counters).

The two functions share the same capacity heuristic and timestamp clamping logic but are
independent implementations. Do not merge them; the decay path needs decay weights, the stats path
needs per-commit deduplication.

## Public API

All scoring symbols are re-exported from `crates/rskim-search/src/lib.rs`:

```rust
pub use temporal::{
    DEFAULT_HALF_LIFE_DAYS,        // f64 = 30.0
    GixSource,                      // TemporalSource impl (has I/O)
    compute_file_risk_scores,       // pure scoring fn → HashMap<String, FileRiskScores>
    compute_file_temporal_stats,    // pure stats fn → HashMap<String, FileTemporalStats>
    decay_weight,                   // pure decay fn
    is_fix_commit,                  // pure regex predicate
};
pub use temporal::storage::{
    CochangeRow, HotspotRow, RiskRow,  // row types for the four SQLite tables
    META_GIT_HEAD, META_LAST_UPDATED,  // meta table key constants
    TemporalDb,                         // connection + migrations + sync
};
pub use types::{
    FileRiskScores,       // { hotspot: f64, fix_density: f64 }
    FileTemporalStats,    // { changes_30d, changes_90d, total_commits, fix_commits: u32 }
};
```

`FileRiskScores` does NOT derive `Serialize`/`Deserialize` — it is an intermediate computation
type. `FileTemporalStats` also does NOT derive `Serialize`/`Deserialize` — it is a persistence
input, not a serialization boundary type. Consumers that need JSON output must map to local
serde-annotated types.

`DEFAULT_HALF_LIFE_DAYS = 30.0` — commits one month old contribute `1/e ≈ 37%` of the weight of
a same-day commit. Always use this constant rather than a hardcoded literal so tuning is
centralized.

Note: `lib.rs` also exposes `pub mod ast_index` (with `LinearNode`, `LinearizeResult`,
`linearize_source`) as an unrelated structural indexing module. It does not interact with the
temporal scoring pipeline but shares the same `SearchError` type.

## SQLite Persistence Layer

`TemporalDb` owns one SQLite connection and four tables: `hotspot`, `risk`, `cochange`, and
`meta`. It lives in the `temporal::storage` sub-module, with its data-manipulation `impl` block
split into `storage_ops.rs` and row types in `storage_types.rs`.

**Connection lifecycle:** `TemporalDb::open(db_path)` opens or creates the file, sets Unix
permissions to `0o600`, configures a 5-second busy timeout, enables WAL mode, and runs schema
migrations. All five steps happen before returning to the caller.

**Schema migrations** are guarded by `PRAGMA user_version`. Each version block is idempotent
(`CREATE TABLE IF NOT EXISTS`). A forward-compat guard returns `SearchError::Database` when the
stored version exceeds `CURRENT_VERSION` to prevent silent corruption by a newer writer.

**Current schema version is 2.** Version 1 created the four base tables. Version 2 adds three
performance indexes: `idx_hotspot_score ON hotspot(score)`, `idx_risk_score ON risk(risk_score)`,
and `idx_cochange_file_b ON cochange(file_b)`. The composite primary key `(file_a, file_b)` already
covers `file_a` lookups so no `idx_cochange_file_a` index is needed. Existing v1 databases are
migrated automatically on the next `TemporalDb::open`.

**Atomic sync:** `TemporalDb::sync(hotspots, risks, cochanges, git_head)` atomically replaces all
four tables in a single transaction (DELETE + batch INSERT for each table, then meta upserts for
`META_GIT_HEAD` and `META_LAST_UPDATED`). Readers never see partially-refreshed state. On error
the transaction is rolled back.

The individual `store_hotspots`, `store_risks`, and `store_cochanges` methods run their own
transactions separately — use `sync` when all three must be consistent with each other.

**Row types** (`HotspotRow`, `RiskRow`, `CochangeRow`) are plain structs with no serde derives.
They map directly to SQLite columns. Note the `i64` column types for integer fields even though
`FileTemporalStats` uses `u32` — SQLite's integer affinity is signed, so the layer converts at the
boundary.

**Thread safety:** `TemporalDb` is not `Sync`. Each thread that needs concurrent reads should open
its own connection to the same WAL-mode file. WAL mode allows multiple readers to coexist with one
writer without blocking.

## Per-File Lookup API

`storage_ops.rs` exposes three point-query methods for fetching a single file's data without
loading the full table:

- `hotspot_for_file(path: &str) -> Result<Option<HotspotRow>>` — returns `None` on miss, not an
  error. Uses `idx_hotspot_score` is irrelevant here; the PK index on `file_path` serves this query.
- `risk_for_file(path: &str) -> Result<Option<RiskRow>>` — same `None`-on-miss contract.
- `cochanges_for_file(path: &str) -> Result<Vec<CochangeRow>>` — bidirectional: queries both
  `file_a = ?1 OR file_b = ?1`. Results are sorted by `jaccard DESC`. Uses `idx_cochange_file_b`
  for the `file_b` side; the `(file_a, file_b)` PK covers the `file_a` side.

These methods are the preferred interface for search-time enrichment when only one file's data is
needed. Avoid `load_hotspots()` / `load_risks()` / `load_cochanges()` for per-file lookups —
those bulk-load the entire table and impose a 500,000-row capacity cap on the caller.

## Top-N Query API

Three ranked-list methods serve dashboard and heatmap use cases:

- `top_hotspots(limit: usize) -> Result<Vec<HotspotRow>>` — highest score first. Backed by
  `idx_hotspot_score`.
- `top_risks(limit: usize) -> Result<Vec<RiskRow>>` — highest `risk_score` first. Backed by
  `idx_risk_score`.
- `top_coldspots(limit: usize) -> Result<Vec<HotspotRow>>` — lowest score first (stable files).
  Also backed by `idx_hotspot_score` (ascending scan).

All three return an empty `Vec` for an empty table. The `limit` parameter is cast to `i64` for
the `LIMIT ?1` clause — this is safe up to `i64::MAX ≈ 9.2 × 10^18`, far above any practical
limit.

## SearchQuery file_filter Field

`SearchQuery` now carries an optional `file_filter: Option<HashSet<FileId>>`. When `Some`, the
search layer restricts scoring to only those `FileId`s in the set. This is the mechanism for
blast-radius pre-filtering: a caller resolves co-change partners via `cochanges_for_file`, maps
paths to `FileId`s, populates the set, and attaches it to the query before calling the search
layer.

The field is `#[serde(skip)]` — it is applied at query construction time in the CLI layer and is
not round-tripped through JSON. `SearchQuery::new` initializes it to `None`.

## Integration with TemporalSource

The I/O side of the temporal module follows a trait-based design. `GixSource` implements
`TemporalSource`, which has a single method:

```rust
fn parse_history(&self, repo_path: &Path, lookback_days: u32) -> Result<HistoryResult>;
```

`HistoryResult` contains `commits: Vec<CommitInfo>` (newest-first) and `metadata: TemporalMetadata`
(includes `is_shallow` for shallow clone detection). The `lookback_days = 0` sentinel means "all
history". Callers pass the resulting `commits` slice to `compute_file_risk_scores` and/or
`compute_file_temporal_stats`, then persist via `TemporalDb::sync`.

`GixSource` is stateless — each `parse_history` call opens a fresh repository handle. This is
intentional: it makes the type trivially `Send + Sync` without locking.

## Determinism Contract

`compute_file_risk_scores` and `compute_file_temporal_stats` are both deterministic: given the
same `commits` and `now_epoch`, repeated calls return identical results. The test
`deterministic_results` calls `compute_file_risk_scores` 50 times and asserts equality to `1e-9`
precision.

Determinism depends on the caller supplying `now_epoch`. Code that reads `SystemTime::now()` must
do so once and pass the result down — never call `SystemTime::now()` inside the scoring functions.
(`TemporalDb::sync` is the one place that legitimately reads the system clock — to record the
`META_LAST_UPDATED` timestamp, which is an observability artifact, not a scoring input.)

## Anti-Patterns

- **Reading the system clock inside scoring functions** — breaks determinism and makes tests
  non-deterministic. Always accept `now_epoch: u64` as a parameter and let the caller supply it.

- **Calling `is_fix_commit` inside the per-file loop** — the pre-classification step in
  `compute_file_risk_scores` exists to avoid this. One regex eval per commit, not one per
  (commit × file).

- **Using a custom regex instead of `is_fix_commit`** — the LazyLock regex is the single source
  of truth for fix classification across the entire temporal module. A parallel regex will diverge.

- **Treating `fix_density = 1.0` as definitive "buggy file"** — a file with a single fix commit
  and no other history scores `1.0`. Weight by hotspot before drawing conclusions.

- **Skipping the `(commits.len() / 4).clamp(64, 50_000)` capacity heuristic** — unique files are
  typically 5–20× fewer than total commit-file touches; `commits.len()` itself over-allocates
  significantly on large repos.

- **Using individual `store_*` methods when all three tables must be consistent** — call `sync`
  instead. Individual methods each run their own transaction, so a crash between calls leaves the
  database in a partially-updated state.

- **Leaking rusqlite types through the storage boundary** — all rusqlite errors are converted to
  `SearchError::Database(String)` via the private `db_err` helper. Never add a `rusqlite` dep to
  modules outside `temporal::storage`.

- **Opening `TemporalDb` from multiple threads and sharing it** — `TemporalDb` is not `Sync`.
  Open a separate connection per thread for concurrent access.

- **Using `load_hotspots()` / `load_risks()` for per-file enrichment at search time** — use the
  point-query methods (`hotspot_for_file`, `risk_for_file`) instead. Bulk-loading the entire table
  for a single path lookup wastes I/O and is blocked by the 500,000-row cap.

## Gotchas

- `decay_weight` panics in debug builds when `half_life_days <= 0.0` (`debug_assert!`). In
  release builds this silently produces `exp(+inf)` which is then clamped to `1.0`. Always
  validate `half_life_days > 0.0` before calling.

- The hotspot formula normalizes against the in-batch maximum, not a historical maximum. Two
  separate calls with different `commits` slices produce incomparable absolute scores; only
  relative ordering within one call's output is meaningful.

- `FileChangeInfo.additions` and `.deletions` are always `0` from `GixSource` — the git parser
  deliberately skips blob-level line counting for performance. Do not rely on these fields for
  churn metrics.

- File paths are stored as `String` (via `path_str().into_owned()`) using lossy UTF-8 conversion.
  On repositories with non-UTF-8 paths (uncommon but valid on Linux), replacement characters
  (`\u{FFFD}`) will appear in map keys and database rows.

- `compute_file_risk_scores` and `compute_file_temporal_stats` both return an empty `HashMap`
  for an empty `commits` slice — not a `SearchError`. Callers expecting a non-empty result should
  check before calling.

- `HotspotRow.changes_30d` and `changes_30d` are `i64` (SQLite signed integer), not `u32`. The
  conversion from `FileTemporalStats.changes_30d: u32` happens at the persistence call site.
  Casting `u32 as i64` is lossless; the reverse (`i64 as u32`) should be guarded for loaded rows.

- `TemporalDb::open` sets file permissions to `0o600` on Unix but silently ignores the error if
  `set_permissions` fails (e.g., on read-only filesystems or Docker volumes). Sensitive data in
  the database is not protected in those environments.

- Schema version is stored in `PRAGMA user_version`, not in the `meta` table. Do not confuse the
  two. The forward-compat guard triggers on `PRAGMA user_version > CURRENT_VERSION`, not on a
  `meta` key.

- `cochanges_for_file` queries `file_a = ?1 OR file_b = ?1`. The `idx_cochange_file_b` index
  covers the `file_b` side; the composite PK `(file_a, file_b)` covers the `file_a` side. If you
  add a query that filters only on `file_b` without the `OR file_a` arm, SQLite may not use the
  index depending on the query planner version — verify with `EXPLAIN QUERY PLAN`.

- `top_hotspots` / `top_risks` / `top_coldspots` cast `limit: usize` to `i64` for the `LIMIT`
  clause. Values above `i64::MAX` would overflow silently. In practice this is unreachable for any
  sane limit, but avoid passing `usize::MAX` as a "no limit" sentinel — use a bounded constant.

## Key Files

- `crates/rskim-search/src/temporal/scoring.rs` — pure scoring logic: `decay_weight`,
  `compute_file_risk_scores`, `compute_file_temporal_stats`, `DEFAULT_HALF_LIFE_DAYS`
- `crates/rskim-search/src/temporal/mod.rs` — module entry point: `FIX_REGEX` LazyLock,
  `is_fix_commit`, public re-exports (includes `storage` pub mod)
- `crates/rskim-search/src/temporal/git_parser.rs` — `GixSource` I/O implementation
- `crates/rskim-search/src/temporal/scoring_tests.rs` — co-located deterministic tests
- `crates/rskim-search/src/temporal/storage.rs` — `TemporalDb` struct, `open`, migrations
  (v1 tables + v2 performance indexes), `schema_version`, `db_err` helper, `META_*` constants
- `crates/rskim-search/src/temporal/storage_ops.rs` — `store_*`, `load_*`, `get_meta`,
  `set_meta`, `sync`, `hotspot_for_file`, `risk_for_file`, `cochanges_for_file`,
  `top_hotspots`, `top_risks`, `top_coldspots`
- `crates/rskim-search/src/temporal/storage_types.rs` — `HotspotRow`, `RiskRow`, `CochangeRow`
- `crates/rskim-search/src/types.rs` — `FileRiskScores`, `FileTemporalStats`, `CommitInfo`,
  `FileChangeInfo`, `HistoryResult`, `TemporalSource` trait, `SearchError::Database` variant,
  `SearchError::AstError` variant (grammar load failures — unrecoverable, distinct from file-level
  parse errors), `SearchQuery` (includes `file_filter: Option<HashSet<FileId>>`)
- `crates/rskim-search/src/lib.rs` — public re-exports for the whole crate; also exposes
  `pub mod ast_index` (CST linearization for structural n-gram extraction)

## Related

- Feature knowledge: `cochange` — the co-change matrix module also consumes `CommitInfo` and
  `FileChangeInfo` from git history. Both modules share the same I/O boundary pattern. `CochangeRow`
  is stored by `TemporalDb::sync` alongside hotspot and risk rows in the same atomic transaction.
  `cochanges_for_file` is the bridge between the temporal persistence layer and the `file_filter`
  blast-radius filtering in `SearchQuery`.
- `crates/rskim-search/src/types.rs` — shared `CommitInfo`, `FileChangeInfo`, `HistoryResult`,
  `TemporalSource`, `FileRiskScores`, `FileTemporalStats`, `SearchQuery.file_filter`,
  `SearchError::Database` variant (storage errors), and `SearchError::AstError` variant (grammar
  load failures) that connect the I/O, scoring, storage, and search layers
- ADR-001: Fix all noticed issues immediately regardless of scope — `SearchError::AstError` was
  added to cover grammar load failures surfaced during ast_index work; the variant was added to the
  shared error type immediately rather than deferred
