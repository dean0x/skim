//! Temporal database schema and migrations.
//!
//! Mirrors the `crates/rskim/src/analytics/schema.rs` pattern:
//! - `PRAGMA user_version` for schema version tracking
//! - Full DDL in a single `apply_migration_v1()` function
//! - Adding a column = new migration function + version bump

/// Current schema version. Bump on any DDL changes.
pub const SCHEMA_VERSION: u32 = 1;

/// Apply v0 → v1 migration (initial schema).
///
/// Creates all tables and indexes in a single batch. Must be called inside a
/// transaction; the caller is responsible for committing after this returns.
pub fn apply_migration_v1(tx: &rusqlite::Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute_batch(V1_SCHEMA_SQL)
}

/// DDL for schema version 1.
///
/// Tables:
/// - `file_paths`: Path intern table. Every file gets a `temporal_file_id`.
/// - `cochange`: Symmetric pairwise co-change scores (file_a < file_b enforced by CHECK).
/// - `hotspot`: 30/90-day commit activity + normalized score per file.
/// - `risk`: Fix-commit density + normalized score per file.
/// - `meta`: Key-value metadata (schema_version, last_commit_hash, etc.).
const V1_SCHEMA_SQL: &str = r#"
    CREATE TABLE file_paths (
        temporal_file_id INTEGER PRIMARY KEY AUTOINCREMENT,
        path TEXT NOT NULL UNIQUE
    );
    CREATE INDEX idx_file_paths_path ON file_paths(path);

    CREATE TABLE cochange (
        file_a INTEGER NOT NULL REFERENCES file_paths(temporal_file_id) ON DELETE CASCADE,
        file_b INTEGER NOT NULL REFERENCES file_paths(temporal_file_id) ON DELETE CASCADE,
        co_occurrences INTEGER NOT NULL,
        jaccard REAL NOT NULL,
        PRIMARY KEY (file_a, file_b),
        CHECK (file_a < file_b)
    );
    CREATE INDEX idx_cochange_a ON cochange(file_a, jaccard DESC);
    CREATE INDEX idx_cochange_b ON cochange(file_b, jaccard DESC);

    CREATE TABLE hotspot (
        temporal_file_id INTEGER PRIMARY KEY REFERENCES file_paths(temporal_file_id) ON DELETE CASCADE,
        commit_count_30d INTEGER NOT NULL,
        commit_count_90d INTEGER NOT NULL,
        score REAL NOT NULL
    );
    CREATE INDEX idx_hotspot_score ON hotspot(score DESC);

    CREATE TABLE risk (
        temporal_file_id INTEGER PRIMARY KEY REFERENCES file_paths(temporal_file_id) ON DELETE CASCADE,
        total_commits INTEGER NOT NULL,
        fix_commits INTEGER NOT NULL,
        fix_density REAL NOT NULL,
        score REAL NOT NULL
    );
    CREATE INDEX idx_risk_score ON risk(score DESC);

    CREATE TABLE meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
"#;
