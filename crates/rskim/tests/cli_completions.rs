//! Integration tests for `skim completions` subcommand (#63).

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::io::Write;
use tempfile::TempDir;

// ============================================================================
// Successful generation
// ============================================================================

#[test]
fn test_completions_bash_outputs_valid_script() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("complete"))
        .stdout(predicate::str::contains("skim"));
}

#[test]
fn test_completions_zsh_outputs_valid_script() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("completions")
        .arg("zsh")
        .assert()
        .success()
        .stdout(predicate::str::contains("#compdef skim"));
}

#[test]
fn test_completions_fish_outputs_valid_script() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("completions")
        .arg("fish")
        .assert()
        .success()
        .stdout(predicate::str::contains("complete"))
        .stdout(predicate::str::contains("skim"));
}

// ============================================================================
// Completion script content quality
// ============================================================================

#[test]
fn test_completions_include_mode_values() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("structure"));
}

#[test]
fn test_completions_include_language_values() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("typescript"));
}

#[test]
fn test_completions_include_subcommand_names() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("completions")
        .arg("bash")
        .assert()
        .success()
        .stdout(predicate::str::contains("completions"));
}

// ============================================================================
// Additional shell coverage
// ============================================================================

#[test]
fn test_completions_powershell_outputs_valid_script() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("completions")
        .arg("powershell")
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

#[test]
fn test_completions_elvish_outputs_valid_script() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("completions")
        .arg("elvish")
        .assert()
        .success()
        .stdout(predicate::str::is_empty().not());
}

// ============================================================================
// Case sensitivity
// ============================================================================

#[test]
fn test_completions_case_sensitive_rejects_uppercase() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("completions")
        .arg("BASH")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown shell"));
}

// ============================================================================
// Extra args silently ignored by clap
// ============================================================================

#[test]
fn test_completions_extra_args_ignored() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("completions")
        .arg("bash")
        .arg("extra")
        .arg("junk")
        .assert()
        .success()
        .stdout(predicate::str::contains("complete"));
}

// ============================================================================
// Syntax validation (pipe through shell -n)
// ============================================================================

#[test]
fn test_completions_bash_syntax_valid() {
    let completions_output = Command::cargo_bin("skim")
        .unwrap()
        .arg("completions")
        .arg("bash")
        .output()
        .unwrap();
    assert!(completions_output.status.success());

    let mut child = std::process::Command::new("bash")
        .arg("-n")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();

    child
        .stdin
        .take()
        .unwrap()
        .write_all(&completions_output.stdout)
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "bash -n rejected completions script: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

// ============================================================================
// File-on-disk precedence (backward compatibility)
// ============================================================================

#[test]
fn test_completions_subcommand_always_routes_to_subcommand() {
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("completions");
    fs::write(&file, "fn setup() {}").unwrap();

    // After the router fix, bare "completions" ALWAYS routes to the subcommand
    // even when a file named "completions" exists on disk.
    // To read such a file, users must use ./completions or the full path.
    Command::cargo_bin("skim")
        .unwrap()
        .current_dir(dir.path())
        .arg("completions")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim completions"));
}

// ============================================================================
// Error handling
// ============================================================================

#[test]
fn test_completions_missing_shell_errors() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("completions")
        .assert()
        .failure()
        .stderr(predicate::str::contains("SHELL").or(predicate::str::contains("shell")));
}

#[test]
fn test_completions_invalid_shell_errors() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("completions")
        .arg("invalid_shell_name")
        .assert()
        .failure();
}

// ============================================================================
// Help
// ============================================================================

#[test]
fn test_completions_help() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("completions")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Generate shell completion"));
}

// ============================================================================
// Nested subcommand completions (#116)
// ============================================================================

#[test]
fn test_completions_include_file_subcommands() {
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["completions", "bash"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    // file subcommand and its tools should appear in completions
    assert!(
        stdout.contains("file"),
        "Bash completions should include 'file' subcommand"
    );
    // Check for at least some known file tools
    for tool in &["find", "grep", "ls", "rg", "tree"] {
        assert!(
            stdout.contains(tool),
            "Bash completions should include '{tool}' as file tool"
        );
    }
}

#[test]
fn test_completions_include_infra_subcommands() {
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["completions", "bash"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("infra"),
        "Bash completions should include 'infra' subcommand"
    );
    for tool in &["aws", "curl", "gh", "wget"] {
        assert!(
            stdout.contains(tool),
            "Bash completions should include '{tool}' as infra tool"
        );
    }
}

#[test]
fn test_completions_include_log_flags() {
    let output = Command::cargo_bin("skim")
        .unwrap()
        .args(["completions", "bash"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("log"),
        "Bash completions should include 'log' subcommand"
    );
    // Check for log-specific flags
    assert!(
        stdout.contains("no-dedup") || stdout.contains("--no-dedup"),
        "Bash completions should include log's --no-dedup flag"
    );
}
