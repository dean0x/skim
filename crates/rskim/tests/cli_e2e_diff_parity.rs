//! Standalone-`diff` byte-parity E2E (A3a + A3b, PF-011 / PF-024).
//!
//! ## What is pinned
//!
//! `skim diff` has no parser: `parse_impl` returns `ParseResult::RawPassthrough`
//! for every input, because no region of the measured input space exists where
//! compressing standalone `diff` beats the native tool (see the module header of
//! `cmd/file/diff.rs`). The observable contract is therefore **byte parity**, and
//! byte parity is what these tests assert — not "contains the changed lines",
//! which a re-render can satisfy while still inflating the reader's context.
//!
//! ## One yardstick: the user's literal command
//!
//! | invocation | must equal | why |
//! |---|---|---|
//! | `skim diff <flag> …` | `diff <flag> …` | pure passthrough, no flag injection |
//! | `skim diff a b` (flagless) | `diff a b` | `prepare_args` is a no-op; skim runs the user's literal command |
//! | `SKIM_PASSTHROUGH=1 skim diff a b` | `diff a b` | escape hatch fires before any handler; user's argv unchanged |
//!
//! Before commit `ec64e53` (A3b) the flagless path injected `-u`, making
//! `skim diff a b` emit `diff -u` output — a different format and LARGER than
//! native `diff a b` in every measured region (PF-024, PF-011).  That injection
//! is now removed: all three rows above produce byte-identical output to the
//! corresponding native invocation.

use std::path::Path;
use std::process::Command;

mod common;

/// The 18 `diff` flag forms that must reach the reader as the native tool's
/// bytes with the native exit code.  `skim diff` is a pure passthrough for
/// every input — no `-u` is injected on any path — so all 18 forms now satisfy
/// the same invariant as the flagless default.
///
/// Historical note: before the injection was removed, 10 of these forms would
/// have run `diff -u <flag> …` and exited 2 with zero stdout, where native
/// `diff <flag> …` exits 1 with output (PF-024, total-loss paths).
const FLAG_FORMS: &[&[&str]] = &[
    // already-unified family
    &["-u"],
    &["--unified"],
    &["-U3"],
    &["--unified=3"],
    // context format
    &["-c"],
    &["-c3"],
    &["-C", "2"],
    &["--context"],
    &["--context=2"],
    // side-by-side
    &["-y"],
    &["--side-by-side"],
    // ed script
    &["-e"],
    &["--ed"],
    // RCS
    &["-n"],
    &["--rcs"],
    // summary only
    &["-q"],
    &["--brief"],
    // explicit default
    &["--normal"],
];

/// Two small, deliberately REALISTIC files: one changed line in four.
///
/// Fixture size is not a tuning knob here. Byte parity is size-independent, so
/// there is no incentive to grow the input until a net-savings guard agrees.
fn fixture(dir: &Path) -> (String, String) {
    let a = dir.join("a.txt");
    let b = dir.join("b.txt");
    std::fs::write(&a, "alpha\nbeta\ngamma\ndelta\n").unwrap();
    std::fs::write(&b, "alpha\nBETA\ngamma\ndelta\n").unwrap();
    (
        a.to_str().unwrap().to_string(),
        b.to_str().unwrap().to_string(),
    )
}

/// Run native `diff` with `args`, returning `(stdout, exit_code)`.
fn native_diff(args: &[&str]) -> (Vec<u8>, Option<i32>) {
    let out = Command::new("diff")
        .args(args)
        .output()
        .expect("native diff must be available");
    (out.stdout, out.status.code())
}

/// Every conflicting / already-unified flag form must reach the reader as the
/// native tool's own bytes, with the native exit code.
///
/// Before A3a, 10 of these ran `diff -u <flag> …` and exited 2 with ZERO stdout
/// where native `diff` exits 1 with output — and `SKIM_PASSTHROUGH=1` did not
/// rescue them either (PF-024).
#[test]
fn a3a_every_conflicting_flag_form_matches_native_bytes_and_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = fixture(dir.path());

    for form in FLAG_FORMS {
        let mut native_args: Vec<&str> = form.to_vec();
        native_args.push(&a);
        native_args.push(&b);
        let (native_stdout, native_code) = native_diff(&native_args);

        let out = common::skim()
            .arg("diff")
            .args(*form)
            .args([&a, &b])
            .output()
            .unwrap();

        assert_eq!(
            out.stdout,
            native_stdout,
            "skim diff {form:?} must emit native `diff {form:?}` bytes verbatim; \
             skim gave {:?}, native gave {:?}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&native_stdout),
        );
        assert_eq!(
            out.status.code(),
            native_code,
            "skim diff {form:?} must propagate native `diff`'s exit code"
        );
    }
}

