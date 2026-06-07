---
feature: temporal-scoring
name: Temporal Risk Scoring
description: "Use when adding temporal ranking signals, modifying decay parameters, working with the SQLite temporal persistence layer, or debugging hotspot/risk score computation. Keywords: temporal, hotspot, risk, fix-density, decay, half-life, TemporalDb, HotspotRow, RiskRow, hotspot_for_file, top_hotspots, top_risks, scoring, FileRiskScores, FileTemporalStats, storage_ops, WAL, schema migrations."
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
updated: 2026-06-06
version: 2
---

# Temporal Risk Scoring

## Overview

The temporal module computes per-file risk signals from git history: hotspot scores (how frequently a file has changed, weighted by recency) and bug-fix density scores (what fraction of commits touching a file were fix commits). These signals are persisted to a WAL-mode SQLite database (`temporal.db`) for fast bulk-loading by ranking pipelines and `skim search` temporal flags (`--hot`, `--cold`, `--risky`, `--blast-radius`).

The module is separate from the co-change matrix (`crates/rskim-search/src/cochange/`) — both consume `HistoryResult`, but produce different signal types and use different storage formats.

## Signal Types

### Hotspot Score

A hotspot score quantifies how actively a file is being modified, with recent changes weighted more heavily than old ones. The score is computed per file, then max-normalized across all files to `[0.0, 1.0]`.

**Formula:**
```
hotspot(file) = Σ_commits decay_weight(elapsed_days, half_life_days)
decay_weight(d, h) = exp(-d / h)    # natural exponential decay (e-folding)
```

- `elapsed_days`: days between the commit timestamp and `now_epoch`
- `half_life_days`: e-folding time in days. Despite the name (inherited from the heatmap module convention), the formula is `exp(-d/h)` not `2^(-d/h)` — the value reaches `1/e ≈ 0.368` (not 0.5) after one period. See the naming note in `DEFAULT_HALF_LIFE_DAYS`.
- Sum over all commits touching the file; recent commits contribute ~1.0, old commits ~0.0
- `DEFAULT_HALF_LIFE_DAYS = 30.0` (commits from 30 days ago contribute ~37%)

The max-normalization means the hottest file always scores 1.0; others are relative fractions. **Max-normalization is performed inside `compute_file_risk_scores` itself** — the returned map already contains normalized scores in `[0.0, 1.0]`.

`HotspotRow` persists:
- `file_path`: repo-root-relative path
- `score`: decay-weighted, max-normalized to `[0.0, 1.0]`
- `changes_30d`: raw commit count in last 30 days
- `changes_90d`: raw commit count in last 90 days

### Bug-Fix Density (Risk Score)

Risk score quantifies how often a file has been part of a fix commit — commits whose message contains fix indicators (see `is_fix_commit` below). This is a static signal: it does not decay over time.

**Formula:**
```
risk_score(file) = fix_commits(file) / total_commits(file)
```

Clamped to `[0.0, 1.0]` after division. Files with zero commits have `risk_score = 0.0`.

`RiskRow` persists:
- `file_path`: repo-root-relative path
- `risk_score`: fix_commits / total_commits, in `[0.0, 1.0]`
- `total_commits`: total commits touching this file
- `fix_commits`: commits classified as fix commits
- `fix_density`: same as `risk_score` (alias stored for query convenience)

### Fix Commit Detection

`is_fix_commit(message: &str) -> bool` (in `mod.rs`) uses a `LazyLock<Regex>` compiled once at first call:

```rust
static FIX_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(fix|bug|hotfix|patch|revert)\b").expect("valid regex"));
```

- Matches (case-insensitive, **word-boundary anchored**): `fix`, `bug`, `hotfix`, `patch`, `revert`
- Word-boundary anchoring means "prefix_word" does NOT match (unlike substring search)
- Pre-classified once per commit via `fix_flags: Vec<bool>` to avoid repeated regex evaluation in the hot loop

Keywords are: `fix`, `bug`, `hotfix`, `patch`, `revert` — note `repair`, `correct`, `resolve` are NOT in the regex.

## Core Entry Points

### `compute_file_risk_scores`

