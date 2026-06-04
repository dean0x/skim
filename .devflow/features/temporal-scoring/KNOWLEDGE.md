---
feature: temporal-scoring
name: Temporal Risk Scoring
description: "Use when adding per-file risk metrics from git history, tuning decay parameters, integrating hotspot scores into search ranking, extending the temporal module with new scoring signals, or working with the SQLite temporal persistence layer. Keywords: temporal, hotspot, fix density, exponential decay, git history, risk scores, half-life, FileRiskScores, FileTemporalStats, compute_file_risk_scores, compute_file_temporal_stats, decay_weight, TemporalDb, storage, HotspotRow, RiskRow, CochangeRow, file_filter, SearchQuery, per-file lookup, top-N, hotspot_for_file, risk_for_file, cochanges_for_file, top_hotspots, top_risks, top_coldspots, blast-radius, storage_tests, storage_perf_tests, git_parser_tests."
category: domain-knowledge
directories: [crates/rskim-search/src/temporal/]
referencedFiles:
  - crates/rskim-search/src/temporal/scoring.rs
  - crates/rskim-search/src/temporal/scoring_tests.rs
  - crates/rskim-search/src/temporal/mod.rs
  - crates/rskim-search/src/temporal/git_parser.rs
  - crates/rskim-search/src/temporal/git_parser_tests.rs
  - crates/rskim-search/src/temporal/storage.rs
  - crates/rskim-search/src/temporal/storage_ops.rs
  - crates/rskim-search/src/temporal/storage_perf_tests.rs
  - crates/rskim-search/src/temporal/storage_tests.rs
  - crates/rskim-search/src/temporal/storage_types.rs
  - crates/rskim-search/src/types.rs
  - crates/rskim-search/src/lib.rs
created: 2026-05-25
updated: 2026-06-04
---

# Temporal Risk Scoring

## Overview

The temporal scoring subsystem computes two per-file risk metrics from a repository's git commit
history: a **hotspot score** (decay-weighted commit frequency) and a **fix density** (fraction of
recent touches that were bug fixes). Both are `f64` values in `[0.0, 1.0]` and are returned as
`FileRiskScores` in a `HashMap<String, FileRiskScores>`.

A companion function, `compute_file_temporal_stats`, computes raw (non-decay-weighted) commit
counts per file within 30-day and 90-day windows, producing `FileTemporalStats` values for
persistence. The `temporal::storage` sub-module persists computed values to a WAL-mode SQLite
database via `TemporalDb`.

The module is deliberately split into three independent layers: `git_parser.rs` handles all I/O
(gix-based history traversal), `scoring.rs` is pure (no I/O, no side effects), and `storage.rs`
owns the SQLite connection and schema lifecycle.

## Test Module Organization

Tests are split into companion files alongside their implementation files:

- `scoring_tests.rs` — co-located with `scoring.rs`; deterministic unit tests for decay,
  accumulation, normalization, fix classification
- `git_parser_tests.rs` — co-located with `git_parser.rs`; integration tests for `GixSource`
  history traversal against a real git repository
- `storage_tests.rs` — comprehensive CRUD, round-trip, schema migration, WAL, per-file lookup
  tests for `TemporalDb`
- `storage_perf_tests.rs` — performance regression tests (e.g., `load_10k_hotspots_under_100ms`);
  gated on `#[cfg(not(debug_assertions))]` to avoid slow runs in debug builds

## Business Context

Risk scores feed downstream ranking signals for the search layer. A file with a high hotspot
score changed frequently and recently. A high fix density means a large fraction of recent touches
were bug fixes. Neither metric is a hard filter; both are signals blended with BM25F scores or
co-change coupling data.

## Core Business Rules

**Hotspot is max-normalized.** After accumulating decay-weighted totals, every file's total is
divided by the global maximum. The most-active file always scores exactly `1.0`.

**Fix density is ratio-based.** Fix density = `weighted_fix_total / weighted_total` per file.

**Fix classification is commit-wide.** `is_fix_commit` classifies the whole commit message. If a
commit changes `a.rs` and `b.rs` and is a fix commit, both files receive the fix weight.

**The half-life parameter is in days, not seconds.** `decay_weight(elapsed_days, half_life_days)`
uses `exp(-elapsed_days / half_life_days)`. At exactly one half-life elapsed the weight is
`1/e ≈ 0.368`, not `0.5`. This is exponential decay, not biological half-life.

**Future-dated commits are treated as elapsed = 0.** Commits ahead of `now_epoch` contribute
weight `1.0`. Pre-epoch timestamps produce near-zero weight.

**`compute_file_temporal_stats` deduplicates per commit** using a cleared `HashSet<String>`.

**Window boundary is inclusive.** A commit at exactly `30.0` or `90.0` elapsed days is included.

## Fix Commit Keywords

Regex in `temporal/mod.rs` compiled once via `std::sync::LazyLock`:

```
(?i)\b(fix|bug|hotfix|patch|revert)\b
```

