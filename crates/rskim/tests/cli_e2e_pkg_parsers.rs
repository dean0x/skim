//! E2E tests for pkg parser degradation tiers (#105).
//!
//! Tests each pkg tool/subcommand at different degradation tiers via stdin piping,
//! verifying structured output markers and stderr diagnostics.
//!
//! v2.8.0: Flat dispatch — tool names are top-level subcommands.
//! e.g. `skim npm install` instead of `skim pkg npm install`.
//!
//! Tier behavior reference:
//! - Full: no stderr markers
//! - Degraded: "[skim:warning] ..." on stderr (only with --debug)
//! - Passthrough: "[skim:notice] output passed through without parsing" on stderr (only with --debug)

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd
}

// ============================================================================
// npm install: Tier 1 (JSON) — Full
// ============================================================================

#[test]
fn test_npm_install_tier1_json() {
    let fixture = include_str!("fixtures/cmd/pkg/npm_install.json");
    skim_cmd()
        .args(["npm", "install"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("npm install"))
        .stdout(predicate::str::contains("added: 127"))
        .stdout(predicate::str::contains("removed: 3"));
}

// ============================================================================
// npm install: Tier 2 (regex) — Degraded
// ============================================================================

#[test]
fn test_npm_install_tier2_regex() {
    let fixture = include_str!("fixtures/cmd/pkg/npm_install_text.txt");
    skim_cmd()
        .args(["--debug", "npm", "install"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("npm install"))
        .stdout(predicate::str::contains("added: 127"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// npm install: Tier 3 — Passthrough
// ============================================================================

#[test]
fn test_npm_install_passthrough() {
    skim_cmd()
        .args(["--debug", "npm", "install"])
        .write_stdin("completely unparseable output")
        .assert()
        .success()
        .stdout(predicate::str::contains("completely unparseable"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// npm audit: Tier 1 (JSON) — Full
// ============================================================================

#[test]
fn test_npm_audit_tier1_json() {
    let fixture = include_str!("fixtures/cmd/pkg/npm_audit.json");
    skim_cmd()
        .args(["npm", "audit"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("npm audit"))
        .stdout(predicate::str::contains("critical: 1"))
        .stdout(predicate::str::contains("total: 3"));
}

#[test]
fn test_npm_audit_clean_tier1() {
    let fixture = include_str!("fixtures/cmd/pkg/npm_audit_clean.json");
    skim_cmd()
        .args(["npm", "audit"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("total: 0"));
}

// ============================================================================
// npm outdated: Tier 1 (JSON) — Full
// ============================================================================

#[test]
fn test_npm_outdated_tier1_json() {
    let fixture = include_str!("fixtures/cmd/pkg/npm_outdated.json");
    skim_cmd()
        .args(["npm", "outdated"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("npm outdated"))
        .stdout(predicate::str::contains("3 packages"));
}

// ============================================================================
// npm ls: Tier 1 (JSON) — Full
// ============================================================================

#[test]
fn test_npm_ls_tier1_json() {
    let fixture = include_str!("fixtures/cmd/pkg/npm_ls.json");
    skim_cmd()
        .args(["npm", "ls"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("npm list"))
        .stdout(predicate::str::contains("4 total"))
        .stdout(predicate::str::contains("1 flagged"));
}

// ============================================================================
// npm: no subcommand shows help
// ============================================================================

#[test]
fn test_npm_no_subcmd_shows_help() {
    skim_cmd()
        .args(["npm"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Subcommands:"))
        .stdout(predicate::str::contains("install"))
        .stdout(predicate::str::contains("audit"));
}

// ============================================================================
// pip install: Tier 1 (regex) — Full
// ============================================================================

#[test]
fn test_pip_install_tier1_regex() {
    let fixture = include_str!("fixtures/cmd/pkg/pip_install.txt");
    skim_cmd()
        .args(["pip", "install"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pip install"))
        .stdout(predicate::str::contains("added: 3"));
}

// ============================================================================
// pip check: clean
// ============================================================================

#[test]
fn test_pip_check_clean() {
    let fixture = include_str!("fixtures/cmd/pkg/pip_check_clean.txt");
    skim_cmd()
        .args(["pip", "check"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pip check"))
        .stdout(predicate::str::contains("0 issues"));
}

// ============================================================================
// pip check: issues
// ============================================================================

#[test]
fn test_pip_check_issues() {
    let fixture = include_str!("fixtures/cmd/pkg/pip_check_issues.txt");
    skim_cmd()
        .args(["pip", "check"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pip check"))
        .stdout(predicate::str::contains("2 issues"));
}

// ============================================================================
// pip list --outdated: JSON
// ============================================================================

#[test]
fn test_pip_list_json() {
    let fixture = include_str!("fixtures/cmd/pkg/pip_outdated.json");
    skim_cmd()
        .args(["pip", "list"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pip outdated"))
        .stdout(predicate::str::contains("2 packages"));
}

// ============================================================================
// pnpm install: regex
// ============================================================================

#[test]
fn test_pnpm_install_regex() {
    let fixture = include_str!("fixtures/cmd/pkg/pnpm_install.txt");
    skim_cmd()
        .args(["pnpm", "install"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pnpm install"))
        .stdout(predicate::str::contains("added: 127"));
}

// ============================================================================
// pnpm audit: JSON
// ============================================================================

#[test]
fn test_pnpm_audit_json() {
    let fixture = include_str!("fixtures/cmd/pkg/pnpm_audit.json");
    skim_cmd()
        .args(["pnpm", "audit"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pnpm audit"))
        .stdout(predicate::str::contains("total: 2"));
}

// ============================================================================
// pnpm outdated: JSON
// ============================================================================

#[test]
fn test_pnpm_outdated_json() {
    let fixture = include_str!("fixtures/cmd/pkg/pnpm_outdated.json");
    skim_cmd()
        .args(["pnpm", "outdated"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pnpm outdated"))
        .stdout(predicate::str::contains("2 packages"));
}

// ============================================================================
// cargo audit: JSON
// ============================================================================

#[test]
fn test_cargo_audit_json() {
    let fixture = include_str!("fixtures/cmd/pkg/cargo_audit.json");
    skim_cmd()
        .args(["cargo", "audit"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("cargo audit"))
        .stdout(predicate::str::contains("critical: 1"))
        .stdout(predicate::str::contains("total: 2"));
}

#[test]
fn test_cargo_audit_clean_json() {
    let fixture = include_str!("fixtures/cmd/pkg/cargo_audit_clean.json");
    skim_cmd()
        .args(["cargo", "audit"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("total: 0"));
}

// ============================================================================
// cargo audit: Tier 2 (regex) — Degraded
// ============================================================================

#[test]
fn test_cargo_audit_tier2_regex() {
    let text = "Crate:   buffer-utils\nVersion: 0.3.1\nTitle:   Buffer overflow\nID:      RUSTSEC-2024-0001";
    skim_cmd()
        .args(["--debug", "cargo", "audit"])
        .write_stdin(text)
        .assert()
        .success()
        .stdout(predicate::str::contains("cargo audit"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// cargo audit: Tier 3 — Passthrough
// ============================================================================

#[test]
fn test_cargo_audit_passthrough() {
    skim_cmd()
        .args(["--debug", "cargo", "audit"])
        .write_stdin("completely unparseable output")
        .assert()
        .success()
        .stdout(predicate::str::contains("completely unparseable"))
        .stderr(predicate::str::contains("[skim:notice]"));
}
