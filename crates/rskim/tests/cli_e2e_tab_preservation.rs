//! E2E tests proving that tab characters and ESC bytes survive the ANSI-strip step
//! for the `gh` and `diff` wrappers (tab-parsing family), the `grep` and `rg`
//! wrappers (passthrough search tools), and the passthrough-only `wc`, `ls`,
//! `find`, `df`, `du`, and `ps` wrappers (ADR-014 / PF-006 fourth wave).
//! Also covers `gh` exit-8 being treated as a parseable result rather than an
//! unexpected failure.
//!
//! ## Why stubs drive the REAL pipeline
//!
//! Unit tests call `parse_impl` / `try_parse_checks_text` directly, bypassing
//! the `run_tool` path where ANSI-stripping occurs.  These e2e tests inject a
//! stub binary on PATH so the full `run_tool` pipeline fires:
//!
//!   stub (emits fixture + chosen exit code)
//!     → run_tool → [skip_ansi_strip gate] → parse → render
//!
//! Per PF-004, this exercises the **hook / direct-invocation surface**
//! (`skim <tool> [args]`).  Flags arrive as ordinary argv and the stub
//! controls stdout/exit so fixture content feeds the live strip→parse path.
//!
//! ## Regression property
//!
//! - Without `skip_ansi_strip: true` on gh's CONFIG, tabs are stripped and
//!   `RE_GH_CHECK_TAB` never matches → passthrough (no "N checks" in output).
//! - Without `expected_exit_codes: &[8]` on gh's CONFIG, exit 8 is classified
//!   as UnexpectedFailure → raw-forwarded before parsing (no "N checks").
//! - Without `skip_ansi_strip: true` on diff's CONFIG, the `--- path\t<mtime>`
//!   header loses its tab → path and mtime are fused in the parsed entry.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;

const GH_PR_CHECKS_TEXT: &str = include_str!("fixtures/cmd/infra/gh_pr_checks_text.txt");
const DIFF_UNIFIED_TEXT: &str = include_str!("fixtures/cmd/file/diff_unified.txt");

/// Create a stub directory with a script named `name` that prints `stdout`
/// and exits with `code`.  Returns the TempDir (must stay alive for PATH use).
fn make_stub(name: &str, stdout: &str, code: i32) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let out_path = dir.path().join(format!("{name}.out"));
    fs::write(&out_path, stdout).unwrap();
    let script = format!("#!/bin/sh\ncat '{}'\nexit {code}\n", out_path.display());
    let script_path = dir.path().join(name);
    fs::write(&script_path, script).unwrap();
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

