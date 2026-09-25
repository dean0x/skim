---
feature: temporal-scoring
name: Temporal Risk Scoring
description: "Use when adding temporal ranking signals, modifying decay parameters, working with the SQLite temporal persistence layer, or debugging hotspot/risk score computation. Keywords: temporal, hotspot, risk, fix-density, decay, half-life, TemporalDb, HotspotRow, RiskRow, hotspot_for_file, top_hotspots, top_risks, scoring, FileRiskScores, FileTemporalStats, storage_ops, WAL, schema migrations, DEFAULT_HALF_LIFE_DAYS, is_fix_commit, FIX_REGEX, MIN_JACCARD_THRESHOLD, MAX_ROWS_PER_TABLE, cochanges_for_file, UNION ALL, set_meta, get_meta, wilson_lower_bound, risk_score_wilson_decay, WILSON_Z_95, volume-weighting, AD-378-1."
category: architecture
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
created: 2026-06-21
updated: 2026-09-03
version: 4
---

# Temporal Risk Scoring

## Overview

The `temporal` module computes per-file risk and activity signals from git
commit history, then persists them to a SQLite database for query-time use.
Two orthogonal signal classes:

1. **Hotspot** — exponential-decay-weighted commit frequency, max-normalized to
   `[0.0, 1.0]`. Drives `--hot` / `--cold` sort modes.
2. **Fix-density** — ratio of fix-classified commits to total commits (weighted
   by decay). Drives `--risky` sort mode.

These signals are complementary: a file can be a hotspot (high churn) without
being risky (low fix-density), or vice versa.

## Module Structure

```
temporal/
  mod.rs               — public re-exports; FIX_REGEX (LazyLock<Regex>);
                         is_fix_commit() public fn
  git_parser.rs        — GixSource: TemporalSource impl using gix
  git_parser_tests.rs  — co-located tests (included via #[path])
  scoring.rs           — pure scoring fns: decay_weight, wilson_lower_bound,
                         risk_score_wilson_decay, compute_file_risk_scores,
                         compute_file_temporal_stats; no I/O
  scoring_tests.rs     — co-located tests (included via #[path])
  storage.rs           — TemporalDb wrapper; WAL setup; schema migrations;
                         #[path] includes storage_types and storage_ops
  storage_types.rs     — HotspotRow, RiskRow, CochangeRow (pure data)
  storage_ops.rs       — impl TemporalDb: store/load/sync/query methods
  storage_tests.rs     — co-located tests (included via #[path])
  storage_perf_tests.rs — perf tests (included via #[path])
```

## Scoring Algorithm

All scoring functions are pure — no I/O, deterministic when `now_epoch` is fixed.

### `decay_weight(elapsed_days: f64, half_life_days: f64) -> f64`

Returns `exp(-elapsed_days / half_life_days)`, clamped to `[0.0, 1.0]`.

**Important naming gotcha**: despite the name `half_life_days`, the formula
is an **e-folding decay** — the weight reaches `1/e ≈ 0.368` (not `0.5`)
after one period. The constant `DEFAULT_HALF_LIFE_DAYS = 30.0` means commits
from 30 days ago contribute ~37% of a today's commit's weight.

Panics when `half_life_days <= 0.0` or not finite. NaN `elapsed_days` is
treated as `0.0` to prevent NaN propagation.

### `wilson_lower_bound(successes: u32, trials: u32) -> f64` (new in #378)

Returns the Wilson score interval lower bound at 95% confidence (`WILSON_Z_95
= 1.96`). This is the statistically correct way to rank a proportion (here:
fix-commit density) while accounting for sample size — small samples are
self-suppressed toward zero, large samples approach the raw ratio.

**Why Wilson (AD-378-1)**: Ranking by a bare ratio `successes / trials`
saturates on tiny samples — a 1-fix/1-commit file ties a 50-fix/50-commit
file at `1.0`. Wilson reduces a 1/1 to ~0.21 and a 50/50 to ~0.93 with no
tuned constant.

Boundary semantics (AC4):
- `wilson_lower_bound(0, 0)` returns `0.0` (no observations → no evidence)
- `successes == 0` returns `0.0`
- `successes` is clamped to `trials` before computation

### `risk_score_wilson_decay(decay_fix_factor: f64, fix_commits: u32, total_commits: u32) -> f64` (new in #378)

`risk_score = decay_fix_factor * wilson_lower_bound(fix_commits, total_commits)`

The persisted `RiskRow.risk_score` used to rank files under `skim search
--risky`. Product of two `[0.0, 1.0]` proportions:

- **`decay_fix_factor`**: the decay-weighted fix proportion from
  `FileRiskScores::fix_density` (`Σ decay·is_fix / Σ decay`). Because decay
  weight appears in both numerator and denominator it largely cancels — this
  factor is the share of recency-weighted touches that were fixes, NOT a pure
  recency weight.
- **`wilson_lower_bound(fix_commits, total_commits)`**: confidence-adjusted
  fix proportion from raw lifetime counts. This is what actually fixes the
  #378 saturation bug — it suppresses tiny-sample files.

**Separation from `fix_density` (AD-378-3)**: `RiskRow.risk_score` (the
product above) is intentionally distinct from `RiskRow.fix_density` (the bare
raw ratio `fix_commits / total_commits` shown in the Fix% column). For any
file with `fix_commits != total_commits` the two differ.

**Grounding (AD-378-2)**: The choice of Wilson+decay is validated by a
temporal predict-future-fixes backtest (ADR-003, #361): risk is computed from
commits before a cutoff, files are labelled by whether they received a
fix-commit after the cutoff, and Wilson+decay MUST score >= the bare-ratio
baseline (AC9 in scoring_tests.rs).

### `compute_file_risk_scores(commits, now_epoch, half_life_days) -> HashMap<String, FileRiskScores>`

Single-pass over commits:
1. Pre-classify each commit with `is_fix_commit` (one regex eval per commit,
   not per file).
2. For each file in each commit: accumulate `(weighted_total, weighted_fix_total)`
   using `decay_weight`.
3. Max-normalize `weighted_total` → `hotspot_score ∈ [0.0, 1.0]`.
4. `fix_density = weighted_fix_total / weighted_total` (0.0 when total < ε).

Returns `FileRiskScores { hotspot: f64, fix_density: f64 }` per file path.

**Note**: `fix_density` here is the DECAY-WEIGHTED fix proportion — it is
the `decay_fix_factor` input to `risk_score_wilson_decay`. It is NOT the bare
ratio stored as `RiskRow.fix_density`.

### `compute_file_temporal_stats(commits, now_epoch) -> HashMap<String, FileTemporalStats>`

Single-pass raw commit counts (no decay weighting):
- `total_commits`, `fix_commits` (is_fix_commit classification)
- `changes_30d` (elapsed ≤ 30.0 days), `changes_90d` (elapsed ≤ 90.0 days)

Boundary semantics: exactly 30.0 days is **included** (`<=`, not `<`).

Uses per-commit deduplication via `dedup_changed_files` (helper fn using a
reused `HashSet<String>` buffer) — a file listed twice in one commit's
`changed_files` is counted once. This differs from `compute_file_risk_scores`
which accumulates from `commit.changed_files` directly.

### `is_fix_commit(message: &str) -> bool`

Matches commit message against `FIX_REGEX`:
```
(?i)\b(fix|bug|hotfix|patch|revert)\b
```
Case-insensitive, word-boundary anchored. The regex is compiled once into a
`LazyLock<Regex>`. Words: `fix`, `bug`, `hotfix`, `patch`, `revert`.

### `GixSource` (git_parser.rs)

Implements `TemporalSource::parse_history` using `gix`. Converts gix types
(`gix::commit`, `gix::diff`) to shared `CommitInfo` / `FileChangeInfo` types
at the parser boundary. No gix types cross the module boundary.

## SQLite Persistence Layer

### `TemporalDb` (storage.rs)

Wraps a single `rusqlite::Connection`. Not `Sync` — open separate instances
per thread. Schema: WAL mode, `synchronous=NORMAL`, 5-second busy timeout.

Rusqlite errors are converted to `SearchError` variants via two helpers —
neither leaks rusqlite types into the public API:
- `db_err` converts any rusqlite error into `SearchError::Database(String)`
  for ordinary I/O, lock, and query failures.
- `classify_sqlite_err` (AD-414-2) inspects the SQLite error code: it returns
  `SearchError::DatabaseCorrupt` for `SQLITE_NOTADB` / `SQLITE_CORRUPT` (the
  file is structurally invalid and safe to discard) and falls through to
  `SearchError::Database` for all other codes, so transient errors are never
  misclassified as corruption.

**Schema version: 2** (current). Migrations are forward-only and idempotent
(each migration block is guarded by `version < N`). A **forward-compat guard**
rejects databases from future schema versions (`version > CURRENT_VERSION`)
by returning `SearchError::UnsupportedSchemaVersion { found, supported }`
(AD-414-11) rather than silently corrupting the newer schema.

Tables:
- `hotspot (file_path TEXT PK, score REAL, changes_30d INT, changes_90d INT)`
- `risk (file_path TEXT PK, risk_score REAL, total_commits INT, fix_commits INT, fix_density REAL)`
- `cochange (file_a TEXT, file_b TEXT, count INT, jaccard REAL, PK(file_a,file_b))`
- `meta (key TEXT PK, value TEXT)`

Indexes added in v2 migration:
- `idx_hotspot_score ON hotspot(score)` — for `top_hotspots` / `top_coldspots`
- `idx_risk_score ON risk(risk_score)` — for `top_risks`
- `idx_cochange_file_b ON cochange(file_b)` — for `cochanges_for_file` WHERE
  `file_b = ?` clause; `(file_a, file_b)` PK already covers `file_a` lookups

File permissions: `0o600` on Unix (owner-only access).

### `TemporalDb::open(db_path: &Path) -> Result<Self>`

Opens or creates the database. Applies v1 and v2 migrations sequentially. Safe
to call on a pre-existing database — idempotent.

### `TemporalDb::open_existing(db_path: &Path) -> Result<Self>`

Like `open`, but opens with `SQLITE_OPEN_READ_WRITE` WITHOUT `SQLITE_OPEN_CREATE`.
Returns `Err` if the file does not exist or cannot be opened; does NOT create a new
database. Introduced by the F3 / AD-414-17 fix in `crates/rskim/src/cmd/search/temporal_build.rs`
so that the `parse_history`-failure fall-through path can update `META_IS_SHALLOW` in an
already-existing `temporal.db` without the TOCTOU risk of creating a new file (which
would then have only the shallow meta row and no `META_GIT_HEAD`, putting the DB in a
misleading state). Callers that need to create a DB should use `open`.

### `TemporalDb::sync(hotspots, risks, cochanges, git_head, is_shallow) -> Result<()>`

The only write entry point. Atomically replaces all data in a single
transaction so readers never see a partially-refreshed state:
1. DELETE FROM hotspot / risk / cochange
2. INSERT all rows
3. SET meta `git_head`, `data_version`, `last_updated`, and `is_shallow`
4. COMMIT

`is_shallow: bool` records whether `.git/shallow` existed at build time
(AD-414-14); `check_staleness` Check 3 later treats its disappearance (after
`git fetch --unshallow`) as a staleness trigger so the now-reachable history
is ingested on the next query without a manual `--rebuild`.

The DELETE+INSERT batch is the canonical "replace-all" pattern. No partial
updates; consistency across tables is guaranteed by the single transaction.

### Key Read Methods (storage_ops.rs)

```rust
pub fn hotspot_for_file(&self, path: &str) -> Result<Option<HotspotRow>>
pub fn risk_for_file(&self, path: &str) -> Result<Option<RiskRow>>
pub fn cochanges_for_file(&self, path: &str) -> Result<Vec<CochangeRow>>
    // Returns pairs where file_a = path OR file_b = path (UNION ALL)
    // Filtered by jaccard >= MIN_JACCARD_THRESHOLD
pub fn top_hotspots(&self, limit: usize) -> Result<Vec<HotspotRow>>
    // ORDER BY score DESC LIMIT ?
pub fn top_risks(&self, limit: usize) -> Result<Vec<RiskRow>>
    // ORDER BY risk_score DESC LIMIT ?
pub fn top_coldspots(&self, limit: usize) -> Result<Vec<HotspotRow>>
    // ORDER BY score ASC LIMIT ?
pub fn set_meta(&self, key: &str, value: &str) -> Result<()>
pub fn get_meta(&self, key: &str) -> Result<Option<String>>
pub fn schema_version(&self) -> Result<i64>
```

`cochanges_for_file` uses `UNION ALL` to find both `file_a = path` and
`file_b = path` rows (because the PK stores pairs in lexical order, only one
direction is in each row). The Jaccard filter is applied at the SQL level using
`MIN_JACCARD_THRESHOLD = 0.10`.

### Meta Keys

- `META_LAST_UPDATED = "last_updated"` — Unix epoch seconds of last `sync`
- `META_GIT_HEAD = "git_head"` — git HEAD SHA at last `sync`
- `META_DATA_VERSION = "data_version"` — numeric value of `TEMPORAL_DATA_VERSION`
  (currently `1`); written unconditionally by `sync` alongside `META_GIT_HEAD`
  as a co-required pair (AD-408-3). A DB without this key is treated as stale
  and triggers an automatic full rebuild.
- `META_GIT_TOPLEVEL = "git_toplevel"` — canonical git repository root path
  recorded after a successful `sync` for subdirectory-root re-anchor detection
  (AD-413-16). Written in a second transaction after the main `sync` commit;
  an absent row means "built before #413" and is adopted rather than refused.
- `META_IS_SHALLOW = "is_shallow"` — `"1"` if `.git/shallow` existed at build
  time, `"0"` otherwise (AD-414-14). An absent row (DB written before
  AD-414-14) means the shallow check is skipped on that DB.

These keys are checked by the CLI staleness layer to decide whether a rebuild
is needed (including by `temporal_db_is_stale` in `staleness.rs`).

## Row Types

```rust
pub struct HotspotRow {
    pub file_path: String,    // repo-root-relative
    pub score: f64,           // decay-weighted, max-normalized to [0,1]
    pub changes_30d: u32,     // raw commit count in last 30 days
    pub changes_90d: u32,     // raw commit count in last 90 days
}

pub struct RiskRow {
    pub file_path: String,
    pub risk_score: f64,      // Wilson+decay composite [0,1] (new in #378)
    pub total_commits: u32,
    pub fix_commits: u32,
    pub fix_density: f64,     // bare raw ratio fix_commits / total_commits
}

pub struct CochangeRow {
    pub file_a: String,       // lexically smaller path
    pub file_b: String,
    pub count: u32,           // co-change commit count
    pub jaccard: f64,         // Jaccard similarity of commit sets
}
```

**`RiskRow.risk_score` vs `RiskRow.fix_density`**: these are intentionally
distinct (AD-378-3). `risk_score` is the Wilson+decay product (suppresses
small-sample files). `fix_density` is the bare raw ratio for display in the
`Fix%` column.

## Crate-Root Re-Exports

```rust
// From temporal module (temporal/mod.rs):
pub use temporal::{
    DEFAULT_HALF_LIFE_DAYS, GixSource, compute_file_risk_scores,
    compute_file_temporal_stats, decay_weight, is_fix_commit,
    risk_score_wilson_decay, wilson_lower_bound,
};

// From temporal::storage (storage.rs):
pub use temporal::storage::{
    CochangeRow, HotspotRow, META_GIT_HEAD, META_LAST_UPDATED,
    MIN_COCHANGE_JACCARD, RiskRow, TemporalDb,
};
```

`MIN_COCHANGE_JACCARD` is the public name for `storage_ops::MIN_JACCARD_THRESHOLD`
— the single source of truth for the Jaccard filter threshold (Decision O-D).

`wilson_lower_bound` and `risk_score_wilson_decay` are now public exports at
the crate root (added in #378), enabling `temporal_build.rs` in the CLI crate
to call `risk_score_wilson_decay` without duplication.

## Anti-Patterns

- **Calling `decay_weight` with `half_life_days <= 0.0` or NaN**: panics with an
  assertion. Always validate before calling, or use `DEFAULT_HALF_LIFE_DAYS`.

- **Assuming `half_life` means the value halves**: the formula is e-folding, not
  half-life. After `DEFAULT_HALF_LIFE_DAYS = 30.0` days, weight is ~37%, not 50%.

- **Calling `TemporalDb::store_hotspots` / `store_risks` / `store_cochanges`
  individually instead of `sync`**: the individual `store_*` methods do NOT wrap
  in a single transaction, so partial failures leave the tables inconsistent.
  Always use `sync` for production writes.

- **Opening `TemporalDb` from multiple threads**: `TemporalDb` is not `Sync`.
  Each thread must open its own connection. For concurrent reads, multiple
  `TemporalDb::open` calls against the same WAL database file are safe.

- **Filtering co-change results with a different Jaccard threshold than
  `MIN_JACCARD_THRESHOLD`**: the write-side builder in `temporal_build.rs` also
  filters at the same threshold before calling `store_cochanges`. Use
  `MIN_COCHANGE_JACCARD` (the re-exported constant) everywhere to stay consistent
  with Decision O-D.

- **Hardcoding `CURRENT_VERSION = 2` in tests**: use `db.schema_version()` to
  assert the migrated version. The schema will increment when new features add
  tables or indexes.

- **Using bare `fix_density` ratio for `--risky` ranking**: the #378 saturation
  bug showed that `fix_commits / total_commits` saturates at 1.0 for any
  file with even a single fix commit. Always use `risk_score_wilson_decay`
  (which goes through `wilson_lower_bound`) to compute the persisted `risk_score`.

- **Treating `FileRiskScores::fix_density` and `RiskRow::fix_density` as the same
  value**: `FileRiskScores::fix_density` is the DECAY-WEIGHTED fix proportion
  (used as `decay_fix_factor`). `RiskRow::fix_density` is the bare raw ratio for
  display. They differ for any file with a mix of old and recent commits.

## Gotchas

- **`risk_score_wilson_decay` is the new `--risky` ranking function** (as of #378):
  `temporal_build.rs` calls `risk_score_wilson_decay(scores[path].fix_density,
  stats[path].fix_commits, stats[path].total_commits)` to compute `RiskRow.risk_score`.
  This replaces the old bare ratio / VOLUME_REF saturation approach.

- **`git_parser_tests.rs` is a co-located test file**: the `git_parser` module
  has co-located tests at `git_parser_tests.rs`, included via `#[path]` in
  `git_parser.rs`.

- **`compute_file_risk_scores` pre-classifies all commits before iterating files**:
  this prevents O(files × regex) cost. The `fix_flags: Vec<bool>` allocation
  bounds cost to O(commits) per call.

- **`compute_file_temporal_stats` deduplicates per-commit file paths** with a
  reused `HashSet<String>` buffer: a file appearing twice in one commit is
  counted once in all four counters (total_commits, fix_commits, changes_30d,
  changes_90d). This differs from `compute_file_risk_scores` which accumulates
  directly from `changed_files` (each occurrence adds weight). The private helper
  `dedup_changed_files` is the deduplication boundary.

- **`load_hotspots` / `load_risks` / `load_cochanges`**: these bulk-read methods
  exist alongside the per-file query methods. They are used by `temporal_build.rs`
  to rebuild the Jaccard-derived co-change rows from the SQLite layer rather than
  re-parsing git history. Not needed for typical query operations.

- **`schema_version` is marked `#[must_use]`**: clippy will warn if you discard
  the return value without handling the `Result`.

- **WAL mode and `synchronous=NORMAL`**: the database uses WAL with
  `synchronous=NORMAL` for performance. This is safe for the use case (derived
  cache, rebuildable from git history) — an OS crash could lose the last
  transaction but the next build will repair it.

- **Wilson is parameter-free at the chosen confidence level**: `WILSON_Z_95 =
  1.96` is the only constant. The old `VOLUME_REF` saturation reference is
  removed entirely in #378.

## Key Files

- `crates/rskim-search/src/temporal/mod.rs` — module structure, `is_fix_commit`,
  `FIX_REGEX`, public re-exports including `wilson_lower_bound` and
  `risk_score_wilson_decay`
- `crates/rskim-search/src/temporal/scoring.rs` — `decay_weight`,
  `wilson_lower_bound`, `risk_score_wilson_decay` (all new in #378),
  `compute_file_risk_scores`, `compute_file_temporal_stats`,
  `DEFAULT_HALF_LIFE_DAYS`; all pure, no I/O
- `crates/rskim-search/src/temporal/git_parser.rs` — `GixSource`; gix integration;
  type conversion boundary
- `crates/rskim-search/src/temporal/storage.rs` — `TemporalDb`; WAL setup;
  migration runner; schema constants; `META_LAST_UPDATED`, `META_GIT_HEAD`,
  `CURRENT_VERSION = 2`
- `crates/rskim-search/src/temporal/storage_types.rs` — `HotspotRow`, `RiskRow`,
  `CochangeRow` (pure data; no deps)
- `crates/rskim-search/src/temporal/storage_ops.rs` — all `impl TemporalDb` store/
  load/sync/query methods; `MIN_JACCARD_THRESHOLD = 0.10`
- `crates/rskim-search/src/types.rs` — `CommitInfo`, `FileChangeInfo`,
  `FileRiskScores`, `FileTemporalStats`, `HistoryResult`, `TemporalSource`

## Related

- Feature: `cochange` — provides `CochangeMatrixBuilder`/`CochangeMatrixReader` as
  library primitives. NOTE: the CLI temporal build path (`temporal_build.rs`) does
  NOT use `CochangeMatrixBuilder` — it uses its own inline `build_cochange_rows`
  function that applies the same Jaccard formula directly. `CochangeRow`,
  `MIN_COCHANGE_JACCARD` are still sourced from this module.
- Feature: `cmd-search` — CLI orchestration layer; `temporal_build.rs` calls
  `compute_file_risk_scores`, `compute_file_temporal_stats`, `TemporalDb::sync`,
  and now also `risk_score_wilson_decay` for volume-weighted risk ranking (#378);
  `temporal.rs` in cmd/search serves `--hot`, `--cold`, `--risky`, `--blast-radius`.
- `crates/rskim-search/src/temporal/storage_ops.rs` — `MIN_JACCARD_THRESHOLD` is
  the single source of truth; re-exported publicly as `MIN_COCHANGE_JACCARD`.
- Issue #191: co-change validation benchmark establishing `MIN_JACCARD_THRESHOLD`.
- Issue #378: volume-weighted risk scoring using Wilson score interval lower bound.