```rust
pub fn compute_file_risk_scores(
    commits: &[CommitInfo],
    now_epoch: u64,
    half_life_days: f64,
) -> HashMap<String, FileRiskScores>
```

Returns a map from file path → `FileRiskScores` with hotspot score **already max-normalized** to `[0.0, 1.0]`. The function performs normalization internally in a second pass over the accumulator before returning. Callers do not need to normalize; they can write `FileRiskScores.hotspot_score` directly into `HotspotRow.score`.

Fields returned:
- `hotspot_score: f64` — max-normalized decay-weighted score in `[0.0, 1.0]`
- `fix_density: f64` — `weighted_fix_total / weighted_total`
- `total_commits: u32`
- `fix_commits: u32`
- `changes_30d: u32` (populated by `compute_file_temporal_stats`, not this function)
- `changes_90d: u32` (populated by `compute_file_temporal_stats`, not this function)

### `compute_file_temporal_stats`

```rust
pub fn compute_file_temporal_stats(
    commits: &[CommitInfo],
    now_epoch: u64,
) -> HashMap<String, FileTemporalStats>
```

A lighter variant that computes raw commit counts (`changes_30d`, `changes_90d`, `total_commits`) without decay weighting. Used by callers that need raw frequency counts, not weighted scores.

### `decay_weight`

```rust
pub fn decay_weight(elapsed_days: f64, half_life_days: f64) -> f64
```

Pure function: `(-elapsed_days / half_life_days).exp().clamp(0.0, 1.0)`. Future commits (negative `elapsed_days`) are treated as elapsed = 0.0 (weight = 1.0). NaN `elapsed_days` is also clamped to 0.0 before computing — `.clamp()` alone does not sanitize NaN. Panics if `half_life_days <= 0.0` or is not finite.

## SQLite Persistence Layer (`TemporalDb`)

### Database Location

`temporal.db` lives at `{cache_dir}/temporal.db` — not in the project's `.skim/search.db`. The temporal signals and the co-change data share this database file; the search index (`search.db`) is separate and in the project root.

### Schema

Three tables:

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

Performance indexes (added in schema v2):
```sql
CREATE INDEX IF NOT EXISTS idx_hotspot_score ON hotspot(score DESC);
CREATE INDEX IF NOT EXISTS idx_risk_score    ON risk(risk_score DESC);
CREATE INDEX IF NOT EXISTS idx_cochange_file_a ON cochange(file_a);
CREATE INDEX IF NOT EXISTS idx_cochange_file_b ON cochange(file_b);
```

These indexes make `top_hotspots(N)`, `top_risks(N)`, `top_coldspots(N)` efficient without full table scans.

### Schema Migrations

Migrations are forward-only, driven by `PRAGMA user_version`:
- Each version gate runs `if schema_version < N { apply_migration_N(); set_version(N); }`
- Databases at a higher version than the binary knows about return `SearchError::Database("unsupported schema version N")`

Current version is **v2** (v1: tables, v2: performance indexes).

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

Atomically replaces all three tables in a single transaction: DELETE + batch INSERT for hotspot, risk, and cochange. Also writes `git_head` to the `meta` table. This is the preferred way to update all signals together — it ensures no partial state is visible.

### Point Queries

| Method | Description |
|--------|-------------|
| `hotspot_for_file(path)` | Single-file hotspot lookup — `Option<HotspotRow>` |
| `risk_for_file(path)` | Single-file risk lookup — `Option<RiskRow>` |
| `cochanges_for_file(path)` | All co-change pairs for a file — `Vec<CochangeRow>` |

### Bulk Queries (Index-Backed)

| Method | Description |
|--------|-------------|
| `top_hotspots(limit)` | Top N by `score DESC` |
| `top_risks(limit)` | Top N by `risk_score DESC` |
| `top_coldspots(limit)` | Top N by `score ASC` (lowest hotspot = coldest) |

### Load / Store (Non-atomic)

`store_hotspots`, `store_risks`, `store_cochanges` each do DELETE + batch INSERT for their respective table only. Use `sync` for atomic multi-table updates; use the individual store methods only for single-table updates.

`load_hotspots`, `load_risks`, `load_cochanges` return full table contents as `Vec<Row>` — no limit parameter.

## WAL Mode

