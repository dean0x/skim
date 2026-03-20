//! Integration tests for `skim completions` subcommand (#63).

use assert_cmd::Command;
use predicates::prelude::*;

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
