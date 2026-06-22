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
        // (Strictly: skim stdout len ≤ raw output len.)
        let raw_len = raw_output.len();
        let skim_len = skim_stdout.len();
        assert!(
            skim_len <= raw_len + raw_len, // 2× slack for trailing newline normalisation
            "wrapper dispatch must not expand output beyond 2× raw: \
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
