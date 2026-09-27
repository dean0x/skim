//! PF-024 for `gh`: when the ADR-001 guard elects raw, the body must be the
//! output of the command the **user** typed — not of the one skim synthesised.
//!
//! # The defect
//!
//! Every `gh` view/list handler injects `--json <fields>` in `prepare_args` so
//! the response is parseable.  When the net-savings guard then decides the
//! structured summary is not smaller than the input, it emits "raw" — and the
//! raw it had was the injected command's JSON.  The reader asked for
//! `gh issue view 93` and got a wire-format JSON object instead, usually larger
//! than what they would have seen without skim in the path at all.
//!
//! # The fix under test
//!
//! `run_tool_rerunnable` hands `execution::RawFallback` the user's argv,
//! captured before `prepare_args` touches it.  `RawFallback` holds the command
//! **unexecuted**; it is resolved at exactly one place — the guard's
//! `Passthrough` arm.  So the second `gh` invocation happens only when the
//! guard has already thrown the compressed view away, never on the common path.
//! `gh api` is excluded outright: it issues whatever HTTP method its caller
//! asked for, and re-running a non-idempotent command to improve a display is
//! not a trade worth making.
//!
//! # Fixtures
//!
//! The two JSON payloads are the ones `cli_e2e_rewrite.rs` already uses and
//! documents, reused for their already-measured guard verdicts (minimal: 114 B,
//! guard serves raw — pinned by
//! `test_gh_minimal_payload_guard_serves_raw_on_both_gate_branches`; populated:
//! 381 B → 352 B summary, guard keeps).  PF-027: they are reused at their
//! measured sizes and must not be resized to move a verdict.

#![cfg(unix)]

mod common;

/// Minimal issue-view payload — nothing for the compressor to find, so the
/// ADR-001 guard elects raw.  This is the branch the fix is about.
const MINIMAL_JSON: &str = r#"{"number":93,"state":"OPEN","title":"Test","body":"__FAKE_GH_SENTINEL__","labels":[],"assignees":[],"comments":[]}"#;

/// Populated payload — the summary is genuinely smaller, so the guard keeps the
/// compressed view and no fallback is owed.
const POPULATED_JSON: &str = r#"{"number":93,"state":"OPEN","title":"Test PR: add feature","body":"__FAKE_GH_SENTINEL__ Feature: structured output. Tasks: API design, tests, implementation, review. Acceptance: all tests pass, no regressions.","labels":[{"name":"enhancement"},{"name":"needs-review"}],"assignees":[{"login":"octocat"}],"comments":[{"author":{"login":"reviewer"},"body":"LGTM, just fix the nits"}]}"#;

/// What the user's own `gh issue view 93` prints — the bytes the fallback owes
/// them.  Deliberately shares no vocabulary with the JSON payloads so an
/// assertion on it cannot be satisfied by the injected output (ADR-003's
/// "assert on content the broken render could not produce").
const USER_VIEW_TEXT: &str = "Test #93 OPEN opened by octocat";

/// Install a `gh` stub that answers differently depending on whether skim's
/// injected `--json` is present, and appends one byte per invocation to
/// `$GH_CALL_LOG` so the number of runs is observable.
fn install_gh_stub(dir: &std::path::Path, json_payload: &str) {
    let script = String::new()
        + "#!/bin/sh\n"
        + "printf 'x' >> \"$GH_CALL_LOG\"\n"
        + "for a in \"$@\"; do\n"
        + "  if [ \"$a\" = \"--json\" ]; then\n"
        + "    printf '%s' '"
        + json_payload
        + "'\n"
        + "    exit 0\n"
        + "  fi\n"
        + "done\n"
        + "printf '%s\\n' '"
        + USER_VIEW_TEXT
        + "'\n"
        + "exit 0\n";
    common::write_stub_script(dir, "gh", &script);
}

/// Number of times the stub was invoked.
fn call_count(log: &std::path::Path) -> usize {
    std::fs::read(log).map(|b| b.len()).unwrap_or(0)
}

/// Run `skim gh <args>` against the stub, returning stdout and the call count.
fn run_skim_gh(dir: &std::path::Path, args: &[&str]) -> (String, usize) {
    let log = dir.join("calls.log");
    let mut cmd = common::skim();
    // The escape hatch would bypass the guard entirely and make every
    // assertion below vacuous (PF-026).
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    let out = cmd
        .env("PATH", common::stub_path(dir))
        .env("GH_CALL_LOG", &log)
        .args(args)
        .output()
        .expect("skim gh must run");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        call_count(&log),
    )
}

