//! Integration tests for `skim discover` subcommand (#61).

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

/// Build a skim command with all session providers neutralized (pointing to nonexistent paths).
/// Callers override specific providers as needed.
fn skim_cmd_neutralized(nonexistent: &std::path::Path) -> Command {
    let mut cmd = skim_cmd();
    cmd.env("SKIM_PROJECTS_DIR", nonexistent.as_os_str())
        .env("SKIM_CODEX_SESSIONS_DIR", nonexistent.as_os_str())
        .env("SKIM_COPILOT_DIR", nonexistent.as_os_str())
        .env(
            "SKIM_CURSOR_DB_PATH",
            nonexistent.join("no-cursor.vscdb").as_os_str(),
        )
        .env("SKIM_GEMINI_DIR", nonexistent.as_os_str())
        .env("SKIM_OPENCODE_DIR", nonexistent.as_os_str());
    cmd
}

#[test]
fn test_discover_help() {
    skim_cmd()
        .args(["discover", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim discover"))
        .stdout(predicate::str::contains("--since"));
}

#[test]
fn test_discover_with_synthetic_session() {
    let dir = TempDir::new().unwrap();
    // Create project structure: <dir>/project-slug/session.jsonl
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/claude_reads.jsonl");
    std::fs::write(project_dir.join("test-session.jsonl"), fixture).unwrap();

    skim_cmd()
        .args(["discover", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("tool invocations"));
}

#[test]
fn test_discover_json_output() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/claude_reads.jsonl");
    std::fs::write(project_dir.join("test-session.jsonl"), fixture).unwrap();

    skim_cmd()
        .args(["discover", "--since", "7d", "--json"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"version\""));
}

#[test]
fn test_discover_empty_dir() {
    let dir = TempDir::new().unwrap();
    // Empty dir -- no sessions found, but the dir exists so provider is detected
    // with no .jsonl files, should report no invocations
    skim_cmd()
        .args(["discover"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success();
}

#[test]
fn test_discover_no_agent_dir() {
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("nonexistent");
    skim_cmd()
        .args(["discover"])
        .env("SKIM_PROJECTS_DIR", nonexistent.to_str().unwrap())
        // Neutralize all providers to ensure no agents are detected
        .env("SKIM_CODEX_SESSIONS_DIR", nonexistent.to_str().unwrap())
        .env("SKIM_COPILOT_DIR", nonexistent.to_str().unwrap())
        .env(
            "SKIM_CURSOR_DB_PATH",
            dir.path().join("no-cursor.vscdb").to_str().unwrap(),
        )
        .env("SKIM_GEMINI_DIR", nonexistent.to_str().unwrap())
        .env("SKIM_OPENCODE_DIR", nonexistent.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("No AI agent sessions found"));
}

#[test]
fn test_discover_agent_filter() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/claude_reads.jsonl");
    std::fs::write(project_dir.join("test-session.jsonl"), fixture).unwrap();

    skim_cmd()
        .args(["discover", "--agent", "claude-code", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success();
}

#[test]
fn test_discover_unknown_agent_error() {
    skim_cmd()
        .args(["discover", "--agent", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown agent"));
}

#[test]
fn test_discover_invalid_since() {
    skim_cmd()
        .args(["discover", "--since", "abc"])
        .assert()
        .failure();
}

#[test]
fn test_discover_bash_commands() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/claude_bash.jsonl");
    std::fs::write(project_dir.join("test-session.jsonl"), fixture).unwrap();

    skim_cmd()
        .args(["discover", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("Commands:"));
}

#[test]
fn test_discover_session_latest() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/claude_reads.jsonl");
    std::fs::write(project_dir.join("session-1.jsonl"), fixture).unwrap();
    std::fs::write(project_dir.join("session-2.jsonl"), fixture).unwrap();

    skim_cmd()
        .args(["discover", "--session", "latest", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success();
}

#[test]
fn test_discover_unknown_flag_error() {
    skim_cmd()
        .args(["discover", "--bogus"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown flag"));
}

#[test]
fn test_discover_since_missing_value() {
    // --since with no value should fail with a descriptive error
    skim_cmd()
        .args(["discover", "--since"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--since requires a value"));
}

#[test]
fn test_discover_agent_missing_value() {
    // --agent with no value should fail with a descriptive error
    skim_cmd()
        .args(["discover", "--agent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--agent requires a value"));
}

#[test]
fn test_discover_json_has_structure() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/claude_bash.jsonl");
    std::fs::write(project_dir.join("test-session.jsonl"), fixture).unwrap();

    let output = skim_cmd()
        .args(["discover", "--since", "7d", "--json"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["version"], 1);
    assert!(json["total_invocations"].is_number());
    assert!(json["code_reads"]["total"].is_number());
    assert!(json["commands"]["total"].is_number());
}

// ============================================================================
// Phase 6: Cross-agent discover tests
// ============================================================================

/// Helper: create a Codex session fixture inside a YYYY/MM/DD/ structure.
fn create_codex_fixture(base_dir: &std::path::Path) {
    let session_dir = base_dir.join("2026/03/25");
    std::fs::create_dir_all(&session_dir).unwrap();
    let fixture = include_str!("fixtures/codex/sample-session.jsonl");
    std::fs::write(session_dir.join("rollout-abc.jsonl"), fixture).unwrap();
}

#[test]
fn test_discover_cross_agent_claude_and_codex() {
    // Set up fixtures for both Claude Code and Codex simultaneously
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("nonexistent");

    // Claude Code fixture
    let claude_dir = dir.path().join("claude-projects");
    let project_dir = claude_dir.join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();
    let claude_fixture = include_str!("fixtures/cmd/session/claude_reads.jsonl");
    std::fs::write(project_dir.join("test-session.jsonl"), claude_fixture).unwrap();

    // Codex fixture
    let codex_dir = dir.path().join("codex-sessions");
    create_codex_fixture(&codex_dir);

    let output = skim_cmd_neutralized(&nonexistent)
        .args(["discover", "--since", "7d", "--json"])
        .env("SKIM_PROJECTS_DIR", claude_dir.to_str().unwrap())
        .env("SKIM_CODEX_SESSIONS_DIR", codex_dir.to_str().unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    // Both agents contributed invocations
    let total = json["total_invocations"].as_u64().unwrap();
    assert!(
        total >= 2,
        "Should have invocations from both agents, got {total}"
    );
}

#[test]
fn test_discover_agent_filter_excludes_other_agents() {
    // Set up both Claude Code and Codex fixtures, filter to claude-code only
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("nonexistent");

    let claude_dir = dir.path().join("claude-projects");
    let project_dir = claude_dir.join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();
    let claude_fixture = include_str!("fixtures/cmd/session/claude_bash.jsonl");
    std::fs::write(project_dir.join("test-session.jsonl"), claude_fixture).unwrap();

    let codex_dir = dir.path().join("codex-sessions");
    create_codex_fixture(&codex_dir);

    // Filter to claude-code only -- should NOT include Codex invocations
    let output_filtered = skim_cmd_neutralized(&nonexistent)
        .args([
            "discover",
            "--agent",
            "claude-code",
            "--since",
            "7d",
            "--json",
        ])
        .env("SKIM_PROJECTS_DIR", claude_dir.to_str().unwrap())
        .env("SKIM_CODEX_SESSIONS_DIR", codex_dir.to_str().unwrap())
        .output()
        .unwrap();
    assert!(output_filtered.status.success());

    // Now get unfiltered results for comparison
    let output_all = skim_cmd_neutralized(&nonexistent)
        .args(["discover", "--since", "7d", "--json"])
        .env("SKIM_PROJECTS_DIR", claude_dir.to_str().unwrap())
        .env("SKIM_CODEX_SESSIONS_DIR", codex_dir.to_str().unwrap())
        .output()
        .unwrap();
    assert!(output_all.status.success());

    let json_filtered: serde_json::Value = serde_json::from_slice(&output_filtered.stdout).unwrap();
    let json_all: serde_json::Value = serde_json::from_slice(&output_all.stdout).unwrap();

    let filtered_total = json_filtered["total_invocations"].as_u64().unwrap();
    let all_total = json_all["total_invocations"].as_u64().unwrap();

    // Filtered total should be strictly less than unfiltered total (Codex excluded)
    assert!(
        filtered_total < all_total,
        "Filtering by claude-code should exclude Codex invocations: filtered={filtered_total}, all={all_total}"
    );
}

#[test]
fn test_discover_agent_filter_codex_only() {
    // Set up both agents, filter to codex only
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("nonexistent");

    let claude_dir = dir.path().join("claude-projects");
    let project_dir = claude_dir.join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();
    let claude_fixture = include_str!("fixtures/cmd/session/claude_reads.jsonl");
    std::fs::write(project_dir.join("test-session.jsonl"), claude_fixture).unwrap();

    let codex_dir = dir.path().join("codex-sessions");
    create_codex_fixture(&codex_dir);

    // Filter to codex only
    let output = skim_cmd_neutralized(&nonexistent)
        .args(["discover", "--agent", "codex", "--since", "7d", "--json"])
        .env("SKIM_PROJECTS_DIR", claude_dir.to_str().unwrap())
        .env("SKIM_CODEX_SESSIONS_DIR", codex_dir.to_str().unwrap())
        .output()
        .unwrap();
    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let total = json["total_invocations"].as_u64().unwrap();
    assert!(
        total >= 1,
        "Should have Codex invocations when filtering by codex, got {total}"
    );
}

// ============================================================================
// Phase 6: skim commands excluded from "missed" count
// ============================================================================

#[test]
fn test_discover_skim_commands_excluded_from_analysis() {
    // Create a session with a mix of skim-prefixed and regular commands.
    // Only regular commands should appear in the "commands" count.
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("nonexistent");
    let claude_dir = dir.path().join("claude-projects");
    let project_dir = claude_dir.join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    // Session with: 2 skim commands (should be excluded) + 1 regular command
    let session = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t01","name":"Bash","input":{"command":"skim test cargo"}}]},"timestamp":"2024-01-01T00:00:00Z","sessionId":"sess1"}
{"type":"user","message":{"content":[{"tool_use_id":"t01","type":"tool_result","content":"ok"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t02","name":"Bash","input":{"command":"skim build clippy"}}]},"timestamp":"2024-01-01T00:01:00Z","sessionId":"sess1"}
{"type":"user","message":{"content":[{"tool_use_id":"t02","type":"tool_result","content":"ok"}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t03","name":"Bash","input":{"command":"cargo test"}}]},"timestamp":"2024-01-01T00:02:00Z","sessionId":"sess1"}
{"type":"user","message":{"content":[{"tool_use_id":"t03","type":"tool_result","content":"test result: ok. 5 passed"}]}}
"#;
    std::fs::write(project_dir.join("mixed.jsonl"), session).unwrap();

    let output = skim_cmd_neutralized(&nonexistent)
        .args(["discover", "--since", "7d", "--json"])
        .env("SKIM_PROJECTS_DIR", claude_dir.to_str().unwrap())
        .output()
        .unwrap();
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let commands_total = json["commands"]["total"].as_u64().unwrap();
    // Only "cargo test" should be counted, not the skim commands
    assert_eq!(
        commands_total, 1,
        "skim commands should be excluded from command analysis, got {commands_total}"
    );
}

#[test]
fn test_discover_only_skim_commands_shows_zero() {
    // Session with only skim-prefixed commands should show 0 in commands count
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("nonexistent");
    let claude_dir = dir.path().join("claude-projects");
    let project_dir = claude_dir.join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let session = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t01","name":"Bash","input":{"command":"skim test cargo"}}]},"timestamp":"2024-01-01T00:00:00Z","sessionId":"sess1"}
{"type":"user","message":{"content":[{"tool_use_id":"t01","type":"tool_result","content":"ok"}]}}
"#;
    std::fs::write(project_dir.join("skim-only.jsonl"), session).unwrap();

    let output = skim_cmd_neutralized(&nonexistent)
        .args(["discover", "--since", "7d", "--json"])
        .env("SKIM_PROJECTS_DIR", claude_dir.to_str().unwrap())
        .output()
        .unwrap();
    assert!(output.status.success());

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let commands_total = json["commands"]["total"].as_u64().unwrap();
    assert_eq!(
        commands_total, 0,
        "Sessions with only skim commands should show 0 commands, got {commands_total}"
    );
}

// ============================================================================
// Step 6e: --debug flag E2E tests
// ============================================================================

#[test]
fn test_discover_debug_flag_accepted() {
    // --debug should not cause an error; exit 0.
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("nonexistent");
    skim_cmd_neutralized(&nonexistent)
        .args(["discover", "--debug"])
        .assert()
        .success();
}

#[test]
fn test_discover_debug_shows_non_rewritable_commands() {
    // --debug mode should include a section for non-rewritable commands when
    // there are bash commands that have no rewrite rule.
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("nonexistent");
    let claude_dir = dir.path().join("claude-projects");
    let project_dir = claude_dir.join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    // Session with a non-rewritable command (node is not rewritable)
    let session = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t01","name":"Bash","input":{"command":"node server.js"}}]},"timestamp":"2024-01-01T00:00:00Z","sessionId":"sess1"}
{"type":"user","message":{"content":[{"tool_use_id":"t01","type":"tool_result","content":"running"}]}}
"#;
    std::fs::write(project_dir.join("node-session.jsonl"), session).unwrap();

    let output = skim_cmd_neutralized(&nonexistent)
        .args(["discover", "--debug", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", claude_dir.to_str().unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // Debug mode should show non-rewritable commands section
    assert!(
        stdout.contains("Non-rewritable") || stdout.contains("non_rewritable"),
        "Expected non-rewritable commands section in --debug output, got: {stdout}"
    );
}

#[test]
fn test_discover_debug_json_includes_non_rewritable() {
    // --debug --json should include non_rewritable_commands in the JSON output.
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("nonexistent");
    let claude_dir = dir.path().join("claude-projects");
    let project_dir = claude_dir.join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    // Session with a non-rewritable command
    let session = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t01","name":"Bash","input":{"command":"node server.js"}}]},"timestamp":"2024-01-01T00:00:00Z","sessionId":"sess1"}
{"type":"user","message":{"content":[{"tool_use_id":"t01","type":"tool_result","content":"running"}]}}
"#;
    std::fs::write(project_dir.join("node-session.jsonl"), session).unwrap();

    let output = skim_cmd_neutralized(&nonexistent)
        .args(["discover", "--debug", "--json", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", claude_dir.to_str().unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    // JSON output should include non_rewritable_commands key inside commands object
    // when debug is enabled and there are non-rewritable commands.
    assert!(
        json["commands"].get("non_rewritable_commands").is_some(),
        "Expected 'non_rewritable_commands' key under commands in debug JSON output, got: {json}"
    );
}
