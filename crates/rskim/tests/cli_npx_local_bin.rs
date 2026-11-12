//! Integration tests for `skim <tool> … --via-npx …` (skim#587).
//!
//! The flag makes skim start `npx --no-install <tool> <args>` instead of the
//! bare `<tool>`, so the run uses the project's own copy under
//! `node_modules/.bin`. These tests cover two sides:
//!
//! - the handler side: the argv of the start, the fail-closed paths, and the
//!   surfaces that must not read the flag;
//! - the hook side: which `npx <tool>` commands the PreToolUse hook rewrites
//!   (only when the project holds the tool), the exact rewritten line, and the
//!   cases where it must stay silent.
//!
//! Every test runs the built binary with a private `PATH` that holds only stub
//! programs. A stub `npx` walks up from its working directory to
//! `node_modules/.bin/<tool>` as the real `npx --no-install` does, and every
//! stub appends one line to a call log. The assertions read that log: it shows
//! which program started, with which arguments, and how often.
//!
//! The handler runs go through `common::run_bounded` (wall-clock bound,
//! process-group kill). The hook runs use the sandboxed command with a timeout.
//! No link to skim is created: the wrapper-surface test sets `argv[0]` instead
//! and names an unrelated folder as `SKIM_WRAPPERS_DIR`.
//!
//! Unix-only: stubs are shell scripts and the runs use process groups.

mod common;

#[cfg(unix)]
mod via_npx {
    use crate::common;
    use serde_json::{Value, json};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// The calls one run made, and how it ended.
    struct Outcome {
        code: Option<i32>,
        stdout: String,
        stderr: String,
        /// One line per stub call, in order: `npx …`, `local-<tool> …`,
        /// `path-<tool> …`.
        calls: Vec<String>,
    }

    /// A temp tree: sandbox home, a project with a `.git` marker, and a stub
    /// folder that is the whole `PATH` of the child.
    struct Rig {
        home: tempfile::TempDir,
    }

    impl Rig {
        fn new() -> Self {
            let rig = Self {
                home: tempfile::tempdir().expect("rig tempdir"),
            };
            // The `.git` folder ends the bin walk here, so the answer never
            // depends on what the host has above the temp folder.
            std::fs::create_dir_all(rig.project().join(".git")).expect("create project");
            std::fs::create_dir(rig.stub_dir()).expect("create stub dir");
            rig
        }

        fn project(&self) -> PathBuf {
            self.home.path().join("project")
        }

        fn stub_dir(&self) -> PathBuf {
            self.home.path().join("stubs")
        }

        fn log(&self) -> PathBuf {
            self.home.path().join("calls.log")
        }

        /// A stub `npx`: logs, insists on `--no-install`, walks up from its
        /// working folder to `node_modules/.bin/<tool>` and runs it, else
        /// prints npm-like text and fails. Shell builtins only: the child's
        /// `PATH` holds nothing else.
        fn install_npx(&self) {
            let script = format!(
                "#!/bin/sh\n\
                 printf 'npx %s\\n' \"$*\" >> '{log}'\n\
                 if [ \"$1\" != \"--no-install\" ]; then echo 'npx: --no-install is required' >&2; exit 2; fi\n\
                 shift\n\
                 tool=\"$1\"\n\
                 shift\n\
                 dir=\"$PWD\"\n\
                 while :; do\n\
                   if [ -x \"$dir/node_modules/.bin/$tool\" ]; then exec \"$dir/node_modules/.bin/$tool\" \"$@\"; fi\n\
                   [ -z \"$dir\" ] && break\n\
                   dir=\"${{dir%/*}}\"\n\
                 done\n\
                 echo 'npm error code E404' >&2\n\
                 exit 1\n",
                log = self.log().display()
            );
            common::write_stub_script(&self.stub_dir(), "npx", &script);
        }

        /// An `npx` that logs, prints npm-like text on stderr, nothing on
        /// stdout, and exits `code`.
        fn install_failing_npx(&self, code: i32) {
            let script = format!(
                "#!/bin/sh\n\
                 printf 'npx %s\\n' \"$*\" >> '{log}'\n\
                 echo 'npm error code E404' >&2\n\
                 exit {code}\n",
                log = self.log().display()
            );
            common::write_stub_script(&self.stub_dir(), "npx", &script);
        }

        /// The project's own copy of `tool`.
        fn install_local(&self, tool: &str) {
            self.install_local_in(&self.project(), tool);
        }

        /// A copy of `tool` in `dir/node_modules/.bin`.
        fn install_local_in(&self, dir: &Path, tool: &str) {
            let bin_dir = Self::bin_dir(dir);
            std::fs::create_dir_all(&bin_dir).expect("create .bin");
            self.install_logging_stub(&bin_dir, tool, "local");
        }

        fn bin_dir(dir: &Path) -> PathBuf {
            dir.join("node_modules").join(".bin")
        }

