//! E2E tests for daemon/streaming command passthrough.
//!
//! Verifies that indefinitely-running commands are routed through
//! `run_inherited_passthrough` instead of being buffered by the normal
//! compression pipeline, and that finite commands are still compressed.
//!
//! Design note: the daemon guard fires regardless of whether stdin is a
//! terminal. Bare `vitest` is indefinite; use
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
mod common;

/// Per-binary cache sandbox — see the identical note in `cli_e2e_rewrite.rs`.
///
/// `skim rewrite --hook` writes the D5 force-raw marker into
/// `{SKIM_CACHE_DIR}/sessions/{ppid}.raw`, keyed to the shared nextest runner
/// PID.  Left pointing at the real `~/.cache/skim`, that marker is visible to
/// wrapper-surface tests in other binaries via `force_raw_requested()`.
static CACHE_SANDBOX: std::sync::LazyLock<tempfile::TempDir> =
    std::sync::LazyLock::new(|| tempfile::tempdir().expect("cache sandbox tempdir must succeed"));

fn skim_cmd() -> Command {
    let mut cmd = common::skim();
    // Remove SKIM_PASSTHROUGH so the daemon guard is active.
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env("SKIM_CACHE_DIR", CACHE_SANDBOX.path());
    // `try_rewrite`'s Step 2b declines to rewrite a program nothing can spawn
    // (#317, PF-038), so the hook-mode assertions below need `jest` to resolve.
    // See `common::rewrite_stub_path`.
    cmd.env("PATH", common::rewrite_stub_path());
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
///
/// The empty stdout this asserts has two possible causes, and only one of them
/// is the daemon guard: `try_rewrite`'s Step 2b also declines when nothing
/// named `jest` can be spawned (#317, PF-038).  On a host with no `jest` the
/// two are indistinguishable and this test stops discriminating — it passes
/// while the guard it is about is never reached (PF-025).  `skim_cmd`'s stub
/// `PATH` removes the second cause, so the empty stdout is attributable again;
/// the pairing with `test_hook_mode_jest_ci_is_rewritten` below, which sees
/// `jest` resolve through the same `PATH` and DOES rewrite, is what makes the
/// attribution observable.
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
/// The output is nonetheless NOT independent of whether `jest` is installed:
/// `try_rewrite`'s Step 2b consults `runner::program_resolves` and declines to
/// rewrite a program nothing can spawn (#317, PF-038), because a rewrite that
/// cannot run hands the reader a failing command in place of a working one.
/// Determinism here is earned rather than inherent — `skim_cmd` prepends
/// `common::rewrite_stub_path`, which guarantees `jest` resolves for the child
/// on every host.  Measured: under the inherited `PATH` on a machine with no
/// `jest`, this invocation returns empty stdout and the assertion below fails.
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
        // with no skip flags, so `jest --ci` rewrites to `skim jest --ci`
        // whenever `jest` resolves — which `skim_cmd`'s stub PATH guarantees.
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
