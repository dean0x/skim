//! Integration test for the PATH-wrapper surface (argv[0] dispatch).
//!
//! ## Two distinct dispatch surfaces in skim
//!
//! skim intercepts sub-agent shell commands through TWO INDEPENDENT mechanisms:
//!
//! 1. **Rewrite engine** (`PreToolUse` hook / `skim rewrite` CLI): operates on the
//!    command *as text, before it runs*.  `try_rewrite()` transforms the string
//!    `grep -rn x` → `skim grep -rn x`.  Flag preservation, corruption-bail, and
//!    pipe-source passthrough are properties of THIS surface.
//!
//! 2. **PATH wrappers** (`skim init --wrappers`): symlinks `~/.skim/bin/<tool>` →
//!    the skim binary so sub-agent shells route through skim even when they bypass
//!    `PreToolUse` hooks.  Here skim IS the tool: the OS runs the binary with
//!    `argv[0]=<tool>`, `main()` calls `strip_skim_wrappers_from_path()` first,
//!    then `detect_argv0_dispatch()` routes straight to `cmd::dispatch(tool, args)`.
//!    `try_rewrite` is **never called** on this surface.
//!
//! ## What these tests verify
//!
//! The existing integration test suite exclusively invokes
//! `Command::cargo_bin("skim").args(...)`, which sets `argv[0]="skim"` and
//! exercises the hook/rewrite dispatch path.  Nothing exercises the wrapper surface.
//!
//! These tests invoke the built skim binary with **argv[0] set to a tool name**
//! using `std::os::unix::process::CommandExt::arg0()`, exercising the wrapper
//! dispatch front-end directly.
//!
//! Assertions:
//! - (a) The binary dispatches correctly and produces output (not empty on success).
//! - (b) The net-savings guard works on the wrapper front-end: skim stdout is
//!   not longer than the raw tool output for a tiny input.
//! - (c) The real exit code propagates.
//!
//! Unix-only: `arg0()` is defined on `std::os::unix::process::CommandExt`.

mod common;

#[cfg(unix)]
mod argv0_dispatch {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt as _;

    /// Path to the skim binary built by `cargo test`.
    ///
    /// `CARGO_BIN_EXE_skim` is set by cargo for integration tests of bin crates.
    /// It points at the binary that was just compiled — the same one
    /// `Command::cargo_bin("skim")` resolves but without the overhead of
    /// a second locate call.
    fn skim_bin() -> std::path::PathBuf {
        // CARGO_BIN_EXE_skim is set by cargo for the binary under test.
        if let Ok(path) = std::env::var("CARGO_BIN_EXE_skim") {
            return std::path::PathBuf::from(path);
        }
        // Fallback: walk from CARGO_MANIFEST_DIR upward to find target/debug/skim.
        let manifest_dir =
            std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set by cargo");
        let mut p = std::path::PathBuf::from(manifest_dir);
        // crates/rskim → workspace root
        p.pop();
        p.pop();
        p.join("target").join("debug").join("skim")
    }

    /// Build a tiny stub directory with a real tool wrapper so PATH resolution
    /// finds the right tool when skim strips its wrappers and spawns the child.
    ///
    /// Returns the temp dir (must be kept alive by caller).
    fn make_stub_dir(name: &str, stdout: &str, code: i32) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join(format!("{name}.out"));
        fs::write(&out_path, stdout).unwrap();
        let script = format!("#!/bin/sh\ncat '{}'\nexit {code}\n", out_path.display());
        let script_path = dir.path().join(name);
        fs::write(&script_path, script).unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    /// Prepend a directory to the current PATH.
    fn prepend_path(dir: &std::path::Path) -> String {
        format!(
            "{}:{}",
            dir.display(),
            std::env::var("PATH").unwrap_or_default()
        )
    }

    // ========================================================================
    // Test (a)+(b): wrapper dispatch produces output and does not expand
    // ========================================================================