/// Build a PATH string with `extra_dir` prepended before the system PATH.
fn prepend_path(extra_dir: &std::path::Path) -> String {
    format!(
        "{}:{}",
        extra_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

// ============================================================================
// gh pr checks — exit 0
// ============================================================================

/// Stub gh emits the tab-separated pr-checks fixture (exit 0).
///
/// With `skip_ansi_strip: true` on gh's CONFIG, tabs survive and
/// `RE_GH_CHECK_TAB` matches → parser produces a compressed summary.
/// Without the flag, tabs would be stripped and the output would raw-forward.
///
/// Asserts: exit 0, stdout contains "5 checks" and the check name "CI / build"
/// (proving fields were correctly separated, not fused).
#[test]
fn test_gh_pr_checks_exit0_compressed_summary() {
    let stub_dir = make_stub("gh", GH_PR_CHECKS_TEXT, 0);
    let path = prepend_path(stub_dir.path());
    let skim = common::skim_bin();

    let out = std::process::Command::new(&skim)
        .args(["gh", "pr", "checks", "421"])
        .env("PATH", &path)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .output()
        .expect("skim gh pr checks must be spawnable");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "exit 0 must propagate; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("5 checks"),
        "must contain '5 checks' summary — tabs must survive ANSI-strip; got: {stdout}"
    );
    assert!(
        stdout.contains("CI / build"),
        "check name 'CI / build' must appear as a separated field; got: {stdout}"
    );
}

// ============================================================================
// gh pr checks — exit 8 (pending/failing checks)
// ============================================================================

/// Stub gh emits the tab-separated pr-checks fixture and exits 8.
///
/// gh exits 8 when any check is pending or failing.  With
/// `expected_exit_codes: &[8]`, this is classified as ExpectedFailure (not
/// UnexpectedFailure) and the output is compressed before forwarding.  Exit 8
/// is then propagated so callers see the true status.
///
/// Without `&[8]`, exit 8 would be UnexpectedFailure → raw-forward BEFORE
/// parsing → no "N checks" summary in stdout.
///
/// Asserts: exit 8, stdout contains "5 checks", the failing check name
/// "CI / lint", and the failure URL "https://" (AD-INFRA-15).
#[test]
fn test_gh_pr_checks_exit8_summary_and_exit_code() {
    let stub_dir = make_stub("gh", GH_PR_CHECKS_TEXT, 8);
    let path = prepend_path(stub_dir.path());
    let skim = common::skim_bin();

    let out = std::process::Command::new(&skim)
        .args(["gh", "pr", "checks", "421"])
        .env("PATH", &path)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .output()
        .expect("skim gh pr checks (exit 8) must be spawnable");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(8),
        "exit 8 must be propagated; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("5 checks"),
        "must contain '5 checks' summary even on exit 8 — exit 8 is a parseable result; got: {stdout}"
    );
    assert!(
        stdout.contains("CI / lint"),
        "failing check name 'CI / lint' must appear; got: {stdout}"
    );
    assert!(
        stdout.contains("https://"),
        "AD-INFRA-15: URL must appear for failing check; got: {stdout}"
    );
}

// ============================================================================
// diff — tab-header path must not be glued to mtime
// ============================================================================

/// Stub diff emits a unified diff with `--- a/src/main.rs\t<mtime>` headers
/// and exits 1 (files differ — the normal, expected exit code).
///
/// With `skip_ansi_strip: true` on diff's CONFIG, the `\t` in the header
/// line survives and `try_parse_standalone_unified` splits on it, keeping
/// path and timestamp separate.  Without the flag, the tab is dropped and
/// the path is fused with the mtime.
///
/// Asserts: exit 1, the entry contains "src/main.rs" as part of a clean
/// path reference, and does NOT contain the glued form "src/main.rs2026".
#[test]
fn test_diff_tab_header_path_not_glued() {
    let stub_dir = make_stub("diff", DIFF_UNIFIED_TEXT, 1);
    let path = prepend_path(stub_dir.path());
    let skim = common::skim_bin();

    let out = std::process::Command::new(&skim)
        .args(["diff", "a/src/main.rs", "b/src/main.rs"])
        .env("PATH", &path)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .output()
        .expect("skim diff must be spawnable");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "exit 1 must propagate (files differ); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("a/src/main.rs:"),
        "path renders as clean field — tab must survive and split path from mtime; got: {stdout}"
    );
    assert!(
        !stdout.contains("main.rs2026") && !stdout.contains("main.rs 2026"),
        "path must not be fused with mtime; got: {stdout}"
    );
}

// ============================================================================
// grep — tab-bearing match content must survive ANSI-strip
// ============================================================================

/// Stub grep emits output with a literal TAB in the match content (Makefile recipe).
///
/// With `skip_ansi_strip: true` on grep's CONFIG, the ANSI-strip step in
/// execution.rs is skipped entirely and the TAB byte (0x09) appears in skim's
/// stdout byte-faithfully.  Without the flag, `strip_escape_sequences`
/// (via `strip_ansi_cow`) would run on the buffer — a #317 fidelity risk for
/// content-bearing bytes (ADR-012 / PF-006).
#[test]
fn test_grep_tab_content_preserved() {
    // Makefile recipe lines begin with a mandatory literal TAB.
    // Simulates: grep -n 'install:' Makefile → "Makefile:2:\tcp foo"
    let fixture = "Makefile:2:\tcp install: $(DEPS)\n";
    let stub_dir = make_stub("grep", fixture, 0);
    let path = prepend_path(stub_dir.path());
    let skim = common::skim_bin();

    let out = std::process::Command::new(&skim)
        .args(["grep", "-n", "install:", "Makefile"])
        .env("PATH", &path)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .output()
        .expect("skim grep must be spawnable");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "exit 0 must propagate; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains('\t'),
        "TAB (0x09) must survive the ANSI-strip step — grep output is byte-faithful \
         (ADR-009 / PF-006); skip_ansi_strip must be true; got: {stdout:?}"
    );
}

