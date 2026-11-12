//! Integration test for the flag zone on the PATH-wrapper surface.
//!
//! Under a `<wrappers-dir>/<tool>` link, `argv[0]` names the wrapped tool, so
//! every argument belongs to that tool.  `skim`'s own flags (`--passthrough`,
//! `--debug`) are read only from a `skim`-typed command line; on this surface
//! `SKIM_PASSTHROUGH=1` and `SKIM_DEBUG=1` are the controls.
//!
//! `main()` picks the zone rule from `wrapper_tool_from_argv0()`.  The pure zone
//! functions have unit tests in `main.rs`, but only a run through a real link
//! proves that `main()` takes the wrapper branch.  Each test below runs the
//! built binary through a link named for a wrapped tool.  A stub of the same
//! name records the argv it receives.  The assertion is that the stub sees the
//! argv unchanged and that skim prints no startup line on stderr.
//!
//! ## Process safety
//!
//! A link to skim in a directory that `SKIM_WRAPPERS_DIR` does not name is never
//! stripped from `PATH`.  When skim spawns the real tool, the name then
//! resolves to the link itself, and the chain of processes grows without bound.
//! Therefore:
//!
//! - Every link sits inside the one directory that `SKIM_WRAPPERS_DIR` names.
//! - The child has a private `PATH`: that directory, then the stub directory.
//! - Every run has a wall-clock bound.  On expiry the whole process group of
//!   the child is killed and the test fails.
//!
//! Unix-only: the links, the stubs and the process group need unix APIs.

mod common;

#[cfg(unix)]
mod wrapper_flag_zone {
    use crate::common;
    use std::path::PathBuf;
    use std::process::Command;

    /// Marker line the stub writes after the arguments of one call.
    const CALL_END: &str = "call-end";

    /// Prefix of each recorded argument line.
    const ARG_PREFIX: &str = "arg:";

    /// What one child run produced.
    struct Outcome {
        code: Option<i32>,
        stderr: String,
        /// One entry per call of the stub, in call order.
        stub_calls: Vec<Vec<String>>,
    }

    /// A temp tree holding the wrappers dir, the stub dir and the sandbox home.
    struct Rig {
        home: tempfile::TempDir,
        tool: String,
    }

    impl Rig {
        /// Create the tree for `tool`: a link to skim in the wrappers dir, and
        /// a stub of the same name in the stub dir.
        fn new(tool: &str) -> Self {
            let rig = Self {
                home: tempfile::tempdir().expect("rig tempdir"),
                tool: tool.to_string(),
            };
            std::fs::create_dir(rig.wrappers_dir()).expect("create wrappers dir");
            std::fs::create_dir(rig.stub_dir()).expect("create stub dir");

            // The only place a link to skim is ever created: the directory that
            // `SKIM_WRAPPERS_DIR` names for the child.
            std::os::unix::fs::symlink(common::skim_bin(), rig.link_path())
                .expect("link skim into the wrappers dir");

            let script = format!(
                "#!/bin/sh\n\
                 for a in \"$@\"; do printf '{ARG_PREFIX}%s\\n' \"$a\"; done >> '{log}'\n\
                 printf '{CALL_END}\\n' >> '{log}'\n\
                 exit 0\n",
                log = rig.stub_log().display()
            );
            common::write_stub_script(&rig.stub_dir(), tool, &script);
            rig
        }

        fn wrappers_dir(&self) -> PathBuf {
            self.home.path().join("wrappers")
        }

        fn stub_dir(&self) -> PathBuf {
            self.home.path().join("stubs")
        }

        fn link_path(&self) -> PathBuf {
            self.wrappers_dir().join(&self.tool)
        }

        fn stub_log(&self) -> PathBuf {
            self.stub_dir().join(format!("{}.calls", self.tool))
        }

        /// Private `PATH`: the wrappers dir first, as on a real install, then
        /// the stub dir.  Nothing else is reachable.
        fn private_path(&self) -> String {
            format!(
                "{}:{}",
                self.wrappers_dir().display(),
                self.stub_dir().display()
            )
        }

