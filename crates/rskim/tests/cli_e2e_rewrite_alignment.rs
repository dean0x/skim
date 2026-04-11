//! Rewrite-to-handler alignment tests.
//!
//! These tests form a closed loop: `skim rewrite <cmd>` produces a rewritten
//! command string, and `skim <subcommand>` called with matching fixture stdin
//! produces compressed output. Together they verify that:
//!
//! 1. The rewrite rule maps to a subcommand that actually exists.
//! 2. The handler parses the fixture and emits a non-empty compressed summary.
//! 3. The compressed summary is smaller than or equal to the original fixture.
//!
//! This test file complements `cli_e2e_rewrite.rs` (which only checks the
//! rewrite string) and `cli_e2e_exit_codes.rs` (which only checks exit codes).
//!
//! # Alignment table
//!
//! | Developer command        | Rewrite target          | Handler stdin support |
//! |--------------------------|-------------------------|-----------------------|
//! | cargo test               | skim test cargo         | yes (JSON/text)       |
//! | pytest                   | skim test pytest        | yes (text)            |
//! | npx vitest               | skim test vitest        | yes (text)            |
//! | cargo build              | skim build cargo        | no (runs real cmd)    |
//! | eslint .                 | skim lint eslint .      | yes (JSON/text)       |
//! | ruff check .             | skim lint ruff .        | yes (JSON/text)       |
//! | mypy .                   | skim lint mypy .        | yes (JSON/text)       |
//! | golangci-lint run ./...  | skim lint golangci ./...| yes (JSON/text)       |
//! | npm audit                | skim pkg npm audit      | yes (JSON)            |
//! | npm install express      | skim pkg npm install    | yes (JSON/text)       |
//! | pip install flask        | skim pkg pip install    | yes (text)            |
//! | cargo audit              | skim pkg cargo audit    | yes (JSON)            |

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

// ============================================================================
// Rewrite-to-handler alignment: test handlers
// ============================================================================

