//! Drives the `skim` CLI as a subprocess, the way agents use it (#203).
//!
//! A JSON query is `skim search --root <clone> --json --limit N [--offset K]
//! [flags] -- <query>`: all flags go before `--`, and the query always
//! follows `--`, so `-D warnings` and `->` parse as text. The scan engine
//! reads the files on every call, so there is nothing to build first.
//!
//! - **Environment**: [`SkimSandbox`] copies `skim_sandboxed_with_bin`
//!   (`crates/rskim/tests/common/mod.rs`, test-only so it cannot be imported)
//!   and adds [`GitIsolation::apply`], so skim and the oracle resolve git's
//!   global excludes from the same empty `HOME`.
//! - **Time**: every subprocess is bounded by [`SKIM_TIMEOUT_SECS`] through
//!   `rskim_research::clone::git_output_with_timeout`; a sweep is bounded by
//!   [`MAX_PAGES`].
//! - **Exit status**: stdout must parse as the JSON envelope. A non-zero exit
//!   with a parsable envelope is accepted (a no-match exit code must not read
//!   as a crash), and the page keeps the real exit code
//!   (`ResultPage::exit_code`), which `stakes.exit` judges. A signal, a
//!   timeout, non-JSON stdout, an `error` envelope or a malformed envelope
//!   (an unknown role included) is an error, which the scoreboard reports as
//!   a harness error (exit 2), never as a regression.
//! - **`[[stakes]]` entries** pass their own flags to skim. `-l` and `-c`
//!   cannot be combined with `--json`, so those calls read the text output
//!   (a path per line, or `path:count`); an entry that expects an exit code
//!   reads the status of a text call and nothing else.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::time::Instant;

use anyhow::Context;
use serde_json::Value;

use crate::scoreboard::MAX_PAGES;
use crate::scoreboard::golden::{StakesEntry, StakesExpect};
use crate::scoreboard::metrics::PlannedQuery;
use crate::scoreboard::types::{EntryKind, ResultPage};
use crate::scoreboard::universe::GitIsolation;

/// Per-subprocess timeout (seconds).
pub const SKIM_TIMEOUT_SECS: u64 = 120;

/// `--limit` for a "full list" call. The CLI accepts 1 to 1000000.
pub const FULL_LIST_LIMIT: u32 = 1_000_000;

/// How much of skim's stdout / stderr an error message quotes.
const EXCERPT_CHARS: usize = 600;

/// Variables removed from the skim environment: real-session state
/// (`skim_sandboxed_with_bin`) plus debug output, analytics and session
/// attribution, for determinism.
const REMOVED_SKIM_ENV: &[&str] = &[
    "SKIM_REWRITTEN_FROM",
    "SKIM_PASSTHROUGH",
    "SKIM_HOOK_VERSION",
    "SKIM_HOOK_BINARY",
    "SKIM_DEBUG",
    "SKIM_ANALYTICS_DB",
    "SKIM_SESSION_ID",
];

// ============================================================================
// Sandbox
// ============================================================================

/// The isolated environment every skim (and oracle git) subprocess runs in:
/// one scoreboard-owned `HOME` per run, holding skim's cache and every agent
/// config dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkimSandbox {
    home: PathBuf,
}

impl SkimSandbox {
    /// Sandbox rooted at `home` (a per-run temporary directory).
    pub fn new(home: impl Into<PathBuf>) -> Self {
        SkimSandbox { home: home.into() }
    }

    /// The sandbox `HOME`.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The git isolation shared by skim and the oracle (same `HOME`).
    pub fn git(&self) -> GitIsolation {
        GitIsolation::new(&self.home)
    }