        /// A copy of `tool` on `PATH` (the "global" install).
        fn install_path_copy(&self, tool: &str) {
            self.install_logging_stub(&self.stub_dir(), tool, "path");
        }

        fn install_logging_stub(&self, dir: &std::path::Path, tool: &str, kind: &str) {
            let script = format!(
                "#!/bin/sh\nprintf '{kind}-{tool} %s\\n' \"$*\" >> '{log}'\nexit 0\n",
                log = self.log().display()
            );
            common::write_stub_script(dir, tool, &script);
        }

        /// The sandbox env block from the shared tables, a private `PATH`, and
        /// the project as the working folder.
        fn command(&self, program: &std::path::Path, args: &[&str]) -> Command {
            let home = self.home.path();
            let mut cmd = Command::new(program);
            cmd.args(args).current_dir(self.project());
            for (var, relative) in common::SANDBOX_REDIRECTED_VARS {
                cmd.env(var, common::sandbox_var_path(home, relative));
            }
            for (var, value) in common::SANDBOX_PINNED_VARS {
                cmd.env(var, value);
            }
            // Also removes SKIM_PASSTHROUGH and SKIM_DEBUG: a test that wants
            // one sets it after this.
            for var in common::SANDBOX_REMOVED_VARS {
                cmd.env_remove(var);
            }
            cmd.env("PATH", self.stub_dir())
                .env("SKIM_WRAPPERS_DIR", home.join("no-wrappers-here"));
            cmd
        }

        fn outcome(&self, cmd: Command) -> Outcome {
            let output = common::run_bounded(cmd);
            let log = std::fs::read_to_string(self.log()).unwrap_or_default();
            Outcome {
                code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                calls: log.lines().map(str::to_string).collect(),
            }
        }

        /// Run `skim <args…>`.
        fn run(&self, args: &[&str]) -> Outcome {
            self.outcome(self.command(&common::skim_bin(), args))
        }

        /// Run `skim <args…>` with `SKIM_PASSTHROUGH=1`.
        fn run_passthrough(&self, args: &[&str]) -> Outcome {
            let mut cmd = self.command(&common::skim_bin(), args);
            cmd.env("SKIM_PASSTHROUGH", "1");
            self.outcome(cmd)
        }

        /// Fire the Claude PreToolUse hook for `payload`, started in `cwd`.
        ///
        /// The sandbox block comes from `common::skim_sandboxed`; the `PATH` is
        /// the stub folder only, so a rewrite can resolve `npx` and the tools the
        /// test installs, and nothing else. Every hook run must exit 0 and write
        /// nothing to stderr.
        fn run_hook(&self, cwd: &Path, payload: &Value) -> HookRun {
            let mut cmd = common::skim_sandboxed(self.home.path());
            cmd.args(["rewrite", "--hook", "--agent", "claude-code"])
                .current_dir(cwd)
                .env("PATH", self.stub_dir())
                .write_stdin(payload.to_string())
                .timeout(common::RUN_TIMEOUT);
            let output = cmd.output().expect("the hook must run");
            let run = HookRun {
                code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            };
            assert_eq!(run.code, Some(0), "the hook must exit 0: {:?}", run.stderr);
            assert!(
                run.stderr.is_empty(),
                "the hook must write nothing to stderr: {:?}",
                run.stderr
            );
            run
        }

        /// The command the hook rewrites `command` to when the hook JSON names
        /// `cwd` and the hook process itself runs in the sandbox home (so the
        /// JSON folder, not the process folder, decides). `None` means no
        /// response: the command runs as typed.
        fn hook_line(&self, command: &str, cwd: &Path) -> Option<String> {
            let payload = pre_payload(command, Some(json!(cwd.to_str().expect("UTF-8 path"))));
            self.run_hook(self.home.path(), &payload).rewritten()
        }
    }

    /// What one hook run did.
    struct HookRun {
        code: Option<i32>,
        stdout: String,
        stderr: String,
    }

    impl HookRun {
        /// The rewritten command, or `None` when the hook sent no response.
        fn rewritten(&self) -> Option<String> {
            if self.stdout.is_empty() {
                return None;
            }
            let response: Value =
                serde_json::from_str(&self.stdout).expect("a hook response is JSON");
            let command = response
                .pointer("/hookSpecificOutput/updatedInput/command")
                .and_then(Value::as_str)
                .unwrap_or_else(|| panic!("no rewritten command in {:?}", self.stdout));
            Some(command.to_string())
        }
    }