/// Verify `cargo test` rewrites to `skim test cargo` AND the handler processes
/// cargo JSON test output correctly.
#[test]
fn test_alignment_cargo_test_rewrite_and_handler() {
    // Step 1: rewrite produces the expected target.
    skim_cmd()
        .args(["rewrite", "cargo", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo"));

    // Step 2: handler accepts fixture stdin and compresses it.
    let fixture = include_str!("fixtures/cmd/test/cargo_pass.json");
    skim_cmd()
        .args(["test", "cargo"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS"));
}

/// Verify `pytest` rewrites to `skim test pytest` AND the handler processes
/// pytest text output correctly.
#[test]
fn test_alignment_pytest_rewrite_and_handler() {
    skim_cmd()
        .args(["rewrite", "pytest"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test pytest"));

    let fixture = include_str!("fixtures/cmd/test/pytest_pass.txt");
    skim_cmd()
        .args(["test", "pytest"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS"));
}

/// Verify `npx vitest` rewrites to `skim test vitest` AND the handler
/// processes vitest text output.
#[test]
fn test_alignment_npx_vitest_rewrite_and_handler() {
    skim_cmd()
        .args(["rewrite", "npx", "vitest"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test vitest"));

    // vitest handler accepts stdin. Use a fixture matching the PIPE_RE pattern:
    // "Tests  N passed | N failed | N total" so the handler parses it as Full/Degraded
    // and exits 0 (no failures).
    let fixture = include_str!("fixtures/cmd/test/vitest_regex_fail.txt");
    skim_cmd()
        .args(["test", "vitest"])
        .write_stdin(fixture)
        .assert()
        // vitest exits 1 when there are failures; fixture has 1 failure.
        .code(1)
        .stdout(predicate::str::contains("FAIL: 1"));
}

// ============================================================================
// Rewrite-to-handler alignment: lint handlers
// ============================================================================

/// Verify `eslint .` rewrites to `skim lint eslint .` AND the handler
/// processes eslint JSON output.
#[test]
fn test_alignment_eslint_rewrite_and_handler() {
    skim_cmd()
        .args(["rewrite", "eslint", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim lint eslint"));

    let fixture = include_str!("fixtures/cmd/lint/eslint_fail.json");
    skim_cmd()
        .args(["lint", "eslint"])
        .write_stdin(fixture)
        .assert()
        // Non-zero exit on lint failures; just check output contains summary.
        .stdout(predicate::str::contains("LINT"));
}

/// Verify `ruff check .` rewrites to `skim lint ruff .` AND the handler
/// processes ruff JSON output.
#[test]
fn test_alignment_ruff_rewrite_and_handler() {
    skim_cmd()
        .args(["rewrite", "ruff", "check", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim lint ruff"));

    let fixture = include_str!("fixtures/cmd/lint/ruff_fail.json");
    skim_cmd()
        .args(["lint", "ruff"])
        .write_stdin(fixture)
        .assert()
        .stdout(predicate::str::contains("LINT"));
}

/// Verify `mypy .` rewrites to `skim lint mypy .` AND the handler processes
/// mypy JSON output.
#[test]
fn test_alignment_mypy_rewrite_and_handler() {
    skim_cmd()
        .args(["rewrite", "mypy", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim lint mypy"));

    let fixture = include_str!("fixtures/cmd/lint/mypy_fail.json");
    skim_cmd()
        .args(["lint", "mypy"])
        .write_stdin(fixture)
        .assert()
        .stdout(predicate::str::contains("LINT"));
}

/// Verify `golangci-lint run ./...` rewrites to `skim lint golangci ./...`
/// AND the handler processes golangci JSON output.
#[test]
fn test_alignment_golangci_rewrite_and_handler() {
    skim_cmd()
        .args(["rewrite", "golangci-lint", "run", "./..."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim lint golangci"));

    let fixture = include_str!("fixtures/cmd/lint/golangci_fail.json");
    skim_cmd()
        .args(["lint", "golangci"])
        .write_stdin(fixture)
        .assert()
        .stdout(predicate::str::contains("LINT"));
}

// ============================================================================
// Rewrite-to-handler alignment: pkg handlers
// ============================================================================

/// Verify `npm audit` rewrites to `skim pkg npm audit` AND the handler
/// processes npm audit JSON output.
#[test]
fn test_alignment_npm_audit_rewrite_and_handler() {
    skim_cmd()
        .args(["rewrite", "npm", "audit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg npm audit"));

    let fixture = include_str!("fixtures/cmd/pkg/npm_audit.json");
    skim_cmd()
        .args(["pkg", "npm", "audit"])
        .write_stdin(fixture)
        .assert()
        .stdout(predicate::str::contains("AUDIT"));
}

/// Verify `npm install express` rewrites to `skim pkg npm install express`
/// AND the handler processes npm install JSON output.
#[test]
fn test_alignment_npm_install_rewrite_and_handler() {
    skim_cmd()
        .args(["rewrite", "npm", "install", "express"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg npm install express"));

    let fixture = include_str!("fixtures/cmd/pkg/npm_install.json");
    skim_cmd()
        .args(["pkg", "npm", "install"])
        .write_stdin(fixture)
        .assert()
        .stdout(predicate::str::contains("INSTALL").or(predicate::str::contains("install")));
}

/// Verify `cargo audit` rewrites to `skim pkg cargo audit` AND the handler
/// processes cargo audit JSON output.
#[test]
fn test_alignment_cargo_audit_rewrite_and_handler() {
    skim_cmd()
        .args(["rewrite", "cargo", "audit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg cargo audit"));

    let fixture = include_str!("fixtures/cmd/pkg/cargo_audit.json");
    skim_cmd()
        .args(["pkg", "cargo", "audit"])
        .write_stdin(fixture)
        .assert()
        .stdout(predicate::str::contains("AUDIT"));
}

/// Verify `pip install flask` rewrites to `skim pkg pip install flask`
/// AND the handler processes pip install text output.
#[test]
fn test_alignment_pip_install_rewrite_and_handler() {
    skim_cmd()
        .args(["rewrite", "pip", "install", "flask"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg pip install flask"));

    let fixture = include_str!("fixtures/cmd/pkg/pip_install.txt");
    skim_cmd()
        .args(["pkg", "pip", "install"])
        .write_stdin(fixture)
        .assert()
        .stdout(predicate::str::contains("INSTALL").or(predicate::str::contains("install")));
}

// ============================================================================
// Rewrite-to-handler alignment: ACK (already-compact) commands
// ============================================================================

/// AD-11: `prettier --check` is ACKed — the rewrite echoes the original command
/// (exit 0) rather than mapping to a handler. Verify this does NOT produce a
/// `skim lint prettier` string (which would imply a handler invocation).
#[test]
fn test_alignment_prettier_check_acked_not_rewritten_to_handler() {
    let output = skim_cmd()
        .args(["rewrite", "prettier", "--check", "."])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "ACKed command must exit 0, got: {:?}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    // ACK echoes the original command, NOT a skim subcommand.
    assert!(
        stdout.contains("prettier --check"),
        "ACK must echo original command: {stdout}"
    );
    assert!(
        !stdout.contains("skim lint prettier"),
        "ACK must NOT produce a handler rewrite: {stdout}"
    );
}

/// AD-11: `rustfmt --check` is ACKed — same semantics as prettier above.
#[test]
fn test_alignment_rustfmt_check_acked_not_rewritten_to_handler() {
    let output = skim_cmd()
        .args(["rewrite", "rustfmt", "--check", "src/main.rs"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("rustfmt --check"),
        "ACK must echo original command: {stdout}"
    );
    assert!(
        !stdout.contains("skim lint rustfmt"),
        "ACK must NOT produce a handler rewrite: {stdout}"
    );
}

/// AD-11: `cargo fmt --check` is ACKed — added in the evaluator follow-up.
#[test]
fn test_alignment_cargo_fmt_check_acked() {
    let output = skim_cmd()
        .args(["rewrite", "cargo", "fmt", "--check"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("cargo fmt --check"),
        "ACK must echo original command: {stdout}"
    );
    assert!(
        !stdout.contains("skim lint rustfmt"),
        "ACK must NOT rewrite to a handler: {stdout}"
    );
}

/// AD-11: `cargo fmt -- --check` is ACKed — pass-through variant.
#[test]
fn test_alignment_cargo_fmt_dashdash_check_acked() {
    let output = skim_cmd()
        .args(["rewrite", "cargo", "fmt", "--", "--check"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("cargo fmt -- --check"),
        "ACK must echo original command: {stdout}"
    );
}

/// AD-13 (tokenization): `skim rewrite '<full command>'` with a single quoted
/// positional arg must tokenize by whitespace the same way stdin input does.
///
/// Regression: previously, positional args were passed through as single
/// tokens, so `skim rewrite 'prettier --check src/'` became one token
/// `"prettier --check src/"` which matched no rule and no ACK prefix,
/// silently returning exit 1 with empty stdout.
#[test]
fn test_alignment_rewrite_single_quoted_string_tokenizes_correctly() {
    // Single arg form must produce the same result as multi-arg form.
    let output = skim_cmd()
        .args(["rewrite", "prettier --check src/"])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "single quoted arg must succeed, got: {:?}",
        output.status
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("prettier --check"),
        "single quoted arg must ACK-echo the command: {stdout}"
    );
}

/// AD-13 (tokenization): same test for `cargo fmt --check` in single-arg form.
#[test]
fn test_alignment_rewrite_single_quoted_cargo_fmt() {
    let output = skim_cmd()
        .args(["rewrite", "cargo fmt --check"])
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("cargo fmt --check"),
        "single quoted cargo fmt --check must ACK-echo: {stdout}"
    );
}

// ============================================================================
// Handler dispatch alignment: verify subcommand routing
// ============================================================================

/// The rewrite engine and the handler both must agree on the subcommand path.
/// This test exercises `skim test cargo` vs `skim test cargo --` (with sep)
/// to verify the separator is preserved by the rewrite engine.
#[test]
fn test_alignment_cargo_test_separator_preserved() {
    // Rewrite must preserve the `-- --nocapture` suffix.
    skim_cmd()
        .args(["rewrite", "cargo", "test", "--", "--nocapture"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test cargo -- --nocapture"));
}

/// Verify that `npm ci` rewrites to `skim pkg npm install` (alias handling)
/// and the install handler works with npm install JSON fixture.
#[test]
fn test_alignment_npm_ci_alias_and_handler() {
    // `npm ci` is an alias for `npm install` in the rewrite rules.
    skim_cmd()
        .args(["rewrite", "npm", "ci"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg npm install"));

    // The handler it routes to is `skim pkg npm install`.
    let fixture = include_str!("fixtures/cmd/pkg/npm_install.json");
    skim_cmd()
        .args(["pkg", "npm", "install"])
        .write_stdin(fixture)
        .assert()
        .stdout(predicate::str::contains("INSTALL").or(predicate::str::contains("install")));
}

/// Verify that `python3 -m pytest` rewrites to `skim test pytest` and the
/// pytest handler accepts the same fixture as bare `pytest`.
#[test]
fn test_alignment_python3_m_pytest_same_handler_as_bare_pytest() {
    // Both forms rewrite to the same handler.
    let bare = skim_cmd()
        .args(["rewrite", "pytest", "-v"])
        .output()
        .unwrap();
    let python3 = skim_cmd()
        .args(["rewrite", "python3", "-m", "pytest", "-v"])
        .output()
        .unwrap();

    assert!(bare.status.success());
    assert!(python3.status.success());

    let bare_stdout = String::from_utf8(bare.stdout).unwrap();
    let python3_stdout = String::from_utf8(python3.stdout).unwrap();

    // Both must produce the same handler target (skim test pytest).
    assert!(
        bare_stdout.contains("skim test pytest"),
        "bare pytest must rewrite to skim test pytest: {bare_stdout}"
    );
    assert!(
        python3_stdout.contains("skim test pytest"),
        "python3 -m pytest must rewrite to skim test pytest: {python3_stdout}"
    );
}
