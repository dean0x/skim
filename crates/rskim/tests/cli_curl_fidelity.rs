//! F13 — the ADR-001 `Passthrough`-verdict sink is byte-exact, end to end.
//!
//! `execution.rs::write_and_flush` appends a trailing newline when its
//! `ensure_trailing_newline` flag is set and the body does not already end in
//! one. On the ADR-001 **`Passthrough`** verdict the net-savings guard has
//! already decided compression does not pay, so the bytes served are the
//! wrapped tool's *own* stdout — and a newline the tool never wrote is a
//! divergence from raw at exit 0 with zero bytes on stderr, which no ADR-011
//! marker discloses. `exec::emit_raw_passthrough_exact` is the sink that arm
//! now uses; this file pins the shipped binary's bytes.
//!
//! # Why `curl`, and not `git`
//!
//! The defect was triaged on `git status --porcelain -z`, but the mechanism was
//! never git-specific. The ADR-022 machine-contract gate
//! (`cmd/git/mod.rs::has_machine_contract_flag`) has exactly one production
//! call site and is therefore **git-only**, so it protects neither this arm nor
//! any other wrapped tool. On the git side the defect is doubly unreachable:
//! every non-newline-terminated git format (`-z`, `--null`,
//! `--pretty=format:`, `--format=`) is in `MACHINE_CONTRACT_FLAGS` and served
//! raw ahead of dispatch, and every *ungated* git argv measured (`status`,
//! `--short`, `-sb`, `log`, `--oneline`, `-n 1`, `show HEAD`, `diff`, `fetch`)
//! is newline-terminated or empty, so the guard is inert there even with the
//! gate removed. A git-shaped acceptance test would therefore pass because of
//! the gate rather than because of this fix. `curl` is the reachable arm: an
//! HTTP response body routinely lacks a trailing newline.
//!
//! # Measurement discipline (PF-026 / PF-009 / PF-031)
//!
//! Measured against the read-only pinned baseline `skim 2.11.0 (c2b4378)`:
//! a 27 B body with no trailing newline served **28 B**, and a 1 B body served
//! **2 B** — trailing-newline-only, exit 0, zero bytes on stderr. The control
//! that already ends in `\n` served **30 B against 30 B**, i.e. unchanged.
//!
//! `SKIM_PASSTHROUGH` and `SKIM_DEBUG` are removed from the subject: the escape
//! hatch would make every assertion here vacuously true, and it is the single
//! mistake that has bitten this campaign repeatedly. The control is pinned as
//! hard as the subject — both sides run the same `curl` binary against the same
//! `file://` URL. Nothing here reaches the network, reads this repository's
//! history, or pins a SHA: every fixture is constructed in a `TempDir`.
//!
//! # Non-vacuity
//!
//! Three independent guards, because byte-identity alone can pass for the wrong
//! reason. (1) Each fixture's raw body is asserted **not** to end in `\n`, or
//! the sink's guard is inert and nothing is being measured. (2) The ADR-001
//! verdict is asserted to be `Passthrough` rather than `Keep` — a `Keep` render
//! routes through `write_line_to_stdout`, a different sink with a different
//! newline contract. (3) [`the_guard_can_still_elect_keep`] proves the
//! discriminator used in (2) is real on this build, so (2) cannot be a string
//! that simply never appears.

mod common;

// ============================================================================
// Hermetic fixture
// ============================================================================

/// The 27 B body from the baseline measurement: multi-line, no trailing `\n`.
///
/// The final line's name is deliberate — a reader of a failure message can see
/// at a glance which fixture is in play, and that it is the unterminated one.
const BODY_NO_NEWLINE: &[u8] = b"alpha\nbeta\nomega-no-newline";

/// The 1 B body. The sharper of the two cases: a single-byte file makes an
/// appended newline **100% overhead**, and it is the shortest body for which
/// the guard's `!s.is_empty()` arm is still true.
const BODY_SINGLE_BYTE: &[u8] = b"x";

