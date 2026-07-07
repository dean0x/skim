//! Both-surfaces paired tests: hook surface vs argv0-dispatch surface.
//!
//! Verifies IDENTICAL output from the two skim interception surfaces for the
//! behaviours introduced in the agent-batch-2 wave:
//!
//! - **F3**: `ls -la d1 d2` output is sectioned on both surfaces.
//! - **F5**: grep leading-indent lines are preserved on both surfaces.
//! - **F2**: mypy synthesized success line appears on both surfaces.
//!
//! ## Surfaces under test
//!
//! 1. **Hook surface** (`skim <tool> [args]`): invoked directly as a skim
//!    subcommand.  This is what the rewrite engine produces after `skim rewrite`
//!    transforms `ls -la d1 d2` → `skim ls -la d1 d2`.
//!
//! 2. **Argv0/wrapper surface** (`arg0("<tool>")`): skim binary invoked with
//!    `argv[0]` set to the tool name, as a PATH symlink would do.
//!
//! Both surfaces route to the SAME handler but through different dispatch
//! front-ends.  Per CLAUDE.md, these are independent mechanisms — a test on one
//! does not verify the other.
//!
//! Unix-only: `arg0()` is defined on `std::os::unix::process::CommandExt`.
//!
//! F1 (git diff) already has paired tests in `cli_git.rs` — not duplicated here.

mod common;

#[cfg(unix)]
mod both_surfaces {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::process::CommandExt as _;

    /// Path to the built skim binary (same resolution as cli_wrapper_argv0.rs).
    fn skim_bin() -> std::path::PathBuf {
        if let Ok(path) = std::env::var("CARGO_BIN_EXE_skim") {
            return std::path::PathBuf::from(path);
        }
        let manifest_dir =
            std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR must be set");
        let mut p = std::path::PathBuf::from(manifest_dir);
        p.pop(); // crates/rskim → crates
        p.pop(); // crates → workspace root
        p.join("target").join("debug").join("skim")
    }

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

    // ========================================================================
    // F3: sectioned ls -la d1 d2 output
    // ========================================================================

    /// Multi-dir `ls -la` output to feed both surfaces.
    const LS_MULTI_DIR_OUTPUT: &str = "\
./d1:\n\
total 8\n\
drwxr-xr-x  2 user group  64 Jan 01 .\n\
drwxr-xr-x  3 user group  96 Jan 01 ..\n\
-rw-r--r--  1 user group 100 Jan 01 alpha.txt\n\
\n\
./d2:\n\
total 4\n\
drwxr-xr-x  2 user group  64 Jan 01 .\n\
drwxr-xr-x  3 user group  96 Jan 01 ..\n\
-rw-r--r--  1 user group 200 Jan 01 beta.txt\n";

    /// Hook surface: `skim ls -la d1 d2` via stub ls on PATH.
    /// Skim is invoked as `skim` with the `ls` subcommand.
    #[test]
    fn f3_ls_sectioned_hook_surface() {
        let stub_dir = make_stub("ls", LS_MULTI_DIR_OUTPUT, 0);
        let path = prepend_path(stub_dir.path());
        let skim = skim_bin();

        let out = std::process::Command::new(&skim)
            .args(["ls", "-la", "d1", "d2"])
            .env("PATH", &path)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("hook surface: skim ls must be spawnable");

        assert_eq!(
            out.status.code(),
            Some(0),
            "hook surface: exit must be 0; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("./d1:"),
            "hook surface: must contain d1 section header; got: {stdout}"
        );
        assert!(
            stdout.contains("./d2:"),
            "hook surface: must contain d2 section header; got: {stdout}"
        );
        assert!(
            stdout.contains("alpha.txt"),
            "hook surface: alpha.txt must appear; got: {stdout}"
        );
        assert!(
            stdout.contains("beta.txt"),
            "hook surface: beta.txt must appear; got: {stdout}"
        );
    }

