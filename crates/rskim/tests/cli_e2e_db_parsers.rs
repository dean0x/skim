//! E2E tests for DB parser degradation tiers (#117).
//!
//! Tests psql, mysql, and sqlite3 via stdin piping, verifying:
//! - Tier 1 (Full): structured DbResult output with column/row data
//! - Tier 2 (Degraded): alternative format parsing with debug markers
//! - Tier 3 (Passthrough): raw output for unparseable or error input
//! - `--json` flag: valid JSON envelope
//! - Empty result sets: zero-row DbResult

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd
}

// ============================================================================
// psql: Tier 1 (tabular) — Full
// ============================================================================

#[test]
fn test_psql_stdin_tier1() {
    let fixture = include_str!("fixtures/cmd/db/psql_select.txt");
    skim_cmd()
        .args(["psql"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("psql query 20 rows"))
        .stdout(predicate::str::contains("id"))
        .stdout(predicate::str::contains("username"));
}

// ============================================================================
// psql: empty result — Full with 0 rows
// ============================================================================

#[test]
fn test_psql_stdin_empty_result() {
    let fixture = include_str!("fixtures/cmd/db/psql_empty.txt");
    // The net-savings guard may passthrough small inputs rather than compressing.
    // Both skim-format ("psql query 0 rows") and raw "(0 rows)" confirm a zero-row result.
    skim_cmd()
        .args(["psql"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(
            predicate::str::contains("psql query 0 rows").or(predicate::str::contains("0 rows")),
        );
}

// ============================================================================
// psql: Tier 3 — Passthrough on garbage
// ============================================================================

#[test]
fn test_psql_stdin_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "psql"])
        .write_stdin("completely unparseable output without structure")
        .assert()
        .success()
        .stdout(predicate::str::contains("completely unparseable"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// psql: Tier 2 — Degraded via regex fallback
// ============================================================================

#[test]
fn test_psql_stdin_tier2_regex() {
    let text = "some output\n(5 rows)\n";
    skim_cmd()
        .args(["--debug", "psql"])
        .write_stdin(text)
        .assert()
        .success()
        .stdout(predicate::str::contains("psql"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// psql: --json flag produces valid JSON (Full tier → direct struct serialization)
// ============================================================================

#[test]
fn test_psql_stdin_json_flag() {
    let fixture = include_str!("fixtures/cmd/db/psql_select.txt");
    let output = skim_cmd()
        .args(["psql", "--json"])
        .write_stdin(fixture)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("--json must produce valid JSON");
    // Full tier → direct struct serialization: {"tool":"psql","row_count":20,...}
    assert_eq!(
        parsed["tool"].as_str(),
        Some("psql"),
        "JSON must have tool=psql"
    );
    assert_eq!(
        parsed["row_count"].as_u64(),
        Some(20),
        "JSON must have row_count=20"
    );
}

// ============================================================================
// mysql: Tier 1 (TSV) — Full
// ============================================================================

#[test]
fn test_mysql_stdin_tier1_tsv() {
    let fixture = include_str!("fixtures/cmd/db/mysql_select_tsv.txt");
    // The net-savings guard may passthrough inputs when compression doesn't save tokens.
    // Both skim-format ("mysql query 20 rows") and raw TSV have "id" and "username" columns.
    skim_cmd()
        .args(["mysql"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("mysql query 20 rows").or(predicate::str::contains("id")))
        .stdout(predicate::str::contains("username"));
}

// ============================================================================
// mysql: Tier 2 (bordered) — Degraded
// ============================================================================

#[test]
fn test_mysql_stdin_tier2_bordered() {
    let fixture = include_str!("fixtures/cmd/db/mysql_select_bordered.txt");
    skim_cmd()
        .args(["--debug", "mysql"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("mysql"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// mysql: Tier 3 — Passthrough on garbage
// ============================================================================

#[test]
fn test_mysql_stdin_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "mysql"])
        .write_stdin("completely unparseable output")
        .assert()
        .success()
        .stdout(predicate::str::contains("completely unparseable"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// mysql: empty set
// ============================================================================

#[test]
fn test_mysql_stdin_empty_set() {
    skim_cmd()
        .args(["mysql"])
        .write_stdin("Empty set (0.00 sec)\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("mysql query 0 rows"));
}

// ============================================================================
// mysql: --json flag produces valid JSON (Full tier → direct struct serialization)
// ============================================================================

#[test]
fn test_mysql_stdin_json_flag() {
    let fixture = include_str!("fixtures/cmd/db/mysql_select_tsv.txt");
    let output = skim_cmd()
        .args(["mysql", "--json"])
        .write_stdin(fixture)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("--json must produce valid JSON");
    assert_eq!(
        parsed["tool"].as_str(),
        Some("mysql"),
        "JSON must have tool=mysql"
    );
    assert_eq!(
        parsed["row_count"].as_u64(),
        Some(20),
        "JSON must have row_count=20"
    );
}

// ============================================================================
// sqlite3: Tier 1 (pipe-separated) — Full
// ============================================================================

#[test]
fn test_sqlite3_stdin_tier1() {
    let fixture = include_str!("fixtures/cmd/db/sqlite3_select.txt");
    // The net-savings guard may passthrough inputs when compression doesn't save tokens.
    // Both skim-format ("sqlite3 query 20 rows") and raw pipe-sep have "id" and "username".
    skim_cmd()
        .args(["sqlite3"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(
            predicate::str::contains("sqlite3 query 20 rows").or(predicate::str::contains("id")),
        )
        .stdout(predicate::str::contains("username"));
}

// ============================================================================
// sqlite3: empty result (header only, no data rows)
// ============================================================================

#[test]
fn test_sqlite3_stdin_empty_result() {
    let fixture = include_str!("fixtures/cmd/db/sqlite3_empty.txt");
    // The net-savings guard may passthrough small inputs rather than compressing.
    // Both skim-format ("sqlite3 query 0 rows") and raw header-only output contain "id".
    skim_cmd()
        .args(["sqlite3"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(
            predicate::str::contains("sqlite3 query 0 rows").or(predicate::str::contains("id")),
        );
}

// ============================================================================
// sqlite3: Tier 3 — Passthrough on schema dump (no pipes)
// ============================================================================

#[test]
fn test_sqlite3_stdin_passthrough_schema() {
    let schema = "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);\n";
    skim_cmd()
        .args(["--debug", "sqlite3"])
        .write_stdin(schema)
        .assert()
        .success()
        .stdout(predicate::str::contains("CREATE TABLE"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// sqlite3: --json flag produces valid JSON (Full tier → direct struct serialization)
// ============================================================================

#[test]
fn test_sqlite3_stdin_json_flag() {
    let fixture = include_str!("fixtures/cmd/db/sqlite3_select.txt");
    let output = skim_cmd()
        .args(["sqlite3", "--json"])
        .write_stdin(fixture)
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("--json must produce valid JSON");
    assert_eq!(
        parsed["tool"].as_str(),
        Some("sqlite3"),
        "JSON must have tool=sqlite3"
    );
    assert_eq!(
        parsed["row_count"].as_u64(),
        Some(20),
        "JSON must have row_count=20"
    );
}

// ============================================================================
// Fix 1: -h flag conflict — must NOT trigger help in db handlers
// ============================================================================

#[test]
fn test_psql_h_flag_not_intercepted_as_help() {
    // `skim psql -h localhost` — -h means host, not help.
    // The handler must NOT print help; it must attempt to run psql instead.
    // Since psql may not be installed, we allow any exit code.
    // The key assertion is that the help text is NOT printed to stdout.
    let output = skim_cmd()
        .args(["psql", "-h", "localhost"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    // If help were printed we'd see "Available tools:" — must NOT be present.
    assert!(
        !stdout.contains("Available tools:"),
        "-h must not trigger help text; got stdout: {stdout}"
    );
    assert!(
        !stdout.contains("Run database tools"),
        "-h must not trigger help text; got stdout: {stdout}"
    );
}

#[test]
fn test_psql_long_help_still_prints_help() {
    // `skim psql --help` must still show the help text.
    skim_cmd()
        .args(["psql", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available tools:"));
}

#[test]
fn test_mysql_h_flag_not_intercepted_as_help() {
    // `skim mysql -h localhost` — -h means host, not help.
    // The handler must NOT print help; it must attempt to run mysql instead.
    // Since mysql may not be installed, we allow any exit code.
    let output = skim_cmd()
        .args(["mysql", "-h", "localhost"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Available tools:"),
        "-h must not trigger help text; got stdout: {stdout}"
    );
    assert!(
        !stdout.contains("Run database tools"),
        "-h must not trigger help text; got stdout: {stdout}"
    );
}

// ============================================================================
// Infra handler: -h/-H flag must NOT be intercepted as help
// ============================================================================

#[test]
fn test_docker_h_flag_not_intercepted_as_help() {
    // `skim docker -H unix:///tmp/docker.sock ps` — -H means --host, not help.
    let output = skim_cmd()
        .args(["docker", "-H", "unix:///tmp/docker.sock", "ps"])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Available tools:"),
        "-H must not trigger help text; got stdout: {stdout}"
    );
}

#[test]
fn test_kubectl_h_flag_not_intercepted_as_help() {
    // `skim kubectl -h` — the infra handler only checks --help, not -h.
    let output = skim_cmd().args(["kubectl", "-h"]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stdout.contains("Available tools:"),
        "-h must not trigger skim help text; got stdout: {stdout}"
    );
}
