//! Integration tests for a skim link or copy that sits OUTSIDE the wrappers
//! folder.
//!
//! `skim init --wrappers` puts its links in one folder, and `SKIM_WRAPPERS_DIR`
//! names it.  A link or copy of skim under a wrapped tool's name anywhere else
//! on `PATH` is not removed from `PATH`.  When skim then spawns the real tool,
//! the name used to resolve to that link, and skim started itself without end.
//!
//! The contract under test, for a start whose `argv[0]` names a wrapped tool:
//!
//! - every `PATH` folder whose `<tool>` is the running binary moves to the end
//!   of `PATH`, so the real tool is found first and the other programs of that
//!   folder still resolve;
//! - when the first `<tool>` on the new `PATH` is still the running binary, skim
//!   exits 126 before it starts any process;
//! - a copy of the binary is another file, so a wrapper started by a wrapper of
//!   the same tool exits 126 as well;
//! - a chain of more than 32 skim processes exits 126 on any surface.
//!
//! ## Process safety
//!
//! These tests start links to skim outside the wrappers folder, which is the
//! shape that used to fork without limit.  Therefore:
//!
//! - Every child has a private `PATH`.  No folder of the host is reachable.
//! - `SKIM_WRAPPERS_DIR` names an unrelated folder.
//! - Every run goes through `common::run_bounded`: a wall-clock bound, and a kill
//!   of the whole process group on expiry.
//! - The pipes stay small.  `run_bounded` reads them after the child exits.
//!
//! Unix-only: the links, the stubs and the process group need unix APIs.

mod common;

#[cfg(unix)]
mod wrapper_path_loop {
    use crate::common;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// The wrapped tool every case uses.  `git status` starts git twice per skim
    /// level: once to probe and once for the parsed run.
    const TOOL: &str = "git";

    /// A second real program that lives in the same folder as the link.  The
    /// real-tool stub calls it, so a run proves the folder is still on `PATH`.
    const OTHER: &str = "zzother";

    /// Exit status of a refused start.
    const REFUSED: i32 = 126;

    /// What one child run produced.
    struct Outcome {
        code: Option<i32>,
        stderr: String,
        /// The lines the stubs wrote, in order.
        calls: Vec<String>,
    }

    /// A temp tree: the sandbox home, a folder for links, a folder for the real
    /// tool, and an unrelated folder for `SKIM_WRAPPERS_DIR`.
    struct Rig {
        home: tempfile::TempDir,
    }

    impl Rig {
        fn new() -> Self {
            let rig = Self {
                home: tempfile::tempdir().expect("rig tempdir"),
            };
            for dir in [rig.links(), rig.second_links(), rig.real(), rig.wrappers()] {
                std::fs::create_dir(dir).expect("create rig folder");
            }
            rig
        }

        fn links(&self) -> PathBuf {
            self.home.path().join("links")
        }

        fn second_links(&self) -> PathBuf {
            self.home.path().join("links2")
        }

        /// Holds the real-tool stub.
        fn real(&self) -> PathBuf {
            self.home.path().join("real")
        }

        /// The unrelated folder `SKIM_WRAPPERS_DIR` names.  No link lives here.
        fn wrappers(&self) -> PathBuf {
            self.home.path().join("wrappers")
        }

        fn log(&self) -> PathBuf {
            self.home.path().join("calls.log")
        }

        fn link_skim(&self, dir: &Path, name: &str) {
            std::os::unix::fs::symlink(common::skim_bin(), dir.join(name))
                .expect("link skim into the rig");
        }

        fn copy_skim(&self, dir: &Path, name: &str) {
            std::fs::copy(common::skim_bin(), dir.join(name)).expect("copy skim into the rig");
        }

        /// The real tool: a stub `git` in `real()` that calls `zzother`, and the
        /// `zzother` program in `links()`.  Each writes one line to the log.
        fn install_real_tool(&self) {
            let log = self.log();
            common::write_stub_script(
                &self.links(),
                OTHER,
                &format!("#!/bin/sh\nprintf '{OTHER}\\n' >> '{}'\n", log.display()),
            );
            common::write_stub_script(
                &self.real(),
                TOOL,
                &format!(
                    "#!/bin/sh\n{OTHER} || exit 97\nprintf '{TOOL}\\n' >> '{}'\nexit 0\n",
                    log.display()
                ),
            );
        }

        /// A private `PATH` made of exactly `dirs`.
        fn path_of(dirs: &[PathBuf]) -> OsString {
            std::env::join_paths(dirs).expect("PATH entries hold no separator")
        }

