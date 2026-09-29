//! F12 — the interior-newline rewrite bail must emit a signal (partial mitigation of #337).
//!
//! Measured at `c2b4378`: a PreToolUse payload whose `command` carries an
//! interior newline is declined by the rewrite engine and produces **0 B stdout,
//! 0 B stderr, exit 0 and an empty cache dir** — no `hook.log` line, no stamp,
//! no sidecar. The same command on one line emits 99 B of hook-response JSON,
//! so the silence is specific to the bail rather than to hook mode.
//!
//! Two invariants live here, and the second one had **no coverage anywhere**
//! before this file: the two tests that reach the bail
//! (`cli_e2e_rewrite.rs::test_hook_multiline_commit_is_never_rewritten` and
//! `::test_hook_newline_in_command_is_never_rewritten`) assert only `.success()`
//! and empty **stdout**, and `test_rewrite_hook_all_agents_zero_stderr` sends
//! only single-line *rewritable* commands, so it never reaches the bail. A
//! stderr write added at the bail site would have failed nothing.
//!
//! 1. The bail records a **rate-limited** line to `hook.log` naming the reason,
//!    and a second bail on the same day does **not** duplicate it.
//! 2. The bail writes **nothing to stderr** — hook mode's zero-stderr invariant
//!    (GRANITE #361 Bug 3), and ADR-011's class-2 rule that a no-loss notice may
//!    never be an unconditional stderr write.

use std::fs;
use std::path::Path;

use tempfile::TempDir;

mod common;

/// Substring identifying the declined-rewrite line in `hook.log`.
///
/// Deliberately a literal rather than an import: `hook.rs`'s message is
/// `pub(super)`-scoped implementation, and an integration test that reads the
/// value it asserts on cannot fail when that value changes.
const DECLINE_MARKER: &str = "rewrite declined: multi-line command";

/// A command whose INTERIOR newline makes the rewrite engine bail.
///
/// Both clauses rewrite individually (`cargo test` → `skim cargo test`,
/// `cargo build` → `skim cargo build`), so the newline is the sole trigger.
const MULTILINE_COMMAND: &str = "cargo test\ncargo build";

/// A hook-mode invocation with every ambient contaminant pinned.
///
/// - `SKIM_CACHE_DIR` — mandatory for any `rewrite --hook` test: the force-raw
///   sidecar is keyed `{ppid}.{tool}.raw` and a marker from another test in the
///   same runner is readable by this one. It is also where `hook.log` and the
///   rate-limit stamp land, which is what makes the log observable hermetically.
/// - `CLAUDE_CONFIG_DIR` — points the ClaudeCode integrity check at an empty
///   directory so the verdict cannot depend on the developer's real `~/.claude`.
/// - `SKIM_PASSTHROUGH` removed — set in the ambient environment it returns from
///   `run_hook_mode` before any of this runs, and every assertion below would
///   report "already perfect" against a binary that did nothing (PF-026).
/// - `SKIM_DEBUG` removed — the signal must be UNCONDITIONAL. ADR-011 debug-gates
///   class-2 banners on *stderr*; a `hook.log` write is outside that taxonomy, and
///   asserting the line appears with no debug flag set is what pins the difference.
/// - `SKIM_HOOK_AUDIT` removed — its opt-in `hook-audit.log` trace is a different
///   sink and must not stand in for this signal.
fn hook_cmd(cache_dir: &Path, config_dir: &Path) -> assert_cmd::Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd.env_remove("SKIM_HOOK_AUDIT");
    cmd.env("SKIM_CACHE_DIR", cache_dir);
    cmd.env("CLAUDE_CONFIG_DIR", config_dir);
    cmd.args(["rewrite", "--hook", "--agent", "claude-code"]);
    cmd
}

/// Build a Claude Code PreToolUse payload, letting `serde_json` own the escaping
/// so an interior newline is one real byte rather than a two-character escape.
fn payload(command: &str) -> String {
    serde_json::json!({ "tool_input": { "command": command } }).to_string()
}

/// Every `hook.log` line carrying the decline marker.
///
/// Filtered rather than counted whole: `hook.log` legitimately carries other
/// lines (the `SKIM_DEBUG` provenance line, integrity and drift warnings), and a
/// total-line count would make this assertion depend on them.
fn decline_lines(cache_dir: &Path) -> Vec<String> {
    match fs::read_to_string(cache_dir.join("hook.log")) {
        Ok(contents) => contents
            .lines()
            .filter(|line| line.contains(DECLINE_MARKER))
            .map(str::to_string)
            .collect(),
        // Absent hook.log — no cache dir was written at all — is zero decline
        // lines, which is exactly the pre-fix state this file exists to reject.
        Err(_) => Vec::new(),
    }
}