/// The control: the same shape as [`BODY_NO_NEWLINE`] but already terminated.
///
/// It must stay byte-identical too. That is what makes this fix *byte-exactness*
/// rather than "never append": the guarded and exact sinks agree on a body that
/// already ends in `\n`, so a regression that started stripping newlines would
/// fail here while passing every other test in this file.
const BODY_WITH_NEWLINE: &[u8] = b"alpha\nbeta\nomega-with-newline\n";

/// First-line prefix of skim's own `curl` render — the `Keep` verdict's
/// signature.
///
/// Measured on the pinned baseline: a compressible JSON body serves
/// `curl response object with 1 key` as line 1. Only the program-name prefix is
/// matched, not the whole sentence, so the discriminator does not break when
/// the summary wording changes.
const SKIM_CURL_SUMMARY_PREFIX: &str = "curl ";

/// Whether `curl` can be run, or this test must account for its absence.
///
/// Returns `false` only on a host with no usable `curl`, and **panics in CI**
/// rather than skipping. A fidelity gate that silently never runs is the
/// `PF-025` shape: it reports the same green as a gate that ran and passed. So
/// the skip exists for a developer laptop, and CI is required to be a host where
/// the assertion actually executes.
fn curl_is_runnable(test: &str) -> bool {
    let ok = std::process::Command::new("curl")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if ok {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "{test}: `curl` is not runnable, but `CI` is set — a fidelity gate that \
         silently does not run reports the same green as one that ran and \
         passed (PF-025). Install curl on the runner, or remove this file \
         deliberately rather than letting it evaporate."
    );
    eprintln!(
        "[SKIPPED] {test}: `curl` not runnable on this host; the F13 \
         byte-exactness gate did NOT run. Re-run with `--nocapture` to see this \
         notice, and note that CI fails rather than skipping."
    );
    false
}

/// Write `bytes` into `dir` and return the `file://` URL naming it.
///
/// The URL is asserted to need no percent-encoding, so a temp path with a space
/// or a non-ASCII byte fails as a *setup* error rather than surfacing later as a
/// confusing curl exit code (PF-009).
fn file_url(dir: &std::path::Path, name: &str, bytes: &[u8]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, bytes)
        .unwrap_or_else(|e| panic!("hermetic setup: writing fixture {name} failed: {e}"));
    let text = path
        .to_str()
        .unwrap_or_else(|| panic!("hermetic setup: fixture path for {name} is not UTF-8"));
    assert!(
        text.is_ascii() && !text.contains(' ') && !text.contains('%'),
        "hermetic setup: the temp path needs percent-encoding, which would make \
         both sides of the comparison depend on this file getting that encoding \
         right: {text}"
    );
    format!("file://{text}")
}

/// Run the real `curl` — the control, and the baseline every assertion is
/// measured against.
fn raw_curl(url: &str) -> std::process::Output {
    std::process::Command::new("curl")
        .args(["-s", url])
        .output()
        .unwrap_or_else(|e| panic!("control `curl -s {url}` failed to spawn: {e}"))
}

/// Run `skim curl -s <url>` — the subject.
fn served(url: &str) -> std::process::Output {
    let mut cmd = common::skim();
    // PF-026: the escape hatch would make every assertion in this file vacuous,
    // and `SKIM_DEBUG` would add class-2 banners the control cannot produce.
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd.args(["curl", "-s", url])
        .output()
        .unwrap_or_else(|e| panic!("subject `skim curl -s {url}` failed to spawn: {e}"))
}

/// Escape a byte string for a failure message without losing non-UTF-8 bytes.
fn show(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).escape_debug().to_string()
}

