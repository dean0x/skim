//! CLI tests for output guardrail (#53)
//!
//! Tests that the guardrail triggers when compressed output is larger than raw,
//! and does not trigger for normal files or in full mode.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;
mod common;

/// Get a command for the skim binary
fn skim_cmd() -> Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd.env_remove("SKIM_REWRITTEN_FROM");
    cmd
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

#[test]
fn test_guardrail_triggers_when_output_inflates() {
    // Structure mode replaces function bodies with ` {...}` (6 bytes).
    // For functions with empty bodies `{ }` (3 bytes), each replacement ADDS
    // 3 bytes. With enough short functions (>= 256 bytes total raw), the
    // compressed output exceeds the raw size in both bytes and tokens,
    // triggering the guardrail warning on stderr.
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("inflating.ts");

    // Each line is ~18 bytes: `function XX() { }\n`
    // 20 functions = ~360 bytes raw (well above the guardrail activation size).
    // Each function body `{ }` (3 bytes) -> ` {...}` (6 bytes) = +3 bytes
    // Total output growth: 20 * 3 = 60 extra bytes -> ~420 bytes output vs ~360 raw
    let mut source = String::new();
    for i in 0..20 {
        source.push_str(&format!("function f{i}() {{ }}\n"));
    }
    assert!(
        source.len() >= 256,
        "Test file must be >= 256 bytes for guardrail to activate, got {}",
        source.len()
    );

    std::fs::write(&file, &source).unwrap();

    skim_cmd()
        .env("SKIM_DEBUG", "1")
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::contains("[skim:guardrail]"));
}

/// SKIM_DEBUG-unset negative twin of `test_guardrail_triggers_when_output_inflates`.
///
/// Same inflating-file scenario (guardrail DOES fire), but with SKIM_DEBUG removed
/// by `skim_cmd()`.  Per ADR-011, when the guardrail fires the banner routes to
/// `io::sink()` when debug mode is off — stderr must be empty.
///
/// Paired with the SKIM_DEBUG=1 positive assertion above to make the gate
/// revert-detectable: reverting `apply_to_stderr` to always-write leaves the
/// positive test green (banner still appears) but breaks this test (stderr would no
/// longer be empty) — CI catches the regression.  Without this twin, all four Fix-5
/// gating edits can be reverted and the suite stays green.
#[test]
fn test_guardrail_fires_silently_without_debug() {
    // Identical inflating source to test_guardrail_triggers_when_output_inflates:
    // 20 functions with empty bodies inflate under --mode=structure (guardrail fires).
    let dir = TempDir::new().unwrap();
    let file = dir.path().join("inflating.ts");

    let mut source = String::new();
    for i in 0..20 {
        source.push_str(&format!("function f{i}() {{ }}\n"));
    }
    assert!(
        source.len() >= 256,
        "Test file must be >= 256 bytes for guardrail to activate, got {}",
        source.len()
    );

    std::fs::write(&file, &source).unwrap();

    // SKIM_DEBUG is unset — skim_cmd() removes it at construction.
    // The guardrail fires (output inflates) but apply_to_stderr routes the banner
    // to io::sink() when debug mode is off.  Stderr must be empty.
    skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--mode=structure")
        .arg("--no-cache")
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
}