/// The bail records the decline to `hook.log`, once per day, naming the reason.
///
/// Non-vacuity (PF-025): the rate-limit half of this test — "one line after two
/// bails" — is satisfied by a mechanism that wrote *zero* lines, so the count
/// after the FIRST bail is asserted to be exactly 1 before the second bail runs.
/// A signal that never fires fails on that assertion rather than passing on the
/// one after it. The byte-equality check on the two snapshots then proves the
/// second bail appended nothing, rather than appending a line this filter missed.
#[test]
fn test_multiline_bail_logs_rate_limited_signal_to_hook_log() {
    let cache = TempDir::new().expect("cache tempdir");
    let config = TempDir::new().expect("config tempdir");

    let first = hook_cmd(cache.path(), config.path())
        .write_stdin(payload(MULTILINE_COMMAND))
        .output()
        .expect("first hook invocation must run");

    assert!(
        first.status.success(),
        "the bail is a passthrough: exit 0 expected, got {:?}",
        first.status.code()
    );
    assert!(
        first.stdout.is_empty(),
        "the bail must still emit no hook response, got: {}",
        String::from_utf8_lossy(&first.stdout)
    );

    let after_first = decline_lines(cache.path());
    assert_eq!(
        after_first.len(),
        1,
        "the interior-newline bail must write exactly one decline line to hook.log; \
         got {} line(s): {after_first:?}",
        after_first.len()
    );

    // The message must be actionable: it names the reason, not merely that
    // something happened, and discloses that it is rate-limited so one line is
    // never read as one occurrence.
    let line = &after_first[0];
    assert!(
        line.contains("interior newline"),
        "decline line must name the reason, got: {line}"
    );
    assert!(
        line.contains("no output is lost"),
        "decline line must state that the original command still runs (ADR-011 class 2), \
         got: {line}"
    );
    assert!(
        line.contains("Rate-limited"),
        "decline line must disclose that it is rate-limited — one line is a class, \
         not a count, got: {line}"
    );

    // Second bail, same cache dir, same day: warn_once_daily must suppress it.
    let second = hook_cmd(cache.path(), config.path())
        .write_stdin(payload(MULTILINE_COMMAND))
        .output()
        .expect("second hook invocation must run");
    assert!(second.status.success(), "second bail must also exit 0");

    let after_second = decline_lines(cache.path());
    assert_eq!(
        after_second.len(),
        1,
        "a second bail within the rate-limit window must not duplicate the line; \
         got {} line(s): {after_second:?}",
        after_second.len()
    );
    assert_eq!(
        after_first, after_second,
        "the suppressed second bail must leave the logged line byte-identical"
    );
}

/// The bail writes nothing to stderr.
///
/// This is the coverage gap the fix depends on and did not have: hook mode's
/// zero-stderr invariant (GRANITE #361 Bug 3) is documented at
/// `hook.rs::warn_once_daily` and `hook_log.rs::log_hook_warning`, and asserted
/// by four tests in `cli_e2e_rewrite.rs` — none of which sends a payload that
/// reaches the bail.
///
/// Non-vacuity: "stderr is empty" also holds for an invocation that never ran,
/// that exited early on `SKIM_PASSTHROUGH`, or that bailed before the signal. The
/// hook.log assertion proves the bail was reached and the signal did fire, so the
/// empty stderr is a property of *this* code path and not of a no-op.
#[test]
fn test_multiline_bail_writes_nothing_to_stderr() {
    let cache = TempDir::new().expect("cache tempdir");
    let config = TempDir::new().expect("config tempdir");

    let out = hook_cmd(cache.path(), config.path())
        .write_stdin(payload(MULTILINE_COMMAND))
        .output()
        .expect("hook invocation must run");

    assert!(
        out.stderr.is_empty(),
        "the bail must write zero bytes to stderr (GRANITE #361 Bug 3; ADR-011 class 2 \
         forbids an unconditional stderr notice), got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.status.success(),
        "the bail is a passthrough: exit 0 expected, got {:?}",
        out.status.code()
    );
    assert_eq!(
        decline_lines(cache.path()).len(),
        1,
        "non-vacuity: the signal must have fired, or empty stderr proves nothing"
    );
}

/// Specificity control: a command that IS rewritten logs no decline line.
///
/// Covers both rewritable shapes, including the one that distinguishes the
/// trigger. `command_needs_passthrough` calls `trim_end()` before testing for a
/// newline, so a command with only a TRAILING newline is rewritten and must stay
/// silent — the signal is keyed on the same condition the bail is, not on "the
/// command contained a newline byte".
#[test]
fn test_rewritten_commands_log_no_decline_signal() {
    let cache = TempDir::new().expect("cache tempdir");
    let config = TempDir::new().expect("config tempdir");

    for (label, command) in [
        ("single-line", "cargo test"),
        ("trailing-newline-only", "cargo test\n"),
    ] {
        let out = hook_cmd(cache.path(), config.path())
            .write_stdin(payload(command))
            .output()
            .unwrap_or_else(|e| panic!("{label} hook invocation must run: {e}"));

        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("skim cargo test"),
            "{label} must be rewritten — otherwise this control proves nothing about \
             the decline path, got: {stdout}"
        );
        assert!(
            decline_lines(cache.path()).is_empty(),
            "{label} was rewritten, so no decline line may be logged: {:?}",
            decline_lines(cache.path())
        );
    }
}
