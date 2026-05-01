//! Integration tests for `skim cargo test` subcommand (#46).
//!
//! v2.8.0: Flat dispatch — `skim cargo test` replaces `skim test cargo`.
//!
//! Tests the end-to-end cargo test parser via the CLI binary.

use assert_cmd::Command;
use predicates::prelude::*;

// ============================================================================
// Real cargo test execution
// ============================================================================

#[test]
fn test_skim_test_cargo_in_this_repo() {
    // Run `skim cargo test -p rskim-core` on skim's own repo.
    // This executes a real `cargo test` and parses the output.
    // We use -p rskim-core to limit scope and speed up the test.
    let assert = Command::cargo_bin("skim")
        .unwrap()
        .args(["cargo", "test", "-p", "rskim-core"])
        .timeout(std::time::Duration::from_secs(120))
        .assert();

    // Should produce structured output with pass count (tier 2 regex)
    assert.stdout(predicate::str::contains("pass:"));
}

// ============================================================================
// Help text
// ============================================================================

#[test]
fn test_skim_cargo_help() {
    // v2.8.0: `skim cargo --help` — "test" is no longer a subcommand.
    Command::cargo_bin("skim")
        .unwrap()
        .args(["cargo", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("skim cargo"));
}

// ============================================================================
// Unknown runner
// ============================================================================

// v2.8.0: "test" is no longer a subcommand. Unknown tool names are unknown
// subcommands at the dispatch level. This test is removed.

// ============================================================================
// Piped stdin parsing
// ============================================================================

#[test]
fn test_skim_test_cargo_stdin_json() {
    // Pipe cargo JSON output via stdin
    let json_input = r#"{"type":"suite","event":"started","test_count":2}
{"type":"test","event":"ok","name":"test_a","exec_time":0.001}
{"type":"test","event":"ok","name":"test_b","exec_time":0.002}
{"type":"suite","event":"ok","passed":2,"failed":0,"ignored":0,"measured":0,"filtered_out":0,"exec_time":0.003}
"#;

    Command::cargo_bin("skim")
        .unwrap()
        .args(["cargo", "test"])
        .write_stdin(json_input)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 2"))
        .stdout(predicate::str::contains("fail: 0"));
}

#[test]
fn test_skim_test_cargo_stdin_plain_text() {
    // Pipe plain text cargo test output (tier 2 regex)
    let text_input = "running 5 tests\n\
        test test_one ... ok\n\
        test test_two ... ok\n\
        test test_three ... ok\n\
        test test_four ... ok\n\
        test test_five ... ok\n\n\
        test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n";

    Command::cargo_bin("skim")
        .unwrap()
        .args(["cargo", "test"])
        .write_stdin(text_input)
        .assert()
        .success()
        .stdout(predicate::str::contains("pass: 5"));
}
