//! Search subcommand: find where a name is defined and where it is used.
//!
//! `skim search PATTERN` reads the files at query time. There is no index to
//! build and nothing is cached or written.
//!
//! # Architecture
//!
//! All I/O lives here (this module). The pieces of a query:
//! - `args` — the closed flag set and its parser
//! - `filter` — the `-g` and `-t` path filters
//! - `universe` — the files a query reads, and the files it counts as left out
//! - `matcher` — the compiled pattern with its case and whole-word rules
//! - `scan` — phase A counts every matching line, phase B labels and keeps lines
//! - `classifier`, `field`, `fields` — comment, string and identifier ranges
//! - `roles` — the role of each match: definition, call, import, ...
//! - `rank` — the file prior and the rank order of both views
//! - `render` — the text, JSON, `-l` and `-c` output
//! - `gitdir` — project-root discovery and git directory lookup
//! - `hooks` — the removal of the search git hooks that older versions wrote
//! - `legacy_cache` — the removal of the index data that older versions cached
//!
//! A query never calls `hooks` or `legacy_cache`. `skim init`, `skim init --uninstall`
//! and `skim --clear-cache` remove old data, and `skim doctor` reports it.

mod args;
mod classifier;
mod field;
mod fields;
mod filter;
mod gitdir;
pub(crate) mod hooks;
pub(crate) mod legacy_cache;
mod matcher;
mod rank;
mod render;
mod roles;
#[cfg(test)]
#[path = "run_tests.rs"]
mod run_tests;
mod scan;
mod universe;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use args::{ListMode, QueryFlags, QueryShape, SearchAction, UsageError};
use matcher::{CaseMode, Matcher, MatcherSpec, PatternKind};
use render::{Format, Layout, MatchMode, QueryInfo, Rendered, Report};
use scan::{Limits, QueryPlan, ScanConfig, ScanQuery};

use crate::cmd::execution::{
    StdoutStatus, pipe_closed_exit, write_line_to_stderr, write_line_to_stdout, write_to_stdout,
};

// ============================================================================
// Public entry point
// ============================================================================

/// Run the `skim search` subcommand.
///
/// - `skim search [FLAGS] PATTERN` — read the files and print the matches
/// - A removed flag (`--ast`, `--hot`, `--cold`, `--risky`, `--blast-radius`,
///   `--weights`, `--phrase`, `--near`, `--lang`, `-j`, and the index commands
///   `--build`, `--rebuild`, `--update`, `--stats`, `--install-hooks`,
///   `--remove-hooks`) — refuse with exit 2 and name the replacement
/// - No args / `--help` / `-h` — print help
///
/// A query records no analytics.
///
/// Exit codes of a query: 0 a line matched, 1 no line matched, 2 a usage or
/// input error (nothing partial is printed), 141 when the reader of stdout
/// closed the pipe.
pub(crate) fn run(
    args: &[String],
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    match args::parse_flags(args) {
        Ok(SearchAction::Help) => Ok(print_help()),
        Ok(SearchAction::Removed(message)) => {
            // A removed flag never writes stdout, with or without `--json`.
            Ok(fail_without_stdout(&message))
        }
        Ok(SearchAction::Query(flags)) => Ok(run_query(&flags)),
        Err(e) => Ok(fail(e.message(), args::wants_json(args))),
    }
}

/// Print the help text.
fn print_help() -> ExitCode {
    match write_line_to_stdout(args::SEARCH_HELP_TEXT) {
        Ok(StdoutStatus::Written) => ExitCode::SUCCESS,
        Ok(StdoutStatus::PipeClosed) => pipe_closed_exit(),
        Err(e) => fail_without_stdout(&format!("cannot write the help text: {e}")),
    }
}

// ============================================================================
// Failures
// ============================================================================

/// End a command with exit 2: the message on stderr with the program prefix,
/// and with `json` the error object on stdout.
fn fail(message: &str, json: bool) -> ExitCode {
    if json {
        match write_line_to_stdout(&render::json_error(message)) {
            Ok(StdoutStatus::Written) => {}
            Ok(StdoutStatus::PipeClosed) => return pipe_closed_exit(),
            Err(_) => {}
        }
    }
    fail_without_stdout(message)
}

/// End a command with exit 2 and the message on stderr only.
fn fail_without_stdout(message: &str) -> ExitCode {
    match write_line_to_stderr(&format!("skim search: {message}")) {
        Ok(StdoutStatus::PipeClosed) => pipe_closed_exit(),
        Ok(StdoutStatus::Written) | Err(_) => ExitCode::from(args::USAGE_EXIT_CODE),
    }
}

