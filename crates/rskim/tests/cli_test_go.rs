//! Integration tests for `skim go test` subcommand (#49).
//!
//! v2.8.0: Flat dispatch — `skim go test` replaces `skim test go`.

use assert_cmd::Command;
use predicates::prelude::*;
use std::process::Command as StdCommand;

// ============================================================================
// Help and basic routing
// ============================================================================

#[test]
fn test_skim_go_help() {
    // v2.8.0: `skim go --help` — "test" is no longer a subcommand.
    Command::cargo_bin("skim")
        .unwrap()
        .arg("go")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("skim go"));
}

// v2.8.0: "test" is no longer a subcommand. Unknown runner tests via
// `skim test nonexistent` are removed — unknown names are handled at the
// dispatch level as unknown subcommands.

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
        .arg("go")
        .arg("test")
        .arg("./...")
        .assert()
        .success()
        .stdout(predicate::str::contains("pass:"));
}
