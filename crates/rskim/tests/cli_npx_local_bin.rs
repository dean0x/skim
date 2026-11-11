//! Integration tests for `skim <tool> … --via-npx …` (skim#587).
//!
//! The flag makes skim start `npx --no-install <tool> <args>` instead of the
//! bare `<tool>`, so the run uses the project's own copy under
//! `node_modules/.bin`. These tests cover the handler side: the argv of the
//! start, the fail-closed paths, and the surfaces that must not read the flag.
//!
//! Every test runs the built binary with a private `PATH` that holds only stub
//! programs. A stub `npx` walks up from its working directory to
//! `node_modules/.bin/<tool>` as the real `npx --no-install` does, and every
//! stub appends one line to a call log. The assertions read that log: it shows
//! which program started, with which arguments, and how often.
//!
//! The runs go through `common::run_bounded` (wall-clock bound, process-group
//! kill). No link to skim is created: the wrapper-surface test sets `argv[0]`
//! instead and names an unrelated folder as `SKIM_WRAPPERS_DIR`.
//!
//! Unix-only: stubs are shell scripts and the runs use process groups.

mod common;

#[cfg(unix)]
mod via_npx {
    use crate::common;
    use std::path::PathBuf;
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
            let bin_dir = self.project().join("node_modules").join(".bin");
            std::fs::create_dir_all(&bin_dir).expect("create .bin");
            self.install_logging_stub(&bin_dir, tool, "local");
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
}
