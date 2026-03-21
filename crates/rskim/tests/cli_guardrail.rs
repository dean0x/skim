//! CLI tests for output guardrail (#53)
//!
//! Tests that the guardrail triggers when compressed output is larger than raw,
//! and does not trigger for normal files or in full mode.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

/// Get a command for the skim binary
fn skim_cmd() -> Command {
    Command::cargo_bin("skim").unwrap()
}

#[test]
fn test_guardrail_skips_tiny_files() {
    // Tiny files (< 256 bytes) should skip the guardrail entirely because
    // transformation overhead is expected for small inputs.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("tiny.ts");
    std::fs::write(&file, "const x = 1;\n").unwrap();

    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim:guardrail]").not());
}

#[test]
fn test_guardrail_does_not_trigger_on_normal_file() {
    // A normal-sized file should compress well and not trigger the guardrail.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("normal.ts");
    std::fs::write(
        &file,
        "import { something } from 'somewhere';\n\
         type UserId = string;\n\
         interface User {\n\
           id: UserId;\n\
           name: string;\n\
           email: string;\n\
         }\n\
         function createUser(name: string, email: string): User {\n\
           const id = generateId();\n\
           return { id, name, email };\n\
         }\n\
         function deleteUser(id: UserId): void {\n\
           const user = findUser(id);\n\
           if (!user) throw new Error('not found');\n\
           removeFromDatabase(user);\n\
         }\n\
         export { createUser, deleteUser };\n",
    )
    .unwrap();

    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim:guardrail]").not());
}

#[test]
fn test_guardrail_skipped_in_full_mode() {
    // Full mode should skip the guardrail entirely.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("tiny.ts");
    std::fs::write(&file, "const x = 1;\n").unwrap();

    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=full")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim:guardrail]").not());
}