// ============================================================================
// diff — ESC bytes in file CONTENT must survive byte-faithfully (ADR-012)
// ============================================================================

/// Stub diff emits a unified diff whose BODY lines contain ESC/CSI bytes
/// originating from file CONTENT (not tool colorization).
///
/// ADR-012 rules that skim NEVER filters ESC/CSI bytes that originate in
/// wrapped-file CONTENT: stripping them would show the reader something
/// different from the raw tool without a loss marker, violating the
/// compress-never-truncate rule (#317).  The tool's OWN colorization is
/// neutralized separately at the child-invocation boundary via `--no-color`
/// (on `git diff`/`git show`; standalone `diff` never emits ANSI itself).
///
/// This test exercises the **hook / direct-invocation surface** end-to-end
/// (the full `run_tool` pipeline including the `skip_ansi_strip` gate) and
/// pairs with `test_adr012_esc_in_diff_body_line_survives` in diff.rs, which
/// pins the same property at the unit level.
///
/// Asserts: exit 1 propagates, and the ESC byte (0x1b) from the file CONTENT
/// body line appears in skim's stdout byte-faithfully.
#[test]
fn test_diff_esc_in_body_content_survives_adr012() {
    // A unified diff body line whose CONTENT contains an ESC/CSI sequence.
    // Simulates a file that stores ANSI bytes verbatim (e.g. a snapshot of
    // coloured terminal output kept as a text fixture).
    let fixture = concat!(
        "--- a/file.txt\t2026-07-25 10:00:00.000000000 +0000\n",
        "+++ b/file.txt\t2026-07-25 10:05:00.000000000 +0000\n",
        "@@ -1,1 +1,1 @@\n",
        "-old line\n",
        "+\x1b[32mcolored new line\x1b[0m\n",
    );
    let stub_dir = make_stub("diff", fixture, 1);
    let path = prepend_path(stub_dir.path());
    let skim = common::skim_bin();

    let out = std::process::Command::new(&skim)
        .args(["diff", "a/file.txt", "b/file.txt"])
        .env("PATH", &path)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .output()
        .expect("skim diff must be spawnable");

    let stdout_bytes = &out.stdout;
    assert_eq!(
        out.status.code(),
        Some(1),
        "exit 1 must propagate (files differ); stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The ESC byte (0x1b) from the file CONTENT body line must appear in
    // skim's stdout.  With skip_ansi_strip: true, the ANSI-strip step is
    // skipped entirely in run_tool — the same flag that also guarantees the
    // \t delimiter survives in --- path\t<mtime> headers (PF-006 / ADR-012).
    assert!(
        stdout_bytes.contains(&0x1b_u8),
        "ADR-012: ESC byte (0x1b) from file CONTENT must survive byte-faithfully \
         through the full skim diff pipeline; skip_ansi_strip: true covers both \
         \\t header delimiters (PF-006) and content-borne ESC sequences (ADR-012); \
         got: {:?}",
        String::from_utf8_lossy(stdout_bytes)
    );
}

// ============================================================================
// rg — tab-bearing match content must survive ANSI-strip
// ============================================================================

/// Stub rg emits output with a literal TAB in the match content (Makefile recipe).
///
/// With `skip_ansi_strip: true` on rg's CONFIG, the ANSI-strip step in
/// execution.rs is skipped entirely and the TAB byte (0x09) appears in skim's
/// stdout byte-faithfully.  Without the flag, `strip_escape_sequences`
/// (via `strip_ansi_cow`) would run on the buffer — a #317 fidelity risk for
/// content-bearing bytes (ADR-012 / PF-006).
#[test]
fn test_rg_tab_content_preserved() {
    // Makefile recipe lines begin with a mandatory literal TAB.
    // Simulates: rg 'install:' Makefile → "Makefile:2:\tcp foo"
    let fixture = "Makefile:2:\tcp install: $(DEPS)\n";
    let stub_dir = make_stub("rg", fixture, 0);
    let path = prepend_path(stub_dir.path());
    let skim = common::skim_bin();

    let out = std::process::Command::new(&skim)
        .args(["rg", "install:", "Makefile"])
        .env("PATH", &path)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .output()
        .expect("skim rg must be spawnable");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "exit 0 must propagate; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains('\t'),
        "TAB (0x09) must survive the ANSI-strip step — rg output is byte-faithful \
         (ADR-009 / PF-006); skip_ansi_strip must be true; got: {stdout:?}"
    );
}