`TemporalDb::open` sets `PRAGMA journal_mode=WAL` and `PRAGMA synchronous=NORMAL` on each connection. WAL mode allows concurrent readers while a write is in progress. `TemporalDb` is **not `Sync`** — each thread must open its own connection.

## Key Files

- `crates/rskim-search/src/temporal/scoring.rs` — `decay_weight`, `compute_file_risk_scores`, `compute_file_temporal_stats`; `FileRiskScores`, `FileTemporalStats` structs
- `crates/rskim-search/src/temporal/scoring_tests.rs` — unit tests for decay formula and file score aggregation
- `crates/rskim-search/src/temporal/mod.rs` — public re-exports; `is_fix_commit` keyword classifier
- `crates/rskim-search/src/temporal/git_parser.rs` — `GixSource` (gix-based git history reader producing `HistoryResult`)
- `crates/rskim-search/src/temporal/storage.rs` — `TemporalDb` struct, `open`, `schema_version`, migration runner
- `crates/rskim-search/src/temporal/storage_ops.rs` — all query and mutation methods for `TemporalDb` (impl block continues from storage.rs)
- `crates/rskim-search/src/temporal/storage_types.rs` — `HotspotRow`, `RiskRow`, `CochangeRow` row structs
- `crates/rskim-search/src/temporal/storage_tests.rs` — integration tests against in-memory SQLite
- `crates/rskim-search/src/temporal/storage_perf_tests.rs` — performance benchmarks for top-N and per-file queries
- `crates/rskim-search/src/types.rs` — `CommitInfo`, `FileChangeInfo`, `HistoryResult`, `FileId`, `SearchError`
- `crates/rskim-search/src/lib.rs` — re-exports `TemporalDb`, `HotspotRow`, `RiskRow`, `CochangeRow` as public crate API

## Anti-Patterns

- **Assuming `compute_file_risk_scores` returns raw unnormalized scores**: the function performs max-normalization internally. The returned `hotspot_score` is already in `[0.0, 1.0]` — do not apply a second normalization pass.
- **Opening `temporal.db` as `Sync`**: `TemporalDb` is `!Sync`. For concurrent access, open multiple instances against the same WAL-mode database file.
- **Using individual `store_*` methods for a full signal refresh**: use `sync` to atomically replace all three tables in one transaction. Individual stores leave the database in a partially-updated state.
- **Treating `top_coldspots` as semantically meaningful on a sparse database**: cold spots (low hotspot score) are only meaningful if the database contains data for all project files. If only hot files were indexed, `top_coldspots` returns nothing useful.

## Gotchas

- `compute_file_risk_scores` deduplicates changed files within each commit via `dedup_changed_files` (private helper using a reused `HashSet<String>` buffer). Without this, a commit with renames would count the same file twice. Note `compute_file_temporal_stats` also uses its own dedup buffer for the same reason.
- `is_fix_commit` uses word-boundary anchoring — "prefixfix" does NOT match. The keywords are exactly: `fix`, `bug`, `hotfix`, `patch`, `revert`. The fix pre-classification is done once per commit (`fix_flags: Vec<bool>`) and reused across all files in that commit.
- Schema version mismatch returns `SearchError::Database(...)`, not `IndexCorrupted`. Rebuild by deleting `temporal.db`.
- `top_coldspots` returns rows sorted by `score ASC` — not the inverse of `top_hotspots` sort, but an explicit ascending sort. Files with score 0.0 come first.
- The `meta` table stores arbitrary key-value strings; `git_head` is the only key written by `TemporalDb::sync`. Future callers may add their own keys without schema migration.
- `changes_30d` and `changes_90d` are raw counts — they are NOT decay-weighted and do NOT correlate with `score`. A file may have `score = 0.9` but `changes_30d = 0` if all its commits were just outside the 30-day window.

## Related

- `crates/rskim-search/src/cochange/` — sibling module; both consume `HistoryResult`; co-change data shares the same `temporal.db` file via the `cochange` table
- `crates/rskim-search/src/types.rs` — `HistoryResult`, `CommitInfo`, `FileId`, `SearchError`
- Temporal CLI flags: `skim search --hot`, `--cold`, `--risky`, `--blast-radius FILE` — all backed by `TemporalDb` queries at search time
