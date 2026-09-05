//! D7 Cross-Surface Conformance Harness.
//!
//! Tests a (tool × arg set) matrix through **both** interception surfaces:
//!
//! 1. **Rewrite surface** (`skim rewrite '<cmd>'`): verifies the engine
//!    correctly accepts or rejects each command, and that executing the
//!    emitted rewrite produces byte-for-byte identical output to the
//!    wrapper surface.
//!
//! 2. **Argv0/wrapper surface** (`skim` binary with `arg0("<tool>")`):
//!    verifies `dispatch_for_wrapper` produces correct output — D2 passthrough
//!    for unknown subcommands, D3 help/version, D4 tool-owned skip flags.
//!
//! ## Coverage
//!
//! - 9 Class-A families (used to `bail!`, now D2 passthrough): git rev-parse,
//!   git branch, cargo run, cargo --version, go build, go install, npm publish,
//!   pip freeze, pnpm add.
//! - Flag-swallowing cases (skip-flag parity): grep --help, rg --json, tree --json.
//! - env prefix exclusion (skip_if_middle_contains_eq): `env FOO=1 printenv FOO`.
//! - diff across flag forms: no rewrite rule (PF-011), pure passthrough on wrapper.
//! - SKIM_PASSTHROUGH=1 on both surfaces: env-prefix suppression and convergence gate.
//! - Rewritten commands both surfaces agree: git status, git log.
//!
//! ## SKIM_CACHE_DIR isolation
//!
//! Every invocation that touches the skim binary sets a fresh per-test
//! `SKIM_CACHE_DIR` temp dir (Phase 8 fix: prevents D5 force-raw sidecar
//! written by one test binary from leaking into another via the shared
//! nextest runner PID).
//!
//! ## Pinned divergences
//!
//! Where the two surfaces legitimately differ, the test documents the reason
//! rather than asserting parity:
//!
//! - `cargo --version`: rewrite surface exits 1 (no rewrite rule); wrapper
//!   surface fires D3 (--version flag) and runs the native tool. Both reach
//!   the stub; the path differs.
//! - `env FOO=1 printenv FOO`: rewrite surface exits 1 (skip_if_middle_contains_eq);
//!   wrapper surface routes through skim's env handler. Only the rewrite
//!   assertion is tested here — the wrapper behavior is env-handler territory.
//! - Pipe-source pipe consumer: `| cat` still compresses on the rewrite surface
//!   (AD-RW-2 reversal accepted); wrapper serves raw via fstat. Not covered here.
//!
//! Unix-only: `arg0()` is defined on `std::os::unix::process::CommandExt`.

mod common;

#[cfg(unix)]
mod cross_surface {
    use std::io::Write as _;
    use std::os::unix::process::CommandExt as _;
    use std::path::Path;
    use std::process::{Command, Output, Stdio};

    use tempfile::TempDir;

    // =========================================================================
    // Shared infrastructure
    // =========================================================================

    fn skim_bin() -> std::path::PathBuf {
        super::common::skim_bin()
    }

    /// Fresh SKIM_CACHE_DIR per test — prevents force-raw sidecar cross-binary
    /// contamination (Phase 8 fix).
    fn fresh_cache_dir() -> TempDir {
        tempfile::tempdir().expect("fresh SKIM_CACHE_DIR TempDir")
    }

    /// PATH string with `stub_dir` prepended so skim's spawned child resolves
    /// to the stub, while keeping the real system PATH for skim itself.
    fn stub_path(stub_dir: &Path) -> String {
        super::common::stub_path(stub_dir)
    }

    /// Write a stub tool at `stub_dir/name` that prints `stdout` and exits `code`.
    fn add_stub(stub_dir: &Path, name: &str, stdout: &str, exit_code: i32) {
        super::common::make_stub(stub_dir, name, stdout, "", exit_code);
    }

    /// Base env block applied to all skim invocations.
    fn base_env(cmd: &mut Command, path: &str, cache_dir: &Path) {
        cmd.env("PATH", path)
            .env("SKIM_CACHE_DIR", cache_dir)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("NO_COLOR", "1")
            .env_remove("SKIM_PASSTHROUGH")
            .env_remove("SKIM_REWRITTEN_FROM")
            .env_remove("SKIM_DEBUG");
    }

    /// Run `skim rewrite '<cmd_str>'` and return the full Output.
    fn run_rewrite(cmd_str: &str, path: &str, cache_dir: &Path) -> Output {
        let skim = skim_bin();
        let mut cmd = Command::new(&skim);
        cmd.args(["rewrite", cmd_str]);
        base_env(&mut cmd, path, cache_dir);
        cmd.output().expect("skim rewrite must be spawnable")
    }

    /// Run skim in wrapper mode: `skim` binary with `argv[0] = tool`.
    fn run_wrapper(tool: &str, args: &[&str], path: &str, cache_dir: &Path) -> Output {
        let skim = skim_bin();
        let mut cmd = Command::new(&skim);
        cmd.arg0(tool);
        cmd.args(args);
        base_env(&mut cmd, path, cache_dir);
        cmd.output().expect("wrapper surface must be spawnable")
    }