// ============================================================================
// Query execution
// ============================================================================

/// The folder a query searches, and the notice that names it.
#[derive(Debug, PartialEq, Eq)]
struct ScanRoot {
    path: PathBuf,
    /// Set when the implicit root is not the current folder.
    notice: Option<String>,
}

/// Decide the folder to search.
///
/// With `--root` the folder must exist. Without it the root is the repository
/// that holds `cwd` (the nearest ancestor with a `.git` entry), found under
/// `rules`; outside a repository it is `cwd` alone. Nothing implicit reaches
/// `/` or the home folder from below. `cwd` is canonical.
///
/// # Errors
///
/// [`UsageError`] for a missing or non-folder `--root`, and for an implicit
/// search from `/`.
fn resolve_scan_root(
    explicit: Option<&Path>,
    cwd: &Path,
    rules: &gitdir::RootRules,
) -> Result<ScanRoot, UsageError> {
    if let Some(dir) = explicit {
        let canonical = dir
            .canonicalize()
            .map_err(|e| UsageError::new(format!("--root {dir:?}: {e}")))?;
        if !canonical.is_dir() {
            return Err(UsageError::new(format!(
                "--root {dir:?} is not a directory"
            )));
        }
        return Ok(ScanRoot {
            path: canonical,
            notice: None,
        });
    }
    if cwd.parent().is_none() {
        return Err(UsageError::new(
            "refusing to search / implicitly; pass --root /",
        ));
    }
    let path = gitdir::discover_project_root_with(cwd, rules);
    let notice = (path != cwd)
        .then(|| format!("searching {path:?} (the repository that holds the current folder)"));
    Ok(ScanRoot { path, notice })
}

