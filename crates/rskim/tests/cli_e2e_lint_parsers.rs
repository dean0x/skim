//! E2E tests for lint parser degradation tiers (#104).
//!
//! Tests each linter at different degradation tiers via stdin piping,
//! verifying structured output markers and stderr diagnostics.
//!
//! Tier behavior reference (from emit_markers in output/mod.rs):
//! - Full: no stderr markers
//! - Degraded: "[skim:warning] ..." on stderr (only with --debug)
//! - Passthrough: "[skim:notice] output passed through without parsing" on stderr (only with --debug)

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

// ============================================================================
// ESLint: Tier 1 (JSON) -- Full
// ============================================================================

#[test]
fn test_eslint_tier1_json_pass() {
    let fixture = include_str!("fixtures/cmd/lint/eslint_pass.json");
    skim_cmd()
        .args(["lint", "eslint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT OK"));
}

#[test]
fn test_eslint_tier1_json_fail() {
    let fixture = include_str!("fixtures/cmd/lint/eslint_fail.json");
    skim_cmd()
        .args(["lint", "eslint"])
        .write_stdin(fixture)
        .assert()
        .code(0) // stdin mode always exits 0
        .stdout(predicate::str::contains("LINT:"))
        .stdout(predicate::str::contains("2 errors"))
        .stdout(predicate::str::contains("3 warnings"));
}

// ============================================================================
// ESLint: Tier 2 (regex) -- Degraded
// ============================================================================