    /// Argv0/wrapper surface: skim binary invoked as argv[0]="ls".
    #[test]
    fn f3_ls_sectioned_argv0_surface() {
        let stub_dir = make_stub("ls", LS_MULTI_DIR_OUTPUT, 0);
        let path = prepend_path(stub_dir.path());
        let skim = skim_bin();

        let out = std::process::Command::new(&skim)
            .arg0("ls")
            .args(["-la", "d1", "d2"])
            .env("PATH", &path)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("argv0 surface: skim-as-ls must be spawnable");

        assert_eq!(
            out.status.code(),
            Some(0),
            "argv0 surface: exit must be 0; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("./d1:"),
            "argv0 surface: must contain d1 section header; got: {stdout}"
        );
        assert!(
            stdout.contains("./d2:"),
            "argv0 surface: must contain d2 section header; got: {stdout}"
        );
        assert!(
            stdout.contains("alpha.txt"),
            "argv0 surface: alpha.txt must appear; got: {stdout}"
        );
        assert!(
            stdout.contains("beta.txt"),
            "argv0 surface: beta.txt must appear; got: {stdout}"
        );
    }

    // ========================================================================
    // F5: grep leading-indent preservation
    // ========================================================================

    /// grep output with semantically-significant leading indentation (Python).
    const GREP_INDENTED_OUTPUT: &str = "\
src/app.py:10:    def process(self):\n\
src/app.py:12:        return self.value\n";

    /// Hook surface: `skim grep` with indented output via stub.
    #[test]
    fn f5_grep_leading_indent_hook_surface() {
        let stub_dir = make_stub("grep", GREP_INDENTED_OUTPUT, 0);
        let path = prepend_path(stub_dir.path());
        let skim = skim_bin();

        let out = std::process::Command::new(&skim)
            .args(["grep", "-n", "process", "src/app.py"])
            .env("PATH", &path)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("hook surface: skim grep must be spawnable");

        let stdout = String::from_utf8_lossy(&out.stdout);
        // Leading indentation must be preserved — not trimmed to "def process".
        assert!(
            stdout.contains("    def process"),
            "hook surface: leading indent must be preserved; got: {stdout}"
        );
        assert!(
            stdout.contains("        return"),
            "hook surface: deeper indent must be preserved; got: {stdout}"
        );
    }

    /// Argv0/wrapper surface: skim invoked as argv[0]="grep".
    #[test]
    fn f5_grep_leading_indent_argv0_surface() {
        let stub_dir = make_stub("grep", GREP_INDENTED_OUTPUT, 0);
        let path = prepend_path(stub_dir.path());
        let skim = skim_bin();

        let out = std::process::Command::new(&skim)
            .arg0("grep")
            .args(["-n", "process", "src/app.py"])
            .env("PATH", &path)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("argv0 surface: skim-as-grep must be spawnable");

        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("    def process"),
            "argv0 surface: leading indent must be preserved; got: {stdout}"
        );
        assert!(
            stdout.contains("        return"),
            "argv0 surface: deeper indent must be preserved; got: {stdout}"
        );
    }

    // ========================================================================
    // F2: mypy synthesized success line
    // ========================================================================

    /// Hook surface: `skim mypy src/` with stub that exits 0 + empty stdout.
    /// Expects synthesized "mypy OK 0 issues" line (R3/synthesize_success_line).
    #[test]
    fn f2_mypy_success_line_hook_surface() {
        // Stub mypy: exits 0, empty stdout.
        let stub_dir = make_stub("mypy", "", 0);
        let path = prepend_path(stub_dir.path());
        let skim = skim_bin();

        let out = std::process::Command::new(&skim)
            .args(["mypy", "src/"])
            .env("PATH", &path)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("hook surface: skim mypy must be spawnable");

        assert_eq!(
            out.status.code(),
            Some(0),
            "hook surface: mypy with no issues must exit 0; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("mypy OK"),
            "hook surface: synthesized success line must appear; got: {stdout}"
        );
    }

    /// Argv0/wrapper surface: skim invoked as argv[0]="mypy".
    #[test]
    fn f2_mypy_success_line_argv0_surface() {
        let stub_dir = make_stub("mypy", "", 0);
        let path = prepend_path(stub_dir.path());
        let skim = skim_bin();

        let out = std::process::Command::new(&skim)
            .arg0("mypy")
            .args(["src/"])
            .env("PATH", &path)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_DEBUG")
            .output()
            .expect("argv0 surface: skim-as-mypy must be spawnable");

        assert_eq!(
            out.status.code(),
            Some(0),
            "argv0 surface: mypy with no issues must exit 0; stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("mypy OK"),
            "argv0 surface: synthesized success line must appear; got: {stdout}"
        );
    }
}