/// Compile the query of `flags`.
///
/// # Errors
///
/// [`UsageError`] for an empty or invalid pattern, or one past a size limit.
fn build_query(flags: &QueryFlags) -> Result<(ScanQuery, QueryInfo), UsageError> {
    let compile =
        |spec: MatcherSpec<'_>| Matcher::new(spec).map_err(|e| UsageError::new(e.to_string()));
    match &flags.shape {
        QueryShape::Pattern { text, identifier } => {
            let kind = if flags.regex {
                PatternKind::Regex
            } else {
                PatternKind::Literal
            };
            let matcher = compile(MatcherSpec {
                pattern: text,
                kind,
                case: flags.case,
                word: flags.word,
            })?;
            let mode = match (flags.regex, flags.word) {
                (true, _) => MatchMode::Regex,
                (false, true) => MatchMode::Word,
                (false, false) => MatchMode::Literal,
            };
            let info = QueryInfo {
                text: text.clone(),
                words: Vec::new(),
                mode,
                ignore_case: matcher.is_case_insensitive(),
            };
            let query = ScanQuery {
                plan: QueryPlan::Pattern(matcher),
                identifier: *identifier,
            };
            Ok((query, info))
        }
        QueryShape::Words(words) => {
            // One case rule for the whole phrase, as `rg` reads smart case.
            let phrase = words.join(" ");
            let insensitive = flags.case.is_insensitive(&phrase);
            let case = if insensitive {
                CaseMode::Insensitive
            } else {
                CaseMode::Sensitive
            };
            let matchers = words
                .iter()
                .map(|word| {
                    compile(MatcherSpec {
                        pattern: word,
                        kind: PatternKind::Literal,
                        case,
                        word: flags.word,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let info = QueryInfo {
                text: phrase,
                words: words.clone(),
                mode: MatchMode::Words,
                ignore_case: insensitive,
            };
            let query = ScanQuery {
                plan: QueryPlan::Words(matchers),
                identifier: false,
            };
            Ok((query, info))
        }
    }
}

/// The output form of `flags`.
fn format_of(flags: &QueryFlags) -> Format {
    match (flags.list, flags.json) {
        (ListMode::Files, _) => Format::FilesWithMatches,
        (ListMode::Counts, _) => Format::Counts,
        (ListMode::Lines, true) => Format::Json,
        (ListMode::Lines, false) => Format::Text,
    }
}

/// What a finished query leaves to print.
struct Outcome {
    rendered: Rendered,
    matched: bool,
}

/// Run a query in the current folder and print the result.
fn run_query(flags: &QueryFlags) -> ExitCode {
    let started = Instant::now();
    let cwd = match std::env::current_dir().and_then(|dir| dir.canonicalize()) {
        Ok(cwd) => cwd,
        Err(e) => {
            return fail(&format!("cannot read the current folder: {e}"), flags.json);
        }
    };
    let root = match resolve_scan_root(flags.root.as_deref(), &cwd, &gitdir::RootRules::from_env())
    {
        Ok(root) => root,
        Err(e) => return fail(e.message(), flags.json),
    };
    if let Some(notice) = &root.notice
        && write_line_to_stderr(&format!("skim search: {notice}"))
            .is_ok_and(|status| status == StdoutStatus::PipeClosed)
    {
        return pipe_closed_exit();
    }
    match search(flags, &root.path, started) {
        Ok(outcome) => emit(&outcome),
        Err(e) => fail(e.message(), flags.json),
    }
}

/// List, filter, scan and render. Reads files; prints nothing.
///
/// # Errors
///
/// [`UsageError`] when the pattern, a filter or the git index is unusable, or
/// the scan cannot start.
fn search(flags: &QueryFlags, root: &Path, started: Instant) -> Result<Outcome, UsageError> {
    let (query, info) = build_query(flags)?;
    let filter = filter::PathFilter::new(root, &flags.globs, &flags.types)
        .map_err(|e| UsageError::new(e.to_string()))?;

    let stage = Instant::now();
    let mut listing = universe::list_universe(root).map_err(|e| UsageError::new(e.to_string()))?;
    let listed = listing.candidates.len();
    listing.retain(|rel| filter.keeps(rel));
    crate::debug_log!(
        "skim search [debug]: list: {} of {listed} files kept in {:?}",
        listing.candidates.len(),
        stage.elapsed()
    );

    let limits = Limits::default();
    let config = ScanConfig {
        root,
        query: &query,
        limits,
        context: flags.context,
    };

    let stage = Instant::now();
    let counted = scan::count(&config, listing).map_err(|e| UsageError::new(e.to_string()))?;
    crate::debug_log!("skim search [debug]: read and match: {:?}", stage.elapsed());

    let stage = Instant::now();
    let output = match flags.list {
        ListMode::Lines => counted.label(),
        ListMode::Files | ListMode::Counts => counted.into_counts(),
    };
    crate::debug_log!(
        "skim search [debug]: parse and label: {:?} ({} files parsed, {} bytes)",
        stage.elapsed(),
        output.cost.files_parsed,
        output.cost.bytes_parsed
    );

    let identifier = matches!(
        &flags.shape,
        QueryShape::Pattern {
            identifier: true,
            ..
        }
    );
    let layout = Layout {
        all: flags.all || !identifier,
        full: flags.full,
        context: flags.context,
        limit: flags.limit,
        offset: flags.offset,
        limits,
    };
    let report = Report {
        query: &info,
        scan: &output,
        layout,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
    };
    let stage = Instant::now();
    let rendered = render::render(&report, format_of(flags));
    crate::debug_log!(
        "skim search [debug]: rank and render: {:?}",
        stage.elapsed()
    );
    Ok(Outcome {
        rendered,
        matched: output.total_lines() > 0,
    })
}

/// Print the result: stdout first, then the notes on stderr. Returns the exit
/// code of the query.
fn emit(outcome: &Outcome) -> ExitCode {
    if !outcome.rendered.stdout.is_empty() {
        match write_to_stdout(&outcome.rendered.stdout) {
            Ok(StdoutStatus::Written) => {}
            Ok(StdoutStatus::PipeClosed) => return pipe_closed_exit(),
            Err(e) => return fail_without_stdout(&format!("cannot write the result: {e}")),
        }
    }
    for line in &outcome.rendered.stderr {
        match write_line_to_stderr(line) {
            Ok(StdoutStatus::Written) => {}
            Ok(StdoutStatus::PipeClosed) => return pipe_closed_exit(),
            Err(e) => return fail_without_stdout(&format!("cannot write a note: {e}")),
        }
    }
    if outcome.matched {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Stub analytics config for tests — analytics disabled, no cost override.
    const TEST_ANALYTICS: crate::analytics::AnalyticsConfig = crate::analytics::AnalyticsConfig {
        enabled: false,
        input_cost_per_mtok: None,
        session_id: None,
    };

    #[test]
    fn test_search_help_returns_success() {
        let result = run(&[], &TEST_ANALYTICS).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_search_help_flag_returns_success() {
        let result = run(&["--help".to_string()], &TEST_ANALYTICS).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }

    #[test]
    fn test_search_short_help_flag_returns_success() {
        let result = run(&["-h".to_string()], &TEST_ANALYTICS).unwrap();
        assert_eq!(result, ExitCode::SUCCESS);
    }
}
