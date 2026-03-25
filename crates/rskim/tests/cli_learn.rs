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
