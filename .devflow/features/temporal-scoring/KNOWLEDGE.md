---
feature: temporal-scoring
name: Temporal Risk Scoring
description: "Use when adding temporal ranking signals, modifying decay parameters, working with the SQLite temporal persistence layer, or debugging hotspot/risk score computation. Keywords: temporal, hotspot, risk, fix-density, decay, half-life, TemporalDb, HotspotRow, RiskRow, hotspot_for_file, top_hotspots, top_risks, scoring, FileRiskScores, FileTemporalStats, storage_ops, WAL, schema migrations, DEFAULT_HALF_LIFE_DAYS, is_fix_commit, FIX_REGEX, MIN_JACCARD_THRESHOLD, MAX_ROWS_PER_TABLE, cochanges_for_file, UNION ALL, set_meta, get_meta."
category: domain-knowledge
directories: [crates/rskim-search/src/temporal/]
referencedFiles:
  - crates/rskim-search/src/temporal/scoring.rs
  - crates/rskim-search/src/temporal/scoring_tests.rs
  - crates/rskim-search/src/temporal/mod.rs
  - crates/rskim-search/src/temporal/git_parser.rs
  - crates/rskim-search/src/temporal/storage.rs
  - crates/rskim-search/src/temporal/storage_ops.rs
  - crates/rskim-search/src/temporal/storage_perf_tests.rs
  - crates/rskim-search/src/temporal/storage_tests.rs
  - crates/rskim-search/src/temporal/storage_types.rs
  - crates/rskim-search/src/types.rs
  - crates/rskim-search/src/lib.rs
created: 2026-06-01
updated: 2026-06-09
version: 4
---

# Temporal Risk Scoring

## Overview

The temporal module computes per-file risk signals from git history: hotspot scores (how
frequently a file has changed, weighted by recency) and bug-fix density scores (what fraction
of weighted touches were fix commits). These signals are persisted to a WAL-mode SQLite
database (`temporal.db`) for fast bulk-loading by ranking pipelines and `skim search`
temporal flags (`--hot`, `--cold`, `--risky`, `--blast-radius`).

The module is separate from the co-change matrix (`crates/rskim-search/src/cochange/`) —
both consume `HistoryResult`, but produce different signal types and use different
storage formats.

## Signal Types

### Hotspot Score

A hotspot score quantifies how actively a file is being modified, with recent changes
weighted more heavily than old ones. The score is computed per file, then max-normalized
across all files to `[0.0, 1.0]`.

**Formula:**
```
hotspot(file) = Σ_commits decay_weight(elapsed_days, half_life_days) / max_total
decay_weight(d, h) = exp(-d / h)
```

- `elapsed_days`: days between the commit timestamp and `now_epoch`
- `half_life_days`: configurable decay rate; default `DEFAULT_HALF_LIFE_DAYS = 30.0`
- After one `half_life_days` period, a commit contributes `1/e ≈ 0.368` (not `0.5`)
  — this is an **e-folding decay**, not a strict half-life. The naming follows the heatmap
  module convention. Doc comments say "~37%".
- Sum over all commits touching the file; recent commits contribute ~1.0, old commits ~0.0
- Max-normalization: the hottest file always scores 1.0; others are relative fractions

`HotspotRow` persists:
- `file_path`: repo-root-relative path
- `score`: decay-weighted, max-normalized to `[0.0, 1.0]`
- `changes_30d`: raw commit count in last 30 days
- `changes_90d`: raw commit count in last 90 days

### Bug-Fix Density (Risk Score)

Risk score quantifies how often a file's weighted touches were fix commits.

**Formula:**
```
fix_density(file) = weighted_fix_total(file) / weighted_total(file)
```

Both numerator and denominator are decay-weighted sums. This is NOT a simple ratio of
raw counts — it is a recency-weighted fraction. Files with zero weighted total have
`fix_density = 0.0`. The result is in `[0.0, 1.0]` and is clamped via epsilon guard.