// ============================================================================
// The fallback body
// ============================================================================

/// The defect, stated as an observable: on the guard's raw branch stdout must
/// carry what `gh issue view 93` prints, and must NOT carry the `--json`
/// response skim asked for on the reader's behalf.
#[test]
fn guard_fallback_serves_the_users_command_not_the_injected_one() {
    let dir = tempfile::tempdir().expect("tempdir");
    install_gh_stub(dir.path(), MINIMAL_JSON);

    let (stdout, calls) = run_skim_gh(dir.path(), &["gh", "issue", "view", "93"]);

    assert!(
        stdout.contains(USER_VIEW_TEXT),
        "the guard's raw branch must emit the user's own command output; \
         got:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"number\":93"),
        "the injected `--json` response must never be served as the fallback \
         body — the reader did not ask for wire format (PF-024); got:\n{stdout}"
    );
    assert!(
        !stdout.contains("issue view"),
        "no skim-structured summary may appear when the guard chose raw; \
         got:\n{stdout}"
    );
    assert_eq!(
        calls, 2,
        "the injected run plus exactly one re-run of the user's argv"
    );
}

/// The same obligation on the catch-all route, which is where
/// `route_rerunnable` actually decides anything at run time: `gh pr list` is
/// one of the three shapes `list::prepare_args` injects into.
#[test]
fn catch_all_list_route_serves_the_users_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    install_gh_stub(dir.path(), MINIMAL_JSON);

    let (stdout, calls) = run_skim_gh(dir.path(), &["gh", "pr", "list"]);

    assert!(
        stdout.contains(USER_VIEW_TEXT),
        "`gh pr list` injects `--json number,title,state,author`, so its \
         fallback owes the user their own output; got:\n{stdout}"
    );
    assert!(
        !stdout.contains("\"number\":93"),
        "the injected list JSON must not be served as the fallback body; \
         got:\n{stdout}"
    );
    assert_eq!(
        calls, 2,
        "the injected run plus one re-run of the user's argv"
    );
}

// ============================================================================
// Laziness
// ============================================================================

/// The re-run is the whole cost of this fix, and it must be paid only on the
/// branch that spends it.  A populated payload makes the guard keep the
/// compressed view, and then `gh` must be invoked exactly once.
///
/// This is what an eagerly-armed `raw_override` could not do: it needs the
/// user's bytes before the guard has an opinion, so it would double every
/// `gh` invocation — a second network round trip on the common path.
#[test]
fn no_second_run_when_the_guard_keeps_the_compressed_view() {
    let dir = tempfile::tempdir().expect("tempdir");
    install_gh_stub(dir.path(), POPULATED_JSON);

    let (stdout, calls) = run_skim_gh(dir.path(), &["gh", "issue", "view", "93"]);

    assert!(
        !stdout.contains(USER_VIEW_TEXT),
        "control: the guard must have KEPT the compressed view for this \
         payload, so the fallback text must be absent; got:\n{stdout}"
    );
    assert_eq!(
        calls, 1,
        "RawFallback must stay unresolved when the guard keeps the compressed \
         view — otherwise it is not lazy and every `gh` call pays twice"
    );
}

// ============================================================================
// gh api is excluded
// ============================================================================

/// `gh api` is a generic HTTP client: the same invocation shape can POST, PATCH
/// or DELETE.  It must be invoked exactly once whatever the guard decides.
#[test]
fn gh_api_is_never_re_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    install_gh_stub(dir.path(), MINIMAL_JSON);

    let (_stdout, calls) = run_skim_gh(dir.path(), &["gh", "api", "repos/o/r/issues/93"]);

    assert_eq!(
        calls, 1,
        "`gh api` is not idempotent — it must never be re-run to improve a \
         display, regardless of the guard's verdict"
    );
}

/// `gh pr create` and friends reach the catch-all route.  Whatever the guard
/// decides there, the command must run once.
#[test]
fn mutating_catch_all_routes_are_never_re_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    install_gh_stub(dir.path(), MINIMAL_JSON);

    let (_stdout, calls) = run_skim_gh(dir.path(), &["gh", "pr", "create", "--fill"]);

    assert_eq!(
        calls, 1,
        "`gh pr create` creates a pull request — a second run is not a display \
         improvement, it is a second pull request"
    );
}
