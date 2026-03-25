//! Integration tests for `skim agents` subcommand.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

#[test]
fn test_agents_help() {
    skim_cmd()
        .args(["agents", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim agents"))
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn test_agents_short_help() {
    skim_cmd()
        .args(["agents", "-h"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim agents"));
}

#[test]
fn test_agents_runs_without_crash() {
    // Should succeed even with no agents detected
    skim_cmd()
        .args(["agents"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Detected agents:"));
}

#[test]
fn test_agents_json_output_valid_json() {
    let output = skim_cmd()
        .args(["agents", "--json"])
        .output()
        .expect("failed to run skim agents --json");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value =
        serde_json::from_str(&stdout).expect("output should be valid JSON");

    // Verify structure
    assert!(parsed.get("agents").is_some(), "missing 'agents' key");
    let agents = parsed["agents"].as_array().expect("agents should be array");
    assert!(!agents.is_empty(), "agents array should not be empty");

    // Each agent should have expected fields
    for agent in agents {
        assert!(agent.get("name").is_some(), "missing 'name' field");
        assert!(agent.get("cli_name").is_some(), "missing 'cli_name' field");
        assert!(
            agent.get("detected").is_some(),
            "missing 'detected' field"
        );
        assert!(agent.get("hooks").is_some(), "missing 'hooks' field");
    }
}

#[test]
fn test_agents_detects_claude_code_with_fixture() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("session.jsonl"), "{}").unwrap();
    std::fs::write(project_dir.join("other.jsonl"), "{}").unwrap();

    skim_cmd()
        .args(["agents"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("Claude Code"))
        .stdout(predicate::str::contains("detected"))
        .stdout(predicate::str::contains("2 files"));
}

#[test]
fn test_agents_json_detects_claude_code_with_fixture() {
    let dir = TempDir::new().unwrap();
    let project_dir = dir.path().join("test-project");
    std::fs::create_dir_all(&project_dir).unwrap();
    std::fs::write(project_dir.join("session.jsonl"), "{}").unwrap();

    let output = skim_cmd()
        .args(["agents", "--json"])
        .env("SKIM_PROJECTS_DIR", dir.path().to_str().unwrap())
        .output()
        .expect("failed to run skim agents --json");

    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");

    let agents = parsed["agents"].as_array().unwrap();
    let claude = agents
        .iter()
        .find(|a| a["cli_name"] == "claude-code")
        .expect("should have claude-code agent");

    assert_eq!(claude["detected"], true);
    assert!(claude["sessions"].is_object(), "sessions should be present");
    assert!(
        claude["sessions"]["detail"]
            .as_str()
            .unwrap()
            .contains("1 files"),
        "expected 1 file in detail"
    );
}

#[test]
fn test_agents_lists_all_supported() {
    let output = skim_cmd()
        .args(["agents", "--json"])
        .output()
        .expect("failed to run skim agents --json");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    let agents = parsed["agents"].as_array().unwrap();

    // Should include all supported agents
    let cli_names: Vec<&str> = agents
        .iter()
        .filter_map(|a| a["cli_name"].as_str())
        .collect();

    assert!(cli_names.contains(&"claude-code"), "missing claude-code");
    assert!(cli_names.contains(&"cursor"), "missing cursor");
    assert!(cli_names.contains(&"codex-cli"), "missing codex-cli");
    assert!(cli_names.contains(&"gemini-cli"), "missing gemini-cli");
    assert!(cli_names.contains(&"copilot-cli"), "missing copilot-cli");
}

#[test]
fn test_agents_text_output_shows_all_names() {
    skim_cmd()
        .args(["agents"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Claude Code"))
        .stdout(predicate::str::contains("Cursor"))
        .stdout(predicate::str::contains("Codex CLI"))
        .stdout(predicate::str::contains("Gemini CLI"))
        .stdout(predicate::str::contains("Copilot CLI"));
}
