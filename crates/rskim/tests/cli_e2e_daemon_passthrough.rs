//! E2E tests for daemon/streaming command passthrough (ADR-008 Part C).
//!
//! Verifies that indefinitely-running commands are routed through
//! `run_inherited_passthrough` instead of being buffered by the normal
//! compression pipeline, and that finite commands are still compressed.
//!
//! Design note: the daemon guard fires regardless of whether stdin is a
//! terminal (ADR-008 alignment fix). Bare `vitest` is indefinite; use
//! `vitest run` for the finite one-shot mode that skim should compress.
//! `should_read_stdin` treats `args == ["run"]` as stdin-eligible, so
//! `skim vitest run` + piped fixture goes through the compression pipeline.
//!
//! Parallelism note: every test here spawns the `skim` binary as a subprocess
//! with a 10-second wall-clock timeout. When the full workspace test suite runs
//! (≥2990 tests in parallel) these subprocesses are starved of CPU time and
//! reach the timeout boundary before completing. The `#[serial]` attribute runs
//! each test sequentially so they never compete with each other or the rest of
//! the parallel suite for process-spawn headroom.

use assert_cmd::Command;
use predicates::prelude::*;
use serial_test::serial;

fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    // Remove SKIM_PASSTHROUGH so the daemon guard is active.
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd
}

// ============================================================================
// Daemon passthrough: `vitest run` is FINITE — still goes through compression
// ============================================================================

/// `vitest run` is the finite one-shot invocation. Skim must still route it
/// through the test parser — the daemon guard only fires for bare `vitest`
/// (watch mode default) and explicit `--watch` variants.
///
/// `should_read_stdin` treats `args == ["run"]` as stdin-eligible so piped
/// fixture data reaches the parser even though args is non-empty.
#[test]
#[serial]
fn test_vitest_run_is_finite_and_compressed() {
    // Pipe a minimal vitest JSON fixture so skim can parse it.
    // `vitest run` is finite — daemon guard does not fire, compression applies.
    let fixture = include_str!("fixtures/cmd/test/vitest_pass.json");
    skim_cmd()
        .args(["vitest", "run"])
        .write_stdin(fixture)
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success()
        // Compression must have run: structured output contains "pass:"
        .stdout(predicate::str::contains("pass:"));
}

// ============================================================================
// Hook mode: indefinite commands produce no rewrite (passthrough)
// ============================================================================

/// In hook mode, `npm run dev` should not be rewritten — it returns empty
/// stdout (exit 0), telling the agent to run the original command unchanged.
#[cfg(unix)]
#[test]
#[serial]
fn test_hook_mode_indefinite_command_not_rewritten() {
    // Construct a minimal Claude Code hook payload for `npm run dev`.
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": {
            "command": "npm run dev"
        }
    });
    let payload_str = serde_json::to_string(&payload).unwrap();

    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(payload_str.as_bytes())
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success()
        // Empty stdout → agent runs the original command unchanged.
        .stdout(predicate::str::is_empty());
}

/// In hook mode, `jest --watch` is indefinite — must not be rewritten.
#[cfg(unix)]
#[test]
#[serial]
fn test_hook_mode_jest_watch_not_rewritten() {
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": {
            "command": "jest --watch"
        }
    });
    let payload_str = serde_json::to_string(&payload).unwrap();

    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(payload_str.as_bytes())
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
}

/// In hook mode, finite `jest --ci` IS rewritten to `skim jest --ci`.
///
/// Hook mode never executes the tool — it only rewrites the command string.
/// The output is therefore deterministic regardless of whether jest is installed.
///
/// Expected output: Claude Code hook response JSON containing the rewritten
/// command `"skim jest --ci"` inside `hookSpecificOutput.updatedInput.command`.
/// The JSON will contain the substring `"skim jest --ci"`.
#[cfg(unix)]
#[test]
#[serial]
fn test_hook_mode_jest_ci_is_rewritten() {
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": {
            "command": "jest --ci"
        }
    });
    let payload_str = serde_json::to_string(&payload).unwrap();

    skim_cmd()
        .args(["rewrite", "--hook"])
        .write_stdin(payload_str.as_bytes())
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success()
        // Hook mode emits a JSON response with the rewritten command.
        // `jest` is in the rule table (prefix: ["jest"], rewrite_to: ["skim", "jest"])
        // with no skip flags, so `jest --ci` always rewrites to `skim jest --ci`.
        // The rewritten command is embedded in the hook response JSON — check both
        // that the output is non-empty and that it contains the rewritten command.
        .stdout(predicate::str::contains("skim jest --ci"));
}

// ============================================================================
// Direct dispatch: indefinite command exits cleanly via inherited passthrough
// ============================================================================

/// Smoke test: `skim nodemon app.js` must return within the timeout and not
/// hang, regardless of whether `nodemon` is installed.
///
/// `nodemon` is always-indefinite, so the daemon guard routes it through
/// `run_inherited_passthrough`:
///   - If `nodemon` is not installed → exit 127 (ENOENT)
///   - If `nodemon` is installed in CI → it starts but we don't wait for it
///     (the timeout safety-net catches any hang)
///
/// The deterministic assertion that exit-127 maps correctly is covered by the
/// unit test `dispatch::tests::test_run_inherited_passthrough_missing_binary`
/// in `crates/rskim/src/cmd/dispatch.rs`, which calls `run_inherited_passthrough`
/// directly with a guaranteed-absent program name.
///
/// This E2E test is a routing / no-hang smoke check only: it proves the guard
/// fires and the binary returns within a reasonable time.
#[cfg(unix)]
#[test]
#[serial]
fn test_direct_dispatch_indefinite_exits_quickly_when_binary_missing() {
    // `nodemon` is always-indefinite per the detection table and is essentially
    // never present in Rust CI toolchains. Exit 127 = ENOENT through
    // run_inherited_passthrough.
    skim_cmd()
        .args(["nodemon", "app.js"])
        // No-hang safety net only (10s matches the sibling smoke tests above).
        // The earlier 5s bound flaked under peak parallel-test CPU contention;
        // the deterministic exit-127 mapping is covered by the dispatch.rs unit
        // test cited above, so this bound just needs enough headroom to never
        // trip on a healthy machine.
        .timeout(std::time::Duration::from_secs(10))
        .assert();
        // Primary check: exits within the timeout (does not hang).
        // Exit-code mapping (127 for not-found vs 0/non-zero) is covered by the
        // dispatch.rs unit test — this test only gates the no-hang property.
}
