//! Integration tests for `skim discover` subcommand (#61).

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
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
        .assert()
        .success()
        .stderr(predicate::str::contains("No AI agent sessions found"));
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