/// The flagless default path: `prepare_args` is a no-op, so `skim diff a b`
/// runs the user's literal `diff a b` and emits its bytes unchanged.
///
/// Before commit `ec64e53` (A3b) `prepare_args` injected `-u`, making this path
/// emit `diff -u` output — a different format, measured 3–6× LARGER than native
/// `diff a b` on sparse changes (PF-024 / PF-011).  The injection is gone;
/// `skim diff a b` is now byte-identical to `diff a b`.
#[test]
fn a3b_flagless_diff_is_byte_identical_to_native_diff() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = fixture(dir.path());
    let (native_stdout, native_code) = native_diff(&[&a, &b]);

    let out = common::skim().args(["diff", &a, &b]).output().unwrap();

    assert_eq!(
        out.stdout,
        native_stdout,
        "flagless `skim diff` must be byte-identical to `diff a b` (no `-u`); got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        out.status.code(),
        native_code,
        "exit code must propagate faithfully"
    );
}

/// `SKIM_PASSTHROUGH=1` must deliver the user's literal command — byte-identical
/// to `diff a b`.
///
/// With the `-u` injection removed the normal path and the escape-hatch path now
/// produce the same output for the flagless invocation.  This test remains as an
/// explicit passthrough-mode contract pin: the dispatch convergence gate fires
/// before any handler runs, so the argv is always the user's.
#[test]
fn a3b_escape_hatch_delivers_the_users_literal_diff_not_the_injected_one() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = fixture(dir.path());
    let (literal_stdout, literal_code) = native_diff(&[&a, &b]);

    let out = common::skim()
        .env("SKIM_PASSTHROUGH", "1")
        .args(["diff", &a, &b])
        .output()
        .unwrap();

    assert_eq!(
        out.stdout,
        literal_stdout,
        "SKIM_PASSTHROUGH=1 must emit `diff a b`, not `diff -u a b`; got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        out.status.code(),
        literal_code,
        "the escape hatch must propagate the native exit code"
    );
}

/// Identical files: exit 0 and ZERO bytes — skim must not synthesize a
/// "files are identical" line the tool never emitted.
#[test]
fn a3b_identical_files_emit_nothing_and_exit_zero() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("same_a.txt");
    let b = dir.path().join("same_b.txt");
    std::fs::write(&a, "alpha\nbeta\n").unwrap();
    std::fs::write(&b, "alpha\nbeta\n").unwrap();

    let out = common::skim()
        .args(["diff", a.to_str().unwrap(), b.to_str().unwrap()])
        .output()
        .unwrap();

    assert!(
        out.stdout.is_empty(),
        "identical files must produce zero stdout bytes; got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(out.status.code(), Some(0));
}

/// ESC/CSI bytes stored as file CONTENT must survive byte-faithfully (ADR-012).
///
/// `skip_ansi_strip: true` is what guarantees this: the ANSI-strip step in
/// `execution.rs` runs BEFORE `parse()` and shadows the `output` binding, so
/// `RawPassthrough` does NOT bypass it. Standalone `diff` never colorizes its
/// own output, so every ESC byte present is content.
///
/// The baseline is `diff a b` (no `-u`): the flagless path no longer injects
/// unified format, so the ESC byte appears in the normal-diff `>` line.
#[test]
fn a3b_esc_bytes_in_file_content_survive_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("esc_a.txt");
    let b = dir.path().join("esc_b.txt");
    std::fs::write(&a, "plain line\n").unwrap();
    std::fs::write(&b, "\x1b[32mcolored line\x1b[0m\n").unwrap();
    let (a, b) = (a.to_str().unwrap(), b.to_str().unwrap());

    let (native_stdout, _) = native_diff(&[a, b]);
    assert!(
        native_stdout.contains(&0x1b),
        "precondition: native `diff a b` output must carry the ESC byte"
    );

    let out = common::skim().args(["diff", a, b]).output().unwrap();
    assert_eq!(
        out.stdout, native_stdout,
        "ESC bytes in diff CONTENT must reach the reader unmodified (ADR-012)"
    );
}

/// TAB bytes in the `--- path\t<mtime>` unified-format header must survive (PF-006).
///
/// The `--- path\t<mtime>` header is produced only by unified format (`-u`).
/// The flagless path no longer injects `-u`, so we pass it explicitly here.
/// The principle under test is `skip_ansi_strip: true`: the ANSI-strip step
/// runs BEFORE `parse()`, so without the flag it would consume the `\t` before
/// `RawPassthrough` can serve the raw bytes.
#[test]
fn a3b_tab_delimiter_in_unified_header_survives_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = fixture(dir.path());

    // Pass -u explicitly: unified format emits `--- path\t<mtime>` headers.
    let (native_stdout, native_code) = native_diff(&["-u", &a, &b]);
    assert!(
        native_stdout.contains(&b'\t'),
        "precondition: native `diff -u` must emit a TAB in the file header"
    );

    let out = common::skim()
        .args(["diff", "-u", &a, &b])
        .output()
        .unwrap();
    assert_eq!(
        out.stdout, native_stdout,
        "`skim diff -u` must be byte-identical to `diff -u`; TAB in header must survive"
    );
    assert_eq!(out.status.code(), native_code, "exit code must propagate");
}