/// Assert the ADR-001 verdict on this body was `Passthrough`, not `Keep`.
///
/// `Keep` is directly observable: it serves skim's own `InfraResult` render,
/// whose first line carries [`SKIM_CURL_SUMMARY_PREFIX`]. `Passthrough` serves
/// curl's bytes, which carry no such header. Without this, a fixture that
/// happened to land on `Keep` would exercise `write_line_to_stdout` — a
/// different sink, with a different newline contract — and the byte-identity
/// assertion would be measuring nothing about F13.
fn assert_passthrough_verdict(label: &str, stdout: &[u8]) {
    let text = String::from_utf8_lossy(stdout);
    let first = text.lines().next().unwrap_or("").to_string();
    assert!(
        !first.starts_with(SKIM_CURL_SUMMARY_PREFIX),
        "{label}: the ADR-001 verdict must be `Passthrough`, not `Keep` — a \
         `Keep` render routes through a different sink with a different newline \
         contract, so byte-identity here would measure nothing about F13. \
         Served first line: {first}"
    );
}

/// Assert `skim curl -s <url>` delivers curl's own bytes, status and stderr.
///
/// The `expect_unterminated` flag carries the precondition: when `true`, the
/// raw body must not end in `\n`, because that is what arms the sink's guard.
/// A fixture whose body already ends in a newline makes the subject assertion
/// vacuous, and this is where that is caught rather than silently tolerated.
fn assert_byte_identical(label: &str, url: &str, expect_unterminated: bool) {
    let raw = raw_curl(url);
    assert!(
        raw.status.success(),
        "{label}: the control `curl -s` must succeed, or the fixture URL is \
         wrong and both sides are comparing failures;\nstderr={}",
        show(&raw.stderr)
    );
    assert!(
        !raw.stdout.is_empty(),
        "{label}: the control served no bytes — the sink's guard has an \
         `!s.is_empty()` arm, so an empty body measures nothing"
    );
    if expect_unterminated {
        assert_ne!(
            raw.stdout.last().copied(),
            Some(b'\n'),
            "{label}: precondition — the raw body must NOT end in a newline, or \
             the trailing-newline guard is inert and this test is vacuous. \
             Raw ({} B): {}",
            raw.stdout.len(),
            show(&raw.stdout)
        );
    } else {
        assert_eq!(
            raw.stdout.last().copied(),
            Some(b'\n'),
            "{label}: precondition — this control exists to cover a body that \
             DOES end in a newline; got: {}",
            show(&raw.stdout)
        );
    }

    let skim = served(url);
    assert_passthrough_verdict(label, &skim.stdout);

    assert_eq!(
        skim.stdout,
        raw.stdout,
        "{label}: on the ADR-001 `Passthrough` verdict skim serves the tool's \
         own stdout, so it must not add or drop a byte.\n  raw  ({} B): {}\n  \
         skim ({} B): {}",
        raw.stdout.len(),
        show(&raw.stdout),
        skim.stdout.len(),
        show(&skim.stdout),
    );
    assert_eq!(
        skim.status.code(),
        raw.status.code(),
        "{label}: exit status must match the raw tool's"
    );
    assert_eq!(
        String::from_utf8_lossy(&skim.stderr),
        String::from_utf8_lossy(&raw.stderr),
        "{label}: a lossless passthrough must not add or drop stderr bytes"
    );
}

// ============================================================================
// F13 — the reachable arm
// ============================================================================

/// The measured defect: a multi-line body with no trailing newline.
///
/// Baseline (`c2b4378`): 28 B served against 27 B raw, exit 0, zero stderr.
#[test]
fn unterminated_body_is_byte_identical_to_raw_curl() {
    let test = "unterminated_body_is_byte_identical_to_raw_curl";
    if !curl_is_runnable(test) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let url = file_url(dir.path(), "body.txt", BODY_NO_NEWLINE);
    assert_byte_identical("27 B body, no trailing newline", &url, true);
}