    /// Apply the sandbox environment to `cmd`.
    pub fn apply(&self, cmd: &mut Command) {
        let home = &self.home;
        cmd.env("CLAUDE_CONFIG_DIR", home.join(".claude"))
            .env("SKIM_CACHE_DIR", home.join(".cache").join("skim"))
            .env("SKIM_WRAPPERS_DIR", home.join(".skim").join("bin"))
            .env("GEMINI_CONFIG_DIR", home.join(".gemini"))
            .env("COPILOT_CONFIG_DIR", home.join(".copilot"))
            .env("CODEX_HOME", home.join(".codex"))
            .env("CRUSH_CONFIG_DIR", home.join(".crush"))
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("NO_COLOR", "1");
        for var in REMOVED_SKIM_ENV {
            cmd.env_remove(var);
        }
        // HOME, GIT_CONFIG_NOSYSTEM=1, and no XDG_CONFIG_HOME / GIT_* redirects.
        self.git().apply(cmd);
    }
}

// ============================================================================
// Arguments
// ============================================================================

/// Arguments for one JSON query: `search --root <root> --json --limit N
/// [--offset K] <flags> -- <query>` (`--offset` only when `K > 0`).
pub fn search_args(
    root: &Path,
    query: &str,
    flags: &[String],
    limit: u32,
    offset: u64,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "search".into(),
        "--root".into(),
        root.as_os_str().to_owned(),
        "--json".into(),
        "--limit".into(),
        limit.to_string().into(),
    ];
    if offset > 0 {
        args.extend(["--offset".into(), offset.to_string().into()]);
    }
    args.extend(flags.iter().map(OsString::from));
    args.extend(["--".into(), query.into()]);
    args
}

/// Arguments for a text-mode run at the default `--limit` (what an agent
/// reads): `search --root <root> <flags> -- <query>`.
pub fn text_args(root: &Path, query: &str, flags: &[String]) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec![
        "search".into(),
        "--root".into(),
        root.as_os_str().to_owned(),
    ];
    args.extend(flags.iter().map(OsString::from));
    args.extend(["--".into(), query.into()]);
    args
}

/// A root-free description of a JSON query, for error messages.
fn query_label(query: &str, flags: &[String], limit: u32, offset: u64) -> String {
    let mut label = format!("skim search --json --limit {limit}");
    if offset > 0 {
        label.push_str(&format!(" --offset {offset}"));
    }
    for flag in flags {
        label.push(' ');
        label.push_str(flag);
    }
    label.push_str(&format!(" -- {query:?}"));
    label
}

/// A root-free description of a text-mode query, for error messages.
fn text_label(query: &str, flags: &[String]) -> String {
    let mut label = String::from("skim search");
    for flag in flags {
        label.push(' ');
        label.push_str(flag);
    }
    label.push_str(&format!(" -- {query:?} (text mode)"));
    label
}

// ============================================================================
// Observations
// ============================================================================

/// Wall-clock and reported duration of one call (INFO).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    pub wall_ms: f64,
    /// The envelope's `duration_ms`, when present.
    pub duration_ms: Option<u64>,
}

/// One page of a pagination sweep.
#[derive(Debug, Clone, PartialEq)]
pub struct SweepPage {
    /// The `--offset` it was fetched at.
    pub offset: u64,
    pub page: ResultPage,
}

/// A pagination sweep at one limit: pages `--offset 0, L, 2L, …` until
/// `has_more` is false or [`MAX_PAGES`] pages were fetched.
#[derive(Debug, Clone, PartialEq)]
pub struct Sweep {
    pub limit: u32,
    pub pages: Vec<SweepPage>,
}

/// A text-mode run's raw output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Everything the runner observed for one golden entry.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryObservation {
    pub id: String,
    /// The full list (`--limit` [`FULL_LIST_LIMIT`], no offset). A
    /// `[[stakes]]` entry's list is read from the call its flags need.
    pub full: ResultPage,
    /// One sweep per limit (`[[pagination]]` only).
    pub sweeps: Vec<Sweep>,
    /// `(limit, page)` per limit (`[[prefix]]` only).
    pub limited: Vec<(u32, ResultPage)>,
    /// Text-mode output (`[[ident]]` / `[[concept]]` only).
    pub text: Option<TextOutput>,
}

/// An observation plus the timings of every call behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct Observed {
    pub observation: EntryObservation,
    pub timings: Vec<Timing>,
}

/// One parsed JSON call.
struct JsonCall {
    value: Value,
    /// The root-free call description, for error messages.
    label: String,
    status: ExitStatus,
    timing: Timing,
}