Word-boundary anchors prevent false positives from `prefix` or `buggy`. `LazyLock` ensures the
regex compiles exactly once across all threads.

## Algorithm Walk-Through

`compute_file_risk_scores(commits, now_epoch, half_life_days)`:

1. **Pre-classify** — build a `Vec<bool>` of fix flags calling `is_fix_commit` once per commit.
2. **Accumulate** — single pass over `(commit, is_fix)` pairs. Per-file Entry API update for
   `(weighted_total, weighted_fix_total)`. Map pre-allocated with
   `(commits.len() / 4).clamp(64, 50_000)`.
3. **Normalize and emit** — find `max_total` via fold; map each entry to `FileRiskScores`.

`compute_file_temporal_stats(commits, now_epoch)`:

1. **Per commit** — `is_fix_commit` inline, elapsed days, `in_30d`/`in_90d` membership, unique
   paths via cleared `HashSet<String>`.
2. **Accumulate** — increment counters via `or_default()`.

The two functions are independent implementations. Do not merge them.

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
    CochangeRow, HotspotRow, RiskRow,  // row types
    META_GIT_HEAD, META_LAST_UPDATED,  // meta table key constants
    TemporalDb,                         // connection + migrations + sync
};
pub use types::{
    FileRiskScores,       // { hotspot: f64, fix_density: f64 }
    FileTemporalStats,    // { changes_30d, changes_90d, total_commits, fix_commits: u32 }
};
```

`FileRiskScores` and `FileTemporalStats` do NOT derive `Serialize`/`Deserialize`. Consumers that
need JSON output must map to local serde-annotated types.

`DEFAULT_HALF_LIFE_DAYS = 30.0` — always use this constant, never a hardcoded literal.

## SQLite Persistence Layer

`TemporalDb` owns one SQLite connection and four tables: `hotspot`, `risk`, `cochange`, and
`meta`.

**Connection lifecycle:** `TemporalDb::open(db_path)` opens or creates the file, sets permissions
to `0o600`, configures 5-second busy timeout, enables WAL mode, and runs schema migrations.

**Schema migrations** are guarded by `PRAGMA user_version`. Each version block is idempotent.
Forward-compat guard returns `SearchError::Database` when stored version exceeds `CURRENT_VERSION`.

**Current schema version is 2.** Version 1 created the four base tables. Version 2 adds three
performance indexes: `idx_hotspot_score ON hotspot(score)`, `idx_risk_score ON risk(risk_score)`,
`idx_cochange_file_b ON cochange(file_b)`. Existing v1 databases are migrated automatically.

**Atomic sync:** `TemporalDb::sync(hotspots, risks, cochanges, git_head)` atomically replaces all
four tables in one transaction. Readers never see partially-refreshed state.

**Row types** (`HotspotRow`, `RiskRow`, `CochangeRow`) use `i64` column types even though
`FileTemporalStats` uses `u32` — SQLite's integer affinity is signed.

## Per-File Lookup API

- `hotspot_for_file(path: &str) -> Result<Option<HotspotRow>>` — `None` on miss, not an error
- `risk_for_file(path: &str) -> Result<Option<RiskRow>>` — same `None`-on-miss contract
- `cochanges_for_file(path: &str) -> Result<Vec<CochangeRow>>` — bidirectional: queries both
  `file_a = ?1 OR file_b = ?1`; sorted by `jaccard DESC`; uses `idx_cochange_file_b` for `file_b`

These methods are the preferred interface for search-time enrichment when only one file's data is
needed. Avoid `load_hotspots()` / `load_risks()` / `load_cochanges()` for per-file lookups.

## Top-N Query API

- `top_hotspots(limit: usize) -> Result<Vec<HotspotRow>>` — highest score first
- `top_risks(limit: usize) -> Result<Vec<RiskRow>>` — highest `risk_score` first
- `top_coldspots(limit: usize) -> Result<Vec<HotspotRow>>` — lowest score first (stable files)

All three return an empty `Vec` for an empty table.

## SearchQuery file_filter Field (blast-radius)

`SearchQuery` carries `file_filter: Option<HashSet<FileId>>`. When `Some`, the search layer
restricts scoring to only those `FileId`s. This is the blast-radius pre-filtering mechanism:
caller resolves co-change partners via `cochanges_for_file`, maps paths to `FileId`s, populates
the set, and attaches it to the query.

The field is `#[serde(skip)]` — not round-tripped through JSON. `SearchQuery::new` initializes it
to `None`. Triggered via `skim search --blast-radius FILE` CLI flag.

## Integration with TemporalSource

`GixSource` implements `TemporalSource`:

```rust
fn parse_history(&self, repo_path: &Path, lookback_days: u32) -> Result<HistoryResult>;
```

`HistoryResult` contains `commits: Vec<CommitInfo>` (newest-first) and
`metadata: TemporalMetadata` (includes `is_shallow` for shallow clone detection).
`lookback_days = 0` means "all history". `GixSource` is stateless — trivially `Send + Sync`.