        /// The sandbox env block from the shared tables, a private `PATH`, and an
        /// unrelated `SKIM_WRAPPERS_DIR`.
        fn command(&self, program: &Path, path: &OsString, args: &[&str]) -> Command {
            let home = self.home.path();
            let mut cmd = Command::new(program);
            cmd.args(args).current_dir(home);
            for (var, relative) in common::SANDBOX_REDIRECTED_VARS {
                cmd.env(var, common::sandbox_var_path(home, relative));
            }
            for (var, value) in common::SANDBOX_PINNED_VARS {
                cmd.env(var, value);
            }
            for var in common::SANDBOX_REMOVED_VARS {
                cmd.env_remove(var);
            }
            cmd.env("SKIM_WRAPPERS_DIR", self.wrappers())
                .env("PATH", path);
            cmd
        }

        fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.log())
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }

        fn outcome(&self, output: &std::process::Output) -> Outcome {
            Outcome {
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                calls: self.calls(),
            }
        }

        /// Run `program status` with `path`, bounded.
        fn run_status(&self, program: &Path, path: &OsString) -> Outcome {
            let output = common::run_bounded(self.command(program, path, &["status"]));
            self.outcome(&output)
        }
    }

    /// The run reached the real tool through a skim start that stood in for
    /// `TOOL`: the stub ran, and every stub run found `zzother` first.
    fn assert_reached_real_tool(outcome: &Outcome) {
        let expected_pair = [OTHER.to_string(), TOOL.to_string()];
        assert!(
            !outcome.calls.is_empty()
                && outcome.calls.len() <= 4
                && outcome.calls.len().is_multiple_of(2)
                && outcome
                    .calls
                    .chunks(2)
                    .all(|pair| pair == expected_pair.as_slice()),
            "`{TOOL} status` must start the real tool (at most twice, one skim level), \
             and the tool must find {OTHER} in the folder of the link; calls={:?} stderr={:?}",
            outcome.calls,
            outcome.stderr
        );
        assert_eq!(
            outcome.code,
            Some(0),
            "the stub's exit code must propagate; stderr={:?}",
            outcome.stderr
        );
    }

    /// The run was refused with exit 126 and the one-line message, before any
    /// stub ran.
    fn assert_refused(outcome: &Outcome, cause: &str) {
        assert_eq!(
            outcome.code,
            Some(REFUSED),
            "must exit 126; stderr={:?}",
            outcome.stderr
        );
        let message = format!("error: {TOOL}: {cause}");
        let lines: Vec<&str> = outcome
            .stderr
            .lines()
            .filter(|l| l.starts_with(&message))
            .collect();
        assert!(
            !lines.is_empty(),
            "stderr must carry the refusal line starting {message:?}; stderr={:?}",
            outcome.stderr
        );
        assert!(
            lines[0].contains(&format!(
                "remove it or run the real {TOOL} by its full path"
            )),
            "the refusal line must say how to cure the cause: {:?}",
            lines[0]
        );
        assert!(
            outcome.calls.is_empty(),
            "no real tool may start; calls={:?}",
            outcome.calls
        );
    }

    /// A copy started a copy of the same tool and the chain stopped there.  The
    /// second start refuses with the one-line message.  The first start reports
    /// a failed `git status` and hands that line on, so its own status is not 0.
    fn assert_copy_chain_stopped(outcome: &Outcome) {
        assert!(
            matches!(outcome.code, Some(code) if code != 0),
            "the failed `{TOOL} status` must not exit 0; stderr={:?}",
            outcome.stderr
        );
        assert!(
            outcome
                .stderr
                .contains(&format!("error: {TOOL}: a skim named {TOOL} started it; ")),
            "stderr must carry the refusal line; stderr={:?}",
            outcome.stderr
        );
        assert!(
            !outcome.stderr.contains("Resource temporarily unavailable"),
            "the chain reached a process limit; stderr={:?}",
            outcome.stderr
        );
    }

    // ------------------------------------------------------------------------
    // A link outside the wrappers folder reaches the real tool.
    // ------------------------------------------------------------------------

    #[test]
    fn link_outside_the_wrappers_folder_reaches_the_real_tool() {
        let rig = Rig::new();
        rig.install_real_tool();
        rig.link_skim(&rig.links(), TOOL);
        let path = Rig::path_of(&[rig.links(), rig.real()]);

        let outcome = rig.run_status(&rig.links().join(TOOL), &path);

        assert_reached_real_tool(&outcome);
    }

    #[test]
    fn link_folder_spelled_with_dot_dot_reaches_the_real_tool() {
        let rig = Rig::new();
        rig.install_real_tool();
        rig.link_skim(&rig.links(), TOOL);
        // The same folder, spelled so that no string comparison matches it.
        let spelled = rig.real().join("..").join("links");
        let path = Rig::path_of(&[spelled, rig.real()]);

        let outcome = rig.run_status(&rig.links().join(TOOL), &path);

        assert_reached_real_tool(&outcome);
    }

    #[test]
    fn copy_of_the_binary_in_front_of_the_real_tool_reaches_the_real_tool() {
        let rig = Rig::new();
        rig.install_real_tool();
        rig.copy_skim(&rig.links(), TOOL);
        let path = Rig::path_of(&[rig.links(), rig.real()]);

        let outcome = rig.run_status(&rig.links().join(TOOL), &path);

        assert_reached_real_tool(&outcome);
    }

    // ------------------------------------------------------------------------
    // A start that could only start skim again is refused.
    // ------------------------------------------------------------------------

    #[test]
    fn link_with_no_real_tool_is_refused_before_any_start() {
        let rig = Rig::new();
        rig.link_skim(&rig.links(), TOOL);
        let path = Rig::path_of(&[rig.links()]);

        let outcome = rig.run_status(&rig.links().join(TOOL), &path);

        assert_refused(
            &outcome,
            &format!("the first {TOOL} on PATH is skim itself"),
        );
    }

    #[test]
    fn two_copies_of_the_binary_and_no_real_tool_stop_at_once() {
        let rig = Rig::new();
        rig.copy_skim(&rig.links(), TOOL);
        rig.copy_skim(&rig.second_links(), TOOL);
        let path = Rig::path_of(&[rig.links(), rig.second_links()]);

        let outcome = rig.run_status(&rig.links().join(TOOL), &path);

        assert_copy_chain_stopped(&outcome);
    }

    /// The test process stands in for the parent wrapper, so the refused start
    /// itself exits 126.
    #[test]
    fn wrapper_started_by_a_wrapper_of_the_same_tool_exits_126() {
        let rig = Rig::new();
        rig.install_real_tool();
        rig.copy_skim(&rig.second_links(), TOOL);
        let path = Rig::path_of(&[rig.second_links(), rig.real(), rig.links()]);
        let mut cmd = rig.command(&rig.second_links().join(TOOL), &path, &["status"]);
        cmd.env(
            "SKIM_WRAPPER_PARENT",
            format!("{TOOL}:{}", std::process::id()),
        );

        let outcome = rig.outcome(&common::run_bounded(cmd));

        assert_refused(&outcome, &format!("a skim named {TOOL} started it"));
    }

    #[test]
    fn wrapper_with_a_marker_of_another_tool_or_pid_starts_the_real_tool() {
        let rig = Rig::new();
        rig.install_real_tool();
        rig.copy_skim(&rig.links(), TOOL);
        let path = Rig::path_of(&[rig.links(), rig.real()]);
        let markers = [
            format!("grep:{}", std::process::id()),
            format!("{TOOL}:{}", std::process::id() + 1),
            "not a marker".to_string(),
        ];
        for marker in markers {
            let mut cmd = rig.command(&rig.links().join(TOOL), &path, &["status"]);
            cmd.env("SKIM_WRAPPER_PARENT", &marker);
            let _ = std::fs::remove_file(rig.log());

            let outcome = rig.outcome(&common::run_bounded(cmd));

            assert_reached_real_tool(&outcome);
        }
    }

    // ------------------------------------------------------------------------
    // The depth limit, on every surface.
    // ------------------------------------------------------------------------

    #[test]
    fn wrapper_at_depth_above_the_limit_is_refused_before_any_start() {
        let rig = Rig::new();
        rig.install_real_tool();
        rig.link_skim(&rig.links(), TOOL);
        let path = Rig::path_of(&[rig.links(), rig.real()]);
        let mut cmd = rig.command(&rig.links().join(TOOL), &path, &["status"]);
        cmd.env("SKIM_DEPTH", "32");

        let outcome = rig.outcome(&common::run_bounded(cmd));

        assert_refused(&outcome, "skim started itself 32 times in a row (limit 32)");
    }

    #[test]
    fn wrapper_at_the_depth_limit_still_starts_the_real_tool() {
        let rig = Rig::new();
        rig.install_real_tool();
        rig.link_skim(&rig.links(), TOOL);
        let path = Rig::path_of(&[rig.links(), rig.real()]);
        let mut cmd = rig.command(&rig.links().join(TOOL), &path, &["status"]);
        cmd.env("SKIM_DEPTH", "31");

        let outcome = rig.outcome(&common::run_bounded(cmd));

        assert_reached_real_tool(&outcome);
    }

    #[test]
    fn wrapper_treats_a_bad_depth_value_as_zero() {
        let rig = Rig::new();
        rig.install_real_tool();
        rig.link_skim(&rig.links(), TOOL);
        let path = Rig::path_of(&[rig.links(), rig.real()]);
        for bad in ["abc", "-1", "99999999999999999999", ""] {
            let mut cmd = rig.command(&rig.links().join(TOOL), &path, &["status"]);
            cmd.env("SKIM_DEPTH", bad);
            let _ = std::fs::remove_file(rig.log());

            let outcome = rig.outcome(&common::run_bounded(cmd));

            assert_reached_real_tool(&outcome);
        }
    }

    #[test]
    fn plain_skim_is_refused_above_the_depth_limit_and_runs_at_it() {
        let home = tempfile::tempdir().expect("home tempdir");

        let refused = common::skim_sandboxed(home.path())
            .env("SKIM_DEPTH", "32")
            .arg("--commit")
            .output()
            .expect("run skim");
        assert_eq!(refused.status.code(), Some(REFUSED));
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            stderr.starts_with("error: skim: skim started itself 32 times in a row (limit 32)"),
            "stderr={stderr:?}"
        );
        assert_eq!(stderr.lines().count(), 1, "one line: {stderr:?}");
        assert!(
            stderr.contains("a program that skim starts runs skim again"),
            "a skim-typed start has no tool link to blame: {stderr:?}"
        );

        let allowed = common::skim_sandboxed(home.path())
            .env("SKIM_DEPTH", "31")
            .arg("--commit")
            .output()
            .expect("run skim");
        assert_eq!(allowed.status.code(), Some(0));
    }

    // ------------------------------------------------------------------------
    // Local only: the two-copy case under a low per-user process cap.
    // ------------------------------------------------------------------------

    /// Number of processes the current user runs, or `None` when `ps` cannot say.
    ///
    /// Linux counts threads against the same limit, so `-L` lists them there.
    fn user_process_count() -> Option<u64> {
        // SAFETY: `getuid(2)` has no preconditions and cannot fail.
        let uid = unsafe { libc::getuid() };
        let mut ps = Command::new("ps");
        ps.args(["-U", &uid.to_string(), "-o", "pid="]);
        if cfg!(target_os = "linux") {
            ps.arg("-L");
        }
        let output = ps.output().ok()?;
        if !output.status.success() {
            return None;
        }
        let count = output
            .stdout
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .count();
        u64::try_from(count).ok().filter(|n| *n > 0)
    }

    /// Extra processes allowed above the current count.  A refused run needs a
    /// handful; the margin absorbs other work that starts meanwhile.
    const PROCESS_CAP_MARGIN: u64 = 64;

    /// The loop case under a per-user process cap just above current usage ends
    /// at the guard, and the shell stays usable.  Without the guard this run ends at
    /// the cap with a spawn error instead.
    ///
    /// Local only: it needs `ps`, and other work on the machine moves the count.
    #[test]
    #[ignore = "local only: sets a per-user process cap from the live process count"]
    fn two_copies_stop_at_once_under_a_low_process_cap() {
        use std::os::unix::process::CommandExt as _;

        let Some(usage) = user_process_count() else {
            eprintln!("skipped: the process count of this user cannot be read");
            return;
        };
        let cap = usage + PROCESS_CAP_MARGIN;

        let rig = Rig::new();
        rig.copy_skim(&rig.links(), TOOL);
        rig.copy_skim(&rig.second_links(), TOOL);
        let path = Rig::path_of(&[rig.links(), rig.second_links()]);
        let mut cmd = rig.command(&rig.links().join(TOOL), &path, &["status"]);
        // SAFETY: the closure runs between fork and exec.  It calls only
        // `getrlimit` and `setrlimit`, which are async-signal-safe, and touches
        // no memory of the parent.
        unsafe {
            cmd.pre_exec(move || {
                let mut limit = libc::rlimit {
                    rlim_cur: 0,
                    rlim_max: 0,
                };
                if libc::getrlimit(libc::RLIMIT_NPROC, &mut limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                limit.rlim_cur = limit.rlim_cur.min(cap as libc::rlim_t);
                if libc::setrlimit(libc::RLIMIT_NPROC, &limit) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let outcome = rig.outcome(&common::run_bounded(cmd));

        assert_copy_chain_stopped(&outcome);
    }
}
