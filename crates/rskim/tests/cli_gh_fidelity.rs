//! Regression tests for gh handler fidelity issues (regression-3, architecture-14).
//!
//! # regression-3 — net-savings guard for `gh` must not emit injected `--json` output
//!
//! Every `gh` handler injects `--json <fields>` in `prepare_args`.  If the
//! net-savings guard fires and elects Passthrough, `emit_raw_passthrough` would
//! emit the *injected* command's stdout — raw `--json` output — rather than the
//! user's original argv output.  `raw_override` is `None` on every `gh` CONFIG
//! (no handler arms it), so the guard cannot honour its contract.
//! `skip_net_savings_guard: true` restores the pre-branch behaviour.
//!
//! # architecture-14 — `gh run watch --json` must be rejected explicitly
//!
//! `gh run watch` is a streaming command with no well-defined JSON envelope.
//! The `--json` flag was silently ignored (the flag was consumed but never read
//! in run_watch.rs).  The fix rejects it explicitly with an actionable error.

use assert_cmd::Command;
use predicates::prelude::*;
mod common;

fn skim_cmd() -> Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

// ============================================================================
// regression-3: net-savings guard bypassed for gh handlers
// ============================================================================

/// regression-3: the output of `skim gh issue view` must be skim's compressed
/// format, never the raw injected `--json` bytes.
///
/// The stub `gh` returns a JSON response that skim's issue_view parser can
/// compress.  Even if the compressed view were larger than the raw JSON
/// (triggering the guard under `skip_net_savings_guard: false`), skim must
/// still emit compressed output because the guard is unconditionally skipped
/// for `gh` (PF-024: `raw_override = None` means the fallback cannot emit the
/// user's original argv output — so the guard must be skipped).
#[cfg(unix)]
#[test]
fn test_gh_issue_view_emits_compressed_not_injected_json() {
    let dir = tempfile::tempdir().unwrap();
    // The stub returns a valid gh issue view JSON response.  It accepts any
    // args (skim will inject --json and field names).
    let json_response = r#"{
  "number": 99,
  "title": "Minimal issue",
  "state": "CLOSED",
  "author": {"login": "testuser"},
  "body": "",
  "labels": [],
  "assignees": [],
  "milestone": null,
  "comments": []
}"#;
    common::make_stub(dir.path(), "gh", json_response, "", 0);

    let output = skim_cmd()
        .env("PATH", common::stub_path(dir.path()))
        .args(["gh", "issue", "view", "99"])
        .assert()
        .success()
        // skim's compressed output must contain the issue title or number.
        .stdout(predicate::str::contains("Minimal issue").or(predicate::str::contains("99")))
        // regression-3: the injected --json raw bytes must NOT be the output.
        // If the guard fired and elected passthrough, stdout would start with '{'.
        // Compressed skim output never starts with a bare JSON object.
        .stdout(predicate::str::starts_with("{").not())
        .get_output()
        .stdout
        .clone();

    // The compressed output must be present (not just whitespace).
    assert!(
        !output.is_empty(),
        "skim gh issue view must produce output (got empty stdout)"
    );
}

// ============================================================================
// architecture-14: gh run watch --json must be rejected
// ============================================================================

/// architecture-14: `skim gh run watch --json` must fail with an actionable
/// error message on stderr rather than silently returning plain streaming text.
///
/// `gh run watch` is a streaming command with no JSON output shape; honouring
/// `--json` would require inventing a JSON envelope that does not exist.  The
/// MUST "fail loud, never silently" design constraint requires an explicit
/// rejection.
#[test]
fn test_gh_run_watch_json_flag_is_rejected() {
    skim_cmd()
        .args(["gh", "run", "watch", "--json"])
        // Pipe stdin so the command does not try to read from the terminal.
        .write_stdin("")
        .assert()
        .failure()
        // The error must name the limitation.
        .stderr(predicate::str::contains("gh run watch --json"))
        .stderr(predicate::str::contains("not supported"));
}