## Determinism Contract

`compute_file_risk_scores` and `compute_file_temporal_stats` are deterministic: same inputs
produce identical outputs. The test `deterministic_results` calls `compute_file_risk_scores` 50
times and asserts equality to `1e-9` precision. Always accept `now_epoch: u64` as a parameter —
never call `SystemTime::now()` inside scoring functions.

## Anti-Patterns

- **Reading the system clock inside scoring functions** — breaks determinism.
- **Calling `is_fix_commit` inside the per-file loop** — pre-classification exists to prevent
  this. One regex eval per commit, not one per (commit × file).
- **Using a custom regex instead of `is_fix_commit`** — the LazyLock regex is the single source
  of truth for fix classification.
- **Treating `fix_density = 1.0` as definitive "buggy file"** — weight by hotspot first.
- **Skipping the capacity heuristic** — use `(commits.len() / 4).clamp(64, 50_000)`.
- **Using individual `store_*` methods when all three tables must be consistent** — call `sync`.
- **Leaking rusqlite types through the storage boundary** — all errors converted via `db_err`.
- **Opening `TemporalDb` from multiple threads and sharing it** — not `Sync`; open per-thread.
- **Using `load_hotspots()` / `load_risks()` for per-file enrichment** — use point-query methods.

## Gotchas

- `decay_weight` panics in debug builds when `half_life_days <= 0.0` (`debug_assert!`).
- Hotspot formula normalizes against the in-batch maximum — not a historical maximum. Two calls
  with different `commits` slices produce incomparable absolute scores.
- `FileChangeInfo.additions` and `.deletions` are always `0` from `GixSource` — blob-level line
  counting is skipped for performance.
- File paths stored as `String` via lossy UTF-8 — non-UTF-8 paths get replacement characters.
- Empty `commits` slice returns an empty `HashMap`, not a `SearchError`.
- `HotspotRow.changes_30d` is `i64` (SQLite), not `u32`. Cast `u32 as i64` is lossless; reverse
  needs a guard.
- Schema version is in `PRAGMA user_version`, not in the `meta` table.
- `cochanges_for_file` queries `file_a = ?1 OR file_b = ?1`. The `idx_cochange_file_b` index
  covers `file_b`; the composite PK covers `file_a`. Verify with `EXPLAIN QUERY PLAN` if adding
  a filter-only-on-`file_b` query.
- `top_hotspots` / `top_risks` / `top_coldspots` cast `limit: usize` to `i64` for `LIMIT` — do
  not pass `usize::MAX` as a "no limit" sentinel.
- `storage_perf_tests.rs` tests are gated on `#[cfg(not(debug_assertions))]` to avoid slow runs
  in debug mode. They will not appear in `cargo test --debug`.

## Key Files

- `crates/rskim-search/src/temporal/scoring.rs` — pure scoring: `decay_weight`,
  `compute_file_risk_scores`, `compute_file_temporal_stats`, `DEFAULT_HALF_LIFE_DAYS`
- `crates/rskim-search/src/temporal/mod.rs` — `FIX_REGEX` LazyLock, `is_fix_commit`, re-exports
- `crates/rskim-search/src/temporal/git_parser.rs` — `GixSource` I/O implementation
- `crates/rskim-search/src/temporal/scoring_tests.rs` — deterministic unit tests
- `crates/rskim-search/src/temporal/storage.rs` — `TemporalDb` struct, `open`, migrations (v2)
- `crates/rskim-search/src/temporal/storage_ops.rs` — all CRUD, point-query, and top-N methods
- `crates/rskim-search/src/temporal/storage_tests.rs` — CRUD + migration + per-file lookup tests
- `crates/rskim-search/src/temporal/storage_perf_tests.rs` — performance regression tests
- `crates/rskim-search/src/temporal/storage_types.rs` — `HotspotRow`, `RiskRow`, `CochangeRow`
- `crates/rskim-search/src/types.rs` — `FileRiskScores`, `FileTemporalStats`, `CommitInfo`,
  `HistoryResult`, `TemporalSource`, `SearchError::Database`, `SearchError::AstError`,
  `SearchQuery` (includes `file_filter: Option<HashSet<FileId>>`)
- `crates/rskim-search/src/lib.rs` — public crate re-exports; also exposes `pub mod ast_index`

## Related

- Feature knowledge: `cochange` — the co-change matrix module also consumes `CommitInfo` and
  `FileChangeInfo`. `CochangeRow` is stored by `TemporalDb::sync`. `cochanges_for_file` bridges
  the persistence layer to the `file_filter` blast-radius filtering in `SearchQuery`.
- `crates/rskim-search/src/types.rs` — shared types connecting I/O, scoring, storage, and search
- ADR-001: Fix all noticed issues immediately — `SearchError::AstError` was added to cover grammar
  load failures surfaced during ast_index work; added immediately rather than deferred