    /// Invoke skim binary with argv[0]="ls" and assert:
    /// - exit code 0 (no crash)
    /// - output is produced (not empty)
    /// - skim stdout is ≤ raw output (net-savings guard on wrapper front-end)
    ///
    /// We use a stub `ls` that produces a tiny, deterministic output to avoid
    /// flakiness from real directory listings.
    #[test]
    fn argv0_ls_wrapper_dispatches_and_does_not_expand() {
        // Tiny deterministic stdout — short enough that net-savings guard may
        // passthrough raw, but guarantees skim never *expands* it.
        let raw_output = "file_a.txt\nfile_b.txt\n";
        let stub_dir = make_stub_dir("ls", raw_output, 0);
        let path = prepend_path(stub_dir.path());

        let skim = skim_bin();
        assert!(
            skim.exists(),
            "skim binary must exist at {}: run `cargo build` first",
            skim.display()
        );

        // Invoke as argv[0]="ls" — exercises the wrapper dispatch path.
        // skim sees argv[0]="ls", strips wrappers, calls dispatch("ls", args).
        let output = std::process::Command::new(&skim)
            // argv[0] set to "ls" — this is what a symlink invocation does.
            .arg0("ls")
            // Pass no positional args so stub ls uses its sidecar output.
            .env("PATH", &path)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("skim binary must be spawnable");

        // (c) Exit code propagates from the stub.
        assert_eq!(
            output.status.code(),
            Some(0),
            "argv[0]=ls wrapper dispatch must exit 0; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );

        let skim_stdout = String::from_utf8_lossy(&output.stdout);

        // (a) Output is produced (not empty).
        assert!(
            !skim_stdout.trim().is_empty(),
            "argv[0]=ls wrapper dispatch must produce non-empty stdout"
        );

        // (b) Net-savings guard: skim must not emit MORE bytes than raw.
        // Trim trailing whitespace on both sides — a single trailing newline
        // difference is not an expansion.  This matches the strict `<=` used in
        // `cli_no_expansion_317.rs` (applies ADR-001).
        let raw_trimmed = raw_output.trim_end();
        let skim_trimmed = skim_stdout.trim_end();
        let raw_len = raw_trimmed.len();
        let skim_len = skim_trimmed.len();
        assert!(
            skim_len <= raw_len,
            "wrapper dispatch must not expand output vs raw (#317 invariant): \
             raw={raw_len}B skim={skim_len}B\n\
             skim_stdout={skim_stdout:?}"
        );
    }

    // ========================================================================
    // Test (c): exit code propagates on the wrapper surface
    // ========================================================================

    /// Verify that a non-zero exit from the underlying tool propagates through
    /// the wrapper dispatch path unchanged.
    #[test]
    fn argv0_wrapper_propagates_nonzero_exit_code() {
        // Stub grep that exits 1 (POSIX "no match" — normal expected exit code).
        let stub_dir = make_stub_dir("grep", "", 1);
        let path = prepend_path(stub_dir.path());

        let skim = skim_bin();
        let output = std::process::Command::new(&skim)
            .arg0("grep")
            .env("PATH", &path)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("skim binary must be spawnable");

        // Exit 1 from grep (no match) must propagate verbatim.
        assert_eq!(
            output.status.code(),
            Some(1),
            "wrapper dispatch must propagate exit code 1 from stub grep; \
             stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // ========================================================================
    // Test: --session-id stripping on the wrapper (argv0) surface
    // ========================================================================

    /// Verify that a stray `--session-id=<value>` injected by an older hook binary
    /// is stripped on the wrapper dispatch surface — not forwarded to the underlying
    /// tool (which would fail with "unrecognised option").
    ///
    /// This is the wrapper-surface counterpart to `cli_session_id_skew.rs`, which
    /// covers the same strip on the hook/rewrite surface.  Both surfaces route
    /// through `cmd::dispatch()` where `strip_session_id_flag` is the first action,
    /// but the two dispatch front-ends are independent (argv[0] vs argv[0]="skim"),
    /// so a test on one surface does not exercise the other.
    ///
    /// Assertions mirror `skew_grep_session_id_stripped` in `cli_session_id_skew.rs`:
    /// - exit code ≠ 2 (no "unrecognised option" failure from grep)
    /// - expected output is produced (grep found the pattern)
    /// - `--session-id` does not appear in stdout
    #[test]
    fn argv0_grep_with_session_id_is_stripped() {
        // Create a tiny file with a known line so grep succeeds.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.txt");
        fs::write(&file, "hello world\n").unwrap();

        // Build a stub grep that executes the real grep, ensuring the wrapper
        // dispatch path finds a real grep rather than itself recursively.
        // We rely on the real system grep here (same as cli_session_id_skew.rs does)
        // since the PATH stripping inside skim prevents recursion.
        let skim = skim_bin();
        assert!(
            skim.exists(),
            "skim binary must exist at {}: run `cargo build` first",
            skim.display()
        );

        // Invoke as argv[0]="grep" with an injected --session-id=<value> (equals form).
        // Without strip_session_id_flag, real grep would reject --session-id with exit 2.
        let output = std::process::Command::new(&skim)
            .arg0("grep")
            .args(["--session-id=skew-test", "hello", file.to_str().unwrap()])
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("skim binary must be spawnable");

        // Exit code must NOT be 2 (grep's "unrecognised option" exit when the
        // stray flag reaches it).  It must be 0 (found a match).
        assert_ne!(
            output.status.code(),
            Some(2),
            "argv[0]=grep with --session-id=skew-test must not exit 2 \
             (strip_session_id_flag must fire on wrapper surface); \
             stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.status.code(),
            Some(0),
            "argv[0]=grep --session-id=skew-test hello <file> must exit 0 \
             (grep found 'hello'); stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );

        let skim_stdout = String::from_utf8_lossy(&output.stdout);

        // grep output must contain the matched line.
        assert!(
            skim_stdout.contains("hello"),
            "argv[0]=grep wrapper dispatch must produce grep output containing 'hello'; \
             got: {skim_stdout:?}"
        );

        // --session-id must not appear in stdout (not forwarded to grep output).
        assert!(
            !skim_stdout.contains("--session-id"),
            "argv[0]=grep wrapper dispatch must not leak --session-id into stdout; \
             got: {skim_stdout:?}"
        );
    }

    /// Space-separated form `--session-id skew-test` on the wrapper surface.
    ///
    /// Mirrors `skew_git_status_session_id_space_form_stripped` from
    /// `cli_session_id_skew.rs` on the argv0 dispatch path.
    #[test]
    fn argv0_grep_with_session_id_space_form_is_stripped() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.txt");
        fs::write(&file, "hello world\n").unwrap();

        let skim = skim_bin();

        // Space-separated: --session-id <value> (two separate argv entries).
        let output = std::process::Command::new(&skim)
            .arg0("grep")
            .args(["--session-id", "skew-test", "hello", file.to_str().unwrap()])
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("skim binary must be spawnable");

        assert_ne!(
            output.status.code(),
            Some(2),
            "argv[0]=grep with space-form --session-id must not exit 2; \
             stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.status.code(),
            Some(0),
            "argv[0]=grep --session-id skew-test hello <file> must exit 0; \
             stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );

        let skim_stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            skim_stdout.contains("hello"),
            "argv[0]=grep wrapper dispatch (space form) must produce grep output; \
             got: {skim_stdout:?}"
        );
        assert!(
            !skim_stdout.contains("--session-id"),
            "argv[0]=grep wrapper dispatch (space form) must not leak --session-id; \
             got: {skim_stdout:?}"
        );
    }

    // ========================================================================
    // Test: D2b (#370) — stdout-destination fidelity on the wrapper (argv0) surface
    //
    // These tests cover the WRAPPER surface.  The D2-A tests in cli_rewrite.rs
    // cover the rewrite/hook surface.  Both surfaces are needed for full coverage.
    //
    // D2b paired-coverage contract (PF-004) — two tests, one tool (ls):
    //   • `argv0_wrapper_stdout_file_passes_raw_bytes` — stdout → regular file:
    //     fstat gate fires, raw bytes reach the file unmodified (no skim header).
    //   • `argv0_ls_wrapper_stdout_pipe_compresses` — stdout → pipe:
    //     fstat gate does NOT fire, skim's ls handler runs (compresses, header
    //     present).  Uses `ls` (still compresses via long-form parser) rather than
    //     grep (always-passthrough after Fix 3) so the two branches emit DIFFERENT
    //     bytes — a discriminating pair that fails if either gate breaks.
    // ========================================================================

    /// D2b (#370): when the wrapper's stdout (fd 1) is a regular file, the
    /// wrapper must run the real tool with inherited stdio so its raw bytes
    /// reach the file unmodified — NOT skim's compressed summary.
    ///
    /// We redirect stdout to a real temp file (`.stdout(File::create(tmp))`
    /// instead of assert_cmd's default pipe), then assert the file holds raw
    /// tool output with no `tool N` skim header or footer.
    #[test]
    fn argv0_wrapper_stdout_file_passes_raw_bytes() {
        // Stub ls that emits a deterministic multi-line output.
        let raw_output = "alpha.txt\nbeta.rs\ngamma.py\n";
        let stub_dir = make_stub_dir("ls", raw_output, 0);
        let path = prepend_path(stub_dir.path());

        let skim = skim_bin();
        assert!(skim.exists(), "skim binary must exist");

        let tmp_dir = tempfile::tempdir().unwrap();
        let out_file = tmp_dir.path().join("out.txt");

        // Invoke with fd 1 → regular file: this is the D2b scenario.
        let status = std::process::Command::new(&skim)
            .arg0("ls")
            .env("PATH", &path)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            // Redirect stdout to a real file — NOT assert_cmd's default pipe.
            .stdout(std::fs::File::create(&out_file).unwrap())
            .status()
            .expect("skim binary must be spawnable");

        assert_eq!(
            status.code(),
            Some(0),
            "wrapper dispatch to file must exit 0"
        );

        let file_contents = std::fs::read_to_string(&out_file).unwrap();

        // Raw bytes must be in the file — no skim header or footer.
        assert!(
            file_contents.contains("alpha.txt"),
            "raw output must be in file; got: {file_contents:?}"
        );
        assert!(
            !file_contents.contains("ls "),
            "skim 'ls N' header must NOT appear in file; got: {file_contents:?}"
        );
        // Exact round-trip: the file should equal the stub's raw output.
        assert_eq!(
            file_contents, raw_output,
            "file must contain exactly the raw tool bytes"
        );
    }

    /// D2b control: when stdout is a PIPE, the wrapper runs skim's handler normally —
    /// the `stdout_is_regular_file()` fstat gate must NOT fire.
    ///
    /// Uses `ls` (which still compresses via the long-form parser, unlike grep after
    /// Fix 3) with a large `ls -la` stub (120 files plus `total` and dotdir lines).
    /// Skim's long-form parser strips `total`/`.`/`..`, caps at MAX_DISPLAY_ENTRIES
    /// (100), and emits a `ls 100/120` ratio header — output structurally distinct
    /// from the raw bytes and clearly shorter.
    ///
    /// Paired with `argv0_wrapper_stdout_file_passes_raw_bytes` (file-stdout side):
    /// that test proves fstat fires (raw bytes reach the file); this test proves
    /// fstat does NOT fire on a pipe (skim processes and compresses).  If fstat
    /// incorrectly fired on pipes too, stdout would equal raw bytes — no `ls ` header,
    /// `total` line present, same byte length — and at least one assertion below fails.
    #[test]
    fn argv0_ls_wrapper_stdout_pipe_compresses() {
        // Large ls -la stub: 120 real files + total + dotdir lines.
        // MAX_DISPLAY_ENTRIES (100) < 120 real files → skim truncates, footer shows
        // omission count, total bytes drops well below raw → net-savings guard allows
        // skim's compressed output through instead of falling back to raw.
        let mut raw_output = String::from("total 120\n");
        raw_output.push_str("drwxr-xr-x  2 user group  4096 Jan 01 .\n");
        raw_output.push_str("drwxr-xr-x  2 user group  4096 Jan 01 ..\n");
        for i in 1..=120 {
            raw_output.push_str(&format!(
                "-rw-r--r--  1 user group {:>5} Jan 01 file_{i:03}.txt\n",
                i * 10
            ));
        }
        let stub_dir = make_stub_dir("ls", &raw_output, 0);
        let path = prepend_path(stub_dir.path());

        let skim = skim_bin();
        assert!(skim.exists(), "skim binary must exist");

        // Default invocation — stdout is a pipe (.output()), NOT a regular file.
        // The fstat gate must not intercept this path.
        let output = std::process::Command::new(&skim)
            .arg0("ls")
            .env("PATH", &path)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("skim binary must be spawnable");

        assert_eq!(output.status.code(), Some(0), "must exit 0");

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Skim header must appear: long-form ls parser ran and emitted "ls N/M".
        // If fstat incorrectly fired, stdout == raw bytes → no skim header, has "total ".
        assert!(
            stdout.starts_with("ls "),
            "skim 'ls N/M' header must appear on pipe path \
             (fstat gate must NOT fire on pipes); got: {stdout:?}"
        );

        // `total` and dotdir lines stripped by skim's long-form ls parser.
        assert!(
            !stdout.contains("total "),
            "ls 'total' line must be stripped by skim parser on pipe path; got: {stdout:?}"
        );

        // Compression: skim capped at 100 entries; output shorter than 123-line raw.
        assert!(
            stdout.len() < raw_output.len(),
            "pipe stdout must be shorter than raw ls -la output (skim compressed and capped at 100); \
             raw={} bytes, skim={} bytes\nskim_stdout={stdout:?}",
            raw_output.len(),
            stdout.len()
        );
    }

    // ========================================================================
    // Test: argv[0]="skim" — normal invocation path is not broken
    // ========================================================================

    /// Confirm that when argv[0]="skim", the binary does NOT enter wrapper
    /// dispatch and falls through to normal clap parsing.  Calling with
    /// --help exits 0 and prints help text.
    #[test]
    fn argv0_skim_normal_path_not_broken() {
        let skim = skim_bin();
        let output = std::process::Command::new(&skim)
            .arg0("skim")
            .arg("--help")
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .output()
            .expect("skim binary must be spawnable");

        assert_eq!(
            output.status.code(),
            Some(0),
            "skim --help must exit 0; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("skim") || stdout.contains("Usage"),
            "skim --help must print usage/help text; got: {stdout:?}"
        );
    }
}