/// The sharper case: one byte, where an appended newline is 100% overhead.
///
/// Baseline (`c2b4378`): 2 B served against 1 B raw.
#[test]
fn single_byte_body_is_byte_identical_to_raw_curl() {
    let test = "single_byte_body_is_byte_identical_to_raw_curl";
    if !curl_is_runnable(test) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let url = file_url(dir.path(), "tiny.txt", BODY_SINGLE_BYTE);
    assert_byte_identical("1 B body, no trailing newline", &url, true);
}

/// The control: a body that already ends in `\n` must stay byte-identical.
///
/// This is what makes the fix *byte-exactness* rather than "never append". The
/// guarded and exact sinks agree on a terminated body, so a regression that
/// began *stripping* a trailing newline would fail here while passing the two
/// tests above.
#[test]
fn newline_terminated_body_is_byte_identical_to_raw_curl() {
    let test = "newline_terminated_body_is_byte_identical_to_raw_curl";
    if !curl_is_runnable(test) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let url = file_url(dir.path(), "body_nl.txt", BODY_WITH_NEWLINE);
    assert_byte_identical("30 B body, trailing newline present", &url, false);
}

/// A JSON body the `curl` render genuinely compresses.
///
/// # The whitespace is the whole point — do not compact this
///
/// `curl`'s tier-1 render is the summary header plus each top-level key's value
/// re-serialised **compactly**, so the render's size is approximately the
/// body's own compact form plus the header. The saving therefore comes from
/// discarding *insignificant JSON whitespace*, not from discarding content.
///
/// Measured: this body is 5 240 B pretty-printed and 3 911 B compact, and the
/// render is 3 942 B — i.e. the compact size plus the 31 B header. A compact
/// body is consequently **not compressible at all**: the render can only be
/// larger than it, so the ADR-001 guard correctly elects `Passthrough` and this
/// control fails its own precondition. An earlier revision of this file emitted
/// compact JSON and did exactly that (`raw=3911 B skim=3911 B`), which is why
/// the indentation below is load-bearing and why the precondition that caught
/// it must not be weakened.
fn compressible_json_body() -> String {
    let mut json = String::from("{\n  \"items\": [\n");
    for i in 0..40 {
        if i > 0 {
            json.push_str(",\n");
        }
        json.push_str(&format!(
            "    {{\n      \"id\": {i},\n      \"name\": \"entry-{i}\",\n      \
             \"desc\": \"{}\"\n    }}",
            "x".repeat(60)
        ));
    }
    json.push_str("\n  ]\n}");
    json
}

/// The guard can still elect `Keep`, and `Keep` is discriminable.
///
/// Without this, [`assert_passthrough_verdict`] could be asserting the absence
/// of a string that never appears on any input, which would make the
/// `Passthrough` pin in the three tests above worthless. A compressible JSON
/// body is served as skim's own render — measured on the pinned baseline at
/// 3 942 B against 5 279 B raw, with `curl response object with 1 key` as line
/// 1 — so the discriminator is proven live on this build.
#[test]
fn the_guard_can_still_elect_keep() {
    let test = "the_guard_can_still_elect_keep";
    if !curl_is_runnable(test) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let json = compressible_json_body();
    let url = file_url(dir.path(), "big.json", json.as_bytes());

    let raw = raw_curl(&url);
    let skim = served(&url);
    assert!(
        skim.stdout.len() < raw.stdout.len(),
        "fixture precondition: this body must compress, or the verdict is not \
         `Keep` and the discriminator is unproven. raw={} B skim={} B",
        raw.stdout.len(),
        skim.stdout.len()
    );
    let text = String::from_utf8_lossy(&skim.stdout);
    let first = text.lines().next().unwrap_or("").to_string();
    assert!(
        first.starts_with(SKIM_CURL_SUMMARY_PREFIX),
        "a `Keep` verdict must carry skim's own summary header, or \
         `assert_passthrough_verdict` is asserting the absence of a string that \
         never appears. Served first line: {first}"
    );
}
