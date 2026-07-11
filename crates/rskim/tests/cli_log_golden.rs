//! CLI golden tests for `skim log` — byte-stability regression guard (#427).
//!
//! Two goldens are captured here:
//!
//! **STABLE golden** (`cli_log_golden_stable`): exercises a log with no continuation
//! lines, no Traceback headers, and no chained-exception separators. This golden
//! MUST stay byte-identical through the entire #427 epic — Pass 4's P1.1 header-count
//! fix does not affect it.
//!
//! **COUNTERFIX golden** (`cli_log_golden_counterfix`): exercises continuation +
//! traceback + chained-exception separators (the P1.1 bug surface). This golden
//! captures the CURRENT (buggy) output and WILL be intentionally re-captured in
//! Pass 4 when the P1.1 header counting is fixed. Do not treat a change to this
//! golden as a regression until Pass 4.
//!
//! ## Capture procedure (for re-capture in Pass 4)
//!
//! ```sh
//! # From repo root — use the debug binary (goldens are stdout text):
//! cargo build -p rskim
//! ./target/debug/skim log < crates/rskim/tests/fixtures/cmd/log/plaintext_mixed.txt
//! ./target/debug/skim log < crates/rskim/tests/fixtures/cmd/log/stack_trace_python_chained.txt
//! ```
//!
//! Update the corresponding `GOLDEN_*` constant with the captured output.

use assert_cmd::Command;

fn skim_cmd() -> Command {
    let mut cmd = Command::cargo_bin("skim").unwrap();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

// ============================================================================
// STABLE golden — plaintext_mixed.txt
//
// Input: standard timestamped log with duplicates and debug lines.
// No traceback headers, no continuation lines, no chained-exception separators.
// This output must stay byte-identical through ALL passes of #427.
// ============================================================================

const STABLE_INPUT: &str = include_str!("fixtures/cmd/log/plaintext_mixed.txt");

/// Captured from `./target/debug/skim log < plaintext_mixed.txt` on 2026-07-11.
/// Pinned against: rskim-compress::log at wave/l3-wave2 HEAD c6585948.
const GOLDEN_STABLE: &str = "12 lines \u{2192} 7 unique (3 duplicates removed)\n\
2 debug lines hidden (skim log --debug-only)\n\
 INFO: server starting on port 8080\n\
 INFO: database connected to localhost:5432\n\
 INFO: request received GET /api/users (\u{00d7}2)\n\
 WARN: slow query detected (1200ms)\n\
 ERROR: connection refused: redis:6379 (\u{00d7}3)\n\
 INFO: request completed 200 OK\n\
 INFO: request completed 500 Internal Server Error";

/// STABLE golden: must be byte-identical through all #427 passes.
///
/// Discriminating: if the log engine's dedup/format output changes for
/// standard timestamped logs (no tracebacks, no chained exceptions),
/// this test fails immediately — no silently-broken output can sneak through.
#[test]
fn cli_log_golden_stable() {
    let output = skim_cmd()
        .arg("log")
        .write_stdin(STABLE_INPUT)
        .output()
        .expect("skim log must run successfully");

    assert!(
        output.status.success(),
        "skim log must exit 0. stderr: {:?}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout must be UTF-8");
    let stdout_trimmed = stdout.trim_end_matches('\n');

    assert_eq!(
        stdout_trimmed,
        GOLDEN_STABLE,
        "STABLE golden mismatch — byte-identical output required through all #427 passes.\n\
         Expected:\n{GOLDEN_STABLE:?}\n\
         Got:\n{stdout_trimmed:?}",
    );
}

// ============================================================================
// COUNTERFIX golden — stack_trace_python_chained.txt
//
// Input: log with chained-exception separator ("The above exception was the
// direct cause..."), two Traceback blocks, and continuation stack-frame lines.
// This golden captures the CURRENT BUGGY OUTPUT (P1.1 counting bug):
//   "4 lines → 5 unique (0 duplicates removed)"
//   (total_lines=4 < unique_messages=5 — saturating_sub yields 0 duplicates)
//
// This golden WILL change in Pass 4 when P1.1 is fixed. Re-capture then.
// Until Pass 4, a change to this golden IS expected and planned.
// ============================================================================

const COUNTERFIX_INPUT: &str = include_str!("fixtures/cmd/log/stack_trace_python_chained.txt");

/// Captured from `./target/debug/skim log < stack_trace_python_chained.txt` on 2026-07-11.
/// Pinned against: rskim-compress::log at wave/l3-wave2 HEAD c6585948.
///
/// NOTE: This output contains the P1.1 bug (total_lines < unique_messages → 0 duplicates).
/// It WILL be re-captured in Pass 4 when the header-counting fix lands (#427 P1.1).
const GOLDEN_COUNTERFIX: &str = "4 lines \u{2192} 5 unique (0 duplicates removed)\n\
 ERROR: Operation failed\n\
Traceback (most recent call last):\n\
File \"/app/db.py\", line 45, in query\n\
cursor.execute(sql)\n\
File \"/app/db.py\", line 102, in execute\n\
return self._run(stmt)\n\
 DatabaseError: connection timeout\n\
 The above exception was the direct cause of the following exception:\n\
Traceback (most recent call last):\n\
File \"/app/api.py\", line 30, in handle\n\
db.query(q)\n\
File \"/app/api.py\", line 55, in respond\n\
return handle(req)\n\
File \"/app/main.py\", line 10, in run\n\
respond(request)\n\
 ServiceError: failed to process request\n\
 INFO: recovered";

/// COUNTERFIX golden: captures current (buggy) P1.1 output.
///
/// This test will INTENTIONALLY FAIL after Pass 4 fixes P1.1. Re-capture then.
///
/// Discriminating NOW: if the header counting regresses in an unplanned way
/// before Pass 4, this test catches the drift. After Pass 4, re-capture the
/// fixed output and this becomes the new permanent stable golden.
#[test]
fn cli_log_golden_counterfix() {
    let output = skim_cmd()
        .arg("log")
        .write_stdin(COUNTERFIX_INPUT)
        .output()
        .expect("skim log must run successfully");

    assert!(
        output.status.success(),
        "skim log must exit 0. stderr: {:?}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout must be UTF-8");
    let stdout_trimmed = stdout.trim_end_matches('\n');

    assert_eq!(
        stdout_trimmed,
        GOLDEN_COUNTERFIX,
        "COUNTERFIX golden mismatch.\n\
         If this is Pass 4 (P1.1 fix landed): re-capture this golden with the fixed output.\n\
         If this is an unplanned change: investigate the log engine regression.\n\
         Expected:\n{GOLDEN_COUNTERFIX:?}\n\
         Got:\n{stdout_trimmed:?}",
    );
}
