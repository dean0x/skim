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
//! ## Two yardsticks, deliberately different
//!
//! | invocation | must equal | why |
//! |---|---|---|
//! | `skim diff <conflicting-flag> …` | `diff <flag> …` | no `-u` is injected, so skim ran the user's literal command |
//! | `skim diff a b` (flagless) | `diff -u a b` | `prepare_args` injects `-u`; that is the command skim ran |
//! | `SKIM_PASSTHROUGH=1 skim diff a b` | `diff a b` | the escape hatch fires at the dispatch convergence point, BEFORE `prepare_args`, so the user's literal command is what runs |
//!
//! The middle row is a KNOWN residual (PF-024): with no parser left, the `-u`
//! injection buys nothing and costs the reader the difference between normal and
//! unified format. It is pinned here as a measurement rather than asserted away,
//! so that removing the injection shows up as a test change, not a silent drift.

use std::path::Path;
use std::process::Command;

mod common;

/// The 18 `diff` flag forms `prepare_args` must recognise as format-conflicting
/// or already-unified — i.e. every form for which `-u` must NOT be injected.
///
/// MEASURED: with `-u` prepended, every one of the 12 non-unified forms exits 2
/// with zero stdout on this platform, where the native form exits 1 with output.
/// A missing entry is a total-loss path, not a formatting nit.
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

/// The flagless default path: skim injects `-u`, so `diff -u` is the command it
/// actually ran, and its bytes are what must reach the reader unmodified.
#[test]
fn a3b_flagless_diff_is_byte_identical_to_the_injected_command() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = fixture(dir.path());
    let (native_stdout, native_code) = native_diff(&["-u", &a, &b]);

    let out = common::skim().args(["diff", &a, &b]).output().unwrap();

    assert_eq!(
        out.stdout,
        native_stdout,
        "flagless `skim diff` must be byte-identical to `diff -u`; got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(out.status.code(), native_code, "exit code must propagate");
}

/// MEASUREMENT, not an aspiration: the flagless path still costs the reader the
/// normal→unified format difference, because `prepare_args` injects `-u` even
/// though nothing parses the result any more (PF-024, unremediated).
///
/// This test exists so that the residual is a recorded number rather than an
/// unstated assumption. If the `-u` injection is ever removed, this test fails
/// and the removal must be acknowledged rather than slipping through.
#[test]
fn a3b_flagless_diff_still_diverges_from_the_users_literal_command() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = fixture(dir.path());
    let (literal_stdout, _) = native_diff(&[&a, &b]);

    let out = common::skim().args(["diff", &a, &b]).output().unwrap();

    assert_ne!(
        out.stdout, literal_stdout,
        "if these are now equal, the `-u` injection was removed — delete this \
         test and tighten a3b_flagless_diff_is_byte_identical_to_the_injected_command \
         to compare against the literal command instead"
    );
    assert!(
        out.stdout.len() > literal_stdout.len(),
        "the injected unified form is expected to be LARGER than the user's \
         literal `diff` output ({} vs {} bytes)",
        out.stdout.len(),
        literal_stdout.len()
    );
}

/// `SKIM_PASSTHROUGH=1` must deliver the USER's literal command — no `-u`.
///
/// This is the PF-024 consequence the dispatch convergence gate closes: the
/// execution-layer hatch streamed `stream_passthrough_raw(program, args, …)`
/// with `args` taken AFTER `prepare_args` had mutated them, so the documented
/// escape hatch emitted a different FORMAT than the user asked for and measured
/// 3.6% LARGER than never invoking skim at all. The gate fires before any
/// handler runs, so the argv is the user's.
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
#[test]
fn a3b_esc_bytes_in_file_content_survive_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("esc_a.txt");
    let b = dir.path().join("esc_b.txt");
    std::fs::write(&a, "plain line\n").unwrap();
    std::fs::write(&b, "\x1b[32mcolored line\x1b[0m\n").unwrap();
    let (a, b) = (a.to_str().unwrap(), b.to_str().unwrap());

    let (native_stdout, _) = native_diff(&["-u", a, b]);
    assert!(
        native_stdout.contains(&0x1b),
        "precondition: native output must carry the ESC byte"
    );

    let out = common::skim().args(["diff", a, b]).output().unwrap();
    assert_eq!(
        out.stdout, native_stdout,
        "ESC bytes in diff CONTENT must reach the reader unmodified (ADR-012)"
    );
}

/// TAB bytes in the `--- path\t<mtime>` header must survive (PF-006).
#[test]
fn a3b_tab_delimiter_in_header_survives_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = fixture(dir.path());

    let out = common::skim().args(["diff", &a, &b]).output().unwrap();
    assert!(
        out.stdout.contains(&b'\t'),
        "the `--- path\\t<mtime>` TAB must survive; got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
}
