//! Integration tests for `skim learn` subcommand (#64).

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

#[test]
fn test_learn_help() {
    skim_cmd()
        .args(["learn", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim learn"))
        .stdout(predicate::str::contains("--generate"));
}

#[test]
fn test_learn_with_error_patterns() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/session_errors.jsonl");
    std::fs::write(project_dir.join("error-session.jsonl"), fixture).unwrap();

    skim_cmd()
        .args(["learn", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("correction"));
}

#[test]
fn test_learn_json_output() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/session_errors.jsonl");
    std::fs::write(project_dir.join("error-session.jsonl"), fixture).unwrap();

    skim_cmd()
        .args(["learn", "--since", "7d", "--json"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("\"version\":"));
}

#[test]
fn test_learn_dry_run() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/session_errors.jsonl");
    std::fs::write(project_dir.join("error-session.jsonl"), fixture).unwrap();

    skim_cmd()
        .args(["learn", "--generate", "--dry-run", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("Would write to:"));
}

#[test]
fn test_learn_generate_writes_file() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/session_errors.jsonl");
    std::fs::write(project_dir.join("error-session.jsonl"), fixture).unwrap();

    // Use a temp working dir for the rules file
    let work_dir = TempDir::new().unwrap();

    skim_cmd()
        .args(["learn", "--generate", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .current_dir(work_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Wrote corrections to:"));

    // Verify the file was created
    let rules_file = work_dir.path().join(".claude/rules/skim-corrections.md");
    assert!(rules_file.exists(), "Rules file should be created");
    let content = std::fs::read_to_string(&rules_file).unwrap();
    assert!(content.contains("CLI Corrections"), "Should have header");
}

#[test]
fn test_learn_empty_session() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    std::fs::write(project_dir.join("empty.jsonl"), "").unwrap();

    skim_cmd()
        .args(["learn", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success();
}

#[test]
fn test_learn_no_sessions() {
    let dir = TempDir::new().unwrap();
    skim_cmd()
        .args(["learn"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success();
}

#[test]
fn test_learn_unknown_flag() {
    skim_cmd()
        .args(["learn", "--nonexistent"])
        .assert()
        .failure();
}

#[test]
fn test_learn_tdd_cycle_excluded() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/session_tdd.jsonl");
    std::fs::write(project_dir.join("tdd-session.jsonl"), fixture).unwrap();

    // TDD cycle should not produce corrections
    skim_cmd()
        .args(["learn", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("No CLI error patterns detected"));
}

#[test]
fn test_learn_json_has_structure() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/session_errors.jsonl");
    std::fs::write(project_dir.join("error-session.jsonl"), fixture).unwrap();

    let output = skim_cmd()
        .args(["learn", "--since", "7d", "--json"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .output()
        .unwrap();

    assert!(output.status.success());
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["version"], 1);
    assert!(json["corrections"].is_array());
    let corrections = json["corrections"].as_array().unwrap();
    assert!(!corrections.is_empty());
    assert!(corrections[0]["failed_command"].is_string());
    assert!(corrections[0]["successful_command"].is_string());
    assert!(corrections[0]["pattern_type"].is_string());
}

#[test]
fn test_learn_invalid_since() {
    skim_cmd()
        .args(["learn", "--since", "abc"])
        .assert()
        .failure();
}

#[test]
fn test_learn_unknown_agent_error() {
    skim_cmd()
        .args(["learn", "--agent", "nonexistent"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown agent"));
}

#[test]
fn test_learn_agent_filter() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/session_errors.jsonl");
    std::fs::write(project_dir.join("error-session.jsonl"), fixture).unwrap();

    skim_cmd()
        .args(["learn", "--agent", "claude-code", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success();
}

#[test]
fn test_learn_no_bash_commands() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    // Use a session with only Read invocations (no Bash)
    let fixture = include_str!("fixtures/cmd/session/claude_reads.jsonl");
    std::fs::write(project_dir.join("read-session.jsonl"), fixture).unwrap();

    skim_cmd()
        .args(["learn", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success();
}

// ============================================================================
// Permission denial exclusion
// ============================================================================

#[test]
fn test_learn_skips_permission_denials() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/session_denial.jsonl");
    std::fs::write(project_dir.join("denial-session.jsonl"), fixture).unwrap();

    // Permission denials should not produce corrections
    skim_cmd()
        .args(["learn", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("No CLI error patterns detected"));
}

// ============================================================================
// Phase 6: Cross-agent learn tests -- per-agent rules file format
// ============================================================================

#[test]
fn test_learn_generate_claude_code_writes_md_file() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/session_errors.jsonl");
    std::fs::write(project_dir.join("error-session.jsonl"), fixture).unwrap();

    let work_dir = TempDir::new().unwrap();

    skim_cmd()
        .args([
            "learn",
            "--generate",
            "--agent",
            "claude-code",
            "--since",
            "7d",
        ])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .current_dir(work_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Wrote corrections to:"));

    let rules_file = work_dir.path().join(".claude/rules/skim-corrections.md");
    assert!(
        rules_file.exists(),
        "Claude Code rules file should be at .claude/rules/skim-corrections.md"
    );
    let content = std::fs::read_to_string(&rules_file).unwrap();
    assert!(content.contains("CLI Corrections"), "Should have header");
    // Claude Code format: no frontmatter
    assert!(
        !content.starts_with("---"),
        "Claude Code format should NOT have frontmatter"
    );
}

#[test]
fn test_learn_generate_default_dry_run_preview() {
    // Cursor rules format test: use Claude Code sessions (the error patterns
    // are agent-agnostic) but request Cursor format output.
    //
    // Since --agent cursor filters providers to Cursor-only (which requires
    // a SQLite DB we can't easily mock in integration tests), we test via
    // dry-run with the Claude Code provider but default agent, then verify
    // the unit-test-covered cursor format separately.
    //
    // The unit tests in learn.rs::tests::test_generate_rules_content_cursor_frontmatter
    // already validate the Cursor frontmatter format. This integration test
    // confirms the default (Claude Code) pipeline works end-to-end.
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();

    let fixture = include_str!("fixtures/cmd/session/session_errors.jsonl");
    std::fs::write(project_dir.join("error-session.jsonl"), fixture).unwrap();

    // Verify the default --generate path works (Claude Code format)
    let work_dir = TempDir::new().unwrap();
    skim_cmd()
        .args(["learn", "--generate", "--dry-run", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .current_dir(work_dir.path())
        .assert()
        .success()
        .stdout(predicate::str::contains("Would write to:"))
        .stdout(predicate::str::contains("CLI Corrections"));
}

#[test]
fn test_learn_generate_copilot_writes_instructions_md_with_frontmatter() {
    // Create Copilot-format session fixture with error patterns
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("nonexistent");
    let copilot_dir = dir.path().join("copilot-sessions");
    std::fs::create_dir_all(&copilot_dir).unwrap();

    // Copilot JSONL with error-retry pairs (carg test -> cargo test, x3 for ≥3 filter)
    let copilot_session = concat!(
        r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "carg test"}, "id": "t-001", "timestamp": "2024-06-15T10:01:00Z" }"#,
        "\n",
        r#"{ "type": "tool_result", "toolUseId": "t-001", "resultType": "error", "content": "error: command not found: carg", "timestamp": "2024-06-15T10:01:05Z" }"#,
        "\n",
        r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "cargo test"}, "id": "t-002", "timestamp": "2024-06-15T10:02:00Z" }"#,
        "\n",
        r#"{ "type": "tool_result", "toolUseId": "t-002", "resultType": "success", "content": "test result: ok. 5 passed; 0 failed", "timestamp": "2024-06-15T10:02:05Z" }"#,
        "\n",
        r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "carg test"}, "id": "t-003", "timestamp": "2024-06-15T10:03:00Z" }"#,
        "\n",
        r#"{ "type": "tool_result", "toolUseId": "t-003", "resultType": "error", "content": "error: command not found: carg", "timestamp": "2024-06-15T10:03:05Z" }"#,
        "\n",
        r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "cargo test"}, "id": "t-004", "timestamp": "2024-06-15T10:04:00Z" }"#,
        "\n",
        r#"{ "type": "tool_result", "toolUseId": "t-004", "resultType": "success", "content": "test result: ok. 5 passed; 0 failed", "timestamp": "2024-06-15T10:04:05Z" }"#,
        "\n",
        r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "carg test"}, "id": "t-005", "timestamp": "2024-06-15T10:05:00Z" }"#,
        "\n",
        r#"{ "type": "tool_result", "toolUseId": "t-005", "resultType": "error", "content": "error: command not found: carg", "timestamp": "2024-06-15T10:05:05Z" }"#,
        "\n",
        r#"{ "type": "tool_use", "toolName": "bash", "toolArgs": {"command": "cargo test"}, "id": "t-006", "timestamp": "2024-06-15T10:06:00Z" }"#,
        "\n",
        r#"{ "type": "tool_result", "toolUseId": "t-006", "resultType": "success", "content": "test result: ok. 5 passed; 0 failed", "timestamp": "2024-06-15T10:06:05Z" }"#,
        "\n"
    );
    std::fs::write(copilot_dir.join("error-session.jsonl"), copilot_session).unwrap();

    let work_dir = TempDir::new().unwrap();

    let mut cmd = skim_cmd();
    cmd.args(["learn", "--generate", "--agent", "copilot", "--since", "7d"])
        .env("SKIM_COPILOT_DIR", copilot_dir.to_str().unwrap())
        .env("SKIM_PROJECTS_DIR", nonexistent.to_str().unwrap())
        .env("SKIM_CODEX_SESSIONS_DIR", nonexistent.to_str().unwrap())
        .env(
            "SKIM_CURSOR_DB_PATH",
            nonexistent.join("no-cursor.vscdb").to_str().unwrap(),
        )
        .env("SKIM_GEMINI_DIR", nonexistent.to_str().unwrap())
        .env("SKIM_OPENCODE_DIR", nonexistent.to_str().unwrap())
        .current_dir(work_dir.path());

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Wrote corrections to:"));

    let rules_file = work_dir
        .path()
        .join(".github/instructions/skim-corrections.instructions.md");
    assert!(
        rules_file.exists(),
        "Copilot rules file should be at .github/instructions/skim-corrections.instructions.md"
    );
    let content = std::fs::read_to_string(&rules_file).unwrap();
    assert!(
        content.starts_with("---\napplyTo:"),
        "Copilot format should have applyTo frontmatter, got: {}",
        &content[..content.len().min(100)]
    );
    assert!(content.contains("CLI Corrections"), "Should have header");
}

#[test]
fn test_learn_generate_codex_prints_to_stdout_no_file() {
    // Create Codex-format session fixture with error patterns in YYYY/MM/DD structure
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("nonexistent");
    let codex_dir = dir.path().join("codex-sessions");
    let codex_session_dir = codex_dir.join("2026/03/25");
    std::fs::create_dir_all(&codex_session_dir).unwrap();

    // Codex JSONL with error-retry pairs (carg test -> cargo test, x3 for ≥3 filter)
    let codex_session = concat!(
        r#"{"type":"codex.tool_decision","tool":"bash","args":{"command":"carg test"},"timestamp":"2026-03-01T10:00:00Z","session_id":"sess-err","tool_decision_id":"td-001"}"#,
        "\n",
        r#"{"type":"codex.tool_result","tool":"bash","result":{"content":"error: command not found: carg","is_error":true},"timestamp":"2026-03-01T10:00:01Z","session_id":"sess-err","tool_decision_id":"td-001"}"#,
        "\n",
        r#"{"type":"codex.tool_decision","tool":"bash","args":{"command":"cargo test"},"timestamp":"2026-03-01T10:00:02Z","session_id":"sess-err","tool_decision_id":"td-002"}"#,
        "\n",
        r#"{"type":"codex.tool_result","tool":"bash","result":{"content":"test result: ok. 5 passed; 0 failed","is_error":false},"timestamp":"2026-03-01T10:00:03Z","session_id":"sess-err","tool_decision_id":"td-002"}"#,
        "\n",
        r#"{"type":"codex.tool_decision","tool":"bash","args":{"command":"carg test"},"timestamp":"2026-03-01T10:00:04Z","session_id":"sess-err","tool_decision_id":"td-003"}"#,
        "\n",
        r#"{"type":"codex.tool_result","tool":"bash","result":{"content":"error: command not found: carg","is_error":true},"timestamp":"2026-03-01T10:00:05Z","session_id":"sess-err","tool_decision_id":"td-003"}"#,
        "\n",
        r#"{"type":"codex.tool_decision","tool":"bash","args":{"command":"cargo test"},"timestamp":"2026-03-01T10:00:06Z","session_id":"sess-err","tool_decision_id":"td-004"}"#,
        "\n",
        r#"{"type":"codex.tool_result","tool":"bash","result":{"content":"test result: ok. 5 passed; 0 failed","is_error":false},"timestamp":"2026-03-01T10:00:07Z","session_id":"sess-err","tool_decision_id":"td-004"}"#,
        "\n",
        r#"{"type":"codex.tool_decision","tool":"bash","args":{"command":"carg test"},"timestamp":"2026-03-01T10:00:08Z","session_id":"sess-err","tool_decision_id":"td-005"}"#,
        "\n",
        r#"{"type":"codex.tool_result","tool":"bash","result":{"content":"error: command not found: carg","is_error":true},"timestamp":"2026-03-01T10:00:09Z","session_id":"sess-err","tool_decision_id":"td-005"}"#,
        "\n",
        r#"{"type":"codex.tool_decision","tool":"bash","args":{"command":"cargo test"},"timestamp":"2026-03-01T10:00:10Z","session_id":"sess-err","tool_decision_id":"td-006"}"#,
        "\n",
        r#"{"type":"codex.tool_result","tool":"bash","result":{"content":"test result: ok. 5 passed; 0 failed","is_error":false},"timestamp":"2026-03-01T10:00:11Z","session_id":"sess-err","tool_decision_id":"td-006"}"#,
        "\n"
    );
    std::fs::write(
        codex_session_dir.join("rollout-errors.jsonl"),
        codex_session,
    )
    .unwrap();

    let work_dir = TempDir::new().unwrap();

    // Codex has no rules_dir() (returns None), so content is printed to stdout
    let mut cmd = skim_cmd();
    cmd.args(["learn", "--generate", "--agent", "codex", "--since", "7d"])
        .env("SKIM_CODEX_SESSIONS_DIR", codex_dir.to_str().unwrap())
        .env("SKIM_PROJECTS_DIR", nonexistent.to_str().unwrap())
        .env("SKIM_COPILOT_DIR", nonexistent.to_str().unwrap())
        .env(
            "SKIM_CURSOR_DB_PATH",
            nonexistent.join("no-cursor.vscdb").to_str().unwrap(),
        )
        .env("SKIM_GEMINI_DIR", nonexistent.to_str().unwrap())
        .env("SKIM_OPENCODE_DIR", nonexistent.to_str().unwrap())
        .current_dir(work_dir.path());

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Add the following to your"))
        .stdout(predicate::str::contains("CLI Corrections"));

    // No file should have been written in the work dir
    assert!(
        !work_dir.path().join(".codex").exists(),
        "Codex should NOT create a file, only print to stdout"
    );
}

#[test]
fn test_learn_no_cross_agent_data_leakage() {
    // Create Claude Code session with errors, but filter to codex.
    // Codex has an empty session with no errors.
    // Result: no corrections found (codex sessions have no errors).
    let dir = TempDir::new().unwrap();
    let nonexistent = dir.path().join("nonexistent");

    // Claude Code session with errors
    let claude_dir = dir.path().join("claude-projects");
    let project_dir = claude_dir.join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();
    let fixture = include_str!("fixtures/cmd/session/session_errors.jsonl");
    std::fs::write(project_dir.join("error-session.jsonl"), fixture).unwrap();

    // Codex session dir with a clean (no-error) session
    let codex_dir = dir.path().join("codex-sessions");
    let codex_session_dir = codex_dir.join("2026/03/25");
    std::fs::create_dir_all(&codex_session_dir).unwrap();
    std::fs::write(
        codex_session_dir.join("rollout-clean.jsonl"),
        concat!(
            r#"{"type":"codex.tool_decision","tool":"bash","args":{"command":"ls"},"timestamp":"2026-03-01T10:00:00Z","session_id":"sess-clean","tool_decision_id":"td-001"}"#,
            "\n",
            r#"{"type":"codex.tool_result","tool":"bash","result":{"content":"file1.rs","is_error":false},"timestamp":"2026-03-01T10:00:01Z","session_id":"sess-clean","tool_decision_id":"td-001"}"#,
            "\n"
        ),
    )
    .unwrap();

    // Filter to codex -- should NOT find Claude Code's error patterns
    let mut cmd = skim_cmd();
    cmd.args(["learn", "--agent", "codex", "--since", "7d"])
        .env("SKIM_PROJECTS_DIR", claude_dir.to_str().unwrap())
        .env("SKIM_CODEX_SESSIONS_DIR", codex_dir.to_str().unwrap())
        .env("SKIM_COPILOT_DIR", nonexistent.to_str().unwrap())
        .env(
            "SKIM_CURSOR_DB_PATH",
            nonexistent.join("no-cursor.vscdb").to_str().unwrap(),
        )
        .env("SKIM_GEMINI_DIR", nonexistent.to_str().unwrap())
        .env("SKIM_OPENCODE_DIR", nonexistent.to_str().unwrap());

    cmd.assert().success().stdout(
        predicate::str::contains("No CLI error patterns detected")
            .or(predicate::str::contains("No Bash commands found"))
            .or(predicate::str::contains("No tool invocations")),
    );
}
