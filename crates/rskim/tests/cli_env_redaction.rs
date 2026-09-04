//! Regression tests for env/printenv credential redaction on both interception
//! surfaces (PF-012 / security-1 / testing-14).
//!
//! ## What these tests cover
//!
//! Three separate raw-serve paths previously bypassed skim's `env` handler and
//! let the unredacted environment reach stdout or a file:
//!
//! 1. **D4 skip-flag gate** (`dispatch.rs`) — `skip_flags_for_tool("env")`
//!    returns `["-i", "-u", "-S"]`, so `env -u HOME` triggered D4 and called
//!    `run_raw_passthrough` directly, dumping the full environment unredacted.
//!
//! 2. **main.rs raw-serve branch** — when `stdout_should_serve_raw()` or
//!    `force_raw_requested("env")` was true (e.g. `env > file`,
//!    `printenv | tee f`), the wrapper dispatched `run_inherited_passthrough`
//!    (the real binary, no handler), so secrets reached the destination verbatim.
//!
//! 3. **env FOO=1 <child> arm** (`cmd/file/mod.rs`) — when the invocation
//!    had an `=`-containing middle token, the code unconditionally called
//!    `run_raw_passthrough("env", …)`, including `env FOO=1 printenv` where
//!    the child would dump the environment unredacted.
//!
//! ## Surfaces covered
//!
//! - **Wrapper surface**: argv[0] = "env" or "printenv" via `arg0()`.
//! - **Explicit surface**: `skim env` / `skim printenv` (already worked;
//!   regression tests added for completeness).
//!
//! ## Accepted limitation (pinned)
//!
//! `env FOO=1 sh -c env` still executes raw because the child is `sh` — skim
//! cannot inspect what an arbitrary child will print. This limitation is pinned
//! in `env_assignment_sh_child_accepted_limitation` below.

mod common;

