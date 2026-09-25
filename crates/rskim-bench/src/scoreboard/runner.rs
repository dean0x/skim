//! Drives the `skim` CLI as a subprocess, the way agents use it (#203).
//!
//! Every query is `skim search --root <clone> --json --limit N [--offset K]
//! [flags] [-- <query>]`: all flags go before `--` (the CLI requires output
//! flags there), and the query always follows `--`, so `-D warnings` and
//! `->` parse as text.
//!
//! - **Environment**: [`SkimSandbox`] copies `skim_sandboxed_with_bin`
//!   (`crates/rskim/tests/common/mod.rs`, test-only so it cannot be imported)
//!   and adds [`GitIsolation::apply`], so skim and the oracle resolve git's
//!   global excludes from the same empty `HOME`.
//! - **Time**: every subprocess is bounded by [`SKIM_TIMEOUT_SECS`] through
//!   `rskim_research::clone::git_output_with_timeout`; a sweep is bounded by
//!   [`MAX_PAGES`].
//! - **Exit status**: stdout must parse as the arm's JSON envelope. A
//!   non-zero exit with a parsable envelope is accepted (a future no-match
//!   exit code must not read as a crash); a signal, a timeout, non-JSON
//!   stdout, or a malformed envelope is an error, which the scoreboard
//!   reports as a harness error (exit 2), never as a regression.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Instant;

use anyhow::Context;
use serde_json::Value;

use crate::scoreboard::MAX_PAGES;
use crate::scoreboard::golden::{QueryFlags, pagination_bound, within_pagination_bound};
use crate::scoreboard::metrics::PlannedQuery;
use crate::scoreboard::types::{Arm, EntryKind, ResultPage, StatsSnapshot};
use crate::scoreboard::universe::GitIsolation;

/// Per-subprocess timeout (seconds).
pub const SKIM_TIMEOUT_SECS: u64 = 120;

/// `--limit` for a "full list" call. The CLI has no upper clamp.
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
/// one scoreboard-owned `HOME` per run, holding skim's cache (and so its
/// index) and every agent config dir.
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
/// [--offset K] <flags> [-- <query>]` (`--offset` only when `K > 0`).
pub fn search_args(
    root: &Path,
    query: Option<&str>,
    flags: &QueryFlags,
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
    args.extend(flags.to_args().into_iter().map(OsString::from));
    if let Some(query) = query {
        args.extend(["--".into(), query.into()]);
    }
    args
}

/// Arguments for a text-mode run at the default `--limit` (what an agent
/// reads): `search --root <root> -- <query>`.
pub fn text_args(root: &Path, query: &str) -> Vec<OsString> {
    vec![
        "search".into(),
        "--root".into(),
        root.as_os_str().to_owned(),
        "--".into(),
        query.into(),
    ]
}