        /// The sandbox env block, from the shared tables, with the two
        /// overrides this surface needs.
        fn command(&self, args: &[&str]) -> Command {
            let home = self.home.path();
            let mut cmd = Command::new(self.link_path());
            cmd.args(args).current_dir(home);
            for (var, relative) in common::SANDBOX_REDIRECTED_VARS {
                cmd.env(var, common::sandbox_var_path(home, relative));
            }
            for (var, value) in common::SANDBOX_PINNED_VARS {
                cmd.env(var, value);
            }
            // Also removes SKIM_PASSTHROUGH and SKIM_DEBUG: neither may decide
            // the outcome.
            for var in common::SANDBOX_REMOVED_VARS {
                cmd.env_remove(var);
            }
            cmd.env("SKIM_WRAPPERS_DIR", self.wrappers_dir())
                .env("PATH", self.private_path());
            cmd
        }

        fn parse_stub_calls(&self) -> Vec<Vec<String>> {
            let log = std::fs::read_to_string(self.stub_log()).unwrap_or_default();
            let mut calls = Vec::new();
            let mut current = Vec::new();
            for line in log.lines() {
                if line == CALL_END {
                    calls.push(std::mem::take(&mut current));
                } else if let Some(arg) = line.strip_prefix(ARG_PREFIX) {
                    current.push(arg.to_string());
                }
            }
            calls
        }

        /// Run the link with `args` and collect what happened.
        fn run(&self, args: &[&str]) -> Outcome {
            let output = common::run_bounded(self.command(args));
            Outcome {
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                stub_calls: self.parse_stub_calls(),
            }
        }
    }

    /// Run `<wrappers-dir>/<tool> <args…>` and assert the wrapped tool received
    /// exactly `args`, once, and that skim itself stayed silent on stderr.
    fn assert_reaches_tool_unchanged(tool: &str, args: &[&str]) {
        let rig = Rig::new(tool);
        let outcome = rig.run(args);

        let expected: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
        assert_eq!(
            outcome.stub_calls,
            vec![expected],
            "`{tool} {args:?}` must reach the wrapped tool once, argv unchanged; \
             stderr={:?}",
            outcome.stderr
        );
        assert!(
            !outcome.stderr.contains("[skim] "),
            "`{tool} {args:?}` must not switch skim's debug output on; \
             stderr={:?}",
            outcome.stderr
        );
        assert_eq!(
            outcome.code,
            Some(0),
            "the stub's exit code must propagate; stderr={:?}",
            outcome.stderr
        );
    }

    // ------------------------------------------------------------------------
    // After a positional.  `docker run img app --passthrough` hands
    // `--passthrough` to the container command.  If `main()` took the
    // `skim`-typed rule on this surface, skim would enter passthrough mode and
    // delete the token, or print its startup line for `--debug`.
    // ------------------------------------------------------------------------

    #[test]
    fn wrapper_forwards_passthrough_after_a_positional() {
        assert_reaches_tool_unchanged("docker", &["run", "img", "app", "--passthrough"]);
    }

    #[test]
    fn wrapper_does_not_enable_debug_for_a_token_after_a_positional() {
        assert_reaches_tool_unchanged("docker", &["run", "img", "app", "--debug"]);
    }

    // ------------------------------------------------------------------------
    // Value of a tool flag.  `grep -e --passthrough f` searches for the pattern
    // `--passthrough`.  A rule that scans the leading run of flags removes the
    // pattern, and grep then reads its pattern `f` from stdin.
    // ------------------------------------------------------------------------

    #[test]
    fn wrapper_forwards_a_pattern_named_passthrough() {
        assert_reaches_tool_unchanged("grep", &["-e", "--passthrough", "f"]);
    }

    #[test]
    fn wrapper_does_not_enable_debug_for_a_pattern_named_debug() {
        assert_reaches_tool_unchanged("grep", &["-e", "--debug", "f"]);
    }

    // ------------------------------------------------------------------------
    // Leading position.  No argument on this surface is read as a skim flag:
    // `SKIM_PASSTHROUGH=1` and `SKIM_DEBUG=1` are the controls.
    // ------------------------------------------------------------------------

    #[test]
    fn wrapper_forwards_a_leading_passthrough_to_the_tool() {
        assert_reaches_tool_unchanged("grep", &["--passthrough", "pattern", "f"]);
    }

    #[test]
    fn wrapper_does_not_enable_debug_for_a_leading_debug_token() {
        assert_reaches_tool_unchanged("docker", &["--debug", "run", "img", "app"]);
    }
}
