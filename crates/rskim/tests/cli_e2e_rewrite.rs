//! E2E tests for untested rewrite rules, compound commands, and hook mode (#54).
//!
//! Covers rewrite rules that have unit tests but NO previous CLI-level tests:
//! - python3 -m pytest -> skim test pytest
//! - python -m pytest -> skim test pytest
//! - npx vitest -> skim test vitest
//! - npx tsc -> skim build tsc
//! - vitest (bare) -> skim test vitest
//! - tsc (bare) -> skim build tsc
//! - cargo clippy -> skim build clippy
//!
//! Also covers hook mode and three-segment compound commands.

use assert_cmd::Command;
use predicates::prelude::*;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
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
        .stdout(predicate::str::contains("skim test pytest"));
}

#[test]
fn test_rewrite_python3_m_pytest_with_args() {
    skim_cmd()
        .args(["rewrite", "python3", "-m", "pytest", "-v", "tests/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test pytest -v tests/"));
}

#[test]
fn test_rewrite_python_m_pytest() {
    skim_cmd()
        .args(["rewrite", "python", "-m", "pytest"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test pytest"));
}

#[test]
fn test_rewrite_python_m_pytest_with_args() {
    skim_cmd()
        .args(["rewrite", "python", "-m", "pytest", "--tb=short"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test pytest --tb=short"));
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
        .stdout(predicate::str::contains("skim test vitest"));
}

#[test]
fn test_rewrite_npx_vitest_with_args() {
    skim_cmd()
        .args(["rewrite", "npx", "vitest", "--reporter=json", "--run"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "skim test vitest --reporter=json --run",
        ));
}

#[test]
fn test_rewrite_npx_tsc() {
    skim_cmd()
        .args(["rewrite", "npx", "tsc"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build tsc"));
}

#[test]
fn test_rewrite_npx_tsc_with_args() {
    skim_cmd()
        .args(["rewrite", "npx", "tsc", "--noEmit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build tsc --noEmit"));
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
        .stdout(predicate::str::contains("skim test vitest"));
}

#[test]
fn test_rewrite_vitest_bare_with_args() {
    skim_cmd()
        .args(["rewrite", "vitest", "--run", "math"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test vitest --run math"));
}

#[test]
fn test_rewrite_tsc_bare() {
    skim_cmd()
        .args(["rewrite", "tsc"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build tsc"));
}

#[test]
fn test_rewrite_tsc_bare_with_args() {
    skim_cmd()
        .args(["rewrite", "tsc", "--noEmit", "--watch"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build tsc --noEmit --watch"));
}

#[test]
fn test_rewrite_cargo_clippy() {
    skim_cmd()
        .args(["rewrite", "cargo", "clippy"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim build clippy"));
}

#[test]
fn test_rewrite_cargo_clippy_with_args() {
    skim_cmd()
        .args(["rewrite", "cargo", "clippy", "--", "-W", "clippy::pedantic"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "skim build clippy -- -W clippy::pedantic",
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
        .stdout(predicate::str::contains("skim test cargo"))
        .stdout(predicate::str::contains("skim build cargo"))
        .stdout(predicate::str::contains("skim build clippy"));
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
        .stdout(predicate::str::contains("skim test cargo"));
}

#[test]
fn test_rewrite_hook_passthrough_already_rewritten() {
    // Commands starting with "skim " should pass through without modification.
    // Hook mode always exits 0 (passthrough is silent success).
    let input = serde_json::json!({
        "tool_input": {
            "command": "skim test cargo"
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
    // Use a command that has no rewrite rule (bare ls without flags is not rewritten).
    let input = serde_json::json!({
        "tool_input": {
            "command": "ls"
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
        .stdout(predicate::str::contains("skim test cargo"))
        .stdout(predicate::str::contains("skim build cargo"));
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
        stdout.contains("skim test cargo"),
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
    assert!(json["hookSpecificOutput"]["updatedInput"]["command"]
        .as_str()
        .unwrap()
        .contains("skim test cargo"));
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
            .contains("skim test cargo"),
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
        json["reason"].as_str().unwrap().contains("skim test cargo"),
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
            .contains("skim test cargo"),
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
fn test_rewrite_hook_agent_opencode_awareness_only() {
    // OpenCode is AwarenessOnly — always empty stdout, exit 0
    let input = serde_json::json!({
        "tool_input": {
            "command": "cargo test"
        }
    });
    let output = skim_cmd()
        .args(["rewrite", "--hook", "--agent", "opencode"])
        .write_stdin(serde_json::to_string(&input).unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.trim().is_empty(),
        "OpenCode (AwarenessOnly) should produce empty stdout, got: {stdout}"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.is_empty(),
        "OpenCode hook mode should produce zero stderr, got: {stderr}"
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
            "opencode",
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
// Lint rewrite rules (#104)
// ============================================================================

#[test]
fn test_rewrite_eslint() {
    skim_cmd()
        .args(["rewrite", "eslint", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim lint eslint ."));
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
        .stdout(predicate::str::contains("skim lint eslint src/"));
}

#[test]
fn test_rewrite_ruff_check() {
    skim_cmd()
        .args(["rewrite", "ruff", "check", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim lint ruff ."));
}

#[test]
fn test_rewrite_ruff_bare() {
    skim_cmd()
        .args(["rewrite", "ruff", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim lint ruff ."));
}

#[test]
fn test_rewrite_mypy() {
    skim_cmd()
        .args(["rewrite", "mypy", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim lint mypy ."));
}

#[test]
fn test_rewrite_python_m_mypy() {
    skim_cmd()
        .args(["rewrite", "python", "-m", "mypy", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim lint mypy ."));
}

#[test]
fn test_rewrite_python3_m_mypy() {
    skim_cmd()
        .args(["rewrite", "python3", "-m", "mypy", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim lint mypy src/"));
}

#[test]
fn test_rewrite_golangci_lint_run() {
    skim_cmd()
        .args(["rewrite", "golangci-lint", "run", "./..."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim lint golangci ./..."));
}

#[test]
fn test_rewrite_golangci_lint_bare() {
    skim_cmd()
        .args(["rewrite", "golangci-lint", "./..."])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim lint golangci ./..."));
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
        .stdout(predicate::str::contains("skim pkg npm audit"));
}

#[test]
fn test_rewrite_npm_i_express() {
    skim_cmd()
        .args(["rewrite", "npm", "i", "express"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg npm install express"));
}

#[test]
fn test_rewrite_npm_ci() {
    skim_cmd()
        .args(["rewrite", "npm", "ci"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg npm install"));
}

#[test]
fn test_rewrite_pip_install_flask() {
    skim_cmd()
        .args(["rewrite", "pip", "install", "flask"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg pip install flask"));
}

#[test]
fn test_rewrite_pip3_check() {
    skim_cmd()
        .args(["rewrite", "pip3", "check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg pip check"));
}

#[test]
fn test_rewrite_cargo_audit() {
    skim_cmd()
        .args(["rewrite", "cargo", "audit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg cargo audit"));
}

#[test]
fn test_rewrite_pnpm_install() {
    skim_cmd()
        .args(["rewrite", "pnpm", "install"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg pnpm install"));
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
        .stdout(predicate::str::contains(
            "skim pkg npm install express lodash",
        ));
}

#[test]
fn test_rewrite_npm_outdated() {
    skim_cmd()
        .args(["rewrite", "npm", "outdated"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg npm outdated"));
}

#[test]
fn test_rewrite_npm_ls() {
    skim_cmd()
        .args(["rewrite", "npm", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg npm ls"));
}

#[test]
fn test_rewrite_pnpm_audit() {
    skim_cmd()
        .args(["rewrite", "pnpm", "audit"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg pnpm audit"));
}

#[test]
fn test_rewrite_pnpm_outdated() {
    skim_cmd()
        .args(["rewrite", "pnpm", "outdated"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg pnpm outdated"));
}

#[test]
fn test_rewrite_pip_list() {
    skim_cmd()
        .args(["rewrite", "pip", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg pip list"));
}

#[test]
fn test_rewrite_pip3_install() {
    skim_cmd()
        .args(["rewrite", "pip3", "install", "flask"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg pip install flask"));
}

#[test]
fn test_rewrite_pip3_list() {
    skim_cmd()
        .args(["rewrite", "pip3", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim pkg pip list"));
}

// ============================================================================
// Phase 8: Wave B rewrite rules (#116)
// ============================================================================

/// AD-11: `prettier --check` is acknowledged as already-compact.
/// The original command is echoed on stdout (exit 0) rather than being
/// rewritten to `skim lint prettier`, per the compress-or-skip rule.
#[test]
fn test_rewrite_prettier_check_acknowledged() {
    skim_cmd()
        .args(["rewrite", "prettier", "--check", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("prettier --check"));
}

/// AD-11: `rustfmt --check` is acknowledged as already-compact.
/// The original command is echoed on stdout rather than being
/// rewritten to `skim lint rustfmt`.
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
        .stdout(predicate::str::contains("skim infra gh"));
}

#[test]
fn test_rewrite_aws_s3_ls() {
    skim_cmd()
        .args(["rewrite", "aws", "s3", "ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim infra aws"));
}

#[test]
fn test_rewrite_curl_api() {
    skim_cmd()
        .args(["rewrite", "curl", "https://api.example.com"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim infra curl"));
}

#[test]
fn test_rewrite_wget_file() {
    skim_cmd()
        .args(["rewrite", "wget", "https://example.com/f.tar.gz"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim infra wget"));
}

#[test]
fn test_rewrite_find_name() {
    skim_cmd()
        .args(["rewrite", "find", ".", "-name", "*.rs"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim file find"));
}

#[test]
fn test_rewrite_ls_la() {
    skim_cmd()
        .args(["rewrite", "ls", "-la"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim file ls"));
}

#[test]
fn test_rewrite_tree_bare() {
    skim_cmd()
        .args(["rewrite", "tree"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim file tree"));
}

#[test]
fn test_rewrite_grep_r() {
    skim_cmd()
        .args(["rewrite", "grep", "-r", "TODO", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim file grep"));
}

#[test]
fn test_rewrite_rg_pattern() {
    skim_cmd()
        .args(["rewrite", "rg", "pattern"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim file rg"));
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
fn test_rewrite_gh_json_rewrites() {
    // --json is no longer a skip flag for gh list commands — handler supports
    // --json output natively, so the rewrite should succeed (exit 0).
    skim_cmd()
        .args(["rewrite", "gh", "pr", "list", "--json", "title"])
        .assert()
        .success();
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
fn test_rewrite_gh_pr_list_json_roundtrip() {
    // `gh pr list --json number` should rewrite to `skim infra gh pr list --json number`
    skim_cmd()
        .args(["rewrite", "gh", "pr", "list", "--json", "number"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "skim infra gh pr list --json number",
        ));
}

#[test]
fn test_rewrite_jest_roundtrip() {
    // `jest src/` should rewrite to `skim test jest src/`
    skim_cmd()
        .args(["rewrite", "jest", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test jest src/"));
}

#[test]
fn test_rewrite_npx_jest_roundtrip() {
    // `npx jest src/` should rewrite to `skim test jest src/`
    skim_cmd()
        .args(["rewrite", "npx", "jest", "src/"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test jest src/"));
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
        .stdout(predicate::str::contains("skim infra gh issue view 42"));
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
        .stdout(predicate::str::contains("skim infra gh pr view 15"));
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
        .stdout(predicate::str::contains("skim infra gh pr checks 15"));
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
        .stdout(predicate::str::contains("skim infra gh run view 12345"));
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
