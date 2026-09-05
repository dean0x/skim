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

    /// Per-binary cache sandbox (PF-017) — see the identical note in
    /// `cli_e2e_rewrite.rs`.
    ///
    /// These are *reader*-side tests: `force_raw_requested()` walks this
    /// process's ancestry for a force-raw marker. Left pointing at the real
    /// `~/.cache/skim`, the PID it reaches is the shared nextest runner, so a
    /// marker written by a hook-mode test in a concurrently-running binary
    /// flips these invocations to `run_inherited_passthrough` — bypassing the
    /// session-id stripping and tree compression they assert. The collision is
    /// scheduling-dependent, so it appears only at certain suite sizes.
    static CACHE_SANDBOX: std::sync::LazyLock<tempfile::TempDir> =
        std::sync::LazyLock::new(|| tempfile::tempdir().expect("cache sandbox tempdir"));

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
            .env("SKIM_CACHE_DIR", CACHE_SANDBOX.path())
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
            .env("SKIM_CACHE_DIR", CACHE_SANDBOX.path())
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
            .env("SKIM_CACHE_DIR", CACHE_SANDBOX.path())
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
            .env("SKIM_CACHE_DIR", CACHE_SANDBOX.path())
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
    // D2b paired-coverage contract (PF-004) — two tests, two tools:
    //   • `argv0_wrapper_stdout_file_passes_raw_bytes` (tool: ls) — stdout →
    //     regular file: fstat gate fires, raw bytes reach the file unmodified
    //     (no skim header).  ls is byte-faithful passthrough (ADR-009); the fstat
    //     test is valid independent of compression because the gate fires before
    //     skim processes anything.
    //   • `argv0_tree_wrapper_stdout_pipe_compresses` (tool: tree) — stdout →
    //     pipe: fstat gate does NOT fire, skim's tree handler runs (Tier-2 text
    //     parser strips depth-4+ entries, emits "tree N/M" header).  tree is used
    //     because it genuinely compresses — unlike ls/grep/wc/find/ps/df/du which
    //     became byte-faithful passthrough in this wave — so the two branches emit
    //     DIFFERENT bytes, making the pair discriminating.
    //
    //   Together the pair proves fstat discriminates: file path → raw bytes,
    //   pipe path → skim-compressed output.  A tool that passes through on the
    //   pipe path too would collapse the pair to equivalent outputs and make
    //   the pipe test vacuous (this is exactly what happened when ls was used
    //   here before being converted to passthrough).
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
            .env("SKIM_CACHE_DIR", CACHE_SANDBOX.path())
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
    /// the `stdout_should_serve_raw()` fstat gate must NOT fire.
    ///
    /// Uses `tree` (Tier-2 text parser, structural compression) with a stub that
    /// includes depth-4+ entries hidden by skim's MAX_DEPTH=3 cap.  Skim strips the
    /// deep entries, appends a footer counting the hidden entries, and emits a
    /// `tree N/M` ratio header — output structurally distinct from the raw bytes and
    /// clearly shorter.
    ///
    /// Paired with `argv0_wrapper_stdout_file_passes_raw_bytes` (file-stdout side,
    /// uses `ls`): that test proves fstat fires (raw bytes in file); this test proves
    /// fstat does NOT fire on a pipe (skim processes and compresses).  If fstat
    /// incorrectly fired on pipes too, stdout would equal raw bytes — no `tree `
    /// header, deep entries present, same byte length — and at least one assertion
    /// below fails.
    ///
    /// GUARD: this test relies on `tree` remaining a structurally-compressing wrapper
    /// (Tier-2 text parser strips depth-capped entries, emits a ratio header).
    /// If `tree` is ever converted to byte-faithful passthrough, both pipe and file
    /// paths would emit identical raw bytes, making this test vacuous.
    /// Retarget it to a tool that still compresses — see the D2b section comment above
    /// for the discriminating-pair requirement.
    #[test]
    fn argv0_tree_wrapper_stdout_pipe_compresses() {
        // Tree text stub: files at depth 0-3 (shown by skim) plus depth-4 entries
        // (hidden by skim's MAX_DEPTH=3 cap in try_parse_tree_text).
        //
        // Depth counting: count_tree_depth counts leading chars matching
        // ' '|'\t'|'|'|'+'|'\\' then divides by 4.  Each "level" is 4 chars of
        // `|   ` (pipe + 3 spaces).
        //   depth 0: 1  leading char  → `|-- name`
        //   depth 1: 5  leading chars → `|   |-- name`
        //   depth 2: 9  leading chars → `|   |   |-- name`
        //   depth 3: 13 leading chars → `|   |   |   |-- name`
        //   depth 4: 17 leading chars → `|   |   |   |   |-- name`  ← HIDDEN (> MAX_DEPTH)
        //
        // 12 depth-4 entries × ~25 bytes each = ~300 bytes removed from output.
        // Skim adds header (~12 bytes) + footer (~50 bytes) = 62 bytes net overhead.
        // Net savings ≈ 238 bytes → compressed.len() < raw.len() → net-savings guard
        // returns Keep, so skim's compressed view reaches stdout (not raw fallback).
        let mut raw_output = String::from(".\n");
        // depth 0 — root-level files and dirs
        raw_output.push_str("|-- README.md\n");
        raw_output.push_str("|-- Cargo.lock\n");
        raw_output.push_str("|-- Cargo.toml\n");
        raw_output.push_str("|-- src\n");
        // depth 1 — src children
        raw_output.push_str("|   |-- main.rs\n");
        raw_output.push_str("|   |-- lib.rs\n");
        raw_output.push_str("|   |-- cmd\n");
        // depth 2 — cmd children
        raw_output.push_str("|   |   |-- mod.rs\n");
        raw_output.push_str("|   |   |-- file\n");
        // depth 3 — file/ children (13 leading chars → shown)
        raw_output.push_str("|   |   |   |-- ls.rs\n");
        raw_output.push_str("|   |   |   |-- env.rs\n");
        raw_output.push_str("|   |   |   |-- find.rs\n");
        raw_output.push_str("|   |   |   |-- du.rs\n");
        raw_output.push_str("|   |   |   |-- df.rs\n");
        raw_output.push_str("|   |   |   |-- ps.rs\n");
        raw_output.push_str("|   |   |   |-- wc.rs\n");
        raw_output.push_str("|   |   |   |-- mod.rs\n");
        raw_output.push_str("|   |   |-- git\n");
        // depth 3 — git/ children
        raw_output.push_str("|   |   |   |-- mod.rs\n");
        raw_output.push_str("|   |   |   |-- status.rs\n");
        raw_output.push_str("|   |   |   |-- diff.rs\n");
        raw_output.push_str("|   |   |   |-- log.rs\n");
        raw_output.push_str("|   |   |   |-- blame.rs\n");
        raw_output.push_str("|   |   |-- deep_nested\n");
        // depth 3 — deep_nested child dir (shown; its children at depth 4 are hidden)
        raw_output.push_str("|   |   |   |-- level4_dir\n");
        // depth 4 — hidden by MAX_DEPTH=3 (17 leading chars → depth 4)
        raw_output.push_str("|   |   |   |   |-- alpha.rs\n");
        raw_output.push_str("|   |   |   |   |-- beta.rs\n");
        raw_output.push_str("|   |   |   |   |-- gamma.rs\n");
        raw_output.push_str("|   |   |   |   |-- delta.rs\n");
        raw_output.push_str("|   |   |   |   |-- epsilon.rs\n");
        raw_output.push_str("|   |   |   |   |-- zeta.rs\n");
        raw_output.push_str("|   |   |   |   |-- eta.rs\n");
        raw_output.push_str("|   |   |   |   |-- theta.rs\n");
        raw_output.push_str("|   |   |   |   |-- iota.rs\n");
        raw_output.push_str("|   |   |   |   |-- kappa.rs\n");
        raw_output.push_str("|   |   |   |   |-- lambda.rs\n");
        raw_output.push_str("|   |   |   |   |-- mu.rs\n");
        // back to depth 1 — more src children
        raw_output.push_str("|   |-- tests\n");
        // depth 2 — tests children
        raw_output.push_str("|   |   |-- cli_e2e.rs\n");
        raw_output.push_str("|   |   |-- cli_wrapper.rs\n");
        raw_output.push_str("|   |   |-- common.rs\n");
        raw_output.push('\n');
        raw_output.push_str("7 directories, 34 files\n");

        let stub_dir = make_stub_dir("tree", &raw_output, 0);
        let path = prepend_path(stub_dir.path());

        let skim = skim_bin();
        assert!(skim.exists(), "skim binary must exist");

        // Default invocation — stdout is a pipe (.output()), NOT a regular file.
        // The fstat gate must not intercept this path.
        let output = std::process::Command::new(&skim)
            .arg0("tree")
            .env("PATH", &path)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("SKIM_CACHE_DIR", CACHE_SANDBOX.path())
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("skim binary must be spawnable");

        assert_eq!(output.status.code(), Some(0), "must exit 0");

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Skim header must appear: tree Tier-2 text parser ran and emitted "tree N/M".
        // If fstat incorrectly fired, stdout == raw bytes → no skim header,
        // "level4_dir" and depth-4 entries present, same byte length — at least one
        // assertion below fails.
        assert!(
            stdout.starts_with("tree "),
            "skim 'tree N/M' header must appear on pipe path \
             (fstat gate must NOT fire on pipes); got: {stdout:?}"
        );

        // Depth-4 entries (17 leading pipe/space chars) must be hidden by skim's
        // MAX_DEPTH=3 cap in try_parse_tree_text.  If raw passthrough occurred, these
        // would be present.
        assert!(
            !stdout.contains("|   |   |   |   |-- alpha.rs"),
            "depth-4 entries must be stripped by skim's tree parser on pipe path \
             (MAX_DEPTH=3 cap); got: {stdout:?}"
        );

        // Footer must mention the hidden entries count (depth_hidden > 0).
        assert!(
            stdout.contains("deeper entries hidden"),
            "skim footer must report hidden depth-4 entries; got: {stdout:?}"
        );

        // Compression: skim hid 12 depth-4 entries; output shorter than raw.
        assert!(
            stdout.len() < raw_output.len(),
            "pipe stdout must be shorter than raw tree output \
             (skim compressed and hid depth-4 entries); \
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
            .env("SKIM_CACHE_DIR", CACHE_SANDBOX.path())
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

    // ========================================================================
    // Tests: B2 (skip_if_middle_contains_eq) and B3 (require_flag) wrapper gaps
    //
    // The rewrite engine has two predicates that prevent certain commands from
    // being rewritten to `skim <tool>`:
    //
    //   B2  `skip_if_middle_contains_eq` (engine.rs ~:132-139): `env LANG=C sort`
    //       contains an `=`-bearing middle token signalling env-var assignment for a
    //       child process, not printenv-style output.  The env rewrite rule sets this
    //       flag (rules.rs ~:1787).  On the wrapper surface, `dispatch_for_wrapper`
    //       currently has no equivalent check — `env FOO=bar printf %s x` falls
    //       through to `dispatch("env", …)`, which routes to skim's env handler.
    //       The handler runs `printenv FOO=bar printf %s x` instead of the real
    //       `env` binary, producing wrong output.
    //
    //   B3  `require_flag` (engine.rs ~:145-161): `psql` requires `-c`/`--command`
    //       and `mysql` requires `-e`/`--execute` — without them the tool opens an
    //       interactive session that should not be intercepted.  The wrapper surface
    //       currently has no equivalent check.
    //
    // The fix (not yet applied) will add both checks to `dispatch_for_wrapper` so
    // those shapes reach `run_raw_passthrough` instead of skim's handlers.
    //
    // Observation technique (all five tests):
    //   — B2 (tests 1-2): The env handler always runs `printenv` regardless of args.
    //     Real `env FOO=bar prog` executes `prog`; skim's handler runs `printenv`
    //     instead and produces wrong (or empty) output.  stdout exactness proves
    //     which path ran.
    //   — B3 (tests 3-5): skim's db handlers set tool-specific env overrides
    //     (`PGPAGER=cat` for psql, `MYSQL_PAGER=cat` for mysql) before spawning the
    //     child binary.  A raw passthrough does NOT inject these overrides.  A fake
    //     shell script stub reads its own env and prints the value — presence of
    //     `cat` in the output proves skim's handler ran; its absence proves raw
    //     passthrough.  Tier 3 (non-parseable) output from the fake passes through
    //     skim verbatim, so the env-var value is observable end-to-end.
    //
    // NOTE on D3 and `-h`: D3 in `dispatch_for_wrapper` passes through any command
    // containing exactly `-h` (as a help flag).  psql's `-h host` short-form ALSO
    // matches, so `psql -h localhost` already escapes via D3 today and the B3 gap
    // is invisible for that shape.  Tests 3 and 5 use `--host=localhost` (long form
    // without the ambiguous `-h`) to expose the gap against the psql/mysql handlers.
    // ========================================================================

    /// Create a temp dir containing a shell-script stub named `name`.
    ///
    /// Unlike [`make_stub_dir`], the script body is arbitrary — the caller
    /// controls exactly what the stub does (including reading its own env).
    fn make_script_stub(name: &str, script_body: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let script = format!("#!/bin/sh\n{script_body}\n");
        let script_path = dir.path().join(name);
        fs::write(&script_path, &script).unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();
        dir
    }

    // ── B2: env with `=`-containing middle token ──────────────────────────────

    /// B2 — `=`-argument passthrough (RED at b79e287^; GREEN since b79e287).
    ///
    /// `env FOO=bar /usr/bin/printf %s x` on the wrapper surface: the handler
    /// in `cmd/file/mod.rs` detects any `=`-containing arg and calls
    /// `run_raw_passthrough("env", …)` directly.  The real `env` binary sets
    /// `FOO=bar` and exec's `/usr/bin/printf %s x`.  stdout = `"x"`, exit 0.
    ///
    /// The wrapper surface needs no D5-style interactivity gate here — the
    /// shape is detected by arg content (contains `'='`) inside the env branch
    /// of `dispatch_inner`, consistent with `skip_if_middle_contains_eq` on
    /// the rewrite surface.
    ///
    /// Observable: exit 0, stdout exactly `"x"`.
    #[test]
    fn wrapper_env_with_assignment_runs_the_real_env() {
        let skim = skim_bin();
        assert!(
            skim.exists(),
            "skim binary must exist at {}: run `cargo build` first",
            skim.display()
        );

        // `/usr/bin/printf` — absolute path avoids any PATH ambiguity after
        // skim strips wrappers.  Stable on macOS and Linux.
        let printf_bin = "/usr/bin/printf";
        assert!(
            std::path::Path::new(printf_bin).exists(),
            "{printf_bin} must exist on this system"
        );

        // argv[0]="env", args: an assignment token + child program + its args.
        // skim detects the '=' token and calls run_raw_passthrough("env", …).
        // Real env executes: env FOO=bar /usr/bin/printf %s x → stdout "x".
        let output = std::process::Command::new(&skim)
            .arg0("env")
            .args(["FOO=bar", printf_bin, "%s", "x"])
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("SKIM_CACHE_DIR", CACHE_SANDBOX.path())
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("skim binary must be spawnable");

        // GREEN since b79e287: real env ran, exit 0.
        assert_eq!(
            output.status.code(),
            Some(0),
            "wrapper env FOO=bar /usr/bin/printf %s x must exit 0 \
             (real env executed printf; GREEN since b79e287)\nstderr={}",
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8_lossy(&output.stdout);

        // GREEN since b79e287: real env ran printf → stdout is exactly "x"
        // (printf %s x outputs raw bytes with no trailing newline).
        assert_eq!(
            stdout, "x",
            "wrapper env FOO=bar /usr/bin/printf %s x must produce stdout 'x' \
             (real env executed printf; GREEN since b79e287)\ngot stdout={stdout:?}"
        );
    }

    /// B2 control — **GREEN today and after fix**.
    ///
    /// When argv[0]="env" receives NO `=`-containing middle tokens (bare `env`),
    /// the shape is env listing the environment — skim's handler IS designed to
    /// process this.  The B2 fix must not break the common case.
    ///
    /// **Observable:** `SKIM_TEST_TOKEN` ends with the `_TOKEN` sensitive suffix.
    /// Skim's env handler calls `printenv` and redacts it to `***`.  If the fix
    /// accidentally bypasses the handler for bare `env`, the raw value
    /// `secret-value-xyz` would appear in stdout instead.
    ///
    /// This test is GREEN today (handler runs, redacts) and must remain GREEN
    /// after the B2 fix (bare `env` has no `=` middle tokens → handler still runs).
    #[test]
    fn wrapper_env_without_assignment_still_uses_skim_handler() {
        let skim = skim_bin();
        assert!(
            skim.exists(),
            "skim binary must exist at {}: run `cargo build` first",
            skim.display()
        );

        // No args — bare `env` invocation.  SKIM_TEST_TOKEN ends with _TOKEN
        // (a SENSITIVE_SUFFIXES entry) → skim's env handler must redact it.
        let output = std::process::Command::new(&skim)
            .arg0("env")
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("SKIM_CACHE_DIR", CACHE_SANDBOX.path())
            .env("SKIM_TEST_TOKEN", "secret-value-xyz")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("skim binary must be spawnable");

        assert_eq!(
            output.status.code(),
            Some(0),
            "wrapper env (bare) must exit 0; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Skim's env handler ran: the raw secret must NOT appear in stdout.
        assert!(
            !stdout.contains("secret-value-xyz"),
            "raw secret must NOT appear — skim's env handler must redact it; \
             got: {stdout:?}"
        );

        // The key must appear with redacted value: confirms the handler ran,
        // not a raw passthrough.
        assert!(
            stdout.contains("SKIM_TEST_TOKEN=***"),
            "SKIM_TEST_TOKEN must be redacted to *** by skim's env handler; \
             got: {stdout:?}"
        );
    }

    // ── B3: psql without -c / --command ──────────────────────────────────────

    /// B3 — psql without `-c` passes through raw (RED at b3a31ec^; GREEN since b3a31ec).
    ///
    /// `psql --host=localhost -U u dbname` has no `-c`/`--command` flag; the
    /// invocation is interactive.  `dispatch_for_wrapper` detects missing
    /// `-c`/`--command` via `require_flags_for_tool` and calls
    /// `run_raw_passthrough` → `PGPAGER` is NOT injected → stdout contains
    /// `"pgpager=UNSET"`.
    ///
    /// **Discriminating observable:** skim's psql handler sets `PGPAGER=cat`
    /// via `CONFIG.env_overrides` before spawning the child binary.  A raw
    /// passthrough does NOT inject `PGPAGER`.  The fake psql stub reads
    /// `$PGPAGER` from its own environment and prints it.  skim's psql parser
    /// cannot parse "FAKE-PSQL pgpager=…" as tabular SQL → Tier 3 passthrough
    /// → skim emits the fake's line verbatim.  The env value is therefore
    /// observable end-to-end in skim's stdout.
    ///
    /// Note: `psql -h localhost` is NOT used here because D3 in
    /// `dispatch_for_wrapper` matches `-h` as a help flag and already passes
    /// through.  `--host=localhost` exposes the require_flag gate.
    #[test]
    fn wrapper_psql_without_command_flag_passes_through_raw() {
        // Fake psql: reads PGPAGER from env (skim's psql handler injects it;
        // raw passthrough does not) and prints a sentinel line.
        // "FAKE-PSQL pgpager=…" is not parseable as tabular psql output →
        // Tier 3 passthrough → skim emits the line verbatim.
        let stub_dir = make_script_stub(
            "psql",
            r#"printf 'FAKE-PSQL pgpager=%s\n' "${PGPAGER:-UNSET}""#,
        );
        let path = prepend_path(stub_dir.path());

        let skim = skim_bin();
        assert!(
            skim.exists(),
            "skim binary must exist at {}: run `cargo build` first",
            skim.display()
        );

        let output = std::process::Command::new(&skim)
            .arg0("psql")
            // --host= long-form avoids the D3 `-h` help-flag match.
            .args(["--host=localhost", "-U", "u", "dbname"])
            .env("PATH", &path)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("SKIM_CACHE_DIR", CACHE_SANDBOX.path())
            // Explicitly unset PGPAGER so any `pgpager=cat` in stdout can
            // only come from skim's psql handler injecting the override.
            .env_remove("PGPAGER")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("skim binary must be spawnable");

        assert_eq!(
            output.status.code(),
            Some(0),
            "wrapper psql (no -c) must exit 0; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Sanity: the fake psql ran at all.
        assert!(
            stdout.contains("FAKE-PSQL"),
            "fake psql sentinel must appear in stdout; got: {stdout:?}"
        );

        // GREEN since b3a31ec: raw passthrough ran — PGPAGER NOT injected by skim.
        assert!(
            !stdout.contains("pgpager=cat"),
            "wrapper psql --host= (no -c) must NOT have skim's psql handler \
             inject PGPAGER=cat; 'pgpager=cat' in stdout proves skim's handler \
             ran instead of raw passthrough (B3, require_flags_for_tool).\n\
             got stdout={stdout:?}"
        );
    }

    /// B3 control — psql with `-c` — **GREEN today and after fix**.
    ///
    /// `psql -c 'select 1'` HAS the required `-c` flag; it is a batch
    /// invocation that skim's psql handler is designed to compress.  The B3
    /// fix must preserve this routing: when `-c` is present, the handler runs.
    ///
    /// **Observable:** skim's psql handler sets `PGPAGER=cat` →
    /// stdout contains `"pgpager=cat"` (proves handler ran, not passthrough).
    ///
    /// This test is GREEN today (handler runs) and must remain GREEN after fix
    /// (require_flag check sees `-c` → lets `dispatch()` handle it normally).
    #[test]
    fn wrapper_psql_with_command_flag_uses_skim_handler() {
        let stub_dir = make_script_stub(
            "psql",
            r#"printf 'FAKE-PSQL pgpager=%s\n' "${PGPAGER:-UNSET}""#,
        );
        let path = prepend_path(stub_dir.path());

        let skim = skim_bin();
        assert!(skim.exists(), "skim binary must exist");

        let output = std::process::Command::new(&skim)
            .arg0("psql")
            .args(["-c", "select 1"])
            .env("PATH", &path)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("SKIM_CACHE_DIR", CACHE_SANDBOX.path())
            .env_remove("PGPAGER")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("skim binary must be spawnable");

        assert_eq!(
            output.status.code(),
            Some(0),
            "wrapper psql -c must exit 0; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Skim's psql handler injected PGPAGER=cat → fake printed "pgpager=cat".
        assert!(
            stdout.contains("pgpager=cat"),
            "wrapper psql -c must route to skim's psql handler \
             (PGPAGER=cat injected by CONFIG.env_overrides); \
             got: {stdout:?}"
        );
    }

    // ── B3: mysql without -e / --execute ─────────────────────────────────────

    /// B3 — mysql without `-e` passes through raw (RED at b3a31ec^; GREEN since b3a31ec).
    ///
    /// `mysql --host=localhost` has no `-e`/`--execute` flag; the invocation
    /// is interactive.  `dispatch_for_wrapper` detects missing `-e`/`--execute`
    /// via `require_flags_for_tool` and calls `run_raw_passthrough` →
    /// `MYSQL_PAGER` NOT injected → stdout contains `"mysqlpager=UNSET"`.
    ///
    /// **Discriminating observable:** skim's mysql handler sets `MYSQL_PAGER=cat`
    /// via `CONFIG.env_overrides` before spawning the child binary.  A raw
    /// passthrough does NOT inject `MYSQL_PAGER`.  The fake mysql stub reads
    /// `$MYSQL_PAGER` from its own env and prints it.  skim's mysql parser
    /// cannot parse "FAKE-MYSQL mysqlpager=…" as tabular mysql output → Tier 3
    /// passthrough → skim emits the line verbatim.
    ///
    /// Note: `mysql -h localhost` is NOT used because D3 matches `-h` as a
    /// help flag and already passes through.
    #[test]
    fn wrapper_mysql_without_execute_flag_passes_through_raw() {
        // Fake mysql: reads MYSQL_PAGER from env (skim's mysql handler injects
        // it; raw passthrough does not).
        // "FAKE-MYSQL mysqlpager=…" is not parseable as TSV or bordered mysql
        // output → Tier 3 passthrough → skim emits the line verbatim.
        let stub_dir = make_script_stub(
            "mysql",
            r#"printf 'FAKE-MYSQL mysqlpager=%s\n' "${MYSQL_PAGER:-UNSET}""#,
        );
        let path = prepend_path(stub_dir.path());

        let skim = skim_bin();
        assert!(
            skim.exists(),
            "skim binary must exist at {}: run `cargo build` first",
            skim.display()
        );

        let output = std::process::Command::new(&skim)
            .arg0("mysql")
            // --host= long-form avoids the D3 `-h` help-flag match.
            .args(["--host=localhost"])
            .env("PATH", &path)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("SKIM_CACHE_DIR", CACHE_SANDBOX.path())
            // Explicitly unset MYSQL_PAGER so any `mysqlpager=cat` in stdout
            // can only come from skim's mysql handler injecting the override.
            .env_remove("MYSQL_PAGER")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("skim binary must be spawnable");

        assert_eq!(
            output.status.code(),
            Some(0),
            "wrapper mysql (no -e) must exit 0; stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Sanity: the fake mysql ran at all.
        assert!(
            stdout.contains("FAKE-MYSQL"),
            "fake mysql sentinel must appear in stdout; got: {stdout:?}"
        );

        // GREEN since b3a31ec: raw passthrough ran — MYSQL_PAGER NOT injected by skim.
        assert!(
            !stdout.contains("mysqlpager=cat"),
            "wrapper mysql --host= (no -e) must NOT have skim's mysql handler \
             inject MYSQL_PAGER=cat; 'mysqlpager=cat' in stdout proves skim's \
             handler ran instead of raw passthrough (B3, require_flags_for_tool).\n\
             got stdout={stdout:?}"
        );
    }
}