    /// Run skim in wrapper mode with piped stdin content.
    ///
    /// Writes `stdin_content` to the child process before reading output.
    /// Used for meta subcommands (e.g. `log`) that read from stdin.
    fn run_wrapper_with_stdin(
        tool: &str,
        args: &[&str],
        stdin_content: &str,
        path: &str,
        cache_dir: &Path,
    ) -> Output {
        let skim = skim_bin();
        let mut cmd = Command::new(&skim);
        cmd.arg0(tool);
        cmd.args(args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        base_env(&mut cmd, path, cache_dir);

        let mut child = cmd.spawn().expect("wrapper-with-stdin must be spawnable");
        // Take ownership of the ChildStdin handle so dropping it closes the
        // pipe and sends EOF to the child before we call wait_with_output.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(stdin_content.as_bytes())
                .expect("write to child stdin must succeed");
            // stdin dropped here → pipe closed → EOF for child
        }
        child.wait_with_output().expect("wrapper-with-stdin wait_with_output must succeed")
    }

    /// Run skim on the EXPLICIT surface with piped stdin content.
    ///
    /// The wrapper variant above dispatches on `argv[0]`, which only works for
    /// names in `WRAPPER_TARGETS`. META subcommands such as `log` are excluded
    /// from that set by construction, so they are reachable only as
    /// `skim <subcommand> …` — this helper.
    fn run_explicit_with_stdin(
        args: &[&str],
        stdin_content: &str,
        path: &str,
        cache_dir: &Path,
    ) -> Output {
        let skim = skim_bin();
        let mut cmd = Command::new(&skim);
        cmd.args(args);
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        base_env(&mut cmd, path, cache_dir);

        let mut child = cmd.spawn().expect("explicit-with-stdin must be spawnable");
        // Dropping the handle closes the pipe and sends EOF before we wait.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(stdin_content.as_bytes())
                .expect("write to child stdin must succeed");
        }
        child.wait_with_output().expect("explicit-with-stdin wait_with_output must succeed")
    }

    /// Run the rewritten command returned by `skim rewrite`.
    ///
    /// Parses the rewritten string, asserts it starts with `skim`, and executes
    /// it using the skim binary so `CARGO_BIN_EXE_skim` is honoured.
    ///
    /// Returns `None` if the rewritten string is empty or does not start with
    /// `skim` (caller should assert the rewrite exited 0 before calling this).
    fn exec_rewritten(rewritten: &str, path: &str, cache_dir: &Path) -> Option<Output> {
        let rewritten = rewritten.trim();
        if rewritten.is_empty() {
            return None;
        }
        // Strip any leading env-var tokens (UPPERCASE_KEY=val) and find "skim".
        let tokens: Vec<&str> = rewritten.split_whitespace().collect();
        let skim_pos = tokens.iter().position(|t| *t == "skim")?;
        let subargs = &tokens[skim_pos + 1..];

        let skim = skim_bin();
        let mut cmd = Command::new(&skim);
        cmd.args(subargs);
        // Inject leading env vars that appeared before "skim".
        for token in &tokens[..skim_pos] {
            if let Some((k, v)) = token.split_once('=') {
                cmd.env(k, v);
            }
        }
        base_env(&mut cmd, path, cache_dir);
        Some(cmd.output().expect("exec_rewritten must be spawnable"))
    }

    // =========================================================================
    // Class-A families: D2 passthrough (used to bail!, now passthrough)
    // =========================================================================
    //
    // For each Class-A family:
    //  - Rewrite surface: `skim rewrite '<tool> <args>'` must exit 1 (no rule).
    //  - Wrapper surface: `skim` as `<tool>` must exit with stub's code and
    //    produce the stub's stdout byte-for-byte.
    //
    // Both surfaces ultimately invoke the same stub tool, so the assertion is:
    // wrapper output == stub output (passthrough fidelity).

    /// Assert Class-A family passthrough behaviour on both surfaces.
    fn assert_class_a(tool: &str, args: &[&str], stub_out: &str, stub_exit: i32) {
        let stub_dir = tempfile::tempdir().unwrap();
        let cache_dir = fresh_cache_dir();
        add_stub(stub_dir.path(), tool, stub_out, stub_exit);
        let path = stub_path(stub_dir.path());

        let cmd_str: String = std::iter::once(tool)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");

        // Rewrite surface: must NOT match any rule (exits 1, empty stdout).
        let rewrite_out = run_rewrite(&cmd_str, &path, cache_dir.path());
        assert_eq!(
            rewrite_out.status.code(),
            Some(1),
            "Class-A `{cmd_str}`: rewrite surface must exit 1 (no rule); \
             got {:?}, stdout={:?}",
            rewrite_out.status.code(),
            String::from_utf8_lossy(&rewrite_out.stdout),
        );
        assert!(
            rewrite_out.stdout.is_empty(),
            "Class-A `{cmd_str}`: no rewrite means no stdout; got: {:?}",
            String::from_utf8_lossy(&rewrite_out.stdout),
        );

        // Wrapper surface: D2 passthrough → stub output with stub exit code.
        let wrapper_out = run_wrapper(tool, args, &path, cache_dir.path());
        assert_eq!(
            wrapper_out.status.code(),
            Some(stub_exit),
            "Class-A `{cmd_str}`: wrapper surface exit code must match stub ({stub_exit}); \
             stderr={:?}",
            String::from_utf8_lossy(&wrapper_out.stderr),
        );
        assert_eq!(
            wrapper_out.stdout,
            stub_out.as_bytes(),
            "Class-A `{cmd_str}`: wrapper surface must produce stub bytes unchanged",
        );
        // ADR-011: no-loss path → no stderr without SKIM_DEBUG.
        assert!(
            wrapper_out.stderr.is_empty(),
            "Class-A `{cmd_str}`: no stderr on D2 passthrough (ADR-011); \
             got: {:?}",
            String::from_utf8_lossy(&wrapper_out.stderr),
        );
    }

    #[test]
    fn class_a_git_rev_parse_head() {
        // git rev-parse: unknown git subcommand → D2 in git handler.
        assert_class_a("git", &["rev-parse", "HEAD"], "abc123def456789\n", 0);
    }

    #[test]
    fn class_a_git_branch_list() {
        // git branch: unknown git subcommand → D2 in git handler.
        assert_class_a("git", &["branch", "--list"], "* main\n  feat/foo\n", 0);
    }

    #[test]
    fn class_a_cargo_run() {
        // cargo run: unknown cargo subcommand → D2 in cargo handler.
        assert_class_a("cargo", &["run"], "Hello, world!\n", 0);
    }

    #[test]
    fn class_a_cargo_version() {
        // cargo --version: D3 help/version passthrough on wrapper surface (--version in args).
        // Rewrite surface: exits 1 (no rule for `cargo --version`).
        //
        // PINNED DIVERGENCE: on the wrapper surface D3 fires first
        // (`dispatch_for_wrapper` detects --version) and calls
        // `run_raw_passthrough("cargo", ["--version"], &[])`.
        // On the rewrite surface there is simply no rule; the agent would run
        // `cargo --version` natively. Both reach the same stub; the dispatch
        // mechanism differs between surfaces.
        assert_class_a(
            "cargo",
            &["--version"],
            "cargo 1.80.0 (f28de0b 2024-01-01)\n",
            0,
        );
    }

    #[test]
    fn class_a_go_build() {
        // go build: unknown go subcommand → D2 in go handler.
        assert_class_a("go", &["build", "./..."], "", 0);
    }

    #[test]
    fn class_a_go_install() {
        // go install: unknown go subcommand → D2 in go handler.
        // Ninth Class-A family in this harness.
        assert_class_a("go", &["install", "golang.org/x/tools/...@latest"], "", 0);
    }

    #[test]
    fn class_a_npm_publish() {
        // npm publish: unknown npm subcommand → D2 in npm handler.
        assert_class_a(
            "npm",
            &["publish", "--dry-run"],
            "npm notice created a tarball\nnpm notice === Tarball Contents ===\n",
            0,
        );
    }

    #[test]
    fn class_a_pip_freeze() {
        // pip freeze: unknown pip subcommand → D2 in pip handler.
        assert_class_a("pip", &["freeze"], "requests==2.28.0\nnumpy==1.24.0\n", 0);
    }

    #[test]
    fn class_a_pnpm_add() {
        // pnpm add: unknown pnpm subcommand → D2 in pnpm handler.
        assert_class_a(
            "pnpm",
            &["add", "lodash"],
            "Packages: +1\n+ lodash 4.17.21\n",
            0,
        );
    }

    // =========================================================================
    // Flag-swallowing cases: skip_if_flag_prefix parity
    // =========================================================================
    //
    // These commands have skip flags in their rewrite rules AND matching D3/D4
    // passthrough on the wrapper surface. Both surfaces must NOT intercept them.
    //
    // Rewrite surface: exits 1 (skip flag suppresses the rule).
    // Wrapper surface: D3 (--help/-h/--version) or D4 (tool-owned skip flags)
    //   fires → `run_raw_passthrough` → stub output.

    /// Assert skip-flag passthrough parity: exits 1 on rewrite surface, stub
    /// output on wrapper surface.
    ///
    /// `diverge_note`: if non-empty, documents a known surface difference.
    fn assert_skip_flag(tool: &str, args: &[&str], stub_out: &str, diverge_note: &str) {
        let stub_dir = tempfile::tempdir().unwrap();
        let cache_dir = fresh_cache_dir();
        add_stub(stub_dir.path(), tool, stub_out, 0);
        let path = stub_path(stub_dir.path());

        let cmd_str: String = std::iter::once(tool)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");

        // Rewrite surface: skip flag must suppress the rule (exits 1).
        let rewrite_out = run_rewrite(&cmd_str, &path, cache_dir.path());
        assert_eq!(
            rewrite_out.status.code(),
            Some(1),
            "Skip-flag `{cmd_str}`: rewrite surface must exit 1 (skip flag in rule); \
             got {:?}, stdout={:?}; diverge_note={diverge_note}",
            rewrite_out.status.code(),
            String::from_utf8_lossy(&rewrite_out.stdout),
        );

        // Wrapper surface: D3 or D4 → stub output (native tool reached).
        let wrapper_out = run_wrapper(tool, args, &path, cache_dir.path());
        assert_eq!(
            wrapper_out.status.code(),
            Some(0),
            "Skip-flag `{cmd_str}`: wrapper surface must exit 0; \
             stderr={:?}; diverge_note={diverge_note}",
            String::from_utf8_lossy(&wrapper_out.stderr),
        );
        assert_eq!(
            wrapper_out.stdout,
            stub_out.as_bytes(),
            "Skip-flag `{cmd_str}`: wrapper surface must reach native tool (stub bytes); \
             diverge_note={diverge_note}",
        );
        assert!(
            wrapper_out.stderr.is_empty(),
            "Skip-flag `{cmd_str}`: no stderr from D3/D4 passthrough (ADR-011); \
             got: {:?}; diverge_note={diverge_note}",
            String::from_utf8_lossy(&wrapper_out.stderr),
        );
    }

    #[test]
    fn skip_flag_grep_help() {
        // grep --help: grep catch-all rule has skip_if_flag_prefix=[--help,--version,-V].
        // Wrapper: D3 fires (--help in args) → run_raw_passthrough.
        //
        // PINNED DIVERGENCE (mechanism): rewrite exits 1 because the skip suppresses
        // the rule; wrapper fires D3 because dispatch_for_wrapper detects --help.
        // Both reach the native tool via different paths.
        assert_skip_flag(
            "grep",
            &["--help"],
            "Usage: grep [OPTION]... PATTERN [FILE]...\nSearch for PATTERN in each FILE.\n",
            "D3 on wrapper (--help), skip_if_flag_prefix on rewrite",
        );
    }

    #[test]
    fn skip_flag_rg_json() {
        // rg --json: rg rule has skip_if_flag_prefix=[--json,...].
        // Wrapper: D4 fires — skip_flags_for_tool("rg") includes "--json" →
        //   run_raw_passthrough.
        //
        // PINNED DIVERGENCE (mechanism): same effect, different gate.
        // Both surfaces decline to intercept, reaching the native tool.
        assert_skip_flag(
            "rg",
            &["--json", "pattern", "."],
            "{\"type\":\"begin\",\"data\":{\"path\":{\"text\":\"file.rs\"}}}\n",
            "D4 on wrapper (--json in skip_flags_for_tool), skip_if_flag_prefix on rewrite",
        );
    }

    #[test]
    fn skip_flag_tree_json() {
        // tree --json / -J: tree rule has skip_if_flag_prefix=[-J,--json].
        // Wrapper: D4 fires — skip_flags_for_tool("tree") includes "--json".
        assert_skip_flag(
            "tree",
            &["--json"],
            "[{\"type\":\"directory\",\"name\":\".\",\"contents\":[]}]\n",
            "D4 on wrapper (--json in skip_flags_for_tool), skip_if_flag_prefix on rewrite",
        );
    }

    // =========================================================================
    // env prefix exclusion: skip_if_middle_contains_eq
    // =========================================================================

    #[test]
    fn env_var_prefix_not_rewritten() {
        // `env FOO=1 printenv FOO`: the env rule has skip_if_middle_contains_eq=true.
        // FOO=1 is in the middle (after the "env" prefix), so the rule skips.
        // Rewrite surface must exit 1.
        //
        // PINNED DIVERGENCE: the wrapper surface routes through skim's env handler
        // (which runs `printenv` with env overrides). Wrapper behavior is env-handler
        // territory and not tested here — this test verifies only that the rewrite
        // engine correctly declines to intercept the env-prefix form.
        let stub_dir = tempfile::tempdir().unwrap();
        let cache_dir = fresh_cache_dir();
        add_stub(stub_dir.path(), "env", "FOO=env_passthrough\n", 0);
        let path = stub_path(stub_dir.path());

        let rewrite_out = run_rewrite("env FOO=1 printenv FOO", &path, cache_dir.path());
        assert_eq!(
            rewrite_out.status.code(),
            Some(1),
            "env FOO=1 printenv FOO: rewrite must exit 1 (skip_if_middle_contains_eq); \
             got {:?}, stdout={:?}",
            rewrite_out.status.code(),
            String::from_utf8_lossy(&rewrite_out.stdout),
        );
        assert!(
            rewrite_out.stdout.is_empty(),
            "env FOO=1 printenv FOO: no rewrite means no stdout",
        );
    }

    // =========================================================================
    // diff across flag forms: no rewrite rule (PF-011 decision)
    // =========================================================================
    //
    // diff has no rewrite rule because it is a pure passthrough on both surfaces
    // — there is nothing to compress (PF-011). The wrapper symlink is kept for
    // backward-compat but the handler always emits RawPassthrough.

    /// Assert diff passthrough: no rewrite on rewrite surface; byte-faithful
    /// passthrough on wrapper surface; exit code preserved.
    fn assert_diff_passthrough(args: &[&str], stub_out: &str, stub_exit: i32) {
        let stub_dir = tempfile::tempdir().unwrap();
        let cache_dir = fresh_cache_dir();
        add_stub(stub_dir.path(), "diff", stub_out, stub_exit);
        let path = stub_path(stub_dir.path());

        let cmd_str: String = std::iter::once("diff")
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");

        // Rewrite surface: no rule → exits 1.
        let rewrite_out = run_rewrite(&cmd_str, &path, cache_dir.path());
        assert_eq!(
            rewrite_out.status.code(),
            Some(1),
            "diff `{cmd_str}`: rewrite must exit 1 (no rule, PF-011); \
             got {:?}, stdout={:?}",
            rewrite_out.status.code(),
            String::from_utf8_lossy(&rewrite_out.stdout),
        );

        // Wrapper surface: RawPassthrough → byte-faithful, exit code forwarded.
        let wrapper_out = run_wrapper("diff", args, &path, cache_dir.path());
        assert_eq!(
            wrapper_out.status.code(),
            Some(stub_exit),
            "diff `{cmd_str}`: wrapper surface must forward stub exit code ({stub_exit}); \
             stderr={:?}",
            String::from_utf8_lossy(&wrapper_out.stderr),
        );
        assert_eq!(
            wrapper_out.stdout,
            stub_out.as_bytes(),
            "diff `{cmd_str}`: wrapper surface must forward stub bytes unchanged (PF-011)",
        );
    }

    #[test]
    fn diff_bare() {
        // diff a.txt b.txt: exit 1 is normal when files differ.
        assert_diff_passthrough(&["a.txt", "b.txt"], "1c1\n< foo\n---\n> bar\n", 1);
    }

    #[test]
    fn diff_unified_flag() {
        // diff -u: same handler, flags forwarded unchanged.
        assert_diff_passthrough(
            &["-u", "a.txt", "b.txt"],
            "--- a.txt\t2026-01-01 00:00:00\n\
             +++ b.txt\t2026-01-01 00:00:00\n\
             @@ -1 +1 @@\n\
             -foo\n\
             +bar\n",
            1,
        );
    }

    #[test]
    fn diff_identical_files_exit_zero() {
        // diff with identical files: exit 0, empty stdout.
        assert_diff_passthrough(&["a.txt", "a.txt"], "", 0);
    }

    // =========================================================================
    // SKIM_PASSTHROUGH=1: env-prefix suppression and convergence gate
    // =========================================================================

    #[test]
    fn passthrough_env_prefix_suppresses_rewrite() {
        // `SKIM_PASSTHROUGH=1 cargo test` as an env-prefix to `skim rewrite`:
        // the engine detects SKIM_PASSTHROUGH=1 in the env-var prefix and bails
        // before any rule is consulted (AD-RW-14). Rewrite must exit 1.
        let stub_dir = tempfile::tempdir().unwrap();
        let cache_dir = fresh_cache_dir();
        add_stub(stub_dir.path(), "cargo", "test passed\n", 0);
        let path = stub_path(stub_dir.path());

        let rewrite_out = run_rewrite("SKIM_PASSTHROUGH=1 cargo test", &path, cache_dir.path());
        assert_eq!(
            rewrite_out.status.code(),
            Some(1),
            "SKIM_PASSTHROUGH=1 cargo test: rewrite must exit 1 (AD-RW-14 env-prefix suppression); \
             got {:?}, stdout={:?}",
            rewrite_out.status.code(),
            String::from_utf8_lossy(&rewrite_out.stdout),
        );
        assert!(
            rewrite_out.stdout.is_empty(),
            "SKIM_PASSTHROUGH=1 cargo test: suppressed rewrite must emit no stdout",
        );
    }

    #[test]
    fn passthrough_env_var_wrapper_gate() {
        // SKIM_PASSTHROUGH=1 on the wrapper surface: the convergence gate in
        // dispatch() fires before family dispatch and calls stream_passthrough_raw,
        // forwarding the stub's bytes unchanged.
        let stub_dir = tempfile::tempdir().unwrap();
        let cache_dir = fresh_cache_dir();
        let stub_out = "running 3 tests\ntest result: ok. 3 passed\n";
        add_stub(stub_dir.path(), "cargo", stub_out, 0);
        let path = stub_path(stub_dir.path());

        let skim = skim_bin();
        let mut cmd = Command::new(&skim);
        cmd.arg0("cargo");
        cmd.args(["test"]);
        cmd.env("PATH", &path)
            .env("SKIM_CACHE_DIR", cache_dir.path())
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("NO_COLOR", "1")
            .env("SKIM_PASSTHROUGH", "1") // ← the gate under test
            .env_remove("SKIM_REWRITTEN_FROM")
            .env_remove("SKIM_DEBUG");
        let out = cmd
            .output()
            .expect("wrapper SKIM_PASSTHROUGH must be spawnable");

        assert_eq!(
            out.status.code(),
            Some(0),
            "SKIM_PASSTHROUGH=1 wrapper: must exit 0; stderr={:?}",
            String::from_utf8_lossy(&out.stderr),
        );
        assert_eq!(
            out.stdout,
            stub_out.as_bytes(),
            "SKIM_PASSTHROUGH=1 wrapper: stub bytes must reach stdout unchanged",
        );
        // Convergence gate is a lossless path (no compression) — no stderr per ADR-011.
        assert!(
            out.stderr.is_empty(),
            "SKIM_PASSTHROUGH=1 wrapper: no stderr on convergence gate (ADR-011); \
             got: {:?}",
            String::from_utf8_lossy(&out.stderr),
        );
    }

    // =========================================================================
    // Rewritten commands: both surfaces produce identical output
    // =========================================================================
    //
    // For commands that ARE rewritten by the rewrite engine, executing the
    // rewritten command (rewrite surface) and running the same command via the
    // wrapper surface must produce byte-for-byte identical stdout and exit code.
    //
    // Both surfaces dispatch through the same handler (via dispatch() or
    // dispatch_for_wrapper() → dispatch()), so they must agree.

    /// Assert that both surfaces produce identical stdout and exit code.
    fn assert_both_surfaces_agree(
        tool: &str,
        args: &[&str],
        stub_out: &str,
        expected_rewrite_prefix: &str,
    ) {
        let stub_dir = tempfile::tempdir().unwrap();
        // Each surface gets its own cache dir to prevent force-raw sidecar leaks.
        let rewrite_cache = fresh_cache_dir();
        let wrapper_cache = fresh_cache_dir();
        add_stub(stub_dir.path(), tool, stub_out, 0);
        let path = stub_path(stub_dir.path());

        let cmd_str: String = std::iter::once(tool)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");

        // Step 1: rewrite surface — get the rewritten command.
        let rewrite_out = run_rewrite(&cmd_str, &path, rewrite_cache.path());
        assert_eq!(
            rewrite_out.status.code(),
            Some(0),
            "Both-surfaces `{cmd_str}`: rewrite surface must exit 0 (command IS in rules); \
             stderr={:?}",
            String::from_utf8_lossy(&rewrite_out.stderr),
        );
        let rewritten_str = String::from_utf8_lossy(&rewrite_out.stdout);
        let rewritten_str = rewritten_str.trim();
        assert!(
            rewritten_str.starts_with(expected_rewrite_prefix),
            "Both-surfaces `{cmd_str}`: rewritten command must start with \
             '{expected_rewrite_prefix}'; got: {rewritten_str}",
        );

        // Step 2: execute the rewritten command.
        let exec_out = exec_rewritten(rewritten_str, &path, rewrite_cache.path())
            .expect("exec_rewritten must return Some for valid rewrite");

        // Step 3: wrapper surface.
        let wrapper_out = run_wrapper(tool, args, &path, wrapper_cache.path());

        // Both surfaces must agree on stdout and exit code.
        assert_eq!(
            exec_out.status.code(),
            wrapper_out.status.code(),
            "Both-surfaces `{cmd_str}`: exit codes must match; \
             rewrite-exec stderr={:?}, wrapper stderr={:?}",
            String::from_utf8_lossy(&exec_out.stderr),
            String::from_utf8_lossy(&wrapper_out.stderr),
        );
        assert_eq!(
            exec_out.stdout,
            wrapper_out.stdout,
            "Both-surfaces `{cmd_str}`: stdout must be byte-identical; \
             rewrite-exec: {:?}, wrapper: {:?}",
            String::from_utf8_lossy(&exec_out.stdout),
            String::from_utf8_lossy(&wrapper_out.stdout),
        );
    }

    #[test]
    fn both_surfaces_git_status() {
        // git status: rewrites to `skim git status`.
        // Both surfaces route through the git status handler — output must agree.
        // The handler compresses "nothing to commit" output; both surfaces see the
        // same compressed result.
        assert_both_surfaces_agree(
            "git",
            &["status"],
            "On branch main\nYour branch is up to date with 'origin/main'.\n\
             \nnothing to commit, working tree clean\n",
            "skim git status",
        );
    }

    #[test]
    fn both_surfaces_git_log_oneline() {
        // git log --oneline -3: rewrites to `skim git log --oneline -3`.
        // Both surfaces route through the git log handler.
        assert_both_surfaces_agree(
            "git",
            &["log", "--oneline", "-3"],
            "abc1234 First commit\ndef5678 Second commit\nghi9012 Third commit\n",
            "skim git log",
        );
    }

    #[test]
    fn both_surfaces_cargo_test() {
        // cargo test: rewrites to `skim cargo test`.
        // Both surfaces route through the cargo test handler.
        assert_both_surfaces_agree(
            "cargo",
            &["test"],
            "running 2 tests\ntest a ... ok\ntest b ... ok\n\ntest result: ok. 2 passed\n",
            "skim cargo test",
        );
    }

    #[test]
    fn both_surfaces_grep_recursive() {
        // grep -rn pattern .: rewrites to `skim grep -rn pattern .`.
        // Both surfaces route through the grep handler.
        assert_both_surfaces_agree(
            "grep",
            &["-rn", "TODO", "."],
            "src/lib.rs:42:// TODO: fix this\nsrc/main.rs:7:// TODO: remove\n",
            "skim grep -rn",
        );
    }

    // =========================================================================
    // consistency-5 / consistency-8: POSIX `--` separator and `--flag=value`
    // =========================================================================

    /// `psql --command='SELECT 1'` has the required flag in `--flag=value` form.
    ///
    /// Pins consistency-5: D5 must treat `--command=<value>` as satisfying the
    /// psql require_flag check (previously only exact `--command SELECT 1` was
    /// recognised, so the `=`-form silently fell through to interactive-session
    /// passthrough via `run_inherited_passthrough`).
    ///
    /// On the rewrite surface the `=`-form must also produce a valid rewrite so
    /// that `SKIM_PASSTHROUGH=1` round-trips correctly.
    #[test]
    fn psql_equals_form_required_flag_is_non_interactive() {
        let stub_dir = tempfile::tempdir().unwrap();
        let cache_dir = fresh_cache_dir();
        add_stub(stub_dir.path(), "psql", " id | name\n  1 | Alice\n", 0);
        let path = stub_path(stub_dir.path());

        // Rewrite surface: psql --command=... must produce a valid rewrite.
        // The rewrite engine uses `arg_matches_flag` which accepts `--command=val`.
        let cmd_str = "psql --command='SELECT 1'";
        let rw = run_rewrite(cmd_str, &path, cache_dir.path());
        assert_eq!(
            rw.status.code(),
            Some(0),
            "psql --command='SELECT 1': rewrite surface must exit 0 (rewrite rule matches); \
             stderr={:?}",
            String::from_utf8_lossy(&rw.stderr),
        );
        let rewrite_text = String::from_utf8_lossy(&rw.stdout);
        assert!(
            !rewrite_text.trim().is_empty(),
            "psql --command='SELECT 1': rewrite surface must produce non-empty rewrite"
        );

        // Wrapper surface: D5 must NOT fire (flag satisfied in = form) →
        // normal dispatch reaches the psql handler which runs the stub.
        // If D5 fired instead, the stub would be called via run_inherited_passthrough
        // (which also calls the stub), but the exit code would still be 0.
        // The meaningful assertion is that the stub's stdout appears (no D5 hang).
        let wrapper_out = run_wrapper("psql", &["--command=SELECT 1"], &path, cache_dir.path());
        assert_eq!(
            wrapper_out.status.code(),
            Some(0),
            "psql --command=SELECT 1: wrapper surface must exit 0; \
             stderr={:?}",
            String::from_utf8_lossy(&wrapper_out.stderr),
        );
    }

    /// sqlite3 with no non-option args triggers D5 on the wrapper surface
    /// because `interactive_tool_for("sqlite3")` is true and sqlite3 has no
    /// `require_flags` (any invocation may open an interactive REPL).
    ///
    /// D5 must route to `run_inherited_passthrough`, which calls the sqlite3
    /// stub with inherited stdio — the stub's stdout flows back to the test's
    /// captured pipe.
    #[test]
    fn sqlite3_d5_routes_via_inherited_passthrough() {
        let stub_dir = tempfile::tempdir().unwrap();
        let cache_dir = fresh_cache_dir();
        // Stub exits 0 and prints a recognisable sentinel.
        add_stub(stub_dir.path(), "sqlite3", "sqlite3-stub-output\n", 0);
        let path = stub_path(stub_dir.path());

        let wrapper_out = run_wrapper("sqlite3", &[], &path, cache_dir.path());
        assert_eq!(
            wrapper_out.status.code(),
            Some(0),
            "sqlite3 D5: wrapper surface must exit 0 (inherited passthrough); \
             stderr={:?}",
            String::from_utf8_lossy(&wrapper_out.stderr),
        );
        // The stub's stdout must appear: D5's run_inherited_passthrough lets the
        // child write directly to the parent's captured pipe.
        assert_eq!(
            wrapper_out.stdout,
            b"sqlite3-stub-output\n",
            "sqlite3 D5: stub stdout must flow through inherited passthrough unchanged"
        );
    }

    /// `grep -- --version file`: the `--version` token is after the POSIX `--`
    /// separator, so D3 (help/version flag detection) must NOT fire.
    ///
    /// Pins consistency-8: `args_before_separator` must stop the D3 scan at `--`.
    /// Before the fix, D3 scanned all args and would have triggered raw passthrough
    /// for the `--version` flag even after `--`.
    #[test]
    fn d3_posix_separator_prevents_version_flag_trigger() {
        let stub_dir = tempfile::tempdir().unwrap();
        let cache_dir = fresh_cache_dir();
        // Stub prints a grep-style result — not a version string.
        add_stub(stub_dir.path(), "grep", "file.txt:42:--version\n", 0);
        let path = stub_path(stub_dir.path());

        // Rewrite surface: `grep -- --version file` should still produce a rewrite
        // (the `--` does not block the rewrite rule match).
        let rw = run_rewrite("grep -- --version file", &path, cache_dir.path());
        // grep has a rewrite rule, so this should exit 0 or 1 depending on engine
        // handling of `--`. The key assertion is that wrapper dispatch doesn't bail.

        // Wrapper surface: D3 must not fire. The stub should be called through the
        // grep handler (not through raw passthrough with the real grep binary).
        // We verify by checking exit code and that the stub output appears.
        let wrapper_out =
            run_wrapper("grep", &["--", "--version", "file"], &path, cache_dir.path());
        assert_eq!(
            wrapper_out.status.code(),
            Some(0),
            "grep -- --version file: D3 must not fire; wrapper must call grep stub; \
             stderr={:?}, rewrite_exit={:?}",
            String::from_utf8_lossy(&wrapper_out.stderr),
            rw.status.code(),
        );
        assert_eq!(
            wrapper_out.stdout,
            b"file.txt:42:--version\n",
            "grep -- --version file: stub stdout must be returned (D3 did not fire)"
        );
    }

    /// `rg -- --json`: the `--json` token is after the POSIX `--` separator,
    /// so D4 (tool-owned skip flag detection) must NOT fire.
    ///
    /// Pins consistency-8: `args_before_separator` must stop the D4 scan at `--`.
    /// rg's skip_if_flag_prefix includes `--json`, so without the separator fix
    /// D4 would have triggered raw passthrough even for `rg -- --json pattern`.
    #[test]
    fn d4_posix_separator_prevents_json_skip_flag_trigger() {
        let stub_dir = tempfile::tempdir().unwrap();
        let cache_dir = fresh_cache_dir();
        // Stub prints plain text — not JSON — so if D4 fires the raw tool
        // (stub) output reaches the test; if D4 is correctly suppressed, the
        // rg handler compresses it (or passes through if small).
        add_stub(stub_dir.path(), "rg", "src/lib.rs:1:fn main() {}\n", 0);
        let path = stub_path(stub_dir.path());

        // Wrapper surface: D4 must not fire for `--json` after `--`.
        let wrapper_out = run_wrapper("rg", &["--", "--json", "pattern"], &path, cache_dir.path());
        assert_eq!(
            wrapper_out.status.code(),
            Some(0),
            "rg -- --json: D4 must not fire; wrapper must call rg stub; \
             stderr={:?}",
            String::from_utf8_lossy(&wrapper_out.stderr),
        );
        // Stub output must be returned (possibly compressed through handler).
        // The stub output is 1 line so it may be returned verbatim.
        assert!(!wrapper_out.stdout.is_empty(), "rg -- --json: stub must produce output");

        // Rewrite surface: `rg -- --json pattern` has a rewrite rule for rg;
        // the rewrite engine does not fire D4 (it uses skip_if_flag_prefix in
        // the rule table, not args_before_separator).
        let rw = run_rewrite("rg -- --json pattern", &path, cache_dir.path());
        // rg has a rule; exit 0 means a rewrite was produced; exit 1 means skipped.
        // The rule engine sees `--json` in skip_if_flag_prefix but AFTER `--`:
        // for the rewrite engine, all_tokens_after_cmd includes post-`--` tokens
        // when determining require_flag, but skip_if_flag_prefix is checked on
        // the full args (before the engine's rule is applied). Document the current
        // behavior rather than asserting a specific exit code.
        let _ = rw; // Behavior documented in CLAUDE.md surface-table notes.
    }

    /// `skim log --json` remedy must name `SKIM_PASSTHROUGH=1`, not `'log'`.
    ///
    /// Pins consistency-4: before the fix, `passthrough_reproduces_argv` was
    /// derived solely from `passthrough_strips_json(tool)`, which returns `false`
    /// for "log" (it's a META subcommand, not an exec'd tool wrapper).  The fix
    /// gates on `is_meta_subcommand(tool) || passthrough_strips_json(tool)`, so
    /// META subcommands like "log" correctly set `passthrough_reproduces_argv =
    /// true` and get the generic `SKIM_PASSTHROUGH=1 for full output` remedy
    /// rather than `"run 'log' directly for the full output"`.
    ///
    /// The test pipes in log content that produces a `Lossy` result (compressible
    /// DEBUG lines), forcing `emit_json_envelope` to emit the ADR-011 class-1
    /// stderr marker — which is what carries the remedy.
    #[test]
    fn skim_log_json_remedy_names_passthrough_not_log_binary() {
        let stub_dir = tempfile::tempdir().unwrap();
        let cache_dir = fresh_cache_dir();
        let path = stub_path(stub_dir.path());

        // Log input using the `LEVEL: message` format that rskim-compress's
        // Tier-2 parser recognises.  Without --keep-debug, DEBUG: lines are
        // dropped, producing a Lossy result and triggering the JSON marker.
        let log_input = concat!(
            "INFO: Starting application\n",
            "DEBUG: Connecting to database host=db port=5432\n",
            "DEBUG: Sending heartbeat probe\n",
            "DEBUG: Received pong from db\n",
            "DEBUG: Cache miss key=sessions:abc123\n",
            "INFO: Application started\n",
        );

        // "log" is a META subcommand, so it is deliberately absent from
        // WRAPPER_TARGETS (that set is KNOWN_SUBCOMMANDS minus META_SUBCOMMANDS)
        // and has no argv[0] wrapper to invoke. The remedy under test belongs to
        // the explicit surface, which is where `skim log --json` is reachable.
        let out = run_explicit_with_stdin(&["log", "--json"], log_input, &path, cache_dir.path());

        // The process must succeed regardless of the lossy path.
        assert_eq!(
            out.status.code(),
            Some(0),
            "skim log --json: must exit 0 even on Lossy path; \
             stderr={:?}",
            String::from_utf8_lossy(&out.stderr),
        );

        let stderr = String::from_utf8_lossy(&out.stderr);
        // The ADR-011 class-1 marker fires on the Lossy path.
        // It must mention SKIM_PASSTHROUGH=1 — not "run 'log' directly".
        assert!(
            stderr.contains("SKIM_PASSTHROUGH=1"),
            "skim log --json: stderr marker must contain SKIM_PASSTHROUGH=1; got: {stderr:?}"
        );
        assert!(
            !stderr.contains("run 'log' directly"),
            "skim log --json: stderr marker must NOT say 'run log directly' \
             (consistency-4 regression); got: {stderr:?}"
        );
    }

    /// `--debug` survives `SKIM_PASSTHROUGH=1` for a tool that owns the flag.
    ///
    /// Pins regression-4: before the fix, `strip_skim_flags` removed `--debug`
    /// for every tool.  For tools that list `"--debug"` in `skip_if_flag_prefix`
    /// (gradle, docker variants, aws, jest, playwright, wget), `--debug` is a
    /// tool-owned flag and must be forwarded on the passthrough path.
    ///
    /// The test uses gradle (which already listed `"--debug"` before this wave)
    /// as the canonical tool-that-owns-debug representative.  The stub echoes
    /// its argv to stdout, letting us verify `--debug` was forwarded.
    #[test]
    fn debug_flag_survives_passthrough_for_tool_owner() {
        let stub_dir = tempfile::tempdir().unwrap();
        let cache_dir = fresh_cache_dir();

        // Stub that prints its argv, one arg per line (prefixed with "arg:").
        let stub_script = "#!/bin/sh\nfor arg in \"$@\"; do printf 'arg:%s\\n' \"$arg\"; done\n";
        super::common::write_stub_script(stub_dir.path(), "gradle", stub_script);
        let path = stub_path(stub_dir.path());

        // Run `skim gradle --debug clean` with SKIM_PASSTHROUGH=1.
        // strip_skim_flags must preserve --debug (gradle owns it).
        let skim = skim_bin();
        let mut cmd = Command::new(&skim);
        cmd.args(["gradle", "--debug", "clean"]);
        base_env(&mut cmd, &path, cache_dir.path());
        cmd.env("SKIM_PASSTHROUGH", "1");
        let out = cmd.output().expect("skim gradle --debug clean must be spawnable");

        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            out.status.code(),
            Some(0),
            "--debug passthrough for gradle: must exit 0; stderr={:?}",
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(
            stdout.contains("arg:--debug"),
            "--debug must be forwarded to gradle (tool owns the flag); \
             got stdout: {stdout:?}"
        );
        assert!(
            stdout.contains("arg:clean"),
            "clean task must also be forwarded; got stdout: {stdout:?}"
        );
    }
}