/// One text-mode call.
struct TextCall {
    output: TextOutput,
    exit_code: i32,
    timing: Timing,
}

// ============================================================================
// Runner
// ============================================================================

/// Runs one skim binary (injected by path) inside a [`SkimSandbox`].
#[derive(Debug, Clone)]
pub struct SkimRunner {
    bin: PathBuf,
    sandbox: SkimSandbox,
    timeout_secs: u64,
}

impl SkimRunner {
    /// Run `bin` in `sandbox`, bounded by [`SKIM_TIMEOUT_SECS`] per call.
    pub fn new(bin: impl Into<PathBuf>, sandbox: SkimSandbox) -> Self {
        SkimRunner {
            bin: bin.into(),
            sandbox,
            timeout_secs: SKIM_TIMEOUT_SECS,
        }
    }

    /// Override the per-call timeout.
    pub fn with_timeout_secs(self, timeout_secs: u64) -> Self {
        SkimRunner {
            timeout_secs,
            ..self
        }
    }

    /// The sandbox (its `HOME` also isolates the oracle's git calls).
    pub fn sandbox(&self) -> &SkimSandbox {
        &self.sandbox
    }

    /// One JSON query page, with the process's exit code (any exit code is
    /// accepted when stdout is a parsable envelope).
    ///
    /// # Errors
    ///
    /// Spawn failure, timeout, a signal, non-JSON stdout, an `error`
    /// envelope, or an envelope that does not parse.
    pub fn page(
        &self,
        root: &Path,
        query: &str,
        flags: &[String],
        limit: u32,
        offset: u64,
    ) -> anyhow::Result<(ResultPage, Timing)> {
        let call = self.json(root, query, flags, limit, offset)?;
        let page = ResultPage::from_json(&call.value).with_context(|| {
            format!("{}: unexpected JSON envelope ({})", call.label, call.status)
        })?;
        let exit_code = call
            .status
            .code()
            .with_context(|| format!("{}: no exit code ({})", call.label, call.status))?;
        Ok((ResultPage { exit_code, ..page }, call.timing))
    }

    /// Run one JSON query and parse stdout as JSON.
    fn json(
        &self,
        root: &Path,
        query: &str,
        flags: &[String],
        limit: u32,
        offset: u64,
    ) -> anyhow::Result<JsonCall> {
        let label = query_label(query, flags, limit, offset);
        let args = search_args(root, query, flags, limit, offset);
        let (out, wall_ms) = self.exec(&args, &label)?;
        ensure_not_signalled(&out, &label)?;
        let value: Value = serde_json::from_slice(&out.stdout).map_err(|e| {
            anyhow::anyhow!(
                "{label}: stdout is not valid JSON ({e}); {}; stdout: {:?}; stderr: {:?}",
                out.status,
                excerpt(&out.stdout),
                excerpt(&out.stderr)
            )
        })?;
        let duration_ms = value.get("duration_ms").and_then(Value::as_u64);
        Ok(JsonCall {
            value,
            label,
            status: out.status,
            timing: Timing {
                wall_ms,
                duration_ms,
            },
        })
    }

    /// Sweep `--offset 0, L, 2L, …` at `limit` until `has_more` is false or
    /// [`MAX_PAGES`] pages were fetched.
    ///
    /// # Errors
    ///
    /// As [`SkimRunner::page`], for any page.
    pub fn sweep(
        &self,
        root: &Path,
        q: &PlannedQuery,
        limit: u32,
    ) -> anyhow::Result<(Sweep, Vec<Timing>)> {
        let mut pages = Vec::new();
        let mut timings = Vec::new();
        for index in 0..MAX_PAGES {
            let offset = u64::from(index) * u64::from(limit);
            let (page, timing) = self.page(root, &q.query, &q.flags, limit, offset)?;
            let more = page.has_more;
            pages.push(SweepPage { offset, page });
            timings.push(timing);
            if !more {
                break;
            }
        }
        Ok((Sweep { limit, pages }, timings))
    }

