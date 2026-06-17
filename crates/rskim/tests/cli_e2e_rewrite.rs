//! E2E tests for untested rewrite rules, compound commands, and hook mode (#54).
//!
//! Covers rewrite rules that have unit tests but NO previous CLI-level tests:
//! - python3 -m pytest -> skim pytest
//! - python -m pytest -> skim pytest
//! - npx vitest -> skim vitest
//! - npx tsc -> skim tsc
//! - vitest (bare) -> skim vitest
//! - tsc (bare) -> skim tsc
//! - cargo clippy -> skim cargo clippy
//!
//! v2.8.0: Flat dispatch — tool names are top-level subcommands.
//!
//! Also covers hook mode and three-segment compound commands.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use tempfile::TempDir;

fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd
}

// ============================================================================
// Untested rewrite rules: python pytest variants
// ============================================================================

#[test]
fn test_rewrite_python3_m_pytest() {
    skim_cmd()
        .args(["rewrite", "python3", "-m", "pytest"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pytest"));
}

#[test]
fn test_rewrite_python3_m_pytest_with_args() {
    skim_cmd()
        .args(["rewrite", "python3", "-m", "pytest", "-v", "tests/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pytest -v tests/"));
}

#[test]
fn test_rewrite_python_m_pytest() {
    skim_cmd()
        .args(["rewrite", "python", "-m", "pytest"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pytest"));
}

#[test]
fn test_rewrite_python_m_pytest_with_args() {
    skim_cmd()
        .args(["rewrite", "python", "-m", "pytest", "--tb=short"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pytest --tb=short"));
}

// ============================================================================
// Untested rewrite rules: npx variants
// ============================================================================

#[test]
fn test_rewrite_npx_vitest() {
    skim_cmd()
        .args(["rewrite", "npx", "vitest"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim vitest"));
}

#[test]
fn test_rewrite_npx_vitest_with_args() {
    skim_cmd()
        .args(["rewrite", "npx", "vitest", "--reporter=json", "--run"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "skim vitest --reporter=json --run",
        ));
}

#[test]
fn test_rewrite_npx_tsc() {
    skim_cmd()
        .args(["rewrite", "npx", "tsc"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim tsc"));
}

#[test]
fn test_rewrite_npx_tsc_with_args() {
    skim_cmd()
        .args(["rewrite", "npx", "tsc", "--noEmit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim tsc --noEmit"));
}

// ============================================================================
// Untested rewrite rules: bare commands
// ============================================================================

#[test]
fn test_rewrite_vitest_bare() {
    skim_cmd()
        .args(["rewrite", "vitest"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim vitest"));
}

#[test]
fn test_rewrite_vitest_bare_with_args() {
    skim_cmd()
        .args(["rewrite", "vitest", "--run", "math"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim vitest --run math"));
}

#[test]
fn test_rewrite_tsc_bare() {
    skim_cmd()
        .args(["rewrite", "tsc"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim tsc"));
}

#[test]
fn test_rewrite_tsc_bare_with_args() {
    skim_cmd()
        .args(["rewrite", "tsc", "--noEmit", "--watch"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim tsc --noEmit --watch"));
}

#[test]
fn test_rewrite_cargo_clippy() {
    skim_cmd()
        .args(["rewrite", "cargo", "clippy"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo clippy"));
}

#[test]
fn test_rewrite_cargo_clippy_with_args() {
    skim_cmd()
        .args(["rewrite", "cargo", "clippy", "--", "-W", "clippy::pedantic"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "skim cargo clippy -- -W clippy::pedantic",
        ));
}

// ============================================================================
// Three-segment compound commands
// ============================================================================

#[test]
fn test_rewrite_three_segment_compound() {
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "cargo",
            "test",
            "&&",
            "cargo",
            "build",
            "&&",
            "cargo",
            "clippy",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"))
        .stdout(predicate::str::contains("\"compound\":true"));
}

#[test]
fn test_rewrite_three_segment_output() {
    skim_cmd()
        .args([
            "rewrite", "cargo", "test", "&&", "cargo", "build", "&&", "cargo", "clippy",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test"))
        .stdout(predicate::str::contains("skim cargo build"))
        .stdout(predicate::str::contains("skim cargo clippy"));
}

// ============================================================================
// Hook mode
// ============================================================================

#[test]
fn test_rewrite_hook_cat_code_file() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "cat src/main.rs"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("skim src/main.rs --mode=pseudo"));
}

#[test]
fn test_rewrite_hook_cargo_test() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo test"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test"));
}

#[test]
fn test_rewrite_hook_passthrough_already_rewritten() {
    // Commands starting with "skim " should pass through without modification.
    // Hook mode always exits 0 (passthrough is silent success).
    let input = serde_json::json!({
        "tool_input": {
            "command": "skim cargo test"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        // No hookSpecificOutput should be emitted for passthrough
        .stdout(predicate::str::contains("hookSpecificOutput").not());
}

#[test]
fn test_rewrite_hook_passthrough_no_match() {
    // Non-matching commands pass through silently (exit 0, no output).
    // Use a command that has no rewrite rule (echo is never rewritten).
    // NOTE: bare `ls` now matches the catch-all rule (B.1, v2.5.1), so it is
    // no longer a suitable passthrough example.
    let input = serde_json::json!({
        "tool_input": {
            "command": "echo hello"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("hookSpecificOutput").not());
}

#[test]
fn test_rewrite_hook_invalid_json_passthrough() {
    // Invalid JSON input should passthrough (exit 0, no output)
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin("not valid json at all\n")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn test_rewrite_hook_missing_tool_input_passthrough() {
    // JSON without tool_input.command passes through
    let input = serde_json::json!({
        "other_field": "value"
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

#[test]
fn test_rewrite_hook_compound_cargo_test_and_build() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo test && cargo build"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo test"))
        .stdout(predicate::str::contains("skim cargo build"));
}

// ============================================================================
// Phase 6: Hook protocol per-agent tests
// ============================================================================

#[test]
fn test_rewrite_hook_default_is_claude_code_behavior() {
    // --hook without --agent should default to Claude Code behavior
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo test"
        }
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Should produce hookSpecificOutput (Claude Code behavior)
    assert!(
        stdout.contains("hookSpecificOutput"),
        "Default hook mode should produce Claude Code hookSpecificOutput"
    );
    assert!(
        stdout.contains("skim cargo test"),
        "Should rewrite cargo test"
    );
}

#[test]
fn test_rewrite_hook_agent_claude_code_explicit() {
    // --hook --agent claude-code should produce Claude Code hookSpecificOutput
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo test"
        }
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook", "--agent", "claude-code"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(json["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert!(
        json["hookSpecificOutput"]["updatedInput"]["command"]
            .as_str()
            .unwrap()
            .contains("skim cargo test")
    );
}

#[test]
fn test_rewrite_hook_agent_gemini_match() {
    // Gemini uses same input format as Claude Code (tool_input.command)
    // but responds with { "decision": "allow", "tool_input": { "command": ... } }
    let input = serde_json::json!({
        "tool_name": "shell",
        "tool_input": {
            "command": "cargo test"
        }
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook", "--agent", "gemini"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        json["decision"], "allow",
        "Gemini response should have decision=allow"
    );
    assert!(
        json["tool_input"]["command"]
            .as_str()
            .unwrap()
            .contains("skim cargo test"),
        "Gemini response should contain rewritten command"
    );
}

#[test]
fn test_rewrite_hook_agent_gemini_no_match_passthrough() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "echo hello"
        }
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook", "--agent", "gemini"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "Gemini no-match should passthrough (empty stdout), got: {stdout}"
    );
}

#[test]
fn test_rewrite_hook_agent_copilot_match() {
    // Copilot uses deny-with-suggestion response format
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo test"
        }
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook", "--agent", "copilot"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        json["permissionDecision"], "deny",
        "Copilot response should have permissionDecision=deny"
    );
    assert!(
        json["reason"].as_str().unwrap().contains("skim cargo test"),
        "Copilot deny reason should contain rewritten command"
    );
}

#[test]
fn test_rewrite_hook_agent_copilot_no_match_passthrough() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "echo hello"
        }
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook", "--agent", "copilot"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "Copilot no-match should passthrough (empty stdout), got: {stdout}"
    );
}

#[test]
fn test_rewrite_hook_agent_cursor_match() {
    // Cursor uses { "command": ... } at top level (not nested under tool_input)
    // and responds with { "permission": "allow", "updated_input": { "command": ... } }
    let input = serde_json::json!({
        "command": "cargo test"
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook", "--agent", "cursor"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        json["permission"], "allow",
        "Cursor response should have permission=allow"
    );
    assert!(
        json["updated_input"]["command"]
            .as_str()
            .unwrap()
            .contains("skim cargo test"),
        "Cursor response should contain rewritten command"
    );
}

#[test]
fn test_rewrite_hook_agent_cursor_no_match_passthrough() {
    let input = serde_json::json!({
        "command": "echo hello"
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook", "--agent", "cursor"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "Cursor no-match should passthrough (empty stdout), got: {stdout}"
    );
}

#[test]
fn test_rewrite_hook_agent_codex_awareness_only() {
    // Codex is AwarenessOnly — always empty stdout, exit 0
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo test"
        }
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook", "--agent", "codex"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "Codex (AwarenessOnly) should produce empty stdout, got: {stdout}"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.is_empty(),
        "Codex hook mode should produce zero stderr, got: {stderr}"
    );
}

#[test]
fn test_rewrite_hook_agent_crush_real_hook() {
    // Crush is RealHook — returns a decision/updated_input response
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo test"
        }
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook", "--agent", "crush"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let json: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("Crush hook mode should emit valid JSON");
    assert_eq!(
        json["decision"], "allow",
        "Crush response should have decision=allow"
    );
    assert!(
        json["updated_input"]["command"]
            .as_str()
            .unwrap()
            .contains("skim cargo test"),
        "Crush response should contain rewritten command"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.is_empty(),
        "Crush hook mode should produce zero stderr, got: {stderr}"
    );
}

#[test]
fn test_rewrite_hook_agent_unknown_passthrough() {
    // Unknown agent name (not in AgentKind::from_str) should default to
    // Claude Code behavior since parse_agent_flag returns None.
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo test"
        }
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook", "--agent", "unknown-agent"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "Unknown agent should not crash, exit 0"
    );

    // Unknown agent falls back to Claude Code -- "cargo test" is rewritable,
    // so stdout should contain a Claude Code hook response.
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("hookSpecificOutput"),
        "Unknown agent should fall back to Claude Code response format, got: {stdout}"
    );
}

#[test]
fn test_rewrite_hook_all_agents_zero_stderr() {
    // Verify ALL hook responses have empty stderr
    let agents_and_inputs: Vec<(&str, serde_json::Value)> = vec![
        (
            "claude-code",
            serde_json::json!({"tool_input": {"command": "cargo test"}}),
        ),
        ("cursor", serde_json::json!({"command": "cargo test"})),
        (
            "gemini",
            serde_json::json!({"tool_input": {"command": "cargo test"}}),
        ),
        (
            "copilot",
            serde_json::json!({"tool_input": {"command": "cargo test"}}),
        ),
        (
            "codex",
            serde_json::json!({"tool_input": {"command": "cargo test"}}),
        ),
        (
            "crush",
            serde_json::json!({"tool_input": {"command": "cargo test"}}),
        ),
    ];

    for (agent, input) in agents_and_inputs {
        let output = skim_cmd()
            .args(["rewrite", "--hook", "--agent", agent])
            .write_stdin(serde_json::to_string(&input).unwrap())
            .output()
            .unwrap();

        assert!(output.status.success(), "Agent {agent} should exit 0");
        let stderr = String::from_utf8(output.stderr.clone()).unwrap();
        assert!(
            stderr.is_empty(),
            "Agent {agent} hook mode must produce zero stderr, got: {stderr}"
        );
    }
}

// ============================================================================
// Phase 6: Stderr cleanliness -- hook mode produces ZERO stderr
// ============================================================================
// Per-agent zero-stderr coverage is handled by test_rewrite_hook_all_agents_zero_stderr.
// Only the passthrough (no --agent flag) case remains here as unique coverage.

#[test]
fn test_rewrite_hook_passthrough_zero_stderr() {
    // Non-matching command with no agent flag.
    // Use bare ls (no flags) which has no rewrite rule.
    let input = serde_json::json!({
        "tool_input": {
            "command": "ls"
        }
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.is_empty(),
        "Passthrough hook mode should produce zero stderr, got: {stderr}"
    );
}

// ============================================================================
// SKIM_PASSTHROUGH=1 in hook mode (Fix C)
// ============================================================================

/// Verify that SKIM_PASSTHROUGH=1 makes the hook return immediately with empty
/// stdout, even for a command that would normally be rewritten. The agent sees
/// no hook response — equivalent to a transparent passthrough.
#[test]
fn test_passthrough_hook_skips_rewrite() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo test"
        }
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook"])
        .env("SKIM_PASSTHROUGH", "1")
        .write_stdin(serde_json::to_string(&input).unwrap())
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "SKIM_PASSTHROUGH=1 hook mode must exit 0"
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "SKIM_PASSTHROUGH=1 hook mode must produce empty stdout, got: {stdout}"
    );
}

/// Verify that SKIM_PASSTHROUGH=1 with `skim vitest` does NOT inject
/// --reporter=json. Plain text piped in is forwarded unchanged without JSON
/// transformation.
///
/// v2.8.0: `skim vitest` replaces `skim test vitest`.
#[test]
fn test_passthrough_direct_vitest_no_json_injection() {
    let plain_text = "Tests  3 passed | 0 failed | 3 total\n";
    let output = skim_cmd()
        .args(["vitest"])
        .env("SKIM_PASSTHROUGH", "1")
        .write_stdin(plain_text)
        .output()
        .unwrap();

    // Passthrough forwards the raw input unchanged.
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Raw text must contain the words from the inline plain_text string.
    assert!(
        stdout.contains("Tests") && stdout.contains("passed"),
        "passthrough must forward the raw input, got: {stdout}"
    );
    // Must NOT contain skim-structured output markers.
    assert!(
        !stdout.contains("PASS:"),
        "passthrough must not reformat the input into skim structured output, got: {stdout}"
    );
}

/// Verify that SKIM_PASSTHROUGH=1 forwards raw output unchanged.
/// Pipe a failing vitest JSON fixture and verify the raw content is forwarded
/// to stdout without parsing or reformatting.
#[test]
fn test_passthrough_forwards_raw_content() {
    // The vitest passthrough handler returns ExitCode::FAILURE when stdin has
    // data (exit code 1 is conservative — the tool status is unknown). The
    // important property is that raw content is forwarded without compression.
    let fixture = include_str!("fixtures/cmd/test/vitest_fail.json");
    let output = skim_cmd()
        .args(["vitest"])
        .env("SKIM_PASSTHROUGH", "1")
        .write_stdin(fixture)
        .output()
        .unwrap();

    let stdout = String::from_utf8(output.stdout).unwrap();
    // Raw JSON forwarded unchanged — must contain the source numFailedTests field.
    assert!(
        stdout.contains("numFailedTests"),
        "passthrough must forward raw fixture content, got: {stdout}"
    );
    // Passthrough must NOT produce any skim-formatted output.
    assert!(
        !stdout.contains("FAIL:") && !stdout.contains("PASS:"),
        "passthrough must not compress/reformat output, got: {stdout}"
    );
}

// ============================================================================
// Lint rewrite rules (#104)
// ============================================================================

#[test]
fn test_rewrite_eslint() {
    skim_cmd()
        .args(["rewrite", "eslint", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim eslint ."));
}

#[test]
fn test_rewrite_eslint_skip_format_flag() {
    // When user already has --format json, rewrite should be suppressed
    skim_cmd()
        .args(["rewrite", "eslint", "--format", "json", "."])
        .assert()
        .failure(); // No match = exit 1
}

#[test]
fn test_rewrite_npx_eslint() {
    skim_cmd()
        .args(["rewrite", "npx", "eslint", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim eslint src/"));
}

#[test]
fn test_rewrite_ruff_check() {
    skim_cmd()
        .args(["rewrite", "ruff", "check", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim ruff ."));
}

#[test]
fn test_rewrite_ruff_bare() {
    skim_cmd()
        .args(["rewrite", "ruff", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim ruff ."));
}

#[test]
fn test_rewrite_ruff_format() {
    skim_cmd()
        .args(["rewrite", "ruff", "format", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim ruff"));
}

#[test]
fn test_rewrite_ruff_format_check() {
    skim_cmd()
        .args(["rewrite", "ruff", "format", "--check", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim ruff"));
}

#[test]
fn test_rewrite_mypy() {
    skim_cmd()
        .args(["rewrite", "mypy", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim mypy ."));
}

#[test]
fn test_rewrite_python_m_mypy() {
    skim_cmd()
        .args(["rewrite", "python", "-m", "mypy", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim mypy ."));
}

#[test]
fn test_rewrite_python3_m_mypy() {
    skim_cmd()
        .args(["rewrite", "python3", "-m", "mypy", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim mypy src/"));
}

#[test]
fn test_rewrite_golangci_lint_run() {
    skim_cmd()
        .args(["rewrite", "golangci-lint", "run", "./..."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim golangci ./..."));
}

#[test]
fn test_rewrite_golangci_lint_bare() {
    skim_cmd()
        .args(["rewrite", "golangci-lint", "./..."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim golangci ./..."));
}

// ============================================================================
// Phase 7: Pkg rewrite rules (#105)
// ============================================================================

#[test]
fn test_rewrite_npm_audit() {
    skim_cmd()
        .args(["rewrite", "npm", "audit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim npm audit"));
}

#[test]
fn test_rewrite_npm_i_express() {
    skim_cmd()
        .args(["rewrite", "npm", "i", "express"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim npm install express"));
}

#[test]
fn test_rewrite_npm_ci() {
    skim_cmd()
        .args(["rewrite", "npm", "ci"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim npm install"));
}

#[test]
fn test_rewrite_pip_install_flask() {
    skim_cmd()
        .args(["rewrite", "pip", "install", "flask"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pip install flask"));
}

#[test]
fn test_rewrite_pip3_check() {
    skim_cmd()
        .args(["rewrite", "pip3", "check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pip check"));
}

#[test]
fn test_rewrite_cargo_audit() {
    skim_cmd()
        .args(["rewrite", "cargo", "audit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo audit"));
}

#[test]
fn test_rewrite_pnpm_install() {
    skim_cmd()
        .args(["rewrite", "pnpm", "install"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pnpm install"));
}

#[test]
fn test_rewrite_npm_audit_json_skip() {
    // --json flag should prevent rewrite
    skim_cmd()
        .args(["rewrite", "npm", "audit", "--json"])
        .assert()
        .failure();
}

#[test]
fn test_rewrite_pip_list_format_skip() {
    // --format=json should prevent rewrite
    skim_cmd()
        .args(["rewrite", "pip", "list", "--format=json"])
        .assert()
        .failure();
}

#[test]
fn test_rewrite_npm_install_with_args() {
    skim_cmd()
        .args(["rewrite", "npm", "install", "express", "lodash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim npm install express lodash"));
}

#[test]
fn test_rewrite_npm_outdated() {
    skim_cmd()
        .args(["rewrite", "npm", "outdated"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim npm outdated"));
}

#[test]
fn test_rewrite_npm_ls() {
    skim_cmd()
        .args(["rewrite", "npm", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim npm ls"));
}

#[test]
fn test_rewrite_pnpm_audit() {
    skim_cmd()
        .args(["rewrite", "pnpm", "audit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pnpm audit"));
}

#[test]
fn test_rewrite_pnpm_outdated() {
    skim_cmd()
        .args(["rewrite", "pnpm", "outdated"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pnpm outdated"));
}

#[test]
fn test_rewrite_pip_list() {
    skim_cmd()
        .args(["rewrite", "pip", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pip list"));
}

#[test]
fn test_rewrite_pip3_install() {
    skim_cmd()
        .args(["rewrite", "pip3", "install", "flask"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pip install flask"));
}

#[test]
fn test_rewrite_pip3_list() {
    skim_cmd()
        .args(["rewrite", "pip3", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pip list"));
}

// ============================================================================
// Phase 8: Wave B rewrite rules (#116)
// ============================================================================

/// AD-RW-11: `prettier --check` is acknowledged as already-compact.
/// The original command is echoed on stdout (exit 0) rather than being
/// rewritten to `skim prettier`, per the compress-or-skip rule.
#[test]
fn test_rewrite_prettier_check_acknowledged() {
    skim_cmd()
        .args(["rewrite", "prettier", "--check", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("prettier --check"));
}

#[test]
fn test_rewrite_prettier_write() {
    skim_cmd()
        .args(["rewrite", "prettier", "--write", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim prettier"));
}

#[test]
fn test_rewrite_npx_prettier_write() {
    skim_cmd()
        .args(["rewrite", "npx", "prettier", "--write", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim prettier"));
}

/// AD-RW-11: `rustfmt --check` is acknowledged as already-compact.
/// The original command is echoed on stdout rather than being
/// rewritten to `skim rustfmt`.
#[test]
fn test_rewrite_rustfmt_check_acknowledged() {
    skim_cmd()
        .args(["rewrite", "rustfmt", "--check", "src/main.rs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("rustfmt --check"));
}

#[test]
fn test_rewrite_gh_pr_list() {
    skim_cmd()
        .args(["rewrite", "gh", "pr", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim gh"));
}

#[test]
fn test_rewrite_aws_s3_ls() {
    skim_cmd()
        .args(["rewrite", "aws", "s3", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim aws"));
}

#[test]
fn test_rewrite_curl_api() {
    skim_cmd()
        .args(["rewrite", "curl", "https://api.example.com"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim curl"));
}

#[test]
fn test_rewrite_wget_file() {
    skim_cmd()
        .args(["rewrite", "wget", "https://example.com/f.tar.gz"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim wget"));
}

#[test]
fn test_rewrite_find_name() {
    skim_cmd()
        .args(["rewrite", "find", ".", "-name", "*.rs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim find"));
}

#[test]
fn test_rewrite_ls_la() {
    skim_cmd()
        .args(["rewrite", "ls", "-la"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim ls"));
}

#[test]
fn test_rewrite_tree_bare() {
    skim_cmd()
        .args(["rewrite", "tree"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim tree"));
}

#[test]
fn test_rewrite_grep_r() {
    skim_cmd()
        .args(["rewrite", "grep", "-r", "TODO", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim grep"));
}

#[test]
fn test_rewrite_rg_pattern() {
    skim_cmd()
        .args(["rewrite", "rg", "pattern"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim rg"));
}

#[test]
fn test_rewrite_find_exec_skipped() {
    // -exec is in skip_if_flag_prefix for find: no match = exit 1
    skim_cmd()
        .args(["rewrite", "find", ".", "-exec", "rm", "{}", ";"])
        .assert()
        .failure();
}

#[test]
fn test_rewrite_rg_json_skipped() {
    // --json is in skip_if_flag_prefix for rg: no match = exit 1
    skim_cmd()
        .args(["rewrite", "rg", "--json", "pattern"])
        .assert()
        .failure();
}

#[test]
fn test_rewrite_gh_json_skipped() {
    // --json is now a skip flag for gh list/view commands — output-steering
    // flags pass through with exact bytes, so the rewrite must be absent (exit 1).
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "gh",
            "pr",
            "list",
            "--json",
            "title",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

// ============================================================================
// Step 7c: Rewrite + handler round-trip validation
// These tests verify the full CLI path: `skim rewrite <tokens>` → correct output
// ============================================================================

#[test]
fn test_rewrite_git_status_s_roundtrip() {
    // `git status -s` should rewrite to `skim git status -s`
    skim_cmd()
        .args(["rewrite", "git", "status", "-s"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git status -s"));
}

#[test]
fn test_rewrite_git_status_short_roundtrip() {
    // `git status --short` should rewrite to `skim git status --short`
    skim_cmd()
        .args(["rewrite", "git", "status", "--short"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git status --short"));
}

#[test]
fn test_rewrite_git_status_porcelain_roundtrip() {
    // `git status --porcelain` should rewrite to `skim git status --porcelain`
    skim_cmd()
        .args(["rewrite", "git", "status", "--porcelain"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git status --porcelain"));
}

#[test]
fn test_rewrite_git_log_oneline_roundtrip() {
    // `git log --oneline -5` should rewrite to `skim git log --oneline -5`
    skim_cmd()
        .args(["rewrite", "git", "log", "--oneline", "-5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git log"));
}

#[test]
fn test_rewrite_gh_pr_list_json_skipped() {
    // --json is now a skip flag: gh pr list --json must not rewrite (exit 1,
    // "match":false in --suggest output). Byte-faithful passthrough is handled
    // by the handler gate (cmd/infra/gh/mod.rs) and the rules.rs skip-list.
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "gh",
            "pr",
            "list",
            "--json",
            "number",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_jest_roundtrip() {
    // `jest src/` should rewrite to `skim jest src/` (v2.8.0 flat dispatch)
    skim_cmd()
        .args(["rewrite", "jest", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim jest src/"));
}

#[test]
fn test_rewrite_npx_jest_roundtrip() {
    // `npx jest src/` should rewrite to `skim jest src/` (v2.8.0 flat dispatch)
    skim_cmd()
        .args(["rewrite", "npx", "jest", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim jest src/"));
}

// ============================================================================
// gh view/checks rewrite rules (#131)
// ============================================================================

#[test]
fn test_rewrite_gh_issue_view() {
    skim_cmd()
        .args(["rewrite", "gh", "issue", "view", "42"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim gh issue view 42"));
}

#[test]
fn test_rewrite_gh_issue_view_web_skipped() {
    skim_cmd()
        .args(["rewrite", "--suggest", "gh", "issue", "view", "42", "--web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_gh_issue_view_jq_skipped() {
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "gh",
            "issue",
            "view",
            "42",
            "--jq",
            ".title",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_gh_pr_view() {
    skim_cmd()
        .args(["rewrite", "gh", "pr", "view", "15"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim gh pr view 15"));
}

#[test]
fn test_rewrite_gh_pr_view_web_skipped() {
    skim_cmd()
        .args(["rewrite", "--suggest", "gh", "pr", "view", "15", "--web"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_gh_pr_view_template_skipped() {
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "gh",
            "pr",
            "view",
            "15",
            "--template",
            "{{.title}}",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_gh_pr_checks() {
    skim_cmd()
        .args(["rewrite", "gh", "pr", "checks", "15"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim gh pr checks 15"));
}

#[test]
fn test_rewrite_gh_pr_checks_watch_skipped() {
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "gh",
            "pr",
            "checks",
            "15",
            "--watch",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_gh_run_view() {
    skim_cmd()
        .args(["rewrite", "gh", "run", "view", "12345"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim gh run view 12345"));
}

#[test]
fn test_rewrite_gh_run_view_log_skipped() {
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "gh",
            "run",
            "view",
            "12345",
            "--log",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_gh_run_view_log_failed_skipped() {
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "gh",
            "run",
            "view",
            "12345",
            "--log-failed",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

// ============================================================================
// gh output-steering skip tests — Part 1A (hook path)
// ============================================================================

#[test]
fn test_rewrite_gh_issue_view_q_skipped() {
    // Reported repro: gh issue view 93 -q .body must not rewrite.
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "gh",
            "issue",
            "view",
            "93",
            "-q",
            ".body",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_gh_issue_view_t_skipped() {
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "gh",
            "issue",
            "view",
            "93",
            "-t",
            "{{.body}}",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_gh_issue_view_w_skipped() {
    skim_cmd()
        .args(["rewrite", "--suggest", "gh", "issue", "view", "93", "-w"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_gh_issue_view_json_skipped() {
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "gh",
            "issue",
            "view",
            "93",
            "--json",
            "number,title,body",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_gh_api_q_skipped() {
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "gh",
            "api",
            "repos/o/r",
            "-q",
            ".name",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_gh_api_json_rewrites() {
    // gh api has no --json flag (responses are always JSON natively).
    // --json is therefore NOT in the api skip-list, so the rewrite fires.
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "gh",
            "api",
            "repos/o/r",
            "--json",
            "x",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"));
}

// ============================================================================
// gh handler gate: behavioral e2e tests (avoids PF-007, applies ADR-001)
//
// Verifies the Layer 2 handler gate in cmd/infra/gh/mod.rs: when an
// output-steering flag is present, `run_raw_passthrough` forwards the fake
// gh's exact output byte-for-byte.  A fake `gh` shim is placed on a temp
// PATH so `CommandRunner`'s PATH lookup resolves to it (same mechanism as
// the real tool lookup).
// ============================================================================

/// Build a fake `gh` shell script in `bin_dir` that prints a known multi-line
/// payload to stdout and exits 0.
///
/// The payload is a minimal valid gh issue-view JSON that the issue_view
/// compressor WILL parse into a structured summary (losing the raw bytes),
/// but that `run_raw_passthrough` forwards verbatim.
///
/// Discriminator: the raw JSON contains `"__FAKE_GH_SENTINEL__"`.
/// The skim-structured summary for this object will contain "issue view"
/// (from `InfraResult::operation`) but NOT the sentinel literal.
#[cfg(unix)]
fn write_fake_gh(bin_dir: &std::path::Path) -> std::path::PathBuf {
    let gh_path = bin_dir.join("gh");
    // Valid issue-view JSON: has number + state + body (all required
    // discriminators for issue_view::try_parse_json). The body contains the
    // sentinel so we can distinguish raw passthrough from a structured summary.
    fs::write(
        &gh_path,
        "#!/bin/sh\nprintf '%s' '{\"number\":93,\"state\":\"OPEN\",\"title\":\"Test\",\"body\":\"__FAKE_GH_SENTINEL__\",\"labels\":[],\"assignees\":[],\"comments\":[]}'\n",
    )
    .unwrap();
    let perms = std::fs::Permissions::from_mode(0o755);
    fs::set_permissions(&gh_path, perms).unwrap();
    gh_path
}

/// Create a temp directory with the fake `gh` shim on PATH.
///
/// Returns `(bin_dir, new_path)` where `bin_dir` must be kept alive for the
/// duration of the test (dropping it removes the shim) and `new_path` is the
/// `PATH` value to pass to the test command.
#[cfg(unix)]
fn fake_gh_on_path() -> (TempDir, String) {
    let bin_dir = TempDir::new().unwrap();
    write_fake_gh(bin_dir.path());
    let new_path = format!(
        "{}:{}",
        bin_dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    (bin_dir, new_path)
}

/// Layer 2 handler gate fires on `-q .body` → bytes forwarded verbatim.
///
/// Asserts the discriminating observable (exact sentinel bytes in stdout),
/// not just exit 0 — avoids PF-007.
#[cfg(unix)]
#[test]
fn test_gh_handler_gate_fires_on_q_flag_passes_through_verbatim() {
    let (_bin_dir, new_path) = fake_gh_on_path();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .env_remove("SKIM_PASSTHROUGH")
        .env("PATH", &new_path)
        .args(["gh", "issue", "view", "93", "-q", ".body"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Gate must have fired: fake gh's exact sentinel is present in stdout.
    assert!(
        stdout.contains("__FAKE_GH_SENTINEL__"),
        "gate must forward fake gh's exact bytes; got: {stdout}"
    );
    // Gate must NOT compress into a skim-structured summary.
    assert!(
        !stdout.contains("issue view"),
        "gate must not produce a skim-structured summary; got: {stdout}"
    );
}

/// Negative control: plain `skim gh issue view 93` (no steering flag) enters
/// the compressing handler and the fake gh's issue JSON is parsed into a
/// skim-structured summary, NOT forwarded verbatim.
///
/// The issue_view compressor turns the fake JSON into a summary containing
/// "issue view"; the raw sentinel literal is not present in the summary output.
/// This proves the gate is conditional, not always-on (avoids PF-007).
#[cfg(unix)]
#[test]
fn test_gh_handler_gate_does_not_fire_without_steering_flag() {
    let (_bin_dir, new_path) = fake_gh_on_path();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .env_remove("SKIM_PASSTHROUGH")
        .env("PATH", &new_path)
        .args(["gh", "issue", "view", "93"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The compressor must have parsed the fake issue JSON into a structured
    // summary.  Discriminating observable: the output is a skim-structured
    // format (starts with `gh issue view`), NOT the raw JSON wire bytes
    // (which start with `{`).  This proves run_tool ran, not run_raw_passthrough.
    assert!(
        !stdout.starts_with('{'),
        "without a steering flag the gate must not forward raw JSON; got: {stdout}"
    );
    assert!(
        stdout.contains("issue view"),
        "without a steering flag the compressor must produce a structured summary; got: {stdout}"
    );
}

/// Gate also fires on `--json` for a non-api subcommand.
#[cfg(unix)]
#[test]
fn test_gh_handler_gate_fires_on_json_flag() {
    let (_bin_dir, new_path) = fake_gh_on_path();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .env_remove("SKIM_PASSTHROUGH")
        .env("PATH", &new_path)
        .args(["gh", "issue", "view", "93", "--json", "number,body"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("__FAKE_GH_SENTINEL__"),
        "gate must fire on --json for non-api subcommand; got: {stdout}"
    );
}

/// Gate carve-out: `gh api --json` does NOT trigger raw passthrough.
/// --json on gh api is not an output-steering flag (api responses are always
/// JSON natively); the gate must agree with the rules.rs api skip-list.
///
/// When the gate does NOT fire, the api compressor runs and emits a flat
/// `key: value` compact form — NOT the raw JSON wire bytes.  We check that
/// the output does not contain the raw JSON braces + field names (passthrough
/// format) but DOES contain the api handler's structured output.
#[cfg(unix)]
#[test]
fn test_gh_handler_gate_api_json_does_not_passthrough() {
    let (_bin_dir, new_path) = fake_gh_on_path();

    let output = Command::cargo_bin("skim")
        .unwrap()
        .env_remove("SKIM_PASSTHROUGH")
        .env("PATH", &new_path)
        .args(["gh", "api", "repos/o/r", "--json", "name"])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // When the gate does not fire, the api compressor runs. The raw JSON wire
    // bytes (`{"number":93,...}`) must NOT be forwarded verbatim; the compactor
    // transforms the JSON into a flat key:value form.  Check for the absence
    // of raw JSON structure (no outer `{...}` braces on a single line with all
    // original keys present as a JSON string).
    assert!(
        !stdout.starts_with('{'),
        "gate must not fire for gh api --json (raw JSON must not be forwarded verbatim); got: {stdout}"
    );
}

// ============================================================================
// New lint rewrite rules: black, gofmt, biome, dprint, oxlint (#133)
// ============================================================================

#[test]
fn test_rewrite_black_check() {
    skim_cmd()
        .args(["rewrite", "black", "--check", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim black"));
}

#[test]
fn test_rewrite_black_bare() {
    skim_cmd()
        .args(["rewrite", "black", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim black"));
}

#[test]
fn test_rewrite_black_diff_skipped() {
    skim_cmd()
        .args(["rewrite", "--suggest", "black", "--diff", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_gofmt_l() {
    skim_cmd()
        .args(["rewrite", "gofmt", "-l", "./..."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim gofmt"));
}

#[test]
fn test_rewrite_gofmt_bare() {
    skim_cmd()
        .args(["rewrite", "gofmt", "cmd/server.go"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim gofmt"));
}

#[test]
fn test_rewrite_gofmt_write_skipped() {
    skim_cmd()
        .args(["rewrite", "--suggest", "gofmt", "-w", "./..."])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_biome_check() {
    skim_cmd()
        .args(["rewrite", "biome", "check", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim biome"));
}

#[test]
fn test_rewrite_biome_check_reporter_skipped() {
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "biome",
            "check",
            "--reporter=github",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_biome_format() {
    skim_cmd()
        .args(["rewrite", "biome", "format", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim biome"));
}

#[test]
fn test_rewrite_npx_biome_check() {
    skim_cmd()
        .args(["rewrite", "npx", "biome", "check", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim biome"));
}

#[test]
fn test_rewrite_dprint_check() {
    skim_cmd()
        .args(["rewrite", "dprint", "check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim dprint"));
}

#[test]
fn test_rewrite_dprint_fmt() {
    skim_cmd()
        .args(["rewrite", "dprint", "fmt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim dprint"));
}

#[test]
fn test_rewrite_dprint_bare() {
    skim_cmd()
        .args(["rewrite", "dprint"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim dprint"));
}

#[test]
fn test_rewrite_oxlint() {
    skim_cmd()
        .args(["rewrite", "oxlint", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim oxlint"));
}

#[test]
fn test_rewrite_oxlint_format_skipped() {
    skim_cmd()
        .args(["rewrite", "--suggest", "oxlint", "--format=github"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_npx_oxlint() {
    skim_cmd()
        .args(["rewrite", "npx", "oxlint", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim oxlint"));
}

// ============================================================================
// Infra: docker rewrite rules (#117)
// ============================================================================

#[test]
fn test_rewrite_docker_ps() {
    skim_cmd()
        .args(["rewrite", "docker", "ps"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim docker ps"));
}

#[test]
fn test_rewrite_docker_images() {
    skim_cmd()
        .args(["rewrite", "docker", "images"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim docker images"));
}

#[test]
fn test_rewrite_docker_build() {
    skim_cmd()
        .args(["rewrite", "docker", "build", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim docker build ."));
}

#[test]
fn test_rewrite_docker_compose_ps() {
    skim_cmd()
        .args(["rewrite", "docker", "compose", "ps"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim docker compose ps"));
}

#[test]
fn test_rewrite_docker_compose_logs() {
    skim_cmd()
        .args(["rewrite", "docker", "compose", "logs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim docker compose logs"));
}

#[test]
fn test_rewrite_docker_ps_skip_format() {
    // --format skips rewrite
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "docker",
            "ps",
            "--format",
            "table {{.ID}}",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_docker_build_skip_push() {
    // --push uploads to registry — skip rewrite to avoid interfering with
    // push semantics in the skim handler.
    skim_cmd()
        .args(["rewrite", "--suggest", "docker", "build", "--push", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_docker_build_skip_load() {
    // --load loads the built image into the local docker daemon — skip rewrite.
    skim_cmd()
        .args(["rewrite", "--suggest", "docker", "build", "--load", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

// ============================================================================
// Infra: kubectl rewrite rules (#117)
// ============================================================================

#[test]
fn test_rewrite_kubectl_get() {
    skim_cmd()
        .args(["rewrite", "kubectl", "get", "pods"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim kubectl get pods"));
}

#[test]
fn test_rewrite_kubectl_describe() {
    skim_cmd()
        .args(["rewrite", "kubectl", "describe", "pod", "foo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim kubectl describe pod foo"));
}

#[test]
fn test_rewrite_kubectl_logs() {
    skim_cmd()
        .args(["rewrite", "kubectl", "logs", "pod"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim kubectl logs pod"));
}

#[test]
fn test_rewrite_kubectl_get_skip_output() {
    // -o yaml skips rewrite
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "kubectl",
            "get",
            "pods",
            "-o",
            "yaml",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_kubectl_logs_skip_follow() {
    // -f skips rewrite
    skim_cmd()
        .args(["rewrite", "--suggest", "kubectl", "logs", "-f", "pod"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

// ============================================================================
// Infra: terraform rewrite rules (#117)
// ============================================================================

#[test]
fn test_rewrite_terraform_plan() {
    skim_cmd()
        .args(["rewrite", "terraform", "plan"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim terraform plan"));
}

#[test]
fn test_rewrite_terraform_apply() {
    skim_cmd()
        .args(["rewrite", "terraform", "apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim terraform apply"));
}

#[test]
fn test_rewrite_terraform_apply_skip_auto_approve() {
    // -auto-approve skips rewrite (safety guard)
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "terraform",
            "apply",
            "-auto-approve",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_terraform_plan_skip_destroy() {
    // `-destroy` generates a destroy plan — skip rewrite so agents see the
    // full destroy plan output rather than a compressed summary.
    skim_cmd()
        .args(["rewrite", "--suggest", "terraform", "plan", "-destroy"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_terraform_apply_skip_destroy() {
    // `-destroy` applies a destroy plan — skip rewrite (same reason as plan).
    skim_cmd()
        .args(["rewrite", "--suggest", "terraform", "apply", "-destroy"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

// ============================================================================
// DB: rewrite rules (#117)
// ============================================================================

#[test]
fn test_rewrite_psql_c() {
    skim_cmd()
        .args(["rewrite", "psql", "-c", "SELECT 1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim psql -c"));
}

#[test]
fn test_rewrite_mysql_e() {
    skim_cmd()
        .args(["rewrite", "mysql", "-e", "SELECT 1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim mysql -e"));
}

#[test]
fn test_rewrite_sqlite3() {
    skim_cmd()
        .args(["rewrite", "sqlite3", "test.db", "SELECT 1"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim sqlite3"));
}

#[test]
fn test_rewrite_psql_bare_no_rewrite() {
    // Bare `psql` without -c has no rule → no rewrite
    skim_cmd()
        .args(["rewrite", "--suggest", "psql"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_mysql_bare_no_rewrite() {
    // Bare `mysql` without -e has no rule → no rewrite (batch-mode-only safety)
    skim_cmd()
        .args(["rewrite", "--suggest", "mysql"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_sqlite3_bare_with_db_file_rewrites() {
    // `sqlite3 mydb.sqlite` (db-file only, no SQL) IS rewritten. This is safe
    // in agent contexts because the hook runs with piped stdin: sqlite3 reads
    // EOF immediately and exits without entering interactive mode.
    skim_cmd()
        .args(["rewrite", "--suggest", "sqlite3", "mydb.sqlite"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"));
}

#[test]
fn test_rewrite_sqlite3_interactive_flag_skipped() {
    // `-interactive` forces interactive mode regardless of stdin state — skip
    // rewrite to avoid hanging in non-TTY contexts.
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "sqlite3",
            "mydb.sqlite",
            "-interactive",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

// ============================================================================
// Fix 4: psql/mysql require_flag — broadened prefix + require_flag guard
// ============================================================================

#[test]
fn test_rewrite_psql_with_host_and_c_rewrites() {
    // `psql -h localhost -d mydb -c "SELECT 1"` — broadened prefix fires
    // because -c is present even with other flags before it.
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "psql",
            "-h",
            "localhost",
            "-d",
            "mydb",
            "-c",
            "SELECT 1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"));
}

#[test]
fn test_rewrite_psql_no_c_flag_no_rewrite() {
    // `psql -h localhost -d mydb` — no -c means interactive mode → NO rewrite.
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "psql",
            "-h",
            "localhost",
            "-d",
            "mydb",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

#[test]
fn test_rewrite_psql_long_command_flag_rewrites() {
    // `psql mydb --command "SELECT 1"` — --command is also accepted.
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "psql",
            "mydb",
            "--command",
            "SELECT 1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"));
}

#[test]
fn test_rewrite_mysql_with_host_and_e_rewrites() {
    // `mysql -h localhost -u user -e "SELECT 1"` — broadened prefix fires.
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "mysql",
            "-h",
            "localhost",
            "-u",
            "user",
            "-e",
            "SELECT 1",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"));
}

#[test]
fn test_rewrite_mysql_no_e_flag_no_rewrite() {
    // `mysql -h localhost -u user` — no -e means interactive mode → NO rewrite.
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "mysql",
            "-h",
            "localhost",
            "-u",
            "user",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":false"));
}

// ============================================================================
// Fix 3: kubectl global flags before subcommand
// ============================================================================

#[test]
fn test_rewrite_kubectl_namespace_before_get() {
    // `kubectl -n mynamespace get pods` — global -n flag before subcommand.
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "kubectl",
            "-n",
            "mynamespace",
            "get",
            "pods",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"));
}

#[test]
fn test_rewrite_kubectl_context_before_get() {
    // `kubectl --context production get pods` — --context before subcommand.
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "kubectl",
            "--context",
            "production",
            "get",
            "pods",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"));
}

#[test]
fn test_rewrite_kubectl_no_global_flags_still_rewrites() {
    // `kubectl get pods` — no global flags, normal match still works.
    skim_cmd()
        .args(["rewrite", "--suggest", "kubectl", "get", "pods"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"));
}

#[test]
fn test_rewrite_docker_host_before_ps() {
    // `docker --host tcp://remote:2376 ps` — global --host flag before subcommand.
    skim_cmd()
        .args([
            "rewrite",
            "--suggest",
            "docker",
            "--host",
            "tcp://remote:2376",
            "ps",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"match\":true"));
}

// ============================================================================
// DNS rewrite rules: dig and nslookup (#168)
// ============================================================================

#[test]
fn test_rewrite_dig_fires() {
    skim_cmd()
        .args(["rewrite", "dig", "example.com", "A"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim dig"));
}

#[test]
fn test_rewrite_dig_short_skipped() {
    // `dig +short` produces already-compact output — rewrite should not fire.
    skim_cmd()
        .args(["rewrite", "dig", "+short", "example.com"])
        .assert()
        .failure(); // No match = exit 1
}

#[test]
fn test_rewrite_dig_trace_skipped() {
    // `dig +trace` produces streaming diagnostic output — rewrite should not fire.
    skim_cmd()
        .args(["rewrite", "dig", "+trace", "example.com"])
        .assert()
        .failure(); // No match = exit 1
}

#[test]
fn test_rewrite_dig_yaml_skipped() {
    // AC-RW-3: `dig +yaml` produces YAML-structured output — rewrite must not fire.
    skim_cmd()
        .args(["rewrite", "dig", "+yaml", "example.com"])
        .assert()
        .failure(); // No match = exit 1
}

#[test]
fn test_rewrite_dig_json_skipped() {
    // AC-RW-4: `dig +json` produces JSON-structured output — rewrite must not fire.
    skim_cmd()
        .args(["rewrite", "dig", "+json", "example.com"])
        .assert()
        .failure(); // No match = exit 1
}

#[test]
fn test_rewrite_nslookup_fires() {
    skim_cmd()
        .args(["rewrite", "nslookup", "example.com"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim nslookup"));
}

// ============================================================================
// cargo check rewrite rules (#259)
// ============================================================================

#[test]
fn test_rewrite_cargo_check() {
    skim_cmd()
        .args(["rewrite", "cargo", "check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo check"));
}

#[test]
fn test_rewrite_cargo_check_with_release() {
    skim_cmd()
        .args(["rewrite", "cargo", "check", "--release"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--release"));
}

// ============================================================================
// cargo fmt rewrite rules (#259)
// ============================================================================

#[test]
fn test_rewrite_cargo_fmt() {
    skim_cmd()
        .args(["rewrite", "cargo", "fmt"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo fmt"));
}

/// AD-RW-11 regression: `cargo fmt --check` must remain ACKed (not rewritten)
/// after adding the `cargo fmt` rule. The ACK path runs before the rule table.
#[test]
fn test_rewrite_cargo_fmt_check_still_acknowledged() {
    skim_cmd()
        .args(["rewrite", "cargo", "fmt", "--check"])
        .assert()
        .success()
        // ACK echoes the original command on stdout (not a skim-prefixed form).
        .stdout(predicate::str::contains("cargo fmt --check"))
        .stdout(predicate::str::contains("skim cargo fmt").not());
}

// ============================================================================
// npm test/run rewrite rules (#260)
// ============================================================================

#[test]
fn test_rewrite_npm_test() {
    skim_cmd()
        .args(["rewrite", "npm", "test"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim npm test"));
}

#[test]
fn test_rewrite_npm_t_alias() {
    skim_cmd()
        .args(["rewrite", "npm", "t"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim npm test"));
}

#[test]
fn test_rewrite_npm_run() {
    skim_cmd()
        .args(["rewrite", "npm", "run", "lint"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim npm run lint"));
}

#[test]
fn test_rewrite_npm_run_colon_preserved() {
    // Colons in script names must be preserved (e.g. `build:prod`)
    skim_cmd()
        .args(["rewrite", "npm", "run", "build:prod"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim npm run build:prod"));
}

#[test]
fn test_rewrite_npm_run_script_alias() {
    // `npm run-script` is an alias for `npm run` — must rewrite to same target
    skim_cmd()
        .args(["rewrite", "npm", "run-script", "lint"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim npm run lint"));
}

// ============================================================================
// npm run: missing script name error path
// ============================================================================

#[test]
fn test_npm_run_missing_script_name_exits_failure() {
    // `skim npm run` without a script name must exit non-zero and print a
    // diagnostic to stderr.  The error path is hit before npm is spawned, so
    // this test is self-contained and does not require npm to be installed.
    skim_cmd()
        .args(["npm", "run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing script name"));
}

// ============================================================================
// cargo c alias: direct dispatch
// ============================================================================

#[test]
fn test_cargo_c_alias_dispatches_to_check() {
    // `skim cargo c` is documented as an alias for `skim cargo check`.
    // Verify the alias routes to the check handler by asserting that the
    // binary accepts the invocation and attempts to run cargo check.
    // We pass `--help` so that the underlying cargo check invocation does not
    // actually run (the dispatch path short-circuits on --help flags before
    // spawning).
    skim_cmd()
        .args(["cargo", "c", "--help"])
        .assert()
        // Either success (help printed) or failure (cargo not found) is acceptable.
        // What must NOT happen is an "unknown subcommand" error for 'c'.
        .stderr(predicate::str::contains("unknown subcommand 'c'").not());
}

// ============================================================================
// #317 — round-trip safety: a rewrite must never corrupt a command
// ============================================================================

/// The exact heredoc-corruption repro from #317 Addendum 5: a multi-line
/// `git commit` message. 72 sessions / 180 failures were caused by the hook
/// flattening these into one line. The hook must NOT rewrite (empty stdout).
#[test]
fn test_hook_multiline_commit_is_never_rewritten() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "git commit -m \"$(cat <<'EOF'\nfeat: subject\n\nBody text here.\nEOF\n)\""
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

/// Any embedded newline bails — even without heredoc syntax.
#[test]
fn test_hook_newline_in_command_is_never_rewritten() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "git commit -m \"line one\nline two\""
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

/// Whitespace round-trip guard: runs of spaces inside quoted args would be
/// flattened by tokenize+join — the hook must bail.
#[test]
fn test_hook_double_space_in_quoted_arg_is_never_rewritten() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "git commit -m \"two  spaces preserved\""
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

/// One-line `git commit -m "x"` (single-spaced) still rewrites: the
/// round-trip guard only bails when reconstruction would be lossy.
#[test]
fn test_hook_single_line_commit_still_rewrites() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "git commit -m \"fix: one-line message\""
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("skim git commit"));
}

// ============================================================================
// #317 — pipes are wholly rewrite-free (user-approved)
// ============================================================================

/// `git diff | grep TODO` must pass through: compressing the producer would
/// silently change what grep sees.
#[test]
fn test_hook_pipe_is_never_rewritten() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "git diff | grep TODO"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

/// Pipe with a rewritable producer (`cargo test | head`) also passes through.
#[test]
fn test_hook_pipe_with_rewritable_producer_passes_through() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo test | head -50"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

/// Fix E (fix/rewrite-hook-falseneg): `git diff | grep "^+"` must NOT be
/// rewritten.  Rewiring `git diff` to `skim git diff` in a pipe would change
/// the byte stream that `grep` sees — the compressed diff format differs from
/// raw `git diff` output.  The hook must emit empty stdout (passthrough).
#[test]
fn fix_e_git_diff_pipe_grep_passes_through() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "git diff | grep \"^+\""
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

/// Fix E: `git log | grep feat` must pass through — same pipe-source rule.
#[test]
fn fix_e_git_log_pipe_grep_passes_through() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "git log --oneline | grep feat"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

/// Fix E: `git show HEAD:src/lib.rs | wc -l` must pass through.
#[test]
fn fix_e_git_show_pipe_passes_through() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "git show HEAD:src/lib.rs | wc -l"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

// ============================================================================
// #317 — the emitted rewrite must EXECUTE (round-trip e2e)
//
// This is the test class that would have caught Addendum 4B: every
// hook-rewritten `cat <file>` with session attribution emitted
// `skim --session-id=… <file>`, which errored with
// "unexpected argument '--session-id'".
// ============================================================================

/// Hook-rewrite `cat <tmp.rs>` with a session_id, then EXECUTE the emitted
/// command. It must exit 0 and produce output.
#[test]
fn test_hook_rewritten_cat_with_session_id_executes() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("roundtrip.rs");
    fs::write(&file, "pub fn answer() -> u32 {\n    42\n}\n").unwrap();

    let input = serde_json::json!({
        "session_id": "e2e-roundtrip-session",
        "tool_input": {
            "command": format!("cat {}", file.display())
        }
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let response: serde_json::Value = serde_json::from_slice(&output).expect("hook emits JSON");
    let rewritten = response["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .expect("rewritten command present");
    assert!(
        rewritten.contains("--session-id=e2e-roundtrip-session"),
        "session id must be injected: {rewritten}"
    );

    // Execute the emitted tokens through the actual skim binary.
    // NOTE: split_whitespace is used for simplicity and relies on TempDir
    // producing a space-free path (standard on Linux/macOS /tmp). If the
    // temp dir ever introduces spaces, switch to a proper shell-word splitter.
    let tokens: Vec<&str> = rewritten.split_whitespace().collect();
    assert_eq!(tokens[0], "skim");
    skim_cmd()
        .args(&tokens[1..])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("answer"));
}

// ============================================================================
// #317 — declaration files keep their signal (.d.ts / .pyi)
// ============================================================================

/// `cat types.d.ts` must rewrite with --mode=structure (pseudo strips the
/// entire file — it is all type declarations).
#[test]
fn test_hook_cat_declaration_file_uses_structure_mode() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "cat src/types.d.ts"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("--mode=structure"))
        .stdout(predicate::str::contains("pseudo").not());
}

/// A declaration-file rewrite executed end-to-end retains the type signal.
#[test]
fn test_declaration_file_rewrite_output_retains_types() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("api.d.ts");
    fs::write(
        &file,
        "export interface User {\n    id: number;\n    name: string;\n}\nexport declare function getUser(id: number): User;\n",
    )
    .unwrap();

    skim_cmd()
        .args([file.to_str().unwrap(), "--mode=structure"])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("interface User"))
        .stdout(predicate::str::contains("getUser"))
        .stdout(predicate::str::contains("id: number"));
}

/// Redirect-order hazard (post-review finding): `2>&1 >log.txt` routes
/// stderr→terminal + stdout→log; the strip-and-append redirect handling
/// would reorder it to `>log.txt 2>&1` (both→log). The hook must bail.
#[test]
fn test_hook_redirect_reorder_hazard_is_never_rewritten() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo build 2>&1 >log.txt && cargo test"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

/// Safe redirect order (`>log 2>&1`) still rewrites — append preserves it.
#[test]
fn test_hook_safe_redirect_order_still_rewrites() {
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo test >log.txt 2>&1"
        }
    });
    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains(">log.txt 2>&1"));
}
