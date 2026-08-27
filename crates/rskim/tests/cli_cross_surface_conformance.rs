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
    use std::os::unix::process::CommandExt as _;
    use std::process::{Command, Output};
    use std::path::Path;

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
        assert_class_a("cargo", &["--version"], "cargo 1.80.0 (f28de0b 2024-01-01)\n", 0);
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
        assert_class_a("pnpm", &["add", "lodash"], "Packages: +1\n+ lodash 4.17.21\n", 0);
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
    fn assert_skip_flag(
        tool: &str,
        args: &[&str],
        stub_out: &str,
        diverge_note: &str,
    ) {
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
        assert_diff_passthrough(
            &["a.txt", "b.txt"],
            "1c1\n< foo\n---\n> bar\n",
            1,
        );
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

        let rewrite_out =
            run_rewrite("SKIM_PASSTHROUGH=1 cargo test", &path, cache_dir.path());
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
            .env("SKIM_PASSTHROUGH", "1")       // ← the gate under test
            .env_remove("SKIM_REWRITTEN_FROM")
            .env_remove("SKIM_DEBUG");
        let out = cmd.output().expect("wrapper SKIM_PASSTHROUGH must be spawnable");

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
    fn assert_both_surfaces_agree(tool: &str, args: &[&str], stub_out: &str, expected_rewrite_prefix: &str) {
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
            exec_out.stdout, wrapper_out.stdout,
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
}