    /// A text-mode run at the default limit. Any normal exit is accepted:
    /// the caller reads the bytes or the exit code.
    fn text_call(&self, root: &Path, query: &str, flags: &[String]) -> anyhow::Result<TextCall> {
        let label = text_label(query, flags);
        let (out, wall_ms) = self.exec(&text_args(root, query, flags), &label)?;
        ensure_not_signalled(&out, &label)?;
        let exit_code = out
            .status
            .code()
            .with_context(|| format!("{label}: no exit code ({})", out.status))?;
        Ok(TextCall {
            output: TextOutput {
                stdout: out.stdout,
                stderr: out.stderr,
            },
            exit_code,
            timing: Timing {
                wall_ms,
                duration_ms: None,
            },
        })
    }

    /// A text-mode run at the default limit, for the byte metrics.
    ///
    /// # Errors
    ///
    /// Spawn failure, timeout, or a signal.
    pub fn text(&self, root: &Path, query: &str) -> anyhow::Result<(TextOutput, Timing)> {
        let call = self.text_call(root, query, &[])?;
        Ok((call.output, call.timing))
    }

    /// Run every call entry `q` needs: the full list; for `[[pagination]]`,
    /// one sweep per limit; for `[[prefix]]`, one limited list per limit; for
    /// `[[ident]]` / `[[concept]]`, one text-mode run; for `[[stakes]]`, the
    /// one call its flags and expectation name ([`SkimRunner::observe_stakes`]).
    ///
    /// # Errors
    ///
    /// Any call error.
    pub fn observe(&self, root: &Path, q: &PlannedQuery) -> anyhow::Result<Observed> {
        if let Some(stakes) = q.stakes_oracle() {
            return self.observe_stakes(root, q, &stakes.entry);
        }
        let mut timings = Vec::new();
        let (full, t) = self.page(root, &q.query, &q.flags, FULL_LIST_LIMIT, 0)?;
        timings.push(t);

        let mut sweeps = Vec::new();
        if q.kind == EntryKind::Pagination {
            for &limit in &q.limits {
                let (sweep, t) = self.sweep(root, q, limit)?;
                sweeps.push(sweep);
                timings.extend(t);
            }
        }

        let mut limited = Vec::new();
        if q.kind == EntryKind::Prefix {
            for &limit in &q.limits {
                let (page, t) = self.page(root, &q.query, &q.flags, limit, 0)?;
                limited.push((limit, page));
                timings.push(t);
            }
        }

        let text = if q.measures_text() {
            let (text, t) = self.text(root, &q.query)?;
            timings.push(t);
            Some(text)
        } else {
            None
        };

        Ok(Observed {
            observation: EntryObservation {
                id: q.id.clone(),
                full,
                sweeps,
                limited,
                text,
            },
            timings,
        })
    }

    /// One `[[stakes]]` call, with the entry's own flags:
    ///
    /// - `expect = "exit"`: a text call, of which only the exit code is read;
    /// - `-l`: a text call, one path per line;
    /// - `-c`: a text call, one `path:count` per line;
    /// - otherwise a JSON call for the full list.
    fn observe_stakes(
        &self,
        root: &Path,
        q: &PlannedQuery,
        entry: &StakesEntry,
    ) -> anyhow::Result<Observed> {
        let text_page = |parse: fn(&[u8], i32) -> anyhow::Result<ResultPage>| {
            let call = self.text_call(root, &q.query, &q.flags)?;
            let page = parse(&call.output.stdout, call.exit_code).with_context(|| {
                format!(
                    "{}: unexpected text output; stderr: {:?}",
                    text_label(&q.query, &q.flags),
                    excerpt(&call.output.stderr)
                )
            })?;
            anyhow::Ok((page, call.timing))
        };
        let (full, timing) = if entry.expect == StakesExpect::Exit {
            let call = self.text_call(root, &q.query, &q.flags)?;
            let page = ResultPage {
                exit_code: call.exit_code,
                ..ResultPage::default()
            };
            (page, call.timing)
        } else if entry.has_flag("-l") {
            text_page(ResultPage::from_files_text)?
        } else if entry.has_flag("-c") {
            text_page(ResultPage::from_counts_text)?
        } else {
            self.page(root, &q.query, &q.flags, FULL_LIST_LIMIT, 0)?
        };
        Ok(Observed {
            observation: EntryObservation {
                id: q.id.clone(),
                full,
                sweeps: Vec::new(),
                limited: Vec::new(),
                text: None,
            },
            timings: vec![timing],
        })
    }

