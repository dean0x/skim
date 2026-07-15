//! Shared test harness for rskim integration tests.
//!
//! ## Design
//!
//! All helpers in this module are `pub` so each integration test binary (each
//! `tests/*.rs` file is compiled as a separate crate) can use them after adding
//! `mod common;` at the top of the file.
//!
//! ## Analytics isolation
//!
//! The safe default is **analytics OFF**: `skim()` sets `SKIM_DISABLE_ANALYTICS=1`
//! so test invocations never write to the developer's real `~/.cache/skim/analytics.db`.
//!
//! Tests that assert on recorded analytics data must use `skim_with_analytics(db)`
//! instead — it points at an isolated temp DB and re-enables recording.
//!
//! ## Dead-code suppression
//!
//! Each test binary compiles `common` independently. A binary that does not call
//! every helper will trigger an "unused" warning without this attribute.
#![allow(dead_code)]

/// Build a `skim` command with analytics disabled — the safe default.
///
/// Sets:
/// - `SKIM_DISABLE_ANALYTICS=1` — no rows written to any analytics DB.
/// - `NO_COLOR=1` — deterministic, color-free output for assertions.
///
/// Callers may chain additional `.env(...)` / `.env_remove(...)` / `.args(...)`
/// calls. Per-test env overrides applied after this call take precedence.
pub fn skim() -> assert_cmd::Command {
    let mut c = assert_cmd::Command::cargo_bin("skim").unwrap();
    c.env("SKIM_DISABLE_ANALYTICS", "1").env("NO_COLOR", "1");
    c
}

/// Return the path to the skim binary built by cargo.
///
/// Use this when you need a `std::process::Command` rather than an
/// `assert_cmd::Command` (e.g. for `argv[0]` override via `CommandExt::arg0`).
pub fn skim_bin() -> std::path::PathBuf {
    assert_cmd::cargo::cargo_bin("skim")
}

/// Build a `skim` command pointed at an isolated analytics DB, with recording
/// **enabled**.
///
/// Use this (and only this) for tests that assert on rows written to the
/// analytics database. Pass a `TempDir`-backed path so the DB is cleaned up
/// after the test.
///
/// Sets:
/// - `SKIM_ANALYTICS_DB=<db>` — all writes go to the isolated file.
/// - `NO_COLOR=1` — deterministic output.
///
/// `SKIM_DISABLE_ANALYTICS` is explicitly removed so recording is active.
pub fn skim_with_analytics(db: &std::path::Path) -> assert_cmd::Command {
    let mut c = assert_cmd::Command::cargo_bin("skim").unwrap();
    c.env("SKIM_ANALYTICS_DB", db)
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("NO_COLOR", "1");
    c
}