#[test]
fn test_eslint_tier2_regex_degraded() {
    let fixture = include_str!("fixtures/cmd/lint/eslint_text.txt");
    skim_cmd()
        .args(["--debug", "lint", "eslint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// ESLint: Tier 3 (passthrough)
// ============================================================================

#[test]
fn test_eslint_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "lint", "eslint"])
        .write_stdin("random garbage not eslint output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// ESLint: --json flag
// ============================================================================

#[test]
fn test_eslint_json_flag_full() {
    let fixture = include_str!("fixtures/cmd/lint/eslint_fail.json");
    skim_cmd()
        .args(["lint", "--json", "eslint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"tool\":\"eslint\""))
        .stdout(predicate::str::contains("\"errors\":2"));
}

#[test]
fn test_eslint_json_flag_degraded() {
    let fixture = include_str!("fixtures/cmd/lint/eslint_text.txt");
    skim_cmd()
        .args(["lint", "--json", "eslint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"tier\":\"degraded\""))
        .stdout(predicate::str::contains("\"tool\":\"eslint\""));
}

// ============================================================================
// Ruff: Tier 1 (JSON) -- Full
// ============================================================================

#[test]
fn test_ruff_tier1_json_fail() {
    let fixture = include_str!("fixtures/cmd/lint/ruff_fail.json");
    skim_cmd()
        .args(["lint", "ruff"])
        .write_stdin(fixture)
        .assert()
        .code(0)
        .stdout(predicate::str::contains("LINT:"))
        .stdout(predicate::str::contains("3 errors"));
}

// ============================================================================
// Ruff: Tier 2 (regex) -- Degraded
// ============================================================================

#[test]
fn test_ruff_tier2_regex_degraded() {
    let fixture = include_str!("fixtures/cmd/lint/ruff_text.txt");
    skim_cmd()
        .args(["--debug", "lint", "ruff"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// Ruff: Tier 3 (passthrough)
// ============================================================================

#[test]
fn test_ruff_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "lint", "ruff"])
        .write_stdin("random garbage not ruff output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// Ruff: --json flag
// ============================================================================

#[test]
fn test_ruff_json_flag_full() {
    let fixture = include_str!("fixtures/cmd/lint/ruff_fail.json");
    skim_cmd()
        .args(["lint", "--json", "ruff"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"tool\":\"ruff\""))
        .stdout(predicate::str::contains("\"errors\":3"));
}

// ============================================================================
// mypy: Tier 1 (NDJSON) -- Full
// ============================================================================

#[test]
fn test_mypy_tier1_json_fail() {
    let fixture = include_str!("fixtures/cmd/lint/mypy_fail.json");
    skim_cmd()
        .args(["lint", "mypy"])
        .write_stdin(fixture)
        .assert()
        .code(0)
        .stdout(predicate::str::contains("LINT:"))
        .stdout(predicate::str::contains("3 errors"));
}

// ============================================================================
// mypy: Tier 2 (regex) -- Degraded
// ============================================================================

#[test]
fn test_mypy_tier2_regex_degraded() {
    let fixture = include_str!("fixtures/cmd/lint/mypy_text.txt");
    skim_cmd()
        .args(["--debug", "lint", "mypy"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// mypy: Tier 3 (passthrough)
// ============================================================================

#[test]
fn test_mypy_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "lint", "mypy"])
        .write_stdin("random garbage not mypy output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// mypy: --json flag
// ============================================================================

#[test]
fn test_mypy_json_flag_full() {
    let fixture = include_str!("fixtures/cmd/lint/mypy_fail.json");
    skim_cmd()
        .args(["lint", "--json", "mypy"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"tool\":\"mypy\""))
        .stdout(predicate::str::contains("\"errors\":3"));
}

// ============================================================================
// golangci-lint: Tier 1 (JSON) -- Full
// ============================================================================

#[test]
fn test_golangci_tier1_json_fail() {
    let fixture = include_str!("fixtures/cmd/lint/golangci_fail.json");
    skim_cmd()
        .args(["lint", "golangci"])
        .write_stdin(fixture)
        .assert()
        .code(0)
        .stdout(predicate::str::contains("LINT:"))
        .stdout(predicate::str::contains("1 error"))
        .stdout(predicate::str::contains("3 warning"));
}

// ============================================================================
// golangci-lint: Tier 2 (regex) -- Degraded
// ============================================================================

#[test]
fn test_golangci_tier2_regex_degraded() {
    let fixture = include_str!("fixtures/cmd/lint/golangci_text.txt");
    skim_cmd()
        .args(["--debug", "lint", "golangci"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

// ============================================================================
// golangci-lint: Tier 3 (passthrough)
// ============================================================================

#[test]
fn test_golangci_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "lint", "golangci"])
        .write_stdin("random garbage not golangci output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

// ============================================================================
// golangci-lint: --json flag
// ============================================================================

#[test]
fn test_golangci_json_flag_full() {
    let fixture = include_str!("fixtures/cmd/lint/golangci_fail.json");
    skim_cmd()
        .args(["lint", "--json", "golangci"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"tool\":\"golangci\""));
}

// ============================================================================
// black: Tier 1 -- Full (check mode)
// ============================================================================

#[test]
fn test_black_tier1_check_fail() {
    let fixture = include_str!("fixtures/cmd/lint/black_check_fail.txt");
    skim_cmd()
        .args(["lint", "black"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stdout(predicate::str::contains("formatting"));
}

#[test]
fn test_black_tier1_check_pass() {
    let fixture = include_str!("fixtures/cmd/lint/black_check_pass.txt");
    skim_cmd()
        .args(["lint", "black"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT OK"));
}

#[test]
fn test_black_tier2_regex_degraded() {
    // Plain `would reformat` without `All done!` context
    skim_cmd()
        .args(["--debug", "lint", "black"])
        .write_stdin("would reformat src/main.py\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"));
}

#[test]
fn test_black_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "lint", "black"])
        .write_stdin("random garbage not black output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

#[test]
fn test_black_json_flag_full() {
    let fixture = include_str!("fixtures/cmd/lint/black_check_fail.txt");
    skim_cmd()
        .args(["lint", "--json", "black"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"tool\":\"black\""))
        .stdout(predicate::str::is_match(r#"\{.*"tool".*\}"#).unwrap());
}

// ============================================================================
// gofmt: Tier 1 -- Full (list mode)
// ============================================================================

#[test]
fn test_gofmt_tier1_list_fail() {
    let fixture = include_str!("fixtures/cmd/lint/gofmt_list_fail.txt");
    skim_cmd()
        .args(["lint", "gofmt"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stdout(predicate::str::contains("formatting"));
}

#[test]
fn test_gofmt_tier2_regex_degraded() {
    let fixture = include_str!("fixtures/cmd/lint/gofmt_diff_fail.txt");
    skim_cmd()
        .args(["--debug", "lint", "gofmt"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

#[test]
fn test_gofmt_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "lint", "gofmt"])
        .write_stdin("random garbage not gofmt output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

#[test]
fn test_gofmt_json_flag_full() {
    let fixture = include_str!("fixtures/cmd/lint/gofmt_list_fail.txt");
    skim_cmd()
        .args(["lint", "--json", "gofmt"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"tool\":\"gofmt\""));
}

// ============================================================================
// biome: Tier 1 -- Full (JSON check mode)
// ============================================================================

#[test]
fn test_biome_tier1_json_fail() {
    let fixture = include_str!("fixtures/cmd/lint/biome_check_fail.json");
    skim_cmd()
        .args(["lint", "biome"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stdout(predicate::str::contains("1 error"));
}

#[test]
fn test_biome_tier2_regex_degraded() {
    let fixture = include_str!("fixtures/cmd/lint/biome_check_text.txt");
    skim_cmd()
        .args(["--debug", "lint", "biome"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

#[test]
fn test_biome_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "lint", "biome"])
        .write_stdin("random garbage not biome output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

#[test]
fn test_biome_json_flag_full() {
    let fixture = include_str!("fixtures/cmd/lint/biome_check_fail.json");
    skim_cmd()
        .args(["lint", "--json", "biome"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"tool\":\"biome\""))
        .stdout(predicate::str::contains("\"errors\":1"));
}

// ============================================================================
// dprint: Tier 1 -- Full (list mode)
// ============================================================================

#[test]
fn test_dprint_tier1_check_fail() {
    let fixture = include_str!("fixtures/cmd/lint/dprint_check_fail.txt");
    skim_cmd()
        .args(["lint", "dprint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stdout(predicate::str::contains("formatting"));
}

#[test]
fn test_dprint_tier2_regex_degraded() {
    skim_cmd()
        .args(["--debug", "lint", "dprint"])
        .write_stdin("from src/main.ts:\n  | diff content\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

#[test]
fn test_dprint_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "lint", "dprint"])
        .write_stdin("random garbage not dprint output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

#[test]
fn test_dprint_json_flag_full() {
    let fixture = include_str!("fixtures/cmd/lint/dprint_check_fail.txt");
    skim_cmd()
        .args(["lint", "--json", "dprint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"tool\":\"dprint\""));
}

// ============================================================================
// oxlint: Tier 1 -- Full (JSON mode)
// ============================================================================

#[test]
fn test_oxlint_tier1_json_fail() {
    let fixture = include_str!("fixtures/cmd/lint/oxlint_fail.json");
    skim_cmd()
        .args(["lint", "oxlint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stdout(predicate::str::contains("1 error"))
        .stdout(predicate::str::contains("2 warning"));
}

#[test]
fn test_oxlint_tier1_json_pass() {
    let fixture = include_str!("fixtures/cmd/lint/oxlint_pass.json");
    skim_cmd()
        .args(["lint", "oxlint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT OK"));
}

#[test]
fn test_oxlint_tier2_regex_degraded() {
    let fixture = include_str!("fixtures/cmd/lint/oxlint_text.txt");
    skim_cmd()
        .args(["--debug", "lint", "oxlint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stderr(predicate::str::contains("[skim:warning]"));
}

#[test]
fn test_oxlint_tier3_passthrough_garbage() {
    skim_cmd()
        .args(["--debug", "lint", "oxlint"])
        .write_stdin("random garbage not oxlint output\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("random garbage"))
        .stderr(predicate::str::contains("[skim:notice]"));
}

#[test]
fn test_oxlint_json_flag_full() {
    let fixture = include_str!("fixtures/cmd/lint/oxlint_fail.json");
    skim_cmd()
        .args(["lint", "--json", "oxlint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"tool\":\"oxlint\""))
        .stdout(predicate::str::contains("\"errors\":1"));
}

// ============================================================================
// Dispatcher: help and unknown linter
// ============================================================================

#[test]
fn test_lint_help() {
    skim_cmd()
        .args(["lint", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available linters:"))
        .stdout(predicate::str::contains("eslint"))
        .stdout(predicate::str::contains("ruff"))
        .stdout(predicate::str::contains("mypy"))
        .stdout(predicate::str::contains("golangci"))
        .stdout(predicate::str::contains("black"))
        .stdout(predicate::str::contains("biome"))
        .stdout(predicate::str::contains("oxlint"));
}

#[test]
fn test_lint_unknown_linter() {
    skim_cmd()
        .args(["lint", "unknown-linter"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown linter 'unknown-linter'"));
}

#[test]
fn test_lint_no_args_shows_help() {
    skim_cmd()
        .args(["lint"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Available linters:"));
}

// ============================================================================
// --show-stats integration
// ============================================================================

#[test]
fn test_lint_show_stats_reports_tokens() {
    let fixture = include_str!("fixtures/cmd/lint/eslint_fail.json");
    skim_cmd()
        .args(["lint", "--show-stats", "eslint"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stderr(predicate::str::contains("tokens"));
}

// ============================================================================
// Stdin detection with mode subcommand args (bugfix: AD-26)
//
// When a user pipes output AND specifies a mode subcommand, e.g.:
//   cat dprint_fmt_output.txt | skim lint dprint fmt
//
// The "fmt" subcommand must not prevent stdin detection. The fix strips the
// consumed mode subcommand from `args` before calling `run_linter`, so
// `args.is_empty()` is true when no file targets remain.
// ============================================================================

/// AD-26: `dprint fmt` subcommand does not block stdin detection.
#[test]
fn test_dprint_fmt_subcommand_with_piped_stdin() {
    let fixture = include_str!("fixtures/cmd/lint/dprint_fmt_output.txt");
    skim_cmd()
        .args(["lint", "dprint", "fmt"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("files formatted"));
}

/// AD-26: `dprint check` subcommand does not block stdin detection.
#[test]
fn test_dprint_check_subcommand_with_piped_stdin() {
    let fixture = include_str!("fixtures/cmd/lint/dprint_check_fail.txt");
    skim_cmd()
        .args(["lint", "dprint", "check"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"))
        .stdout(predicate::str::contains("formatting"));
}

/// AD-26: `ruff format` subcommand does not block stdin detection.
#[test]
fn test_ruff_format_subcommand_with_piped_stdin() {
    let fixture = include_str!("fixtures/cmd/lint/ruff_format_pass.txt");
    skim_cmd()
        .args(["lint", "ruff", "format"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT OK"));
}

/// AD-26: `ruff check` subcommand does not block stdin detection.
#[test]
fn test_ruff_check_subcommand_with_piped_stdin() {
    let fixture = include_str!("fixtures/cmd/lint/ruff_fail.json");
    skim_cmd()
        .args(["lint", "ruff", "check"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"));
}

/// AD-26: `biome format` subcommand does not block stdin detection.
#[test]
fn test_biome_format_subcommand_with_piped_stdin() {
    let fixture = include_str!("fixtures/cmd/lint/biome_format_fail.txt");
    skim_cmd()
        .args(["lint", "biome", "format"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"));
}

/// AD-26: `biome check` subcommand does not block stdin detection.
#[test]
fn test_biome_check_subcommand_with_piped_stdin() {
    let fixture = include_str!("fixtures/cmd/lint/biome_check_fail.json");
    skim_cmd()
        .args(["lint", "biome", "check"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("LINT:"));
}

/// AD-26: file args after a mode subcommand still trigger binary execution.
///
/// `skim lint dprint fmt .` has remaining args=["."] after stripping "fmt",
/// so `use_stdin` is false and dprint binary is invoked.
/// We can't run the binary in CI (it won't be installed), so we only verify
/// the error path is "binary not found", not "stdin read failure".
#[test]
fn test_dprint_fmt_with_file_args_invokes_binary() {
    // When file args are present, stdin should NOT be used even if piped.
    // The binary won't be installed, so we expect a "not found" style error.
    let result = skim_cmd()
        .args(["lint", "dprint", "fmt", "."])
        .write_stdin("Formatted 1 files.\n")
        .output()
        .unwrap();
    // Exit is non-zero (binary not installed) OR the output doesn't contain
    // "files formatted" (since we didn't parse stdin). Either way, stdin was
    // NOT consumed as the parse input.
    let stdout = String::from_utf8_lossy(&result.stdout);
    // If binary ran and succeeded, output would contain "files formatted" from
    // the parse result. Since dprint is not installed in CI, we check that we
    // did NOT parse the piped "Formatted 1 files." as stdin.
    // The key invariant: output does NOT contain "files formatted" from stdin parse.
    assert!(
        !stdout.contains("files formatted"),
        "stdin should not be consumed when file args are present, but output contained \
         'files formatted': {stdout}"
    );
}

// ============================================================================
// Format-mode render path: biome format success and prettier --write (E2E)
// ============================================================================

/// E2E: piping `biome format` success output produces "files formatted" in rendered output.
///
/// `Formatted N files in Xms` is matched by `RE_BIOME_FORMAT_SUCCESS` and rendered as
/// `LINT OK | biome (N files formatted)` via `LintResult::formatted`.
#[test]
fn test_biome_format_success_produces_files_formatted() {
    let fixture = include_str!("fixtures/cmd/lint/biome_format_pass.txt");
    skim_cmd()
        .args(["lint", "biome", "format"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("files formatted"));
}

/// E2E: piping `prettier --write` output produces "files formatted" in rendered output.
///
/// `prettier --write` emits one file path per line. `parse_format_impl` matches via
/// `RE_PRETTIER_WRITTEN_PATH` and calls `LintResult::formatted`, rendering as
/// `LINT OK | prettier (N files formatted)`.
#[test]
fn test_prettier_write_output_produces_files_formatted() {
    let fixture = include_str!("fixtures/cmd/lint/prettier_write_output.txt");
    skim_cmd()
        .args(["lint", "prettier", "--write"])
        .write_stdin(fixture)
        .assert()
        .success()
        .stdout(predicate::str::contains("files formatted"));
}