    /// Spawn the skim binary with `args` in the sandbox, bounded by the
    /// timeout. Returns the output and the wall-clock milliseconds.
    fn exec(&self, args: &[OsString], label: &str) -> anyhow::Result<(Output, f64)> {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.sandbox.apply(&mut cmd);
        let start = Instant::now();
        let out = rskim_research::clone::git_output_with_timeout(cmd, label, self.timeout_secs)
            .with_context(|| format!("running {} ({label})", self.bin.display()))?;
        Ok((out, start.elapsed().as_secs_f64() * 1000.0))
    }
}

/// A crash (killed by a signal) is a harness error whatever stdout holds.
fn ensure_not_signalled(out: &Output, label: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        out.status.code().is_some(),
        "{label} was killed by a signal ({}); stderr: {:?}",
        out.status,
        excerpt(&out.stderr)
    );
    Ok(())
}

/// The first [`EXCERPT_CHARS`] characters of `bytes`, lossily decoded and
/// trimmed.
fn excerpt(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    let mut out: String = trimmed.chars().take(EXCERPT_CHARS).collect();
    if trimmed.chars().count() > EXCERPT_CHARS {
        out.push('…');
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;

    use super::*;
    use crate::scoreboard::golden::parse_golden;
    use crate::scoreboard::metrics::plan;
    #[cfg(unix)]
    use crate::scoreboard::test_support::script_runner;

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn flags(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// A valid envelope of no files.
    #[cfg(unix)]
    const EMPTY_ENVELOPE: &str = r#"{"files":[],"has_more":false,"total_lines":0,"definitions":{"count":0,"at_least":false,"sites":[]},"searched_files":3,"left_out":{"binary":1},"parse":{"unlabelled_files":0},"cost":{"files_read":3,"bytes_read":9,"files_parsed":0,"bytes_parsed":0}}"#;

    #[test]
    fn json_args_put_every_flag_before_the_separator_and_the_query_after() {
        let args = search_args(Path::new("/c"), "-D warnings", &flags(&["-t", "ts"]), 7, 14);
        assert_eq!(
            strings(&args),
            [
                "search",
                "--root",
                "/c",
                "--json",
                "--limit",
                "7",
                "--offset",
                "14",
                "-t",
                "ts",
                "--",
                "-D warnings"
            ]
        );
    }

    #[test]
    fn offset_zero_omits_offset() {
        let args = search_args(Path::new("/c"), "fn", &[], FULL_LIST_LIMIT, 0);
        assert_eq!(
            strings(&args),
            [
                "search", "--root", "/c", "--json", "--limit", "1000000", "--", "fn"
            ]
        );
    }

    #[test]
    fn text_args_use_the_default_limit_and_no_json() {
        assert_eq!(
            strings(&text_args(Path::new("/c"), "build lock", &[])),
            ["search", "--root", "/c", "--", "build lock"]
        );
        assert_eq!(
            strings(&text_args(
                Path::new("/c"),
                "x",
                &flags(&["-l", "-g", "!a"])
            )),
            ["search", "--root", "/c", "-l", "-g", "!a", "--", "x"]
        );
    }

    #[test]
    fn the_sandbox_isolates_home_cache_agents_and_git() {
        let sandbox = SkimSandbox::new("/sb");
        let mut cmd = Command::new("skim");
        sandbox.apply(&mut cmd);
        let envs: BTreeMap<String, Option<String>> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        let set = |k: &str| envs.get(k).cloned().flatten();
        assert_eq!(set("HOME").as_deref(), Some("/sb"));
        assert_eq!(set("SKIM_CACHE_DIR").as_deref(), Some("/sb/.cache/skim"));
        assert_eq!(set("CLAUDE_CONFIG_DIR").as_deref(), Some("/sb/.claude"));
        assert_eq!(set("CODEX_HOME").as_deref(), Some("/sb/.codex"));
        assert_eq!(set("SKIM_DISABLE_ANALYTICS").as_deref(), Some("1"));
        assert_eq!(set("GIT_CONFIG_NOSYSTEM").as_deref(), Some("1"));
        for removed in [
            "SKIM_HOOK_VERSION",
            "SKIM_HOOK_BINARY",
            "SKIM_PASSTHROUGH",
            "SKIM_DEBUG",
            "SKIM_SESSION_ID",
            "SKIM_ANALYTICS_DB",
            "XDG_CONFIG_HOME",
        ] {
            assert_eq!(envs.get(removed), Some(&None), "{removed} must be removed");
        }
        assert_eq!(cmd.get_program(), OsStr::new("skim"));
    }

    // --- the exit status of the process -----------------------------------------

    #[cfg(unix)]
    fn page_of(runner: &SkimRunner) -> anyhow::Result<(ResultPage, Timing)> {
        runner.page(Path::new("/c"), "qzvx_absent_7713", &[], 10, 0)
    }

    #[cfg(unix)]
    #[test]
    fn a_page_carries_the_exit_code_of_the_process() {
        for code in [0, 1, 3] {
            let dir = tempfile::tempdir().unwrap();
            let runner = script_runner(
                dir.path(),
                &format!("printf '%s' '{EMPTY_ENVELOPE}'\nexit {code}"),
            );
            let (page, _) = page_of(&runner).unwrap();
            assert_eq!(page.exit_code, code);
            assert_eq!(page.searched_files, Some(3));
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_error_envelope_is_a_harness_error_whatever_the_exit_code() {
        for code in [0, 2] {
            let dir = tempfile::tempdir().unwrap();
            let runner = script_runner(
                dir.path(),
                &format!("printf '{{\"error\":\"bad glob\"}}'\nexit {code}"),
            );
            let err = page_of(&runner).expect_err("an error envelope");
            assert!(format!("{err:#}").contains("bad glob"), "{err:#}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn stdout_that_is_not_json_is_a_harness_error_whatever_the_exit_code() {
        for code in [0, 1] {
            let dir = tempfile::tempdir().unwrap();
            let runner = script_runner(
                dir.path(),
                &format!("printf 'skim search: no repository'\nexit {code}"),
            );
            let err = page_of(&runner).expect_err("stdout is not JSON");
            assert!(format!("{err:#}").contains("not valid JSON"), "{err:#}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_envelope_with_an_unknown_role_is_a_harness_error() {
        let dir = tempfile::tempdir().unwrap();
        let envelope = EMPTY_ENVELOPE.replace(
            r#""files":[]"#,
            r#""files":[{"path":"a.rs","score":1.0,"line_count":1,"words_matched":1,"words_total":1,"occurrences":[{"line":1,"role":"nonsense"}]}]"#,
        );
        let runner = script_runner(dir.path(), &format!("printf '%s' '{envelope}'"));
        let err = page_of(&runner).expect_err("an unknown role");
        assert!(format!("{err:#}").contains("nonsense"), "{err:#}");
    }

    #[cfg(unix)]
    #[test]
    fn a_process_killed_by_a_signal_is_a_harness_error_not_an_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let runner = script_runner(dir.path(), "printf '%s' '{}'\nkill -9 $$");
        let err = page_of(&runner).expect_err("a killed process");
        assert!(format!("{err:#}").contains("signal"), "{err:#}");
    }

    // --- [[stakes]] calls ---------------------------------------------------------

    /// The planned `[[stakes]]` entry made of `flags` and `expect`.
    #[cfg(unix)]
    fn stakes_entry(flags: &str, expect: &str) -> PlannedQuery {
        let golden = parse_golden(&format!(
            "corpus = \"skim\"\ncommit = \"b8a0a79463382347820f1c2572bde37b68e87c76\"\n\
             [[stakes]]\nid = \"skim-Q09\"\nquery = \"Launch\"\nflags = {flags}\nexpect = \"{expect}\"\n{}",
            if expect == "exit" { "exit = 1\n" } else { "" }
        ))
        .unwrap();
        plan(&golden).unwrap().remove(0)
    }

    #[cfg(unix)]
    #[test]
    fn a_list_stakes_call_is_a_text_call_that_reads_a_path_per_line() {
        let dir = tempfile::tempdir().unwrap();
        // `-l` cannot be combined with `--json`: the script refuses it.
        let runner = script_runner(
            dir.path(),
            "case \" $* \" in *' --json '*) exit 2;; esac\nprintf 'a.rs\\nb/c.rs\\n'",
        );
        let q = stakes_entry("[\"-l\"]", "files");
        let observed = runner.observe(Path::new("/c"), &q).unwrap();
        let paths: Vec<&str> = observed
            .observation
            .full
            .rows
            .iter()
            .map(|r| r.path.as_str())
            .collect();
        assert_eq!(paths, ["a.rs", "b/c.rs"]);
        assert_eq!(observed.observation.full.exit_code, 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_count_stakes_call_reads_path_and_count_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let runner = script_runner(
            dir.path(),
            "case \" $* \" in *' --json '*) exit 2;; esac\nprintf 'a.rs:3\\nb/c.rs:10\\n'",
        );
        let q = stakes_entry("[\"-c\"]", "counts");
        let full = runner
            .observe(Path::new("/c"), &q)
            .unwrap()
            .observation
            .full;
        assert_eq!(
            full.line_counts,
            BTreeMap::from([("a.rs".to_string(), 3), ("b/c.rs".to_string(), 10)])
        );
        assert_eq!(full.rows.len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn a_count_line_without_a_count_is_a_harness_error() {
        let dir = tempfile::tempdir().unwrap();
        let runner = script_runner(dir.path(), "printf 'a.rs\\n'");
        let q = stakes_entry("[\"-c\"]", "counts");
        let err = runner
            .observe(Path::new("/c"), &q)
            .expect_err("a -c line is path:count");
        assert!(format!("{err:#}").contains("path:count"), "{err:#}");
    }

    #[cfg(unix)]
    #[test]
    fn an_exit_stakes_call_reads_the_status_and_nothing_else() {
        for code in [0, 1, 2] {
            let dir = tempfile::tempdir().unwrap();
            // The output is not a valid listing and not JSON: it is not read.
            let runner = script_runner(
                dir.path(),
                &format!("printf 'whatever: x\\n\\n'\nexit {code}"),
            );
            let q = stakes_entry("[\"-l\"]", "exit");
            let full = runner
                .observe(Path::new("/c"), &q)
                .unwrap()
                .observation
                .full;
            assert_eq!(full.exit_code, code);
            assert!(full.rows.is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_stakes_call_without_a_list_flag_is_a_json_call_with_the_entry_flags() {
        let dir = tempfile::tempdir().unwrap();
        // The script answers only a call that has `--json` and `-i` before `--`.
        let runner = script_runner(
            dir.path(),
            &format!(
                "case \" $* \" in *' --json '*' -i '*'-- Launch ') printf '%s' '{EMPTY_ENVELOPE}';; *) exit 9;; esac"
            ),
        );
        let q = stakes_entry("[\"-i\"]", "lines");
        let observed = runner.observe(Path::new("/c"), &q).unwrap();
        assert_eq!(observed.observation.full.searched_files, Some(3));
    }

    #[test]
    fn labels_carry_no_root() {
        let label = query_label("fn", &flags(&["-t", "rust"]), 5, 10);
        assert_eq!(
            label,
            "skim search --json --limit 5 --offset 10 -t rust -- \"fn\""
        );
        assert_eq!(
            text_label("fn", &flags(&["-l"])),
            "skim search -l -- \"fn\" (text mode)"
        );
    }

    #[test]
    fn excerpts_are_bounded() {
        let long = "x".repeat(EXCERPT_CHARS + 10);
        let e = excerpt(long.as_bytes());
        assert_eq!(e.chars().count(), EXCERPT_CHARS + 1);
        assert!(e.ends_with('…'));
        assert_eq!(excerpt(b"  short \n"), "short");
    }
}