`RiskRow` persists:
- `file_path`: repo-root-relative path
- `risk_score`: decay-weighted fix density, in `[0.0, 1.0]`
- `total_commits`: total commits touching this file
- `fix_commits`: commits classified as fix commits
- `fix_density`: same as `risk_score` (alias stored for query convenience)

### Fix Commit Detection

`is_fix_commit(message: &str) -> bool` (in `mod.rs`) uses a compiled regex with word-
boundary anchoring:

```rust
static FIX_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(fix|bug|hotfix|patch|revert)\b").expect("valid regex")
});
```

Recognised keywords (case-insensitive, **word-boundary anchored**): `fix`, `bug`,
`hotfix`, `patch`, `revert`.

**Important**: word boundaries mean `"prefix_fix"` does NOT match. The old substring
approach (`str::contains`) was replaced with regex to prevent false positives from
words containing fix-related substrings. The keywords `repair`, `correct`, and
`resolve` are no longer matched.

## Core Entry Points

### `compute_file_risk_scores`

```rust
pub fn compute_file_risk_scores(
    commits: &[CommitInfo],
    now_epoch: u64,
    half_life_days: f64,
) -> HashMap<String, FileRiskScores>
```

Returns a map from file path → `FileRiskScores` containing:
- `hotspot: f64` — decay-weighted sum, **max-normalized** to `[0.0, 1.0]` (normalization
  happens inside this function before return)
- `fix_density: f64` — ratio of fix-weighted touches to total weighted touches

**Implementation notes:**
- Pre-classifies fix commits once via `is_fix_commit` before the hot loop (avoids
  repeated regex evaluation).
- Per-commit path deduplication is NOT done here (that is `compute_file_temporal_stats`'s
  concern). Within this function, each `commit.changed_files` entry is processed in
  order; duplicate appearances receive separate weight increments.
- Uses a borrow-first HashMap probe: checks `accum.get_mut(path_ref)` with a `&str`
  before calling `into_owned()`, reducing allocations to O(unique_files).

**Precondition**: `half_life_days` must be positive and finite (`assert!` in both this
function and `decay_weight`).

### `compute_file_temporal_stats`

```rust
pub fn compute_file_temporal_stats(
    commits: &[CommitInfo],
    now_epoch: u64,
) -> HashMap<String, FileTemporalStats>
```

Computes raw (non-decay-weighted) commit counts per file within 30-day and 90-day windows.
Also tracks `total_commits` and `fix_commits` (counts, not weighted).

**Important**: Per-commit deduplication via `dedup_changed_files` (private helper using
a reused `HashSet<String>` buffer) — a file appearing twice in one commit's `changed_files`
is counted once. Boundary semantics: `elapsed_days <= 30.0` and `<= 90.0` (inclusive).
Uses `saturating_add` for all counters.

### `decay_weight`

```rust
pub fn decay_weight(elapsed_days: f64, half_life_days: f64) -> f64
```

Pure function: `(-elapsed / half_life_days).exp().clamp(0.0, 1.0)`.

**Panics** when `half_life_days <= 0.0` or is not finite (enforced by `assert!`).
NaN `elapsed_days` is treated as `0.0` (present-moment weight = 1.0) — NaN sanitization
is explicit before the `exp()` call because `clamp()` alone does not sanitize NaN.

### `DEFAULT_HALF_LIFE_DAYS`

`pub const DEFAULT_HALF_LIFE_DAYS: f64 = 30.0` — exported from `scoring.rs` via the
`temporal` module re-exports. Use this when calling `compute_file_risk_scores` without
a domain-specific reason to override.

## `FileRiskScores` and `FileTemporalStats` Types

```rust
// In types.rs:
pub struct FileRiskScores {
    pub hotspot: f64,      // max-normalized decay-weighted frequency
    pub fix_density: f64,  // decay-weighted fix ratio
}

pub struct FileTemporalStats {
    pub changes_30d: u32,
    pub changes_90d: u32,
    pub total_commits: u32,
    pub fix_commits: u32,
}
```

`FileRiskScores` has exactly two fields. The raw count fields (`changes_30d`, etc.) live
in `FileTemporalStats` — a separate type returned by `compute_file_temporal_stats`.
Do not conflate the two: `FileRiskScores` is returned by `compute_file_risk_scores`;
`FileTemporalStats` is returned by `compute_file_temporal_stats`.