// ============================================================================
// Passthrough family: wc, ls, find, df, du, ps  (ADR-014 / PF-006 fourth wave)
//
// These wrappers return RawPassthrough — their parse_impl ignores its argument
// and always returns ParseResult::RawPassthrough.  That means the bytes the
// reader receives are exactly what execution.rs holds in `output.stdout` AFTER
// the ANSI-strip step runs.  With skip_ansi_strip: false the strip runs first,
// shadows the output binding, and RawPassthrough then serves the ALREADY-STRIPPED
// bytes.  `strip_escape_sequences` (via `strip_ansi_cow`) removes ESC-rooted
// sequences; any content-level ESC bytes in the buffer are lost, and the
// stripping step itself could affect bytes the reader expects intact (PF-006).
//
// Fix: skip_ansi_strip must be true for every wrapper whose bytes reach the reader
// unparsed (RawPassthrough).  The strip runs upstream of parsing and shadows
// `output`; "served without parsing" is precisely WHY the flag must be true.
// ============================================================================

/// Asserts byte-faithful passthrough for a file-op tool: exit 0, TAB present in
/// stdout bytes, ESC (0x1b) present in stdout bytes, and stdout is byte-identical
/// to the fixture.
///
/// Fixtures must end in `\n` — `emit_raw_passthrough` appends one otherwise,
/// producing a spurious extra byte in the comparison.
///
/// In Rust tests, byte detection uses `contains(&0x09_u8)` / `contains(&0x1b_u8)`;
/// never go through a shell `grep` command — `grep -c '\033'` matches literal text,
/// not the actual ESC byte (0x1b).
fn assert_byte_faithful_passthrough(tool: &str, args: &[&str], fixture: &str) {
    let stub_dir = make_stub(tool, fixture, 0);
    let path = prepend_path(stub_dir.path());
    let skim = common::skim_bin();

    let mut cmd = std::process::Command::new(&skim);
    cmd.arg(tool);
    for a in args {
        cmd.arg(a);
    }

    let out = cmd
        .env("PATH", &path)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .output()
        .unwrap_or_else(|e| panic!("skim {tool} must be spawnable: {e}"));

    assert_eq!(
        out.status.code(),
        Some(0),
        "{tool}: exit must be 0; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stdout.contains(&0x09_u8),
        "{tool}: TAB (0x09) must survive the ANSI-strip step — skip_ansi_strip must \
         be true for passthrough wrappers (ADR-014 / PF-006); got bytes: {:?}",
        out.stdout
    );
    assert!(
        out.stdout.contains(&0x1b_u8),
        "{tool}: ESC (0x1b) must survive byte-faithfully (ADR-012); \
         got bytes: {:?}",
        out.stdout
    );
    assert_eq!(
        out.stdout,
        fixture.as_bytes(),
        "{tool}: stdout must be byte-identical to fixture — any stripping is a #317 violation"
    );
}

/// wc: passthrough must preserve TAB and ESC byte-faithfully.
///
/// wc returns RawPassthrough; without skip_ansi_strip: true the ESC byte in the
/// fixture triggers `strip_ansi_cow` → `Cow::Owned`, and `strip_escape_sequences`
/// removes ESC sequences from the buffer — any content-level ESC bytes are lost.
#[test]
fn test_wc_passthrough_preserves_tabs_and_esc() {
    // Simulates colored wc output: ESC bytes for bold + TAB between columns.
    let fixture = "      \x1b[1m10\x1b[0m\t      20\t      30 src/main.rs\n";
    assert_byte_faithful_passthrough("wc", &["-l", "src/main.rs"], fixture);
}