    /// A PreToolUse payload for `command`. `cwd` is the JSON `cwd` field as
    /// given (`None` leaves the field out).
    fn pre_payload(command: &str, cwd: Option<Value>) -> Value {
        let mut payload = json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": command },
        });
        if let Some(cwd) = cwd {
            payload["cwd"] = cwd;
        }
        payload
    }

    /// One `skim` line per rewrite shape: the words before the flag, then the
    /// arguments after it. The words are what `engine.rs` emits before
    /// `--via-npx`.
    struct ToolCase {
        words: &'static [&'static str],
        rest: &'static [&'static str],
        tool: &'static str,
    }

    const CASES: &[ToolCase] = &[
        ToolCase {
            words: &["tsc"],
            rest: &["--noEmit"],
            tool: "tsc",
        },
        ToolCase {
            words: &["eslint"],
            rest: &["src"],
            tool: "eslint",
        },
        ToolCase {
            words: &["prettier", "--write"],
            rest: &["src"],
            tool: "prettier",
        },
        ToolCase {
            words: &["prettier"],
            rest: &["src"],
            tool: "prettier",
        },
        ToolCase {
            words: &["biome", "check"],
            rest: &["src"],
            tool: "biome",
        },
        ToolCase {
            words: &["oxlint"],
            rest: &["src"],
            tool: "oxlint",
        },
    ];

    impl ToolCase {
        /// `skim <words> --via-npx <rest>` as an argument list.
        fn args(&self) -> Vec<&'static str> {
            let mut args = self.words.to_vec();
            args.push("--via-npx");
            args.extend_from_slice(self.rest);
            args
        }
    }

    // ------------------------------------------------------------------------
    // AC-6: the start is `npx --no-install <tool> <prepared args>`
    // ------------------------------------------------------------------------

    /// The bare tool is never started; the project's copy runs once, through
    /// npx, even with a copy of the tool on `PATH`. The flag does not reach the
    /// tool.
    #[test]
    fn test_via_npx_runs_the_local_copy_through_npx_not_the_path_copy() {
        for case in CASES {
            let rig = Rig::new();
            rig.install_npx();
            rig.install_local(case.tool);
            rig.install_path_copy(case.tool);

            let outcome = rig.run(&case.args());

            assert_eq!(
                outcome.code,
                Some(0),
                "{:?}: stderr={:?}",
                case.words,
                outcome.stderr
            );
            assert_eq!(
                outcome.calls.len(),
                2,
                "{:?}: one npx call and one local call, no more: {:?}",
                case.words,
                outcome.calls
            );
            assert!(
                outcome.calls[0].starts_with(&format!("npx --no-install {} ", case.tool)),
                "{:?}: {:?}",
                case.words,
                outcome.calls
            );
            assert!(
                outcome.calls[1].starts_with(&format!("local-{} ", case.tool)),
                "the project's copy must run, not the PATH copy: {:?}",
                outcome.calls
            );
            assert!(
                outcome.calls.iter().all(|c| !c.contains("--via-npx")),
                "the flag must not reach npx or the tool: {:?}",
                outcome.calls
            );
        }
    }

    /// The prepared arguments follow the tool, in order: the handler's own
    /// injection (`--format json`) sits where it always sat.
    #[test]
    fn test_via_npx_keeps_the_prepared_argument_order() {
        let rig = Rig::new();
        rig.install_npx();
        rig.install_local("eslint");

        let outcome = rig.run(&["eslint", "--via-npx", "src", "lib"]);

        assert_eq!(outcome.code, Some(0), "stderr={:?}", outcome.stderr);
        assert_eq!(
            outcome.calls[0], "npx --no-install eslint --format json src lib",
            "{:?}",
            outcome.calls
        );
    }

    /// The `SKIM_PASSTHROUGH` branch starts the same program with the same
    /// shape; the arguments are the user's own.
    #[test]
    fn test_via_npx_passthrough_branch_starts_npx_no_install() {
        let rig = Rig::new();
        rig.install_npx();
        rig.install_local("eslint");

        let outcome = rig.run_passthrough(&["eslint", "--via-npx", "src"]);

        assert_eq!(outcome.code, Some(0), "stderr={:?}", outcome.stderr);
        assert_eq!(
            outcome.calls,
            vec![
                "npx --no-install eslint src".to_string(),
                "local-eslint src".to_string()
            ]
        );
    }

    /// The watch-mode branch (inherited stdio) starts the same program too.
    #[test]
    fn test_via_npx_watch_branch_starts_npx_no_install() {
        let rig = Rig::new();
        rig.install_npx();
        rig.install_local("tsc");

        let outcome = rig.run(&["tsc", "--via-npx", "--watch"]);

        assert_eq!(outcome.code, Some(0), "stderr={:?}", outcome.stderr);
        assert_eq!(
            outcome.calls,
            vec![
                "npx --no-install tsc --watch".to_string(),
                "local-tsc --watch".to_string()
            ]
        );
    }

    // ------------------------------------------------------------------------
    // AC-7: the bin is gone at start time — one error, exit 127, nothing starts
    // ------------------------------------------------------------------------

    fn assert_refused(outcome: &Outcome, what: &str) {
        assert_eq!(
            outcome.code,
            Some(127),
            "{what}: stderr={:?}",
            outcome.stderr
        );
        assert!(
            outcome.stderr.contains("error: "),
            "{what}: stderr={:?}",
            outcome.stderr
        );
        assert!(
            outcome.stderr.contains("hint: "),
            "{what}: stderr={:?}",
            outcome.stderr
        );
        assert_eq!(
            outcome.stderr.matches("error: ").count(),
            1,
            "{what}: exactly one error: {:?}",
            outcome.stderr
        );
        assert!(
            outcome.stdout.is_empty(),
            "{what}: stdout={:?}",
            outcome.stdout
        );
        assert!(
            outcome.calls.is_empty(),
            "{what}: neither npx nor the bare tool may start: {:?}",
            outcome.calls
        );
    }

    #[test]
    fn test_via_npx_with_the_local_bin_gone_refuses_on_every_branch() {
        for case in CASES {
            let rig = Rig::new();
            rig.install_npx();
            rig.install_path_copy(case.tool);
            // No project-local copy.

            let parsed = rig.run(&case.args());
            assert_refused(&parsed, &format!("parsed {:?}", case.words));
        }

        let rig = Rig::new();
        rig.install_npx();
        rig.install_path_copy("tsc");
        rig.install_path_copy("eslint");

        let passthrough = rig.run_passthrough(&["eslint", "--via-npx", "src"]);
        assert_refused(&passthrough, "SKIM_PASSTHROUGH");

        let watch = rig.run(&["tsc", "--via-npx", "--watch"]);
        assert_refused(&watch, "watch mode");
    }

    /// A bin that is present but not executable is as good as gone.
    #[test]
    fn test_via_npx_with_a_non_executable_bin_refuses() {
        let rig = Rig::new();
        rig.install_npx();
        let bin_dir = rig.project().join("node_modules").join(".bin");
        std::fs::create_dir_all(&bin_dir).expect("create .bin");
        std::fs::write(bin_dir.join("tsc"), b"#!/bin/sh\nexit 0\n").expect("write tsc");

        let outcome = rig.run(&["tsc", "--via-npx", "--noEmit"]);

        assert_refused(&outcome, "non-executable bin");
    }

    // ------------------------------------------------------------------------
    // AC-8: npx fails — npm's text raw, its exit code, no summary, one call
    // ------------------------------------------------------------------------

    #[test]
    fn test_via_npx_failure_is_forwarded_raw_with_the_exit_code() {
        for case in CASES {
            // 1 is the code eslint and others use for "findings"; 7 is not
            // expected by any parser.
            for code in [1, 7] {
                let rig = Rig::new();
                rig.install_failing_npx(code);
                rig.install_local(case.tool);

                let outcome = rig.run(&case.args());

                assert_eq!(
                    outcome.code,
                    Some(code),
                    "{:?}: stderr={:?}",
                    case.words,
                    outcome.stderr
                );
                assert_eq!(
                    outcome.stderr, "npm error code E404\n",
                    "{:?} exit {code}: npm's text, raw, nothing added",
                    case.words
                );
                assert!(
                    outcome.stdout.is_empty(),
                    "{:?} exit {code}: no parser summary: {:?}",
                    case.words,
                    outcome.stdout
                );
                assert_eq!(
                    outcome.calls.len(),
                    1,
                    "{:?} exit {code}: the command must not run twice: {:?}",
                    case.words,
                    outcome.calls
                );
            }
        }
    }

    /// With no `npx` on `PATH` the error names npx and gives no global-install
    /// hint: installing the tool globally would not help an npx start.
    #[test]
    fn test_via_npx_without_npx_names_npx_and_gives_no_install_hint() {
        for case in CASES {
            let rig = Rig::new();
            rig.install_local(case.tool);
            rig.install_path_copy(case.tool);

            let outcome = rig.run(&case.args());

            assert_ne!(outcome.code, Some(0), "{:?}", case.words);
            assert!(
                outcome.stderr.contains("'npx' not found"),
                "{:?}: stderr={:?}",
                case.words,
                outcome.stderr
            );
            assert!(
                !outcome.stderr.contains("npm install -g")
                    && !outcome.stderr.contains("Install ")
                    && !outcome.stderr.contains("hint:")
                    && !outcome.stderr.contains("Hint:"),
                "{:?}: no global-install hint: {:?}",
                case.words,
                outcome.stderr
            );
            assert!(
                outcome.calls.is_empty(),
                "{:?}: the bare tool must not start as a fallback: {:?}",
                case.words,
                outcome.calls
            );
        }
    }

    // ------------------------------------------------------------------------
    // AC-9: what must not change
    // ------------------------------------------------------------------------

    /// Typed `skim <tool>` without the flag starts the bare tool, as before.
    #[test]
    fn test_typed_skim_tool_without_the_flag_starts_the_bare_tool() {
        let rig = Rig::new();
        rig.install_npx();
        rig.install_local("tsc");
        rig.install_path_copy("tsc");

        let outcome = rig.run(&["tsc", "--noEmit"]);

        assert_eq!(outcome.code, Some(0), "stderr={:?}", outcome.stderr);
        assert_eq!(outcome.calls, vec!["path-tsc --noEmit".to_string()]);
    }

    /// Outside its one position the flag belongs to the tool.
    #[test]
    fn test_via_npx_outside_its_position_reaches_the_tool() {
        let rig = Rig::new();
        rig.install_npx();
        rig.install_local("eslint");
        rig.install_path_copy("eslint");

        let outcome = rig.run(&["eslint", "src", "--via-npx"]);

        assert_eq!(outcome.code, Some(0), "stderr={:?}", outcome.stderr);
        assert_eq!(
            outcome.calls,
            vec!["path-eslint --format json src --via-npx".to_string()],
            "no npx start, and the flag is the tool's own argument"
        );
    }

    /// The wrapper surface never reads the flag: a program named after the
    /// tool passes `--via-npx` on like any other argument and never starts npx.
    #[test]
    fn test_wrapper_surface_never_reads_the_flag() {
        use std::os::unix::process::CommandExt as _;

        let rig = Rig::new();
        rig.install_npx();
        rig.install_local("tsc");
        rig.install_path_copy("tsc");

        // `argv[0]` names the tool, so skim takes the wrapper branch. The
        // binary itself is not linked anywhere on disk.
        let mut cmd = rig.command(&common::skim_bin(), &["--via-npx", "--noEmit"]);
        cmd.arg0("tsc");
        let outcome = rig.outcome(cmd);

        assert_eq!(outcome.code, Some(0), "stderr={:?}", outcome.stderr);
        assert_eq!(
            outcome.calls,
            vec!["path-tsc --via-npx --noEmit".to_string()],
            "the wrapper surface starts the bare tool with every argument as given"
        );
    }

    // ------------------------------------------------------------------------
    // The PreToolUse hook (AC-1 to AC-5, AC-9)
    // ------------------------------------------------------------------------

    /// The node tools that have an `npx` rule.
    const NPX_TOOLS: [&str; 5] = ["tsc", "eslint", "prettier", "biome", "oxlint"];

    /// The ten `npx <tool>` rules: a typed command, and the line the hook gives
    /// when the project holds the tool. The flag sits after every word the rule
    /// rewrites to (so `skim prettier --write` and `skim biome check` keep
    /// matching a permission rule) and before the user's own arguments.
    const HOOK_RULES: &[(&str, &str)] = &[
        ("npx tsc --noEmit", "skim tsc --via-npx --noEmit"),
        ("npx eslint src", "skim eslint --via-npx src"),
        (
            "npx prettier --write src",
            "skim prettier --write --via-npx src",
        ),
        ("npx prettier -w src", "skim prettier -w --via-npx src"),
        ("npx prettier --check src", "skim prettier --via-npx src"),
        ("npx biome check src", "skim biome check --via-npx src"),
        ("npx biome format src", "skim biome format --via-npx src"),
        ("npx biome lint src", "skim biome lint --via-npx src"),
        ("npx biome ci src", "skim biome --via-npx ci src"),
        ("npx oxlint src", "skim oxlint --via-npx src"),
    ];

    /// The tool a typed `npx <tool> …` command names.
    fn tool_of(typed: &str) -> &str {
        typed.split_whitespace().nth(1).expect("npx <tool> …")
    }

    /// A rig with `npx` on `PATH` and every tool in the project's own
    /// `node_modules/.bin`.
    fn rig_with_every_local_tool() -> Rig {
        let rig = Rig::new();
        rig.install_npx();
        for tool in NPX_TOOLS {
            rig.install_local(tool);
        }
        rig
    }

    /// A folder with a `.git` marker and no tool: the walk ends there.
    fn bare_repo(rig: &Rig, name: &str) -> PathBuf {
        let dir = rig.home.path().join(name);
        std::fs::create_dir_all(dir.join(".git")).expect("create bare repo");
        dir
    }

    /// TP-1: each of the ten rules (`prettier --check` included) gives the exact
    /// line, with the flag after the tool words.
    #[test]
    fn test_hook_rewrites_each_npx_rule_with_the_flag_after_the_tool_words() {
        let rig = rig_with_every_local_tool();

        for (typed, expected) in HOOK_RULES {
            assert_eq!(
                rig.hook_line(typed, &rig.project()).as_deref(),
                Some(*expected),
                "{typed}"
            );
        }
    }

    /// TP-1, TP-6: the line the hook gives is a command skim runs, and it
    /// starts the project's copy through `npx --no-install` once. This ties the
    /// engine's flag position to the position the handler accepts.
    #[test]
    fn test_hook_line_starts_the_local_copy_through_npx_for_each_rule() {
        for (typed, _) in HOOK_RULES {
            let rig = rig_with_every_local_tool();
            rig.install_path_copy("tsc");
            let tool = tool_of(typed);

            let line = rig
                .hook_line(typed, &rig.project())
                .unwrap_or_else(|| panic!("{typed} must rewrite"));
            let args: Vec<&str> = line.split_whitespace().skip(1).collect();
            let outcome = rig.run(&args);

            assert_eq!(outcome.code, Some(0), "{line}: stderr={:?}", outcome.stderr);
            assert_eq!(outcome.calls.len(), 2, "{line}: {:?}", outcome.calls);
            assert!(
                outcome.calls[0].starts_with(&format!("npx --no-install {tool} ")),
                "{line}: {:?}",
                outcome.calls
            );
            assert!(
                outcome.calls[1].starts_with(&format!("local-{tool} ")),
                "{line}: the project's copy must run: {:?}",
                outcome.calls
            );
            assert!(
                outcome.calls.iter().all(|c| !c.contains("--via-npx")),
                "{line}: the flag must not reach npx or the tool: {:?}",
                outcome.calls
            );
        }
    }

    /// TP-2: the hook finds the bin in a parent folder up to the repository
    /// root: a hoisted root above a nested package, and a plain subfolder.
    #[test]
    fn test_hook_finds_a_bin_in_a_parent_folder_up_to_the_repo_root() {
        let rig = Rig::new();
        rig.install_npx();
        rig.install_local("tsc");
        let package = rig.project().join("packages").join("app").join("src");
        let plain = rig.project().join("docs").join("guide");
        std::fs::create_dir_all(&package).expect("create nested package");
        std::fs::create_dir_all(&plain).expect("create plain subfolder");

        for (what, cwd) in [("nested package", package), ("plain subfolder", plain)] {
            assert_eq!(
                rig.hook_line("npx tsc --noEmit", &cwd).as_deref(),
                Some("skim tsc --via-npx --noEmit"),
                "{what}"
            );
        }
    }

    /// TP-2: a bin only above the repository root gives no rewrite, because
    /// `npx` would run the root's copy only up to that point.
    #[test]
    fn test_hook_ignores_a_bin_above_the_repository_root() {
        let rig = Rig::new();
        rig.install_npx();
        // `home/node_modules/.bin/tsc` is above `home/project/.git`.
        rig.install_local_in(rig.home.path(), "tsc");
        let nested = rig.project().join("packages").join("app");
        std::fs::create_dir_all(&nested).expect("create nested package");

        for (what, cwd) in [("repo root", rig.project()), ("nested", nested)] {
            assert_eq!(
                rig.hook_line("npx tsc --noEmit", &cwd),
                None,
                "{what}: the walk ends at the repository root"
            );
        }
    }

    /// TP-3: with no executable bin in the project, the hook sends nothing and
    /// the command runs as typed. Four ways to have no usable bin.
    #[test]
    fn test_hook_stays_silent_without_an_executable_local_bin() {
        use std::os::unix::fs::PermissionsExt as _;

        let silent = |rig: &Rig, what: &str| {
            assert_eq!(
                rig.hook_line("npx tsc --noEmit", &rig.project()),
                None,
                "{what}"
            );
        };

        let rig = Rig::new();
        rig.install_npx();
        rig.install_path_copy("tsc");
        silent(&rig, "the tool is on PATH only");

        let rig = Rig::new();
        rig.install_npx();
        silent(&rig, "the tool is not installed");

        let rig = Rig::new();
        rig.install_npx();
        let bin_dir = Rig::bin_dir(&rig.project());
        std::fs::create_dir_all(&bin_dir).expect("create .bin");
        std::os::unix::fs::symlink(bin_dir.join("gone-target"), bin_dir.join("tsc"))
            .expect("create broken link");
        silent(&rig, "the bin is a broken link");

        let rig = Rig::new();
        rig.install_npx();
        let bin_dir = Rig::bin_dir(&rig.project());
        std::fs::create_dir_all(&bin_dir).expect("create .bin");
        let bin = bin_dir.join("tsc");
        std::fs::write(&bin, b"#!/bin/sh\nexit 0\n").expect("write tsc");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o644))
            .expect("drop the exec bit");
        silent(&rig, "the bin has no exec bit");
    }

    /// TP-4: the folder in the hook JSON decides, not the folder of the hook
    /// process.
    #[test]
    fn test_hook_json_cwd_decides_over_the_process_cwd() {
        let rig = Rig::new();
        rig.install_npx();
        rig.install_local("tsc");
        let empty = bare_repo(&rig, "empty");
        let cwd_of = |dir: &Path| Some(json!(dir.to_str().expect("UTF-8 path")));

        let with_bin = pre_payload("npx tsc --noEmit", cwd_of(&rig.project()));
        assert_eq!(
            rig.run_hook(&empty, &with_bin).rewritten().as_deref(),
            Some("skim tsc --via-npx --noEmit"),
            "JSON folder holds the bin, process folder does not"
        );

        let without_bin = pre_payload("npx tsc --noEmit", cwd_of(&empty));
        assert_eq!(
            rig.run_hook(&rig.project(), &without_bin).rewritten(),
            None,
            "JSON folder lacks the bin, process folder holds it"
        );
    }

    /// TP-4: a `cwd` that is relative, not a string, or a missing folder gives
    /// no npx rewrite, even when the process folder holds the bin. A rewrite
    /// that needs no folder is not affected.
    #[test]
    fn test_hook_invalid_cwd_gives_no_npx_rewrite() {
        let rig = Rig::new();
        rig.install_npx();
        rig.install_local("tsc");
        rig.install_path_copy("tsc");
        let project = rig.project();
        let missing = rig.home.path().join("absent");

        for (what, cwd) in [
            ("relative name", json!("project")),
            // "." would resolve to the process folder, which holds the bin.
            ("relative dot", json!(".")),
            ("empty string", json!("")),
            ("number", json!(42)),
            ("null", json!(null)),
            ("array", json!([project.to_str().expect("UTF-8 path")])),
            (
                "missing folder",
                json!(missing.to_str().expect("UTF-8 path")),
            ),
        ] {
            let npx = pre_payload("npx tsc --noEmit", Some(cwd.clone()));
            assert_eq!(
                rig.run_hook(&project, &npx).rewritten(),
                None,
                "{what}: no npx rewrite"
            );

            let bare = pre_payload("tsc --noEmit", Some(cwd));
            assert_eq!(
                rig.run_hook(&project, &bare).rewritten().as_deref(),
                Some("skim tsc --noEmit"),
                "{what}: a rewrite with no folder requirement still happens"
            );
        }
    }

    /// TP-4: with no `cwd` field the hook uses its own working folder.
    #[test]
    fn test_hook_absent_cwd_uses_the_process_cwd() {
        let rig = Rig::new();
        rig.install_npx();
        rig.install_local("tsc");
        let empty = bare_repo(&rig, "empty");
        let payload = pre_payload("npx tsc --noEmit", None);

        assert_eq!(
            rig.run_hook(&rig.project(), &payload)
                .rewritten()
                .as_deref(),
            Some("skim tsc --via-npx --noEmit")
        );
        assert_eq!(rig.run_hook(&empty, &payload).rewritten(), None);
    }

    /// TP-5: only the first segment of a compound can be an npx rewrite. A later
    /// `npx` segment runs in a folder the hook does not know.
    #[test]
    fn test_hook_rewrites_only_a_first_segment_npx_command() {
        let rig = Rig::new();
        rig.install_npx();
        rig.install_local("tsc");
        rig.install_path_copy("cargo");

        for typed in ["cd x && npx tsc", "echo hi && npx eslint ."] {
            assert_eq!(rig.hook_line(typed, &rig.project()), None, "{typed}");
        }
        assert_eq!(
            rig.hook_line("npx tsc && npx eslint .", &rig.project())
                .as_deref(),
            Some("skim tsc --via-npx && npx eslint ."),
            "the first segment rewrites, the second stays as typed"
        );
        assert_eq!(
            rig.hook_line("npx tsc && cargo test", &rig.project())
                .as_deref(),
            Some("skim tsc --via-npx && skim cargo test")
        );
    }

    /// AC-5: a failed probe drops the whole compound rewrite, also the segments
    /// that need no folder. The command runs as typed.
    #[test]
    fn test_hook_failed_probe_drops_the_whole_compound() {
        let rig = Rig::new();
        rig.install_npx();
        rig.install_path_copy("cargo");

        assert_eq!(rig.hook_line("npx tsc && cargo test", &rig.project()), None);
    }

    /// TP-9: what the probe must not change. A bare tool name, the test-family
    /// `npx` rules, a typed `skim` command and the auto-mode withhold behave as
    /// before, with or without a local bin.
    #[test]
    fn test_hook_rewrites_that_need_no_local_bin_are_unchanged() {
        let rig = Rig::new();
        rig.install_npx();
        rig.install_path_copy("tsc");

        for (typed, expected) in [
            ("tsc --noEmit", Some("skim tsc --noEmit")),
            ("npx vitest run", Some("skim vitest run")),
            ("skim eslint src", None),
        ] {
            assert_eq!(
                rig.hook_line(typed, &rig.project()).as_deref(),
                expected,
                "{typed}"
            );
        }
    }

    /// TP-9: in auto permission mode the hook still withholds an npx rewrite,
    /// with or without a local bin.
    #[test]
    fn test_hook_auto_mode_still_withholds_an_npx_rewrite() {
        let rig = rig_with_every_local_tool();
        let empty = bare_repo(&rig, "empty");

        for dir in [rig.project(), empty] {
            let mut payload = pre_payload(
                "npx tsc --noEmit",
                Some(json!(dir.to_str().expect("UTF-8 path"))),
            );
            payload["permission_mode"] = json!("auto");
            assert_eq!(rig.run_hook(&dir, &payload).rewritten(), None, "{dir:?}");
        }
    }

    /// TP-9: the after-run hook keys on the typed command, not on the project,
    /// so `npx eslint` output is compressed in auto mode with no local bin.
    #[test]
    fn test_post_hook_in_auto_mode_compresses_npx_eslint_without_a_local_bin() {
        let rig = Rig::new();
        rig.install_npx();

        // Nine warnings over three files, the default `eslint` text format.
        let mut spots: Vec<(String, u32)> = (0..9u32)
            .map(|n| (format!("/work/src/mod_{}.ts", n % 3), 10 + 7 * n))
            .collect();
        spots.sort();
        let mut raw = String::new();
        let mut current = "";
        for (file, line) in &spots {
            if file != current {
                raw += &format!("\n{file}\n");
                current = file;
            }
            raw += &format!(
                "  {line}:5  warning  'value' is assigned a value but never used  \
                 @typescript-eslint/no-unused-vars\n"
            );
        }
        raw += "\n✖ 9 problems (0 errors, 9 warnings)\n";

        let mut payload: Value =
            serde_json::from_str(include_str!("fixtures/hooks/post_tool_use_plain.json"))
                .expect("fixture is JSON");
        payload["tool_input"]["command"] = json!("npx eslint src/");
        payload["tool_response"]["stdout"] = json!(raw);
        payload["tool_response"]["stderr"] = json!("");

        let run = rig.run_hook(&rig.project(), &payload);

        let response: Value = serde_json::from_str(&run.stdout).expect("a compressed response");
        let after = &response["hookSpecificOutput"]["updatedToolOutput"];
        let served = after["stdout"].as_str().expect("served stdout");
        assert_eq!(
            response["hookSpecificOutput"]["hookEventName"],
            json!("PostToolUse")
        );
        assert!(served.len() < raw.len(), "{served:?}");
        for (file, line) in &spots {
            assert!(
                served.contains(&format!("{file}:{line}")),
                "the view must keep {file}:{line}: {served:?}"
            );
        }
    }

    /// TP-9: a run started with `--via-npx` is recorded under the tool name, as
    /// a run of the bare tool is. Neither `npx` nor the flag reaches the label.
    #[test]
    fn test_via_npx_run_records_the_tool_name_as_the_analytics_label() {
        let rig = Rig::new();
        rig.install_npx();
        // A local eslint that prints a real report. Shell builtins only.
        let report = rig.home.path().join("eslint-report.json");
        std::fs::write(&report, include_str!("fixtures/cmd/lint/eslint_fail.json"))
            .expect("write report");
        let bin_dir = Rig::bin_dir(&rig.project());
        std::fs::create_dir_all(&bin_dir).expect("create .bin");
        let script = format!(
            "#!/bin/sh\n\
             printf 'local-eslint %s\\n' \"$*\" >> '{log}'\n\
             while IFS= read -r line || [ -n \"$line\" ]; do printf '%s\\n' \"$line\"; done < '{report}'\n\
             exit 0\n",
            log = rig.log().display(),
            report = report.display()
        );
        common::write_stub_script(&bin_dir, "eslint", &script);

        // The sandbox turns recording off and points the database into a
        // folder skim does not create; turn it on, at a path in the rig home.
        let db = rig.home.path().join("analytics.db");
        let mut cmd = rig.command(&common::skim_bin(), &["eslint", "--via-npx", "src"]);
        cmd.env_remove("SKIM_DISABLE_ANALYTICS")
            .env("SKIM_ANALYTICS_DB", &db);
        let outcome = rig.outcome(cmd);

        assert_eq!(outcome.code, Some(0), "stderr={:?}", outcome.stderr);
        assert_eq!(outcome.calls.len(), 2, "{:?}", outcome.calls);
        let conn = rusqlite::Connection::open(&db).expect("open analytics db");
        let labels: Vec<String> = conn
            .prepare("SELECT original_cmd FROM token_savings")
            .expect("prepare")
            .query_map([], |row| row.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows");
        assert_eq!(labels.len(), 1, "one run, one row: {labels:?}");
        assert!(
            labels[0].starts_with("skim lint eslint "),
            "the label names the tool: {labels:?}"
        );
        assert!(
            !labels[0].contains("npx") && !labels[0].contains("--no-install"),
            "neither npx nor the flag is in the label: {labels:?}"
        );
    }
}
