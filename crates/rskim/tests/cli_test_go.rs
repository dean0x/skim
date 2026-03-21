//! Integration tests for `skim test go` subcommand (#49).

use assert_cmd::Command;
use predicates::prelude::*;
use std::process::Command as StdCommand;

// ============================================================================
// Help and basic routing
// ============================================================================

#[test]
fn test_skim_test_help() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("test")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim test"));
}

#[test]
fn test_skim_test_go_unknown_runner() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("test")
        .arg("nonexistent")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown runner"));
}

#[test]
fn test_skim_test_no_args_shows_help() {
    Command::cargo_bin("skim")
        .unwrap()
        .arg("test")
        .assert()
        .success()
        .stdout(predicate::str::contains("Available runners"));
}

// ============================================================================
// Real `go test` integration (gated on Go being installed)
// ============================================================================

#[test]
fn test_skim_test_go_real_execution() {
    // Gate: skip if Go is not installed
    if StdCommand::new("go").arg("version").output().is_err() {
        eprintln!("Skipping: Go not installed");
        return;
    }

    // Create a minimal Go project in a temp dir
    let dir = tempfile::TempDir::new().unwrap();
    let go_mod = dir.path().join("go.mod");
    let go_test_file = dir.path().join("add_test.go");

    std::fs::write(&go_mod, "module example.com/test\n\ngo 1.21\n").unwrap();

    std::fs::write(
        &go_test_file,
        r#"package test

import "testing"

func TestAdd(t *testing.T) {
    if 1+1 != 2 {
        t.Fatal("math is broken")
    }
}
"#,
    )
    .unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .current_dir(dir.path())
        .arg("test")
        .arg("go")
        .arg("./...")
        .assert()
        .success()
        .stdout(predicate::str::contains("PASS"));
}