/// ls: passthrough must preserve TAB and ESC byte-faithfully.
///
/// `ls -G` emits ANSI color codes; skim must not strip them.  This closes the
/// `CLICOLOR_FORCE=1 ls -G` regression (0 ESC out of 9 measured at HEAD).
#[test]
fn test_ls_passthrough_preserves_tabs_and_esc() {
    // Simulates `ls -G` multi-column output: files colored with a tab between them.
    let fixture = "\x1b[1;34m.\x1b[0m\ntotal 0\n\x1b[32mfoo.txt\x1b[0m\t\x1b[31mbar.txt\x1b[0m\n";
    assert_byte_faithful_passthrough("ls", &["-la"], fixture);
}

/// find: passthrough must preserve TAB and ESC byte-faithfully.
#[test]
fn test_find_passthrough_preserves_tabs_and_esc() {
    // Simulates find output with a colorized path and a tab before an annotation.
    let fixture = "\x1b[32m./src/main.rs\x1b[0m\t(modified)\n";
    assert_byte_faithful_passthrough("find", &[".", "-name", "*.rs"], fixture);
}

/// df: passthrough must preserve TAB and ESC byte-faithfully.
#[test]
fn test_df_passthrough_preserves_tabs_and_esc() {
    // Simulates df output with tab-separated columns and a colored filesystem name.
    let fixture = "Filesystem\tSize\tUsed\tAvail Use% Mounted on\n\x1b[32m/dev/sda1\x1b[0m\t  50G\t  20G\t  30G  40% /\n";
    assert_byte_faithful_passthrough("df", &["-h"], fixture);
}

/// du: passthrough must preserve TAB and ESC byte-faithfully.
///
/// du's POSIX format is `size<TAB>path`; with skip_ansi_strip: false an ESC byte
/// anywhere in the buffer causes strip_ansi_cow to process the whole buffer and
/// destroy all 0x09 bytes.
#[test]
fn test_du_passthrough_preserves_tabs_and_esc() {
    // `size\tpath` with a colorized path — both bytes must survive.
    let fixture = "4\t\x1b[32m./file.txt\x1b[0m\n8\t.\n";
    assert_byte_faithful_passthrough("du", &["-a"], fixture);
}

/// ps: passthrough must preserve TAB and ESC byte-faithfully.
#[test]
fn test_ps_passthrough_preserves_tabs_and_esc() {
    // Simulates colored ps output with tab-separated fields.
    let fixture = "USER\tPID\t%CPU COMMAND\n\x1b[32mroot\x1b[0m\t  1\t 0.0 /sbin/init\n";
    assert_byte_faithful_passthrough("ps", &["aux"], fixture);
}

