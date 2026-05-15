//! E2E tests for the 9 new parsers added in #118:
//! cypress, playwright, swift, dotnet (test family)
//! rubocop, swiftlint (lint family)
//! gradle, maven (build family — real-command tests, skip when tool absent)
//! yarn (pkg family — install, audit, outdated)
//!
//! Pattern follows `cli_e2e_test_parsers.rs` and `cli_e2e_lint_parsers.rs`.
//! Each test pipes a fixture through the full binary via stdin and asserts on
//! stdout content and tier markers on stderr.
//!
//! Tier behavior reference:
//! - Full: no stderr markers
//! - Degraded: "[skim:warning] ..." on stderr (only with --debug)
//! - Passthrough: "[skim:notice] ..." on stderr (only with --debug)

use assert_cmd::Command;
use predicates::prelude::*;
use std::process::Command as StdCommand;

fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

// ============================================================================
// Cypress: Tier 1 (JSON / Mocha format) — Full
// ============================================================================

#[test]
fn test_cypress_tier1_json_pass() {
    let fixture = include_str!("fixtures/cmd/test/cypress_pass.json");
    skim_cmd()
        .args(["cypress", "run"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 3"))
        .stdout(predicate::str::contains("fail: 0"));
}

#[test]
fn test_cypress_tier1_json_fail() {
    let fixture = include_str!("fixtures/cmd/test/cypress_fail.json");
    skim_cmd()
        .args(["cypress", "run"])
        .write_stdin(fixture)
        .assert()
        // Exits non-zero because fail > 0 (run_test_runner uses fallback exit code 1)
        .code(predicate::ne(0))
        .stdout(predicate::str::contains("pass: 1"))
        .stdout(predicate::str::contains("fail: 1"));
}

// ============================================================================
// Cypress: Tier 2 (regex) — Degraded
// ============================================================================

#[test]
fn test_cypress_tier2_regex_degraded() {
    let text = "Passing: 3\nFailing: 0\nPending: 1\n";
    skim_cmd()
        .args(["--debug", "cypress", "run"])
        .write_stdin(text)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 3"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// Cypress: Tier 3 (passthrough) — Passthrough
// ============================================================================

#[test]
fn test_cypress_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "cypress", "run"])
        .write_stdin("random garbage not cypress output\n")
        .assert()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// Playwright: Tier 1 (JSON) — Full
// ============================================================================

#[test]
fn test_playwright_tier1_json_pass() {
    let fixture = include_str!("fixtures/cmd/test/playwright_pass.json");
    skim_cmd()
        .args(["playwright", "test"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 3"))
        .stdout(predicate::str::contains("fail: 0"));
}

#[test]
fn test_playwright_tier1_json_fail() {
    let fixture = include_str!("fixtures/cmd/test/playwright_fail.json");
    skim_cmd()
        .args(["playwright", "test"])
        .write_stdin(fixture)
        .assert()
        // Exits non-zero because fail > 0 (run_test_runner uses fallback exit code 1)
        .code(predicate::ne(0))
        .stdout(predicate::str::contains("fail: 1"))
        .stdout(predicate::str::contains("pass: 1"));
}

// ============================================================================
// Playwright: Tier 2 (regex) — Degraded
// ============================================================================

#[test]
fn test_playwright_tier2_regex_degraded() {
    // fail > 0 → non-zero exit; run_test_runner uses fallback exit code 1 for stdin path
    let text = "Running 4 tests using 2 workers\n  3 passed (2s)\n  1 failed\n";
    skim_cmd()
        .args(["--debug", "playwright", "test"])
        .write_stdin(text)
        .assert()
        .code(predicate::ne(0))
        .stdout(predicate::str::contains("pass: 3"))
        .stdout(predicate::str::contains("fail: 1"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// Playwright: Tier 3 (passthrough) — Passthrough
// ============================================================================

#[test]
fn test_playwright_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "playwright", "test"])
        .write_stdin("random garbage not playwright output\n")
        .assert()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// Swift: Tier 1 (XCTest text) — Full
// ============================================================================

#[test]
fn test_swift_tier1_xctest_pass() {
    let fixture = include_str!("fixtures/cmd/test/swift_pass.txt");
    skim_cmd()
        .args(["swift", "test"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 2"))
        .stdout(predicate::str::contains("fail: 0"));
}

#[test]
fn test_swift_tier1_xctest_fail() {
    let fixture = include_str!("fixtures/cmd/test/swift_fail.txt");
    skim_cmd()
        .args(["swift", "test"])
        .write_stdin(fixture)
        .assert()
        // Exits non-zero because fail > 0
        .code(predicate::ne(0))
        .stdout(predicate::str::contains("fail: 1"))
        .stdout(predicate::str::contains("pass: 1"));
}

// ============================================================================
// Swift: Tier 3 (passthrough) — Passthrough
// ============================================================================

#[test]
fn test_swift_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "swift", "test"])
        .write_stdin("random garbage not swift test output\n")
        .assert()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// dotnet: Tier 2 (regex console summary) — Degraded
//
// NOTE: dotnet's Tier 1 (TRX XML) requires a spawned process to produce a
// Results File path, which is unavailable in stdin-only mode. The regex
// summary tier is the highest achievable tier via piped stdin.
// ============================================================================

#[test]
fn test_dotnet_tier2_regex_pass() {
    let fixture = include_str!("fixtures/cmd/test/dotnet_pass.txt");
    skim_cmd()
        .args(["--debug", "dotnet", "test"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 5"))
        .stdout(predicate::str::contains("fail: 0"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

#[test]
fn test_dotnet_tier2_regex_fail() {
    let fixture = include_str!("fixtures/cmd/test/dotnet_fail.txt");
    skim_cmd()
        .args(["--debug", "dotnet", "test"])
        .write_stdin(fixture)
        .assert()
        // fail > 0 → non-zero exit code
        .code(predicate::ne(0))
        .stdout(predicate::str::contains("fail: 1"))
        .stdout(predicate::str::contains("pass: 4"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// dotnet: Tier 3 (passthrough) — Passthrough
// ============================================================================

#[test]
fn test_dotnet_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "dotnet", "test"])
        .write_stdin("random garbage not dotnet test output\n")
        .assert()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// RuboCop: Tier 1 (JSON) — Full
// ============================================================================

#[test]
fn test_rubocop_tier1_json_fail() {
    // Fixture: 1 error (Lint/UnusedMethodArgument), 1 warning (Metrics/MethodLength),
    // 1 info/convention (Style/StringLiterals — not counted in error/warning totals).
    let fixture = include_str!("fixtures/cmd/lint/rubocop_fail.json");
    skim_cmd()
        .args(["rubocop"])
        .write_stdin(fixture)
        .assert()
        .code(0) // stdin mode exits 0 (lint parsers use run_linter, not run_test_runner)
        .stdout(predicate::str::contains("rubocop "))
        .stdout(predicate::str::contains("1 error"))
        .stdout(predicate::str::contains("1 warning"));
}

#[test]
fn test_rubocop_tier1_json_pass() {
    let fixture = include_str!("fixtures/cmd/lint/rubocop_pass.json");
    skim_cmd()
        .args(["rubocop"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains(" OK"));
}

// ============================================================================
// RuboCop: Tier 2 (regex) — Degraded
// ============================================================================

#[test]
fn test_rubocop_tier2_regex_degraded() {
    let fixture = include_str!("fixtures/cmd/lint/rubocop_text.txt");
    skim_cmd()
        .args(["--debug", "rubocop"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("rubocop "))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// RuboCop: Tier 3 (passthrough)
// ============================================================================

#[test]
fn test_rubocop_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "rubocop"])
        .write_stdin("random garbage not rubocop output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// SwiftLint: Tier 1 (JSON array) — Full
// ============================================================================

#[test]
fn test_swiftlint_tier1_json_fail() {
    // Fixture: 1 error (force_cast), 2 warnings (line_length, trailing_whitespace).
    let fixture = include_str!("fixtures/cmd/lint/swiftlint_fail.json");
    skim_cmd()
        .args(["swiftlint"])
        .write_stdin(fixture)
        .assert()
        .code(0) // stdin mode exits 0 (lint parsers use exit code from process, stdin = 0)
        .stdout(predicate::str::contains("swiftlint "))
        .stdout(predicate::str::contains("1 error"))
        .stdout(predicate::str::contains("2 warning"));
}

#[test]
fn test_swiftlint_tier1_json_pass() {
    let fixture = include_str!("fixtures/cmd/lint/swiftlint_pass.json");
    skim_cmd()
        .args(["swiftlint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains(" OK"));
}

// ============================================================================
// SwiftLint: Tier 2 (regex) — Degraded
// ============================================================================

#[test]
fn test_swiftlint_tier2_regex_degraded() {
    let fixture = include_str!("fixtures/cmd/lint/swiftlint_text.txt");
    skim_cmd()
        .args(["--debug", "swiftlint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("swiftlint "))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// SwiftLint: Tier 3 (passthrough)
// ============================================================================

#[test]
fn test_swiftlint_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "swiftlint"])
        .write_stdin("random garbage not swiftlint output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// Gradle: real-command tests (skip when gradle not installed)
//
// NOTE: Gradle/Maven are build parsers that always spawn the real command;
// they have no stdin read path. Like `make`, tests are skipped when the
// binary is absent (CI without JDK). The tests verify dispatcher routing and
// exit-code semantics.
// ============================================================================

#[test]
fn test_gradle_dispatches_when_installed() {
    if StdCommand::new("gradle").arg("--version").output().is_err() {
        eprintln!("skipping: gradle not installed");
        return;
    }
    // `skim gradle --help` routes through the build dispatcher; gradle --help
    // exits 0, so the exit code here is 0.
    skim_cmd()
        .args(["gradle", "--help"])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success();
}

#[test]
fn test_gradle_not_installed_surfaces_hint() {
    if StdCommand::new("gradle").arg("--version").output().is_ok() {
        eprintln!("skipping: gradle is installed");
        return;
    }
    // When gradle is absent, skim must fail with a non-zero exit and mention
    // an install hint in stderr.
    let output = skim_cmd()
        .args(["gradle", "build"])
        .output()
        .unwrap();
    assert_ne!(
        output.status.code(),
        Some(0),
        "expected non-zero exit when gradle is not installed"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("gradle.org") || stderr.contains("gradlew") || stderr.contains("Gradle"),
        "expected gradle install hint in stderr, got: {stderr}"
    );
}

// ============================================================================
// Maven: real-command tests (skip when mvn not installed)
// ============================================================================

#[test]
fn test_maven_dispatches_when_installed() {
    if StdCommand::new("mvn").arg("--version").output().is_err() {
        eprintln!("skipping: mvn not installed");
        return;
    }
    skim_cmd()
        .args(["mvn", "--help"])
        .timeout(std::time::Duration::from_secs(30))
        .assert()
        .success();
}

#[test]
fn test_maven_not_installed_surfaces_hint() {
    if StdCommand::new("mvn").arg("--version").output().is_ok() {
        eprintln!("skipping: mvn is installed");
        return;
    }
    let output = skim_cmd()
        .args(["mvn", "compile"])
        .output()
        .unwrap();
    assert_ne!(
        output.status.code(),
        Some(0),
        "expected non-zero exit when mvn is not installed"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("maven.apache.org") || stderr.contains("mvnw") || stderr.contains("Maven"),
        "expected maven install hint in stderr, got: {stderr}"
    );
}

// ============================================================================
// yarn install: Tier 1 (NDJSON) — Full
// ============================================================================

#[test]
fn test_yarn_install_tier1_ndjson() {
    let fixture = include_str!("fixtures/cmd/pkg/yarn_install.ndjson");
    skim_cmd()
        .args(["yarn", "install"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("yarn install"));
}

// ============================================================================
// yarn install: Tier 2 (regex) — Degraded
// ============================================================================

#[test]
fn test_yarn_install_tier2_regex() {
    let fixture = include_str!("fixtures/cmd/pkg/yarn_install.txt");
    skim_cmd()
        .args(["--debug", "yarn", "install"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("yarn install"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// yarn install: Tier 3 (passthrough)
// ============================================================================

#[test]
fn test_yarn_install_tier3_passthrough() {
    skim_cmd()
        .args(["--debug", "yarn", "install"])
        .write_stdin("completely unparseable output")
        .assert()
        .success()
        .stdout(predicate::str::contains("completely unparseable"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// yarn audit: Tier 1 (NDJSON) — Full
// ============================================================================

#[test]
fn test_yarn_audit_tier1_ndjson() {
    let fixture = include_str!("fixtures/cmd/pkg/yarn_audit.ndjson");
    skim_cmd()
        .args(["yarn", "audit"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("yarn audit"))
        .stdout(predicate::str::contains("total: 2"));
}

// ============================================================================
// yarn audit: Tier 3 (passthrough)
// ============================================================================

#[test]
fn test_yarn_audit_tier3_passthrough() {
    skim_cmd()
        .args(["--debug", "yarn", "audit"])
        .write_stdin("completely unparseable output")
        .assert()
        .success()
        .stdout(predicate::str::contains("completely unparseable"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// yarn outdated: Tier 1 (NDJSON) — Full
// ============================================================================

#[test]
fn test_yarn_outdated_tier1_ndjson() {
    let fixture = include_str!("fixtures/cmd/pkg/yarn_outdated.ndjson");
    skim_cmd()
        .args(["yarn", "outdated"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("yarn outdated"))
        .stdout(predicate::str::contains("3 packages"));
}

// ============================================================================
// yarn outdated: Tier 3 (passthrough)
// ============================================================================

#[test]
fn test_yarn_outdated_tier3_passthrough() {
    skim_cmd()
        .args(["--debug", "yarn", "outdated"])
        .write_stdin("completely unparseable output")
        .assert()
        .success()
        .stdout(predicate::str::contains("completely unparseable"))
        .stderr(predicate::str::contains("[skim:notice]"));
}