## SQLite Persistence Layer (`TemporalDb`)

### Database Location

`temporal.db` lives at `{cache_dir}/temporal.db` — not in the project's `.skim/`.
The temporal signals and the co-change data share this database file; the search
index (`search.db`) is separate.

### Schema

Four tables:

```sql
CREATE TABLE hotspot (
    file_path  TEXT PRIMARY KEY,
    score      REAL NOT NULL,
    changes_30d  INTEGER NOT NULL,
    changes_90d  INTEGER NOT NULL
);

CREATE TABLE risk (
    file_path    TEXT PRIMARY KEY,
    risk_score   REAL NOT NULL,
    total_commits INTEGER NOT NULL,
    fix_commits  INTEGER NOT NULL,
    fix_density  REAL NOT NULL
);

CREATE TABLE cochange (
    file_a  TEXT NOT NULL,
    file_b  TEXT NOT NULL,
    count   INTEGER NOT NULL,
    jaccard REAL    NOT NULL,
    PRIMARY KEY (file_a, file_b)
);

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

Performance indexes (added in schema **v2**):
```sql
CREATE INDEX IF NOT EXISTS idx_hotspot_score ON hotspot(score);
CREATE INDEX IF NOT EXISTS idx_risk_score    ON risk(risk_score);
-- idx_cochange_file_b only: PK (file_a, file_b) already covers file_a queries
CREATE INDEX IF NOT EXISTS idx_cochange_file_b ON cochange(file_b);
```

Note: there is no `idx_cochange_file_a` — the composite primary key `(file_a, file_b)`
already serves `file_a` prefix queries efficiently. The migration comment in `storage.rs`
explicitly documents this.

### Schema Migrations

Migrations are forward-only, driven by `PRAGMA user_version`:
- Each version gate: `if schema_version < N { apply_migration_N(); set_version(N); }`
- Databases at a higher version than the binary knows about return
  `SearchError::Database("database schema version N is newer than supported version 2; ...")`

Current version is **v2** (v1: tables, v2: performance indexes).

### `TemporalDb::open` Setup Steps

1. Opens the SQLite file (creates if absent)
2. Sets file permissions to `0o600` on Unix (owner-only)
3. Configures a 5-second busy timeout
4. Enables WAL journal mode; fails loud if WAL is not granted
5. Sets `PRAGMA synchronous=NORMAL`
6. Runs schema migrations

### Meta Table Constants

```rust
pub const META_LAST_UPDATED: &str = "last_updated";  // Unix timestamp written by sync
pub const META_GIT_HEAD: &str = "git_head";           // Commit SHA written by sync
```

Both re-exported via `rskim_search::temporal::storage::{META_GIT_HEAD, META_LAST_UPDATED}`
and at the crate root.

### `TemporalDb::sync`

```rust
pub fn sync(
    &self,
    hotspots: &[HotspotRow],
    risks: &[RiskRow],
    cochanges: &[CochangeRow],
    git_head: &str,
) -> Result<()>
```

Atomically replaces all three data tables in a single transaction: DELETE + batch INSERT
for hotspot, risk, and cochange. Also writes `git_head` to `META_GIT_HEAD` and a Unix
timestamp to `META_LAST_UPDATED` in the `meta` table. Returns `CapacityExceeded` if
any slice exceeds `MAX_ROWS_PER_TABLE = 500_000`.

### Per-File Lookup Methods

| Method | Description |
|--------|-------------|
| `hotspot_for_file(path)` | Single-file hotspot lookup — `Option<HotspotRow>` |
| `risk_for_file(path)` | Single-file risk lookup — `Option<RiskRow>` |
| `cochanges_for_file(path)` | All co-change pairs for a file — `Vec<CochangeRow>` |
| `get_meta(key)` | Retrieve a single meta value — `Option<String>` |

**`cochanges_for_file` details**: Uses `UNION ALL` of two indexed sub-queries (one on
`file_a`, one on `file_b`) rather than `OR` to avoid SQLite full-scan degradation at
large row counts. Applies `MIN_JACCARD_THRESHOLD = 0.10` filter (pairs below this are
noise) and `LIMIT 10000`. Results are sorted by Jaccard descending. The `UNION ALL`
(not `UNION`) is safe because the `file_a < file_b` canonical ordering guarantee means
no row can satisfy both arms.

### Bulk Query Methods (Index-Backed)

| Method | Description |
|--------|-------------|
| `top_hotspots(limit)` | Top N by `score DESC` |
| `top_risks(limit)` | Top N by `risk_score DESC` |
| `top_coldspots(limit)` | Bottom N by `score ASC` (lowest hotspot = coldest) |

All three silently clamp `limit` to `MAX_ROWS_PER_TABLE` before binding to SQLite.

### Mutation Methods

- `store_hotspots`, `store_risks`, `store_cochanges` — DELETE + batch INSERT for their
  respective table in a single transaction. Each checks `rows.len() <= MAX_ROWS_PER_TABLE`
  first. Use `sync` for atomic multi-table updates.
- `load_hotspots`, `load_risks`, `load_cochanges` — return full table contents (up to
  `MAX_ROWS_PER_TABLE + 1` rows with overflow check).
- `set_meta(key, value)` — `INSERT OR REPLACE` a single meta key.

### Capacity Constant

`MAX_ROWS_PER_TABLE = 500_000` — defined in `storage_ops.rs`. Applies to all store,
load, and sync operations. Returns `SearchError::CapacityExceeded` when exceeded.

## WAL Mode

`TemporalDb::open` sets `PRAGMA journal_mode=WAL` and `PRAGMA synchronous=NORMAL` on each
connection. WAL mode allows concurrent readers while a write is in progress. `TemporalDb`
is **not `Sync`** — each thread must open its own connection.

## Key Files

- `crates/rskim-search/src/temporal/scoring.rs` — `decay_weight`, `compute_file_risk_scores`,
  `compute_file_temporal_stats`; `FileRiskScores`, `FileTemporalStats` structs; `DEFAULT_HALF_LIFE_DAYS`
- `crates/rskim-search/src/temporal/scoring_tests.rs` — unit tests for decay formula and file
  score aggregation
- `crates/rskim-search/src/temporal/mod.rs` — public re-exports; `is_fix_commit` using
  `FIX_REGEX` (word-boundary regex with LazyLock)
- `crates/rskim-search/src/temporal/git_parser.rs` — `GixSource` (gix-based git history reader
  producing `HistoryResult`); `MAX_COMMITS = 100_000` safety cap
- `crates/rskim-search/src/temporal/storage.rs` — `TemporalDb` struct, `open`, `schema_version`,
  migration runner; `META_GIT_HEAD` and `META_LAST_UPDATED` public constants; `db_err` helper
- `crates/rskim-search/src/temporal/storage_ops.rs` — all query and mutation methods for
  `TemporalDb`; `MAX_ROWS_PER_TABLE = 500_000`; `MIN_JACCARD_THRESHOLD = 0.10`
- `crates/rskim-search/src/temporal/storage_types.rs` — `HotspotRow`, `RiskRow`, `CochangeRow`
  row structs
- `crates/rskim-search/src/temporal/storage_tests.rs` — integration tests against in-memory SQLite
- `crates/rskim-search/src/temporal/storage_perf_tests.rs` — performance benchmarks for top-N
  and per-file queries
- `crates/rskim-search/src/types.rs` — `CommitInfo`, `FileChangeInfo`, `HistoryResult`,
  `FileRiskScores`, `FileTemporalStats`, `FileId`, `SearchError`
- `crates/rskim-search/src/lib.rs` — re-exports `TemporalDb`, `HotspotRow`, `RiskRow`,
  `CochangeRow` as public crate API

## Anti-Patterns

- **Using the raw return from `compute_file_risk_scores` as un-normalized**: the returned
  `hotspot` field IS already max-normalized inside the function. Do not normalize again.
  (Old gotcha: callers used to need to normalize manually; the function now does it.)
- **Confusing `FileRiskScores` with `FileTemporalStats`**: `FileRiskScores` has only two
  fields (`hotspot` and `fix_density`) and is returned by `compute_file_risk_scores`.
  `FileTemporalStats` has `changes_30d`, `changes_90d`, `total_commits`, `fix_commits`
  and is returned by `compute_file_temporal_stats`. They serve different use cases.
- **Opening `temporal.db` as `Sync`**: `TemporalDb` is `!Sync`. For concurrent access,
  open multiple instances against the same WAL-mode database file.
- **Using individual `store_*` methods for a full signal refresh**: use `sync` to
  atomically replace all three tables in one transaction.
- **Assuming `is_fix_commit` matches substrings**: the regex uses word boundaries (`\b`).
  "prefix_fix" does NOT match. Only the exact keywords `fix`, `bug`, `hotfix`, `patch`,
  `revert` match (case-insensitive).
- **Adding `idx_cochange_file_a` to the schema**: it is redundant — the PRIMARY KEY
  `(file_a, file_b)` already serves `file_a = ?` queries. Adding it would waste space.
- **Calling `cochanges_for_file` and expecting all pairs regardless of Jaccard**: the
  method filters by `MIN_JACCARD_THRESHOLD = 0.10` and caps at 10,000 rows. Pairs below
  0.10 Jaccard are excluded.
- **Treating `half_life_days` as a strict half-life**: the formula is `exp(-d/h)`, not
  `2^(-d/h)`. After one period, weight is `1/e ≈ 0.368`, not `0.5`.

## Gotchas

- `is_fix_commit` uses `FIX_REGEX` (a `LazyLock<Regex>`) compiled once. The regex
  panics at startup if the pattern is invalid — but the pattern is hardcoded and always
  valid, so `expect` is appropriate here.
- `compute_file_risk_scores` does NOT deduplicate files within a commit. That is
  `compute_file_temporal_stats`'s responsibility (via `dedup_changed_files`). In the
  risk scorer, duplicate file appearances within a commit each receive their own weight.
- Schema version mismatch (future version) returns `SearchError::Database(...)`, not
  `IndexCorrupted`. Delete `temporal.db` to reset.
- `top_coldspots` returns rows sorted `score ASC` — files with score 0.0 come first.
  It is only semantically useful if the database contains entries for all project files.
- The `meta` table stores arbitrary key-value strings. Only `META_GIT_HEAD` and
  `META_LAST_UPDATED` are reserved by `TemporalDb::sync`. Future callers may add their
  own keys via `set_meta` without schema migration.
- `changes_30d` and `changes_90d` in `HotspotRow` are raw counts — they do NOT correlate
  with `score`. A file may have `score = 0.9` but `changes_30d = 0` if all its commits
  were just outside the 30-day window.
- `CochangeRow.count` is `u32` in `storage_types.rs`. SQLite stores it as INTEGER but
  rusqlite maps it to `u32` via `row.get(2)?`. No `i64` cast is needed.
- `TemporalDb::open` restricts file permissions to `0o600` on Unix silently (emits a
  warning to stderr if `set_permissions` fails, but does not return an error).
- `cochanges_for_file` uses `UNION ALL` with two parameters (`?1` for path, `?2` for
  `MIN_JACCARD_THRESHOLD`) bound via `rusqlite::params![path, MIN_JACCARD_THRESHOLD]`.

## Related

- `crates/rskim-search/src/cochange/` — sibling module; both consume `HistoryResult`;
  co-change data shares the same `temporal.db` file via the `cochange` table
- `crates/rskim-search/src/types.rs` — `HistoryResult`, `CommitInfo`, `FileId`,
  `SearchError`, `FileRiskScores`, `FileTemporalStats`
- `crates/rskim-search/src/io_util.rs` — `atomic_write` shared helper; used by the
  cochange builder and the AST index store builder (not by `TemporalDb` which uses
  rusqlite transactions instead)
- Temporal CLI flags: `skim search --hot`, `--cold`, `--risky`, `--blast-radius FILE` —
  all backed by `TemporalDb` queries at search time