/// TAB preservation across lines is all-or-nothing: `strip_ansi_cow` returns
/// `Cow::Owned` when ANY ESC byte appears anywhere in the buffer, and the
/// `strip_escape_sequences` scanner then processes the whole buffer — content-level
/// ESC bytes on any line are in scope for removal.  An ESC byte in line 2's
/// filename is therefore a risk for every other line in the buffer too.
///
/// This is the highest-value test in the set: it is the only one that would fail
/// if someone later "optimised" strip_ansi_cow to operate per-line.  It also
/// asserts that the ESC-bearing filename survives byte-for-byte (`./esc\x1bname`)
/// rather than being corrupted by sequence-scanning that consumes the byte after ESC.
#[test]
fn test_du_esc_in_one_filename_strips_tabs_on_every_line() {
    // Three `size\tpath` lines; only line 2 contains an ESC byte in the path.
    // With skip_ansi_strip: false: strip_ansi_cow sees the ESC on line 2,
    // returns Cow::Owned, and strip_escape_sequences processes the entire buffer —
    // the ESC byte on line 2 and any bytes consumed as part of its sequence are
    // lost, and the whole-buffer scan means every line is in scope.
    let fixture = "4\t./file1.txt\n8\t./esc\x1bname\n16\t.\n";
    let stub_dir = make_stub("du", fixture, 0);
    let path = prepend_path(stub_dir.path());
    let skim = common::skim_bin();

    let out = std::process::Command::new(&skim)
        .args(["du", "-a"])
        .env("PATH", &path)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_DEBUG")
        .output()
        .expect("skim du must be spawnable");

    assert_eq!(
        out.status.code(),
        Some(0),
        "du: exit must be 0; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // All three lines must still contain a TAB — the all-or-nothing strip
    // would kill tabs on lines 1 and 3 even though only line 2 has an ESC.
    let stdout = &out.stdout;
    let lines: Vec<&[u8]> = stdout
        .split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        lines.len(),
        3,
        "expected 3 non-empty lines; got: {:?}",
        lines
    );
    for (i, line) in lines.iter().enumerate() {
        assert!(
            line.contains(&0x09_u8),
            "line {}: TAB must survive even though only line 2 carries an ESC byte \
             (all-or-nothing strip would kill all tabs); got: {:?}",
            i + 1,
            line
        );
    }

    // The ESC-bearing filename must survive byte-for-byte: `./esc\x1bname`,
    // not the corrupted `./escame` produced by consuming the byte following ESC.
    let esc_path = b"./esc\x1bname";
    assert!(
        stdout.windows(esc_path.len()).any(|w| w == esc_path),
        "ESC-bearing filename must be `./esc\\x1bname` byte-for-byte (not `./escame`); \
         got: {:?}",
        stdout
    );

    // Full byte-identical check.
    assert_eq!(
        *stdout,
        fixture.as_bytes().to_vec(),
        "du: stdout must be byte-identical to fixture — any byte mutation is a #317 violation"
    );
}

// ============================================================================
// Issue #465 — strip_ansi_cow preserves TABs when ESC byte present
// ============================================================================

/// E2E regression test for #465: `strip_ansi_cow` (invoked by `run_tool` when
/// `skip_ansi_strip: false`) formerly used `strip_ansi_escapes::strip_str` (a
/// `vte` state machine that emitted only printables + `\n`, discarding ALL C0
/// controls including TABs whenever any ESC byte appeared).  The replacement,
/// `strip_escape_sequences`, preserves all bytes that are not part of an
/// ESC-rooted sequence — TABs and other C0 controls survive.
///
/// Uses an `eslint` stub (`skip_ansi_strip: false`) so the live `strip_ansi_cow`
/// path fires before parsing.  The fixture is not valid eslint output → the parser
/// falls through to Tier 3 (Passthrough) and the bytes reach the reader directly.
///
/// Before the fix: all 3 TABs destroyed.
/// After the fix: only the two CSI sequences are stripped; all 3 TABs survive.
#[test]
fn test_465_strip_ansi_cow_preserves_tabs_in_eslint_passthrough() {
    // Three lines: line 1 — TAB only; line 2 — CSI colour sequences + TAB;
    // line 3 — TAB only.
    let fixture = "a\tb\n\x1b[32mc\x1b[0m\td\n e\tf\n";

    // eslint has skip_ansi_strip: false → strip_ansi_cow runs on this fixture.
    // The fixture is neither valid JSON nor the eslint text format → Passthrough.
    let stub_dir = make_stub("eslint", fixture, 1);
    let path = prepend_path(stub_dir.path());
    let skim = common::skim_bin();

    let out = std::process::Command::new(&skim)
        .args(["eslint", "."])
        .env("PATH", &path)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env_remove("SKIM_PASSTHROUGH")
        .output()
        .expect("skim eslint must be spawnable");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let tab_count = stdout.matches('\t').count();
    assert_eq!(
        tab_count, 3,
        "all 3 TABs must survive the strip_ansi_cow step — \
         strip_escape_sequences removes only ESC-rooted sequences, not C0 controls \
         (#465); stdout: {stdout:?}"
    );
    assert!(
        !stdout.contains('\x1b'),
        "ESC sequences must be removed from the Passthrough payload; stdout: {stdout:?}"
    );
}