/// A root-free description of a JSON query, for error messages.
fn query_label(query: Option<&str>, flags: &QueryFlags, limit: u32, offset: u64) -> String {
    let mut label = format!("skim search --json --limit {limit}");
    if offset > 0 {
        label.push_str(&format!(" --offset {offset}"));
    }
    for flag in flags.to_args() {
        label.push(' ');
        label.push_str(&flag);
    }
    if let Some(query) = query {
        label.push_str(&format!(" -- {query:?}"));
    }
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
    /// The full list (`--limit` [`FULL_LIST_LIMIT`], no offset).
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

    /// `skim search --build --root <root>`; must exit 0.
    ///
    /// # Errors
    ///
    /// Spawn failure, timeout, or a non-zero exit (stderr quoted).
    pub fn build(&self, root: &Path) -> anyhow::Result<()> {
        let args: Vec<OsString> = vec![
            "search".into(),
            "--build".into(),
            "--root".into(),
            root.as_os_str().to_owned(),
        ];
        let label = "skim search --build";
        let (out, _) = self.exec(&args, label)?;
        anyhow::ensure!(
            out.status.success(),
            "{label} failed ({}): {}",
            out.status,
            excerpt(&out.stderr)
        );
        Ok(())
    }

    /// `skim search --stats --json --root <root>`.
    ///
    /// # Errors
    ///
    /// Spawn failure, timeout, a signal, or stdout that is not a stats
    /// object (including skim's `{"error": …}` envelope).
    pub fn stats(&self, root: &Path) -> anyhow::Result<StatsSnapshot> {
        let args: Vec<OsString> = vec![
            "search".into(),
            "--stats".into(),
            "--json".into(),
            "--root".into(),
            root.as_os_str().to_owned(),
        ];
        let label = "skim search --stats --json";
        let (out, _) = self.exec(&args, label)?;
        ensure_not_signalled(&out, label)?;
        StatsSnapshot::parse(&out.stdout)
            .with_context(|| format!("{label} ({}); stderr: {}", out.status, excerpt(&out.stderr)))
    }

    /// One JSON query page.
    ///
    /// # Errors
    ///
    /// Spawn failure, timeout, a signal, non-JSON stdout, or an envelope
    /// that does not parse for `arm`.
    pub fn page(
        &self,
        root: &Path,
        arm: Arm,
        query: Option<&str>,
        flags: &QueryFlags,
        limit: u32,
        offset: u64,
    ) -> anyhow::Result<(ResultPage, Timing)> {
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
        let page = ResultPage::from_json(arm, &value).with_context(|| {
            format!("{label}: unexpected {arm:?} JSON envelope ({})", out.status)
        })?;
        let duration_ms = value.get("duration_ms").and_then(Value::as_u64);
        Ok((
            page,
            Timing {
                wall_ms,
                duration_ms,
            },
        ))
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
            let (page, timing) =
                self.page(root, q.arm, q.query.as_deref(), &q.flags, limit, offset)?;
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
    /// only the bytes are measured, and the JSON calls for the same query
    /// judge correctness.
    ///
    /// # Errors
    ///
    /// Spawn failure, timeout, or a signal.
    pub fn text(&self, root: &Path, query: &str) -> anyhow::Result<(TextOutput, Timing)> {
        let label = format!("skim search -- {query:?} (text mode)");
        let (out, wall_ms) = self.exec(&text_args(root, query), &label)?;
        ensure_not_signalled(&out, &label)?;
        Ok((
            TextOutput {
                stdout: out.stdout,
                stderr: out.stderr,
            },
            Timing {
                wall_ms,
                duration_ms: None,
            },
        ))
    }

    /// Run every call `q` needs: the full list; for `[[pagination]]`, one
    /// sweep per limit (after checking an oracle-less full list against the
    /// pagination bound); for `[[prefix]]`, one limited list per limit; for
    /// `[[ident]]` / `[[concept]]`, one text-mode run.
    ///
    /// # Errors
    ///
    /// Any call error, or an oracle-less (`--ast` / `--blast-radius`)
    /// pagination entry whose full list exceeds `min(limits) × (MAX_PAGES −
    /// 1)` — golden integrity cannot bound those in advance.
    pub fn observe(&self, root: &Path, q: &PlannedQuery) -> anyhow::Result<Observed> {
        let mut timings = Vec::new();
        let query = q.query.as_deref();
        let (full, t) = self.page(root, q.arm, query, &q.flags, FULL_LIST_LIMIT, 0)?;
        timings.push(t);

        let mut sweeps = Vec::new();
        if q.kind == EntryKind::Pagination {
            if q.oracle.is_none() {
                let count = u64::try_from(full.rows.len())?;
                anyhow::ensure!(
                    within_pagination_bound(count, &q.limits),
                    "skim's full list has {count} rows, over the pagination bound {:?} \
                     (min(limits) x (MAX_PAGES - 1)); narrow the query or raise its limits",
                    pagination_bound(&q.limits)
                );
            }
            for &limit in &q.limits {
                let (sweep, t) = self.sweep(root, q, limit)?;
                sweeps.push(sweep);
                timings.extend(t);
            }
        }

        let mut limited = Vec::new();
        if q.kind == EntryKind::Prefix {
            for &limit in &q.limits {
                let (page, t) = self.page(root, q.arm, query, &q.flags, limit, 0)?;
                limited.push((limit, page));
                timings.push(t);
            }
        }

        let text = match (q.measures_text(), query) {
            (true, Some(query)) => {
                let (text, t) = self.text(root, query)?;
                timings.push(t);
                Some(text)
            }
            (true, None) => anyhow::bail!("a ranking entry has no query"),
            (false, _) => None,
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

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    fn flags(v: &[&str]) -> QueryFlags {
        QueryFlags::parse(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn json_args_put_every_flag_before_the_separator_and_the_query_after() {
        let args = search_args(
            Path::new("/c"),
            Some("-D warnings"),
            &flags(&["--near", "5", "--phrase"]),
            7,
            14,
        );
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
                "--phrase",
                "--near",
                "5",
                "--",
                "-D warnings"
            ]
        );
    }

    #[test]
    fn offset_zero_and_standalone_arms_omit_offset_and_separator() {
        let args = search_args(
            Path::new("/c"),
            None,
            &flags(&["--ast", "god-function"]),
            FULL_LIST_LIMIT,
            0,
        );
        assert_eq!(
            strings(&args),
            [
                "search",
                "--root",
                "/c",
                "--json",
                "--limit",
                "1000000",
                "--ast",
                "god-function"
            ]
        );
    }

    #[test]
    fn text_args_use_the_default_limit_and_no_json() {
        assert_eq!(
            strings(&text_args(Path::new("/c"), "build lock")),
            ["search", "--root", "/c", "--", "build lock"]
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

    #[test]
    fn labels_carry_no_root() {
        let label = query_label(Some("fn"), &flags(&["--hot"]), 5, 10);
        assert_eq!(
            label,
            "skim search --json --limit 5 --offset 10 --hot -- \"fn\""
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
