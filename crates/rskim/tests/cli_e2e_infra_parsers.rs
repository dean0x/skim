//! E2E tests for DNS tool parsers: dig and nslookup (#168).
//!
//! Tests pipe fixture output through the full binary (stdin → stdout) and assert
//! on compressed output content and tier markers.
//!
//! # Stdin mode
//!
//! When no args are provided AND stdin is not a terminal (piped mode), infra
//! tools read from stdin instead of executing the real binary. This is the
//! same mechanism used by all other infra parser E2E tests.
//!
//! Tier behavior reference:
//! - Full: no stderr tier markers (even with --debug, Full emits nothing)
//! - Degraded: "[skim:warning] ..." on stderr (only with --debug)
//! - Passthrough: "[skim:notice] ..." on stderr (only with --debug)

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

// ============================================================================
// dig: Tier 1 (Full parse)
// ============================================================================

#[test]
fn test_dig_tier1_a_record_compressed() {
    let fixture = include_str!("fixtures/cmd/infra/dig_a_record.txt");
    skim_cmd()
        .args(["dig"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("dig query"));
}

#[test]
fn test_dig_tier1_nxdomain_contains_error_status() {
    let fixture = include_str!("fixtures/cmd/infra/dig_nxdomain.txt");
    skim_cmd()
        .args(["dig"])
        .write_stdin(fixture)
        .assert()
        .stdout(predicate::str::contains("NXDOMAIN"));
}

#[test]
fn test_dig_tier1_mx_record_compressed() {
    let fixture = include_str!("fixtures/cmd/infra/dig_mx_record.txt");
    skim_cmd()
        .args(["dig"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("dig query"));
}

// ============================================================================
// dig: Tier 3 (Passthrough for +short output)
// ============================================================================

#[test]
fn test_dig_short_output_passthrough() {
    // dig +short produces bare IPs with no headers — no parse possible
    let fixture = include_str!("fixtures/cmd/infra/dig_short.txt");
    skim_cmd()
        .args(["--debug", "dig"])
        .write_stdin(fixture)
        .assert()
        // Passthrough: raw content preserved on stdout
        .stdout(predicate::str::contains("172.66").or(predicate::str::contains("104.20")))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// dig: JSON output mode
// ============================================================================

#[test]
fn test_dig_json_output_valid() {
    let fixture = include_str!("fixtures/cmd/infra/dig_a_record.txt");
    skim_cmd()
        .args(["dig", "--json"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::is_match(r#""tool"\s*:\s*"dig""#).unwrap())
        .stdout(predicate::str::is_match(r#""operation"\s*:\s*"query""#).unwrap())
        .stdout(predicate::str::is_match(r#""items""#).unwrap());
}

// ============================================================================
// nslookup: Tier 1 (Full parse)
// ============================================================================

#[test]
fn test_nslookup_tier1_a_record_compressed() {
    let fixture = include_str!("fixtures/cmd/infra/nslookup_a_record.txt");
    skim_cmd()
        .args(["nslookup"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("nslookup query"));
}

#[test]
fn test_nslookup_tier1_nxdomain_contains_error() {
    let fixture = include_str!("fixtures/cmd/infra/nslookup_nxdomain.txt");
    skim_cmd()
        .args(["nslookup"])
        .write_stdin(fixture)
        .assert()
        .stdout(predicate::str::contains("NXDOMAIN"));
}

#[test]
fn test_nslookup_tier1_mx_record_compressed() {
    let fixture = include_str!("fixtures/cmd/infra/nslookup_mx_record.txt");
    skim_cmd()
        .args(["nslookup"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("nslookup query"));
}

// ============================================================================
// nslookup: Tier 3 (Passthrough for garbage)
// ============================================================================

#[test]
fn test_nslookup_garbage_passthrough() {
    skim_cmd()
        .args(["--debug", "nslookup"])
        .write_stdin("random garbage output not from nslookup\n")
        .assert()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}