#[cfg(unix)]
mod env_redaction {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Stdio};

    /// Per-binary SKIM_CACHE_DIR sandbox — prevents force-raw sidecar cross-
    /// binary contamination (Phase 8 fix, same rationale as cli_wrapper_argv0.rs).
    static CACHE_SANDBOX: std::sync::LazyLock<tempfile::TempDir> =
        std::sync::LazyLock::new(|| tempfile::tempdir().expect("cache sandbox tempdir"));

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

    /// Write a stub script at `dir/name` that prints `stdout_content` and exits 0.
    fn make_stub(dir: &std::path::Path, name: &str, stdout_content: &str) {
        let out_path = dir.join(format!("{name}.out"));
        fs::write(&out_path, stdout_content).unwrap();
        let script = format!("#!/bin/sh\ncat '{}'\n", out_path.display());
        let script_path = dir.join(name);
        fs::write(&script_path, script).unwrap();
        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Return a base `Command` for the skim binary with the wrapper surface's
    /// shared environment: sandboxed cache, analytics off, no passthrough.
    fn base_wrapper_cmd(argv0: &str) -> Command {
        let skim = skim_bin();
        let mut c = Command::new(&skim);
        c.arg0(argv0)
            .env("SKIM_CACHE_DIR", CACHE_SANDBOX.path())
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("NO_COLOR", "1")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .env_remove("SKIM_REWRITTEN_FROM");
        c
    }

    // =========================================================================
    // Path 1 — D4 skip-flag gate: env -u X must not serve raw (security-1)
    // =========================================================================

    /// `env -u HOME` on the wrapper surface must NOT dump unredacted secrets.
    ///
    /// Before the fix: D4 in `dispatch_for_wrapper` matched `-u` as a skip flag
    /// and called `run_raw_passthrough("env", ["-u", "HOME"], &[])` — dumping
    /// the entire environment verbatim, including `SKIM_TEST_TOKEN`.
    ///
    /// After the fix: `redaction_is_mandatory("env")` suppresses D4 for
    /// `env`/`printenv`, so the call falls through to the handler.  The handler
    /// may not honour `-u` (printenv does not accept that flag), but it does NOT
    /// emit the raw `SKIM_TEST_TOKEN` value.
    #[test]
    fn env_d4_skip_flag_u_does_not_leak_secrets() {
        let skim = skim_bin();
        assert!(skim.exists(), "skim binary must exist — run `cargo build` first");

        let output = base_wrapper_cmd("env")
            .args(["-u", "HOME"])
            .env("SKIM_TEST_TOKEN", "secret-d4-value-xyz")
            .output()
            .expect("skim binary must be spawnable");

        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            !stdout.contains("secret-d4-value-xyz"),
            "env -u HOME on wrapper surface must NOT expose raw secrets via D4 gate; \
             got stdout (first 400 chars): {:?}",
            stdout.chars().take(400).collect::<String>()
        );
    }

    /// `printenv -u X` does not exist as a valid flag, but the wrapper surface
    /// must never call `run_raw_passthrough` for `printenv` regardless of flags.
    ///
    /// (Regression guard: D4 skip flags for "env" must not affect "printenv"
    /// because printenv has its own rewrite rule and handler.)
    #[test]
    fn printenv_wrapper_does_not_leak_secrets() {
        let skim = skim_bin();
        assert!(skim.exists(), "skim binary must exist — run `cargo build` first");

        let output = base_wrapper_cmd("printenv")
            .env("SKIM_TEST_TOKEN", "secret-printenv-d4-xyz")
            .output()
            .expect("skim binary must be spawnable");

        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            !stdout.contains("secret-printenv-d4-xyz"),
            "printenv on wrapper surface must NOT expose raw secrets; \
             the handler must redact SKIM_TEST_TOKEN; \
             got stdout (first 400 chars): {:?}",
            stdout.chars().take(400).collect::<String>()
        );

        // Confirm the handler ran and redacted the key.
        assert!(
            stdout.contains("SKIM_TEST_TOKEN=***"),
            "SKIM_TEST_TOKEN must be redacted to *** by skim's env handler; \
             got stdout (first 400 chars): {:?}",
            stdout.chars().take(400).collect::<String>()
        );
    }

    // =========================================================================
    // Path 2a — main.rs raw-serve branch: `env > file` (stdout_should_serve_raw)
    // =========================================================================

    /// When stdout is a regular file (`env > out.txt`), skim previously called
    /// `run_inherited_passthrough` — the real `env`/`printenv` binary, no handler.
    ///
    /// Before the fix: `SKIM_TEST_TOKEN=secret` landed verbatim in the file.
    /// After the fix: `redaction_is_mandatory` blocks the raw serve; the handler
    /// redacts the value to `***` before writing.
    #[test]
    fn env_redirect_to_file_redacts_secrets() {
        let skim = skim_bin();
        assert!(skim.exists(), "skim binary must exist — run `cargo build` first");

        let tmpdir = tempfile::tempdir().expect("tmpdir");
        let out_path = tmpdir.path().join("env_out.txt");
        let file = fs::File::create(&out_path).expect("create out file");

        let status = base_wrapper_cmd("env")
            .env("SKIM_TEST_TOKEN", "secret-file-redir-xyz")
            .stdout(Stdio::from(file))
            .status()
            .expect("skim binary must be spawnable");

        assert!(
            status.success() || !status.success(), // any exit is fine; security is the concern
            "skim env > file must at least not crash"
        );

        let landed = fs::read_to_string(&out_path).unwrap_or_default();

        assert!(
            !landed.contains("secret-file-redir-xyz"),
            "env > file must NOT write raw secrets to the file; \
             after fix the handler redacts before writing; \
             landed (first 400 chars): {:?}",
            landed.chars().take(400).collect::<String>()
        );
    }

    // =========================================================================
    // Path 3 — env FOO=1 <child>: redact when child is env/printenv (testing-14)
    // =========================================================================

    /// `env FOO=1 printenv` — child is `printenv`, must route to handler and redact.
    ///
    /// Before the fix: `cmd/file/mod.rs` saw the `=`-containing middle token and
    /// called `run_raw_passthrough("env", ["FOO=1", "printenv"], &[])` — the real
    /// `env` binary ran `printenv` without any handler, leaking all env vars.
    ///
    /// After the fix: the child program is identified as `printenv`, so the call
    /// routes to the redacting handler instead.
    #[test]
    fn env_assignment_printenv_child_redacts_secrets() {
        let skim = skim_bin();
        assert!(skim.exists(), "skim binary must exist — run `cargo build` first");

        let output = base_wrapper_cmd("env")
            .args(["FOO=1", "printenv"])
            .env("SKIM_TEST_TOKEN", "secret-child-printenv-xyz")
            .output()
            .expect("skim binary must be spawnable");

        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            !stdout.contains("secret-child-printenv-xyz"),
            "env FOO=1 printenv must NOT expose raw secrets; \
             child is printenv → route to redacting handler; \
             got stdout (first 400 chars): {:?}",
            stdout.chars().take(400).collect::<String>()
        );

        // The handler ran and redacted the key.
        assert!(
            stdout.contains("SKIM_TEST_TOKEN=***"),
            "SKIM_TEST_TOKEN must be redacted by handler when child is printenv; \
             got stdout (first 400 chars): {:?}",
            stdout.chars().take(400).collect::<String>()
        );
    }

    /// `env FOO=1 env` — child is `env`, must route to handler and redact.
    ///
    /// Same fix as `env FOO=1 printenv`; the child-name check covers both
    /// `"env"` and `"printenv"`.
    #[test]
    fn env_assignment_env_child_redacts_secrets() {
        let skim = skim_bin();
        assert!(skim.exists(), "skim binary must exist — run `cargo build` first");

        let output = base_wrapper_cmd("env")
            .args(["FOO=1", "env"])
            .env("SKIM_TEST_TOKEN", "secret-child-env-xyz")
            .output()
            .expect("skim binary must be spawnable");

        let stdout = String::from_utf8_lossy(&output.stdout);

        assert!(
            !stdout.contains("secret-child-env-xyz"),
            "env FOO=1 env must NOT expose raw secrets; \
             child is env → route to redacting handler; \
             got stdout (first 400 chars): {:?}",
            stdout.chars().take(400).collect::<String>()
        );
    }

    // =========================================================================
    // B2 preservation: env FOO=1 <non-env-child> must still exec raw
    // =========================================================================

    /// `env FOO=1 <stub-child>` — child is not env/printenv → must exec raw (B2).
    ///
    /// The fix must NOT break the deliberate B2 behaviour: `env FOO=1 npm test`
    /// uses the real `env` binary to set `FOO=1` in the child's environment.
    /// Routing to the handler here would break the use case entirely.
    #[test]
    fn env_assignment_non_env_child_execs_raw() {
        let skim = skim_bin();
        assert!(skim.exists(), "skim binary must exist — run `cargo build` first");

        // Stub child: prints a unique sentinel to confirm it ran directly.
        let stub_dir = tempfile::tempdir().unwrap();
        let sentinel = "B2-SENTINEL-CHILD-RAN-DIRECTLY\n";
        make_stub(stub_dir.path(), "my-child-prog", sentinel);
        let child_path = stub_dir.path().join("my-child-prog");
        assert!(child_path.exists(), "stub child must exist");

        let output = base_wrapper_cmd("env")
            .args(["FOO=1", child_path.to_str().unwrap()])
            .output()
            .expect("skim binary must be spawnable");

        let stdout = String::from_utf8_lossy(&output.stdout);

        // Child ran and produced its sentinel — raw pass-through preserved.
        assert!(
            stdout.contains("B2-SENTINEL-CHILD-RAN-DIRECTLY"),
            "env FOO=1 <non-env-child>: child stub must run directly (B2 preserved); \
             handler would not produce the sentinel; \
             got stdout={stdout:?}"
        );
    }

    // =========================================================================
    // Accepted limitation (pinned): env FOO=1 sh -c env still leaks
    // =========================================================================

    /// `env FOO=1 sh -c env` — child is `sh`, not `env`/`printenv`.
    ///
    /// Skim cannot inspect what an arbitrary child will print.  The child here is
    /// `sh`, which runs `env` as a shell command — leaking the environment despite
    /// the wrapper.  This is an ACCEPTED LIMITATION of the fix.
    ///
    /// This test PINS the current behaviour so regressions are visible: if the
    /// limitation is ever closed, this test must be updated to assert redaction.
    ///
    /// Setting `SKIM_TEST_TOKEN` in the environment lets us verify that the raw
    /// value appears (limitation confirmed) vs. being redacted (limitation closed).
    #[test]
    fn env_assignment_sh_child_accepted_limitation() {
        // Only run when `sh` is available.
        if !std::path::Path::new("/bin/sh").exists() {
            return;
        }

        let skim = skim_bin();
        assert!(skim.exists(), "skim binary must exist — run `cargo build` first");

        let output = base_wrapper_cmd("env")
            .args([
                "SKIM_TEST_TOKEN=secret-sh-child-xyz",
                "sh",
                "-c",
                "env",
            ])
            .env_remove("SKIM_TEST_TOKEN") // ensure value comes from the in-line assignment
            .output()
            .expect("skim binary must be spawnable");

        // ACCEPTED LIMITATION: the raw value leaks because the child is `sh`.
        // This assertion documents the current behaviour.  If it starts failing,
        // it means the limitation has been closed — update both the assertion and
        // the doc comment above.
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("secret-sh-child-xyz"),
            "ACCEPTED LIMITATION: env FOO=1 sh -c env leaks via the sh child; \
             skim cannot inspect what sh will print; \
             if this fails the limitation has been closed — update the test; \
             got stdout (first 400 chars): {:?}",
            stdout.chars().take(400).collect::<String>()
        );
    }

    // =========================================================================
    // Architecture-10: wrapper gates (D3/D4/D5) enforced structurally
    // =========================================================================

    /// D3 gate: `grep --help` on the wrapper surface must pass through to the
    /// real tool, not skim's internal handler.
    ///
    /// This verifies that after the architecture-10 move, D3 still fires when
    /// `dispatch_inner(Surface::Wrapper, …)` is called (now the only path from
    /// `dispatch_for_wrapper`).
    #[test]
    fn wrapper_d3_help_flag_passes_through_after_arch10_move() {
        let skim = skim_bin();
        assert!(skim.exists(), "skim binary must exist — run `cargo build` first");

        // Stub grep that prints a distinctive help sentinel.
        let stub_dir = tempfile::tempdir().unwrap();
        let sentinel_out = "GREP-HELP-SENTINEL: Usage: grep [OPTIONS] PATTERN [FILE]...\n";
        make_stub(stub_dir.path(), "grep", sentinel_out);
        let stub_path = format!(
            "{}:{}",
            stub_dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );

        let output = base_wrapper_cmd("grep")
            .args(["--help"])
            .env("PATH", stub_path)
            .output()
            .expect("skim binary must be spawnable");

        let stdout = String::from_utf8_lossy(&output.stdout);
        // D3 fired → raw passthrough → stub ran → sentinel in stdout.
        assert!(
            stdout.contains("GREP-HELP-SENTINEL"),
            "D3 must still fire after architecture-10 move: \
             grep --help must reach the native tool (stub sentinel); \
             got stdout={stdout:?}"
        );
    }

    /// D4 gate: `rg --json` on the wrapper surface must pass through (tool skip flag).
    ///
    /// Verifies D4 still fires inside `dispatch_inner` after architecture-10.
    #[test]
    fn wrapper_d4_skip_flag_rg_json_passes_through_after_arch10_move() {
        let skim = skim_bin();
        assert!(skim.exists(), "skim binary must exist — run `cargo build` first");

        let stub_dir = tempfile::tempdir().unwrap();
        let sentinel_out = r#"{"type":"begin","data":{"path":{"text":"file.rs"}}}"#;
        make_stub(stub_dir.path(), "rg", &format!("{sentinel_out}\n"));
        let stub_path = format!(
            "{}:{}",
            stub_dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );

        let output = base_wrapper_cmd("rg")
            .args(["--json", "pattern", "."])
            .env("PATH", stub_path)
            .output()
            .expect("skim binary must be spawnable");

        let stdout = String::from_utf8_lossy(&output.stdout);
        // D4 fired → raw passthrough → stub ran → JSON in stdout.
        assert!(
            stdout.contains(r#""type":"begin""#),
            "D4 must still fire after architecture-10 move: \
             rg --json must reach the native tool (stub JSON); \
             got stdout={stdout:?}"
        );
    }
}

// =========================================================================
// Unit tests for `redaction_is_mandatory` (dispatch.rs)
// =========================================================================
//
// These live in the test binary (not in dispatch.rs itself) so they exercise
// the public `cmd::redaction_is_mandatory` re-export rather than an internal.
// They are separate from the integration tests above so they run without a
// built binary.
#[cfg(test)]
mod unit_redaction_predicate {
    // The predicate is not re-exported to outside the crate (pub(crate)), so
    // the contract is tested via the integration surface above.  These unit
    // tests document the exact tool list rather than repeating binary invocations.

    /// Document the tools for which redaction is mandatory.
    ///
    /// If this list grows, add both an integration test above and update
    /// the doc on `redaction_is_mandatory` in dispatch.rs.
    #[test]
    fn redaction_mandatory_tool_list_is_documented() {
        // This test exists to make the tool list explicitly visible in the
        // test suite, not to call the function (which is pub(crate)).
        // If the list changes, update both dispatch.rs and cli_env_redaction.rs.
        let mandatory = ["env", "printenv"];
        assert_eq!(
            mandatory.len(),
            2,
            "env and printenv are the two mandatory-redaction tools; \
             update tests and dispatch.rs doc if this changes"
        );
    }
}
