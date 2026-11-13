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
//! - `gitdir` — project-root discovery
//!
//! The index commands (`--build`, `--rebuild`, `--update`, `--stats`,
//! `--install-hooks`, `--remove-hooks`) still run on the old index code:
//! `walk`, `manifest`, `index`, `query`, `snippet`, `staleness`, `hooks`.

mod args;
mod build_lock;
mod classifier;
mod field;
mod fields;
mod filter;
mod gitdir;
pub(crate) mod hooks;
mod index;
mod manifest;
mod matcher;
mod query;
mod rank;
mod render;
mod roles;
#[cfg(test)]
#[path = "run_tests.rs"]
mod run_tests;
mod scan;
#[cfg(test)]
mod snippet;
mod staleness;
mod types;
mod universe;
mod walk;

use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use args::{IndexCommand, ListMode, QueryFlags, QueryShape, SearchAction, UsageError};
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
/// - `skim search --build | --rebuild | --update | --stats | --install-hooks |
///   --remove-hooks` — the commands of the old index
/// - A removed flag (`--ast`, `--hot`, `--cold`, `--risky`, `--blast-radius`,
///   `--weights`, `--phrase`, `--near`, `--lang`, `-j`) — refuse with exit 2
///   and name the replacement
/// - No args / `--help` / `-h` — print help
///
/// A query records no analytics: `analytics` serves only the old index
/// commands.
///
/// Exit codes of a query: 0 a line matched, 1 no line matched, 2 a usage or
/// input error (nothing partial is printed), 141 when the reader of stdout
/// closed the pipe.
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    match args::parse_flags(args) {
        Ok(SearchAction::Help) => Ok(print_help()),
        Ok(SearchAction::Removed(message)) => {
            // A removed flag never writes stdout, with or without `--json`.
            Ok(fail_without_stdout(&message))
        }
        Ok(SearchAction::Index {
            command,
            root,
            json,
        }) => run_index_command(command, &root, json, analytics),
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

/// Run a command of the old index.
fn run_index_command(
    command: IndexCommand,
    root: &Option<PathBuf>,
    json: bool,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    match command {
        IndexCommand::Build => run_build(false, root, analytics),
        IndexCommand::Rebuild => run_build(true, root, analytics),
        IndexCommand::Update => run_update(root, analytics),
        IndexCommand::Stats => run_stats(json, root),
        IndexCommand::InstallHooks => run_install_hooks(root),
        IndexCommand::RemoveHooks => run_remove_hooks(root),
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

// ============================================================================
// Shared project-root + cache-dir resolution
// ============================================================================

fn resolve_root_and_cache(root_override: &Option<PathBuf>) -> anyhow::Result<(PathBuf, PathBuf)> {
    let root = match root_override {
        // AD-400-1: `--root` is validated up-front at this single funnel so a
        // non-existent or non-directory value FAILS LOUD (skim's "fail loud with
        // actionable messages" invariant; #400) BEFORE resolve_search_cache_dir or
        // any create_dir_all runs — hence NO cache directory is created for a garbage
        // root. The former silent `.unwrap_or_else(|_| r.clone())` let a bogus root
        // reach resolve_search_cache_dir's AD-381-2 lexical fallback, index 0 files,
        // and return "no results" with exit 0. `canonicalize()` rejects a missing
        // path (its io::Error carries no path, so we prepend --root + the spelling);
        // the explicit `is_dir()` guard additionally rejects an existing *file* root,
        // since canonicalize() succeeds for files. Both bail → exit 1 (dispatch maps
        // anyhow::Err → ExitCode::FAILURE; exit 2 is reserved for the parse path).
        Some(r) => {
            let canonical = r.canonicalize().map_err(|e| {
                // AD-412-4: use {:?} (Debug, quoted) on the raw user-supplied path so
                // ANSI-escape or newline bytes in a crafted `--root` value cannot clear
                // the terminal or forge log lines in AI-agent output (same pattern as
                // the unrecognised-flag and mixed-form error paths).
                anyhow::anyhow!("--root {:?}: {e}. Pass the directory to index.", r)
            })?;
            anyhow::ensure!(
                canonical.is_dir(),
                // Canonical path is filesystem-derived (post-canonicalize), but apply
                // the same {:?} guard for consistency with the AD-412-4 hardening.
                "--root {:?} is not a directory. Pass the directory to index.",
                canonical
            );
            canonical
        }
        None => {
            let cwd = std::env::current_dir()?;
            walk::discover_project_root(&cwd)?
        }
    };
    let cache_dir = index::resolve_search_cache_dir(&root)?;
    Ok((root, cache_dir))
}

// ============================================================================
// --build / --rebuild
// ============================================================================

fn run_build(
    force: bool,
    root_override: &Option<PathBuf>,
    _analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;
    std::fs::create_dir_all(&cache_dir)?;
    let config = types::IndexConfig {
        root: root.clone(),
        max_files: None,
        force,
        cache_dir_override: Some(cache_dir.clone()),
    };
    let result = index::build_index(&config)?;
    eprintln!(
        "skim search: indexed {} files ({} skipped, {} cache hits) in {:.1}s",
        result.file_count,
        result.skipped,
        result.cache_hits,
        result.duration.as_secs_f64(),
    );

    // AD-395-6: emit the bounded, stable-key-sorted skip sample to stderr so
    // previously-silent skips are observable (PF-012 determinism: sample is
    // sorted by path string ascending, CapReached last, from build_skip_sample).
    if !result.skip_sample.is_empty() {
        // Show up to 10 named skip reasons; remainder surfaced as "...and N more".
        // Pass result.skipped (the exact uncapped total) so N reflects the true
        // remainder, not just the bounded sample length (AD-395-2).
        let sample_display =
            index::format_skip_sample(&result.skip_sample, 10, result.skipped as usize);
        eprintln!("{sample_display}");
    }

    // AD-405-7 / AC-405-17: emit AST coverage notice after an explicit build or
    // rebuild (D-4 cadence).  `result.ast_coverage` was computed in index.rs
    // before `new_manifest.save()` — zero extra I/O (AC-405-12).
    query::emit_ast_coverage_notice(&result.ast_coverage);

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// --update
// ============================================================================

fn run_update(
    root_override: &Option<PathBuf>,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;
    std::fs::create_dir_all(&cache_dir)?;
    let (outcome, manifest) = staleness::auto_refresh_if_stale(&root, &cache_dir, analytics)?;
    if !outcome.refreshed() {
        eprintln!("skim search: index is current");
    } else {
        // AD-405-7 / AC-405-17: emit AST coverage notice after --update refreshes
        // the index (D-4 cadence).  Manifest is the post-refresh state.
        query::emit_ast_coverage_notice(&manifest.ast_coverage());
    }
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// --stats
// ============================================================================

#[allow(clippy::disallowed_methods)] // Search stats output; streaming BufWriter for potentially large index statistics
fn run_stats(json: bool, root_override: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let (root, cache_dir) = resolve_root_and_cache(root_override)?;

    // AD-381-1: surface the resolved search cache directory so the (otherwise
    // hidden) on-disk location is discoverable from `--stats` alone. Computed
    // once here and reused by both the no-index early-return and the populated
    // branch below, in both text and JSON modes.
    let cache_dir_display = cache_dir.display().to_string();

    // AD-414-23: `--stats` REPORTS on an index; it never creates one.
    //
    // The AC-14 self-heal below repairs an index that exists but is structurally
    // broken (F-Body-A/B/C) — a repair whose cost is bounded by work the user has
    // already paid for once. A cold start is a different thing: building from
    // scratch can take minutes on a large corpus, and doing it silently because
    // someone asked "what is in my index?" is the opposite of "fail loud with
    // actionable messages". So this arm keeps the #413 AC21 contract verbatim —
    // `{"error":"no index found","cache_dir":"<path>"}` on stdout in JSON mode,
    // the two guidance lines on stderr in text mode, exit 1 in both — which the
    // #414 plan (Step 11 / C11) treats as the reference shape when it normalises
    // `build_stats_json`'s own early return to include `cache_dir`.
    //
    // Consequence for the `.staleness` contract (F5): `check_staleness` returns
    // `StalenessCheck::NoIndex` on exactly this condition (`staleness.rs`, "Cold
    // start: no lexical index file"), so `--stats` can never report its `Display`
    // form `"no index"` — the gather never runs. The reportable value set is the
    // other four; CLAUDE.md and the CHANGELOG entry are corrected to match.
    // (A concurrent deleter racing between this probe and `gather_stats` could
    // still produce it; that is a TOCTOU window, not a contract.)
    let index_path = cache_dir.join("index.skidx");
    if !index_path.exists() {
        if json {
            // AC7: single parseable object retaining `error`, plus `cache_dir`
            // (the "where would it go?" path). Exit FAILURE is unchanged.
            let no_index = serde_json::json!({
                "error": "no index found",
                "cache_dir": cache_dir_display,
            });
            println!("{}", serde_json::to_string(&no_index)?);
        } else {
            // AC5: print the resolved cache-dir path even with no index, in
            // addition to the existing guidance. Exit FAILURE is unchanged.
            eprintln!("skim search: no index found — run `skim search --build` first");
            eprintln!("  cache dir     : {cache_dir_display}");
        }
        return Ok(ExitCode::FAILURE);
    }

    let mut out = BufWriter::new(std::io::stdout());

    // AC6/AC11/AD-414-10: gather_stats performs the self-heal (AC-14), snapshots
    // pre-refresh state (AC-15, AC-22), and collects all index data in one pass.
    // Both the text branch and the JSON branch below read from the same snapshot,
    // eliminating the git_head divergence fixed by Finding [high/architecture]:
    // the previous code gave each branch its own copy of the 5-step gather sequence
    // and they diverged on git_head (text: post-heal manifest, JSON: pre-heal probe).
    //
    // AC-13 / T-13: if gather_stats returns Err (CRC-level corruption the structural
    // probe cannot detect), emit a structured JSON error and exit FAILURE in --json mode;
    // propagate the error in text mode.
    let snapshot = match gather_stats(&cache_dir, &root) {
        Ok(s) => s,
        Err(e) => {
            if json {
                let err_json = serde_json::json!({
                    "error": format!("{e:#}"),
                    "cache_dir": cache_dir_display,
                });
                writeln!(out, "{}", serde_json::to_string_pretty(&err_json)?)?;
                out.flush()?;
                return Ok(ExitCode::FAILURE);
            } else {
                return Err(e);
            }
        }
    };

    if json {
        // AC6/AC11: shared JSON serialisation via build_stats_json — same code path
        // as the test helper (stats_json_for_test) so AC11 back-compat tests guard
        // the production JSON shape. AD-395-6: `skipped`/`skipped_by_reason` additive.
        let extended = build_stats_json(&snapshot)?;
        writeln!(out, "{}", serde_json::to_string_pretty(&extended)?)?;
    } else {
        // Text rendering from the shared snapshot.
        // AD-380-4: the TRUE total over all on-disk artifacts (AC-6/AC-7).
        writeln!(out, "skim search index stats:")?;
        writeln!(out, "  files indexed : {}", snapshot.file_count)?;
        writeln!(out, "  total n-grams : {}", snapshot.total_ngrams)?;
        writeln!(
            out,
            "  index size    : {} bytes (lexical)",
            snapshot.index_size_bytes
        )?;
        writeln!(out, "  total on disk : {} bytes", snapshot.total_on_disk)?;
        if let Some(ts) = snapshot.last_updated {
            writeln!(out, "  last updated  : {ts}")?;
        }
        // AC-22 / AD-414-10: git_head is the PRE-REFRESH snapshot (same as JSON).
        writeln!(
            out,
            "  git HEAD      : {}",
            snapshot.pre_refresh_git_head.as_deref().unwrap_or("(none)")
        )?;
        writeln!(out, "  staleness     : {}", snapshot.staleness_status)?;
        // AC4: resolved cache dir.
        writeln!(out, "  cache dir     : {}", snapshot.cache_dir_display)?;
        // AD-395-6: skip counts by reason (text mode).
        // PF-012: BTreeMap gives stable sorted order consistent with JSON path.
        if !snapshot.skip_entries.is_empty() {
            let mut by_reason: std::collections::BTreeMap<&str, u64> =
                std::collections::BTreeMap::new();
            for e in &snapshot.skip_entries {
                *by_reason.entry(e.reason_label()).or_insert(0) += 1;
            }
            writeln!(
                out,
                "  skipped       : {} (content-skipped files)",
                snapshot.skip_entries.len()
            )?;
            for (reason, count) in &by_reason {
                writeln!(out, "    {reason}: {count}")?;
            }
        }
        // AD-405-7 / AC-405-9 / AC-405-15: AST size-coverage section (D-4 cadence).
        // Omit when clean (is_clean() == true) — byte-identical to the pre-fix binary
        // on a corpus with zero excluded / zero undetermined files (AC-405-15).
        if !snapshot.ast_coverage.is_clean() {
            let coverage = &snapshot.ast_coverage;
            writeln!(out, "  ast eligible  : {}", coverage.size_eligible_files)?;
            if coverage.size_excluded_files > 0 {
                writeln!(
                    out,
                    "  ast excluded  : {} (exceed 1 MiB cap)",
                    coverage.size_excluded_files
                )?;
                // PF-012: excluded_by_lang is a BTreeMap — already sorted.
                for (lang, count) in &coverage.excluded_by_lang {
                    writeln!(out, "    {lang}: {count}")?;
                }
            }
            if coverage.undetermined_files > 0 {
                // "ast no-size" (11 chars) + 3 spaces = 14-char label field;
                // colon at column 16 — aligned with all other stats lines.
                writeln!(out, "  ast no-size   : {}", coverage.undetermined_files)?;
            }
        }
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// The fixed set of on-disk index artifacts summed by [`total_on_disk_bytes`].
///
/// AD-380-4 (#380 / AC-6, AC-7): a FIXED, known filename list — `--stats` MUST
/// NOT recursively walk the cache directory. Each artifact is stat'd via
/// `metadata().len()` and a missing one counts as 0 bytes (fail-soft, AC-7).
/// Adding a new index artifact means extending this list (one source of truth).
const ON_DISK_ARTIFACTS: [&str; 6] = [
    "index.skidx",       // lexical n-gram index
    "index.skpost",      // lexical posting lists
    "index.skfiles",     // binary file manifest (this ticket)
    "ast_index.skidx",   // AST n-gram index header + metadata
    "ast_index.skpost",  // AST posting lists
    "ast_index.skcache", // AST extraction cache
];

/// Return the byte length of one artifact in `cache_dir`, or 0 when it is absent
/// or unreadable (fail-soft, AC-7).
fn artifact_len(cache_dir: &std::path::Path, name: &str) -> u64 {
    std::fs::metadata(cache_dir.join(name))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// Sum the on-disk byte size of every present index artifact in `cache_dir`.
///
/// AD-380-4 (#380): reports the TRUE on-disk footprint of the search index. The
/// previous `--stats` "index size" line reported only the lexical skidx+skpost
/// pair, undercounting the real footprint. This iterates a FIXED filename list
/// via `metadata()` (O(1), no directory walk, AC-7) and treats a missing
/// artifact as 0 bytes so partial indexes (e.g. a lexical-only build before AST
/// ran) report exactly the sum of the files that exist (AC-7).
fn total_on_disk_bytes(cache_dir: &std::path::Path) -> u64 {
    ON_DISK_ARTIFACTS
        .iter()
        .map(|name| artifact_len(cache_dir, name))
        .sum()
}

// ============================================================================
// Stats snapshot (Finding [high/architecture] + [medium/architecture] fix)
// ============================================================================

/// All data needed to render `--stats` output, gathered in a single pass by
/// [`gather_stats`].
///
/// Both text and JSON branches of [`run_stats`] render from this snapshot,
/// ensuring they describe the same invocation state (Finding [high/architecture]:
/// `--stats` text and `--stats --json` previously reported different `git_head`
/// values for the same invocation because the text branch read git_head from the
/// post-heal manifest while the JSON branch snapshotted it pre-heal).
struct StatsGathered {
    /// git HEAD as stored in the manifest BEFORE any self-heal (AC-22 / AD-414-10).
    ///
    /// Derived from a header-only probe ([`FileManifest::load_git_head`]) so the
    /// second full manifest decode that `check_staleness` already performs inside
    /// `auto_refresh_if_stale` is avoided (Finding [medium/performance] fix).
    pre_refresh_git_head: Option<String>,
    /// Live git HEAD resolution state for the `git_head_state` JSON key (AC-20).
    git_head_state_str: &'static str,
    /// Staleness classification at INVOCATION TIME — the verdict from
    /// [`staleness::check_staleness`] run before `auto_refresh_if_stale` so
    /// `--stats --json` `.staleness` and the text `staleness` line report whether
    /// the index was stale when the user ran the command (AC-11 /
    /// Finding [medium/regression]: the post-heal value is always `Current` and
    /// therefore non-informative).
    staleness_status: staleness::StalenessCheck,
    // --- Index stats from the lexical reader (post-heal) ---
    file_count: u32,
    total_ngrams: u64,
    index_size_bytes: u64,
    last_updated: Option<u64>,
    total_on_disk: u64,
    // --- Manifest-derived data (post-heal) ---
    skip_entries: Vec<types::SkippedEntry>,
    ast_coverage: rskim_search::AstCoverage,
    // --- Display string (avoids threading cache_dir into build_stats_json) ---
    cache_dir_display: String,
}

/// Gather all data needed by [`run_stats`] in one pass (repair → collect).
///
/// Implements the repair-then-report pattern (Finding [medium/architecture] CQS fix):
/// the self-heal (`auto_refresh_if_stale`) and the data collection live here so
/// [`build_stats_json`] is a pure serialiser with no I/O side effects.
fn gather_stats(
    cache_dir: &std::path::Path,
    root: &std::path::Path,
) -> anyhow::Result<StatsGathered> {
    let cache_dir_display = cache_dir.display().to_string();

    // Finding [medium/performance] fix: header-only probe for the pre-refresh HEAD.
    // `auto_refresh_if_stale` internally calls `check_staleness → FileManifest::load`
    // which fully decodes the manifest (60 k-node BTreeMap + 60 k String allocations
    // for a max-size index). Decoding a second full copy here — as the previous code
    // did in `build_stats_json` — doubles that cost for one header field. The probe
    // reads the same bytes but decodes only the fixed header block via `decode_header`
    // and returns just the git_head string; the entry table is never materialised.
    let pre_refresh_git_head = manifest::FileManifest::load_git_head(root, cache_dir);

    let git_head_state_str = match staleness::git_head_state(root) {
        staleness::HeadState::Resolved(_) => "resolved",
        staleness::HeadState::Unresolved => "unresolved",
        staleness::HeadState::NotARepo => "not_a_repo",
    };

    // AC-14: self-heal structural corruption before opening the reader.
    // Finding [medium/architecture] CQS fix: repair lives here, not in
    // `build_stats_json`, so the reporting function has no I/O side effects.
    let noop_analytics = crate::analytics::AnalyticsConfig {
        enabled: false,
        input_cost_per_mtok: None,
        session_id: None,
    };
    // Pre-heal staleness: snapshot the invocation-time verdict BEFORE
    // auto_refresh_if_stale runs, so --stats --json `.staleness` and the text
    // `staleness` line reflect whether the index WAS stale when the user ran the
    // command (AC-11 / Finding [medium/regression]).  auto_refresh_if_stale calls
    // check_staleness internally; the extra call here is acceptable for the
    // infrequent --stats path.  The Option<FileManifest> returned by check_staleness
    // is discarded — auto_refresh_if_stale loads the authoritative post-heal manifest.
    let (staleness_status, _pre_check_manifest) = staleness::check_staleness(cache_dir, root);

    let (_, loaded_manifest) = staleness::auto_refresh_if_stale(root, cache_dir, &noop_analytics)?;

    // AD-414-7: opens successfully for F-Body-A/B/C after the self-heal above.
    // Only CRC-level corruption not detectable by the structural probe propagates
    // as Err here (T-13); the caller (`run_stats`) maps it to a structured error.
    let reader = query::open_lexical_reader(cache_dir)?;
    let stats = reader.stats();

    let total_on_disk = total_on_disk_bytes(cache_dir);

    // Collect skip and coverage data from the post-heal manifest.
    let skip_entries: Vec<types::SkippedEntry> = loaded_manifest.skipped().cloned().collect();
    let ast_coverage = loaded_manifest.ast_coverage();

    Ok(StatsGathered {
        pre_refresh_git_head,
        git_head_state_str,
        staleness_status,
        file_count: stats.file_count,
        total_ngrams: stats.total_ngrams,
        index_size_bytes: stats.index_size_bytes,
        last_updated: stats.last_updated,
        total_on_disk,
        skip_entries,
        ast_coverage,
        cache_dir_display,
    })
}

// ============================================================================
// --install-hooks / --remove-hooks
// ============================================================================

fn run_install_hooks(root_override: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let (root, _) = resolve_root_and_cache(root_override)?;
    // AD-413-15: install_search_hooks resolves the shared hooks dir and emits
    // the AC34(b) shared-scope notice automatically; the returned `dir` names
    // what was actually touched (may differ from `<root>/.git/hooks`).
    let outcome = hooks::install_search_hooks(&root)?;
    // Normalize to remove any `..` components that arise from a relative `gitdir:`
    // pointer (submodules write e.g. `gitdir: ../.git/modules/sub`).
    let display_dir = outcome
        .dir
        .canonicalize()
        .unwrap_or_else(|_| outcome.dir.clone());
    eprintln!("skim search: git hooks installed in {:?}", display_dir);
    Ok(ExitCode::SUCCESS)
}

fn run_remove_hooks(root_override: &Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let (root, _) = resolve_root_and_cache(root_override)?;
    // AD-413-15 / AC31(a): remove_search_hooks resolves the shared dir,
    // emits the AC34(b) shared-scope notice when applicable, and returns the
    // resolved dir.  Only print "removed from" when a marker block was actually
    // removed (`outcome.changed`).
    let outcome = hooks::remove_search_hooks(&root)?;
    if outcome.changed {
        // Normalize to remove any `..` components (submodule gitdir pointers).
        let display_dir = outcome
            .dir
            .canonicalize()
            .unwrap_or_else(|_| outcome.dir.clone());
        eprintln!("skim search: git hooks removed from {:?}", display_dir);
    }
    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Shared JSON stats builder
// ============================================================================

/// Build the `--stats --json` value used by both [`run_stats`] and the test helper.
///
/// Pure serialiser (Finding [medium/architecture] CQS fix): all data is pre-gathered
/// by [`gather_stats`], which performs the self-heal and takes the pre-refresh
/// snapshots.  This function has **no I/O side effects**.
///
/// Shared so that AC11 back-compat tests guard the actual production code path
/// rather than a hand-duplicated reimplementation.  `run_stats` calls this in its
/// JSON branch; `stats_json_for_test` runs the same `gather_stats` + serialise pair.
///
/// AD-413-13: `git_head` is the manifest's stored HEAD ("HEAD-at-last-build", read
/// by [`manifest::FileManifest::load_git_head`]) while `git_head_state` is the live
/// HEAD state resolved at invocation time.  The two keys can legitimately diverge —
/// a frozen linked worktree before its first post-fix rebuild produces
/// `{"git_head": null, "git_head_state": "resolved"}`, which is a legal transient
/// state, not a contradiction.
///
/// AD-395-6: `skipped` / `skipped_by_reason` are additive keys (AC11 back-compat).
/// `skipped_by_reason` uses `BTreeMap` for byte-stable key order (PF-012).
///
/// **Snapshot asymmetry (AD-414-10 / AC-22):** `git_head` and `staleness` are
/// PRE-heal snapshots from `snapshot`; all other fields reflect the post-heal state.
fn build_stats_json(snapshot: &StatsGathered) -> anyhow::Result<serde_json::Value> {
    let skipped_arr: Vec<serde_json::Value> = snapshot
        .skip_entries
        .iter()
        .map(|e| serde_json::json!({ "path": e.path, "reason": e.reason_label() }))
        .collect();
    // PF-012: BTreeMap gives deterministic (sorted) key order in JSON output,
    // consistent with the alphabetical sort used in the text-mode path.
    let mut skipped_by_reason: std::collections::BTreeMap<&str, u64> =
        std::collections::BTreeMap::new();
    for e in &snapshot.skip_entries {
        *skipped_by_reason.entry(e.reason_label()).or_insert(0) += 1;
    }
    // AD-405-9 / AC-405-9 / AC-405-15: `ast_coverage` is additive (never replaces
    // existing keys) and OMITTED when clean (is_clean() == true).
    let ast_coverage_val: Option<serde_json::Value> = if snapshot.ast_coverage.is_clean() {
        None
    } else {
        Some(
            serde_json::to_value(&snapshot.ast_coverage)
                .map_err(|e| anyhow::anyhow!("ast_coverage serialization error: {e}"))?,
        )
    };
    let mut result = serde_json::json!({
        "file_count": snapshot.file_count,
        "total_ngrams": snapshot.total_ngrams,
        "index_size_bytes": snapshot.index_size_bytes,
        "total_on_disk_bytes": snapshot.total_on_disk,
        "last_updated": snapshot.last_updated,
        "git_head": snapshot.pre_refresh_git_head,
        "git_head_state": snapshot.git_head_state_str,
        "staleness": snapshot.staleness_status.to_string(),
        "cache_dir": snapshot.cache_dir_display,
        "skipped": skipped_arr,
        "skipped_by_reason": skipped_by_reason,
    });
    // Insert ast_coverage only when non-clean (omit-when-clean AC-405-9/15).
    if let Some(val) = ast_coverage_val {
        result["ast_coverage"] = val;
    }
    Ok(result)
}

// ============================================================================
// Test helpers (cfg(test) only — not compiled into production builds)
// ============================================================================

/// Delegate to [`gather_stats`] + [`build_stats_json`] so AC4/AC11 tests cover
/// the production code path rather than a hand-duplicated reimplementation.
///
/// Used by `index_tests.rs` for:
/// - AC4 (#395): assert `skipped` array / `skipped_by_reason` JSON fields.
/// - AC11 (#395): assert all pre-existing keys survive with unchanged types.
///
/// Runs the same gather+serialise sequence as the production `run_stats --json`
/// path.
#[cfg(test)]
pub(crate) fn stats_json_for_test(
    cache_dir: &std::path::Path,
    root: &std::path::Path,
) -> anyhow::Result<serde_json::Value> {
    let snapshot = gather_stats(cache_dir, root)?;
    build_stats_json(&snapshot)
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

    /// Locate the built `skim` binary for subprocess-level tests.
    ///
    /// Cargo sets `CARGO_BIN_EXE_skim` only for integration tests, so these
    /// in-source unit tests walk up from `current_exe()` to the first ancestor
    /// that holds `skim`. The walk covers both cargo layouts for the test binary:
    /// `target/<profile>/deps/skim-<hash>` and, from cargo 1.100,
    /// `target/<profile>/build/rskim/<hash>/out/skim-<hash>`. It stops at the
    /// cargo target dir (the folder with `CACHEDIR.TAG`), so a `skim` file
    /// outside the build output is never used.
    ///
    /// PRECONDITION: the `skim` binary is built from the current tree
    /// (`cargo test -p rskim --bins`, `--all-targets`, or `cargo build -p rskim`
    /// before nextest). A stale binary runs old code and can give a false pass.
    pub(super) fn skim_bin_path() -> String {
        if let Ok(bin) = std::env::var("CARGO_BIN_EXE_skim") {
            return bin;
        }
        let exe = std::env::current_exe().expect("test binary path must resolve");
        let name = format!("skim{}", std::env::consts::EXE_SUFFIX);
        for dir in exe.ancestors().skip(1) {
            let candidate = dir.join(&name);
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
            if dir.join("CACHEDIR.TAG").is_file() {
                break;
            }
        }
        panic!(
            "no built `{name}` between {} and the cargo target dir; \
             run `cargo build -p rskim` first",
            exe.display()
        )
    }

    // ========================================================================
    // --stats total-on-disk size (#380, AC-6 / AC-7)
    // ========================================================================

    /// AC-7 (#380): with ONLY the three lexical files present, the reported total
    /// equals EXACTLY their summed sizes (falsifiable) — missing artifacts count
    /// as 0, no error.
    #[test]
    fn test_total_on_disk_lexical_only_sums_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();

        std::fs::write(cache_dir.join("index.skidx"), vec![0u8; 100]).unwrap();
        std::fs::write(cache_dir.join("index.skpost"), vec![0u8; 250]).unwrap();
        std::fs::write(cache_dir.join("index.skfiles"), vec![0u8; 30]).unwrap();
        // No AST/temporal artifacts present.

        let total = total_on_disk_bytes(cache_dir);
        assert_eq!(
            total, 380,
            "total must equal the sum of the three present files (100+250+30), missing=0 (AC-7)"
        );
    }

    /// AC-6 (#380): when AST artifacts also exist, the total includes them — it
    /// MUST NOT report lexical-only.
    #[test]
    fn test_total_on_disk_includes_all_artifacts() {
        let dir = tempfile::tempdir().unwrap();
        let cache_dir = dir.path();

        let sizes = [
            ("index.skidx", 10u64),
            ("index.skpost", 20),
            ("index.skfiles", 5),
            ("ast_index.skidx", 7),
            ("ast_index.skpost", 11),
            ("ast_index.skcache", 13),
        ];
        for (name, n) in &sizes {
            std::fs::write(cache_dir.join(name), vec![0u8; *n as usize]).unwrap();
        }
        let expected: u64 = sizes.iter().map(|(_, n)| n).sum();
        let lexical_only: u64 = 10 + 20 + 5;

        let total = total_on_disk_bytes(cache_dir);
        assert_eq!(total, expected, "total must sum all 6 artifacts (AC-6)");
        assert!(
            total > lexical_only,
            "total ({total}) must exceed lexical-only ({lexical_only}) when AST artifacts exist (AC-6 negative)"
        );
    }

    /// AC-7 (#380): an empty cache dir (no artifacts) reports 0, never an error.
    #[test]
    fn test_total_on_disk_empty_dir_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            total_on_disk_bytes(dir.path()),
            0,
            "no artifacts → 0 bytes (AC-7)"
        );
    }

    /// AC-7 (#380): `artifact_len` fail-soft — a missing file is 0 bytes.
    #[test]
    fn test_artifact_len_missing_is_zero() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(artifact_len(dir.path(), "index.skidx"), 0);
    }

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

    /// Shared git-repo helper — delegates to the canonical `staleness::create_real_git_repo`.
    /// Named identically to its counterpart in `staleness_tests.rs` so readers
    /// scanning the two test files see a single shared-helper relationship rather
    /// than two apparently-distinct helpers.
    fn create_real_git_repo(
        dir: &std::path::Path,
        commit_specs: &[(&str, &[(&str, &str)])],
    ) -> String {
        staleness::create_real_git_repo(dir, commit_specs)
    }

    /// Locate the single per-root search cache directory beneath an isolated
    /// `SKIM_CACHE_DIR`, without re-reading the env var in this process.
    fn only_search_cache_dir(cache_root: &std::path::Path) -> std::path::PathBuf {
        let search_dir = cache_root.join("search");
        let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(&search_dir)
            .unwrap_or_else(|e| panic!("no search cache dir at {search_dir:?}: {e}"))
            .map(|e| e.unwrap().path())
            .filter(|p| p.is_dir())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "expected exactly one per-root cache dir under {search_dir:?}, got {entries:?}"
        );
        entries.pop().unwrap()
    }

    /// Shared helper for #402 unit tests: create a real git repo with a
    /// tracked-but-.gitignored file, used by both `walk_tests` and `index_tests`
    /// (eliminates the two near-byte-identical per-module copies that matched the
    /// per-module helper convention but lived in the SAME rskim crate).
    ///
    /// Layout after init + commit:
    /// ```text
    /// root/
    ///   .gitignore       <- contains "secretdoc.md"
    ///   secretdoc.md     <- tracked via `git add -f`; content = "ZZUNIQUETOKEN"
    ///   src/a.rs         <- regular tracked file
    /// ```
    pub(crate) fn make_tracked_ignored_repo() -> tempfile::TempDir {
        use std::process::Command as StdCmd;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        StdCmd::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .expect("git init");
        StdCmd::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(root)
            .output()
            .expect("git config email");
        StdCmd::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(root)
            .output()
            .expect("git config name");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".gitignore"), "secretdoc.md\n").unwrap();
        std::fs::write(root.join("secretdoc.md"), "ZZUNIQUETOKEN\n").unwrap();
        std::fs::write(root.join("src/a.rs"), "fn a() {}\n").unwrap();
        StdCmd::new("git")
            .args(["add", "-f", "secretdoc.md", "src/a.rs", ".gitignore"])
            .current_dir(root)
            .output()
            .expect("git add");
        StdCmd::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(root)
            .output()
            .expect("git commit");
        dir
    }

    // =========================================================================
    // Step 11: T-13/T-14 — actionable lexical error and stats self-heal
    // (AD-414-7, AD-414-10, AC-13, AC-14)
    // =========================================================================

    /// T-13 / AC-13: a size-preserving byte flip in `index.skpost` (with
    /// `index.skverify` deleted to force the full CRC32 path) MUST cause a query
    /// to exit 1 with an error message that:
    ///
    /// (1) names the artifact directory (the cache-dir path),
    /// (2) names the failing artifact (`index.skpost`),
    /// (3) contains `skim search --rebuild`,
    /// (4) contains `No such file or directory` at most once.
    ///
    /// PF-007: asserts observable error message content, not just exit code.
    /// PF-018: byte-flip preserves file length so the structural size probe passes;
    ///         only the full CRC32 detects the corruption.
    /// SKIM_CACHE_DIR isolation: #[serial] prevents concurrent env-var mutations
    /// from other in-process tests flipping the resolved cache dir between the
    /// --rebuild call and the subsequent resolve_search_cache_dir probe.
    #[serial_test::serial]
    #[test]
    fn t13_crc_mismatch_names_artifact_and_rebuild_hint() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let root_str = root.to_string_lossy().to_string();

        create_real_git_repo(
            root,
            &[
                ("feat: alpha", &[("src/alpha.rs", "fn alpha() {}")]),
                ("feat: beta", &[("src/beta.rs", "fn beta() {}")]),
            ],
        );

        // Build a full index so index.skpost exists with a valid CRC.
        let result = run(
            &[
                "--rebuild".to_string(),
                "--root".to_string(),
                root_str.clone(),
            ],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            ExitCode::SUCCESS,
            "T-13 setup: --rebuild must succeed before byte-flip"
        );

        let cache_dir = index::resolve_search_cache_dir(root).unwrap();
        let post_path = cache_dir.join("index.skpost");
        let verify_path = cache_dir.join("index.skverify");

        // Byte-flip the last byte of index.skpost, preserving file length (PF-018).
        // The structural size probe compares lengths only, so it passes; the full
        // CRC32 (which covers post_mmap + idx_mmap body) detects the corruption.
        {
            let mut bytes = std::fs::read(&post_path).expect("T-13: index.skpost must exist");
            assert!(
                !bytes.is_empty(),
                "T-13: index.skpost must be non-empty to flip a byte"
            );
            let last = bytes.last_mut().unwrap();
            *last = last.wrapping_add(1);
            std::fs::write(&post_path, &bytes).expect("T-13: rewriting index.skpost must succeed");
        }

        // Delete index.skverify so the fast-path (cached CRC signature) is bypassed
        // and the full CRC32 computation runs.
        let _ = std::fs::remove_file(&verify_path);

        let cache_dir_display = cache_dir.display().to_string();

        // Opening the index must fail with an actionable error (AC-13). A query
        // no longer opens the index; `--stats` and the index commands do.
        let err = query::open_lexical_reader(&cache_dir)
            .err()
            .expect("T-13: a byte-flipped index.skpost must not open");
        let msg = format!("{err:#}");

        // AC-13 clause 1: error names the artifact directory.
        assert!(
            msg.contains(&cache_dir_display),
            "T-13/AC-13 clause 1: error must name the artifact directory; got: {msg:?}"
        );
        // AC-13 clause 2: error names the failing artifact.
        assert!(
            msg.contains("index.skpost"),
            "T-13/AC-13 clause 2: error must name the failing artifact (index.skpost); got: {msg:?}"
        );
        // AC-13 clause 3: error contains the rebuild hint.
        assert!(
            msg.contains("skim search --rebuild"),
            "T-13/AC-13 clause 3: error must contain 'skim search --rebuild'; got: {msg:?}"
        );
        // AC-13 clause 4: OS error phrase is not duplicated.
        let no_such_file_count = msg.matches("No such file or directory").count();
        assert!(
            no_such_file_count <= 1,
            "T-13/AC-13 clause 4: 'No such file or directory' must appear at most once; \
             got {no_such_file_count} times in: {msg:?}"
        );
    }

    /// T-14 / AC-14: `--stats --json` on F-Body-A (truncated `index.skpost`) MUST
    /// exit 0 after self-healing through `auto_refresh_if_stale` → rebuild, and the
    /// returned JSON must be a complete stats object (not an error object).
    ///
    /// Covers the G-4 fix: previously `build_stats_json` opened the reader BEFORE
    /// `check_staleness`, so body-corrupt indexes never reached the self-heal path
    /// and `--stats` always exited 1 with no JSON on stdout.
    ///
    /// PF-007: asserts observable output, not just exit code.
    /// SKIM_CACHE_DIR isolation: #[serial] prevents concurrent env-var mutations
    /// from other in-process tests racing the resolve_search_cache_dir call.
    #[serial_test::serial]
    #[test]
    fn t14_stats_json_self_heals_on_fbody_a_truncated_skpost() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        // Need a real git repo so the HEAD-based staleness check also runs.
        create_real_git_repo(
            root,
            &[
                ("feat: first", &[("src/a.rs", "fn alpha() {}")]),
                ("feat: second", &[("src/b.rs", "fn beta() {}")]),
            ],
        );

        // Build the index.
        let result = run(
            &[
                "--rebuild".to_string(),
                "--root".to_string(),
                root.to_string_lossy().to_string(),
            ],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            ExitCode::SUCCESS,
            "T-14 setup: --rebuild must succeed"
        );

        let cache_dir = index::resolve_search_cache_dir(root).unwrap();
        let skpost_path = cache_dir.join("index.skpost");

        // Corrupt: truncate skpost to half its size (F-Body-A).
        let original_len = std::fs::metadata(&skpost_path).unwrap().len();
        let half_len = (original_len / 2).max(1);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&skpost_path)
            .unwrap();
        file.set_len(half_len).unwrap();

        // --stats --json must self-heal and return a complete stats object.
        let stats = stats_json_for_test(&cache_dir, root)
            .expect("T-14: stats_json_for_test must succeed after self-heal on F-Body-A");

        // Must NOT be an error object (PF-007).
        assert!(
            stats.get("error").is_none(),
            "T-14: stats after self-heal must not contain 'error'; got: {stats:?}"
        );
        assert!(
            stats.get("file_count").is_some(),
            "T-14: stats after self-heal must contain 'file_count'; got: {stats:?}"
        );

        // index.skpost must be restored to a consistent length.
        let restored_len = std::fs::metadata(&skpost_path).unwrap().len();
        assert!(
            restored_len != half_len,
            "T-14: skpost must be restored (not still half-length) after self-heal"
        );
    }

    /// T-14(b) / AC-14: `--stats` text mode on F-Body-C (removed `index.skpost`)
    /// must exit 0 after self-healing (same route as T-14 but text mode).
    /// SKIM_CACHE_DIR isolation: #[serial] prevents concurrent env-var mutations
    /// from other in-process tests racing the resolve_search_cache_dir call.
    #[serial_test::serial]
    #[test]
    fn t14b_stats_text_self_heals_on_fbody_c_removed_skpost() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();

        create_real_git_repo(
            root,
            &[
                ("feat: x", &[("src/x.rs", "fn x() {}")]),
                ("feat: y", &[("src/y.rs", "fn y() {}")]),
            ],
        );

        let root_str = root.to_string_lossy().to_string();
        let result = run(
            &[
                "--rebuild".to_string(),
                "--root".to_string(),
                root_str.clone(),
            ],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            ExitCode::SUCCESS,
            "T-14b setup: --rebuild must succeed"
        );

        let cache_dir = index::resolve_search_cache_dir(root).unwrap();
        // F-Body-C: remove skpost entirely.
        std::fs::remove_file(cache_dir.join("index.skpost")).unwrap();

        // --stats text mode: exit 0 after self-heal.
        let result = run(
            &["--stats".to_string(), "--root".to_string(), root_str],
            &TEST_ANALYTICS,
        )
        .unwrap();
        assert_eq!(
            result,
            ExitCode::SUCCESS,
            "T-14b: --stats on F-Body-C (removed skpost) must exit 0 after self-heal (AC-14)"
        );

        // skpost must be restored.
        assert!(
            cache_dir.join("index.skpost").exists(),
            "T-14b: index.skpost must be restored after self-heal"
        );
    }
    // ========================================================================
    // T-16 / F5 / AD-414-23 — the `--stats` staleness contract, via the CLI
    // ========================================================================

    /// Run `skim search <args>` as a subprocess against an isolated cache dir.
    ///
    /// Returns `(exit_success, stdout, stderr)`. Driving the real binary is what
    /// makes these tests CLI-entry tests: `--stats`' no-index early return lives
    /// in `run_stats` and is invisible to the in-process `stats_json_for_test`
    /// helper, which starts at `gather_stats` (PF-007 — the earlier revisions of
    /// T-16/T-16b asserted a value that the CLI cannot produce precisely because
    /// they entered below the arm that decides it).
    fn run_skim_search_capture(bin: &str, cache: &str, args: &[&str]) -> (bool, String, String) {
        let out = std::process::Command::new(bin)
            .arg("search")
            .args(args)
            .env("SKIM_CACHE_DIR", cache)
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .env("NO_COLOR", "1")
            .env_remove("SKIM_DEBUG")
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn skim search {args:?}: {e}"));
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    /// T-16 / F5 / AD-414-10: `--stats --json` reports `staleness` from the
    /// PRE-self-heal state while `file_count` comes from the POST-heal state.
    ///
    /// F-Body-C (remove `index.skpost`, keep `index.skidx`) is the reachable
    /// demonstration of that asymmetry through the CLI: `check_staleness` sees a
    /// size-inconsistent lexical index and returns `NoStoredHead`, the self-heal
    /// rebuilds, and the reported `file_count` counts the rebuilt corpus.
    ///
    /// The expected string comes from `StalenessCheck`'s own `Display`, never a
    /// hand-written literal, so a reworded variant fails here rather than drifting.
    ///
    /// `#[serial]` matches its T-13/T-14/T-15 siblings; the child process is given
    /// an explicit `SKIM_CACHE_DIR`, so here it is defence in depth rather than a
    /// strict requirement.
    #[serial_test::serial]
    #[test]
    fn t16_f5_stats_json_staleness_is_the_pre_heal_snapshot() {
        let bin = skim_bin_path();
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let root_str = root.to_string_lossy().to_string();
        let cache = tempfile::TempDir::new().unwrap();
        let cache_str = cache.path().to_string_lossy().to_string();

        create_real_git_repo(
            root,
            &[
                ("feat: add a", &[("src/a.rs", "fn alpha() {}")]),
                ("feat: add b", &[("src/b.rs", "fn beta() {}")]),
            ],
        );

        let (ok, _, stderr) =
            run_skim_search_capture(&bin, &cache_str, &["--rebuild", "--root", &root_str]);
        assert!(ok, "T-16 setup: --rebuild must succeed; stderr:\n{stderr}");

        let search_cache = only_search_cache_dir(cache.path());

        // Healthy precondition: staleness == "current".
        let (ok, stdout, stderr) = run_skim_search_capture(
            &bin,
            &cache_str,
            &["--stats", "--json", "--root", &root_str],
        );
        assert!(
            ok,
            "T-16 setup: --stats --json must exit 0; stderr:\n{stderr}"
        );
        let healthy: serde_json::Value = serde_json::from_str(&stdout).unwrap_or_else(|e| {
            panic!("T-16 setup: stdout must be one JSON object ({e}): {stdout}")
        });
        assert_eq!(
            healthy["staleness"].as_str().unwrap_or(""),
            staleness::StalenessCheck::Current.to_string(),
            "T-16 pre-condition: a healthy index must report staleness == 'current'"
        );

        // F-Body-C: remove the posting body, keep the header.
        let skpost = search_cache.join("index.skpost");
        std::fs::remove_file(&skpost).unwrap();

        let (ok, stdout, stderr) = run_skim_search_capture(
            &bin,
            &cache_str,
            &["--stats", "--json", "--root", &root_str],
        );
        assert!(
            ok,
            "T-16/AC-14: --stats --json must exit 0 after self-healing; stderr:\n{stderr}"
        );
        let stats: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("T-16: stdout must be one JSON object ({e}): {stdout}"));
        assert!(
            stats.get("error").is_none(),
            "T-16: a self-healed --stats must not return an error object; got {stats}"
        );
        assert_eq!(
            stats["staleness"].as_str().unwrap_or(""),
            staleness::StalenessCheck::NoStoredHead.to_string(),
            "T-16/AD-414-10: staleness is the PRE-heal verdict; got {}",
            stats["staleness"]
        );
        assert!(
            stats["file_count"].as_u64().unwrap_or(0) > 0,
            "T-16/AD-414-10: file_count is the POST-heal state and must be > 0 even \
             though staleness reports the pre-heal verdict; got {stats}"
        );
        assert!(
            skpost.exists(),
            "T-16/AC-14: index.skpost must be restored by the self-heal"
        );
    }

    /// T-16(b) / F5: the same pre-heal `staleness` verdict on the **text** arm.
    ///
    /// `run_stats`' text branch renders from the same snapshot as the JSON branch;
    /// this pins that they cannot diverge. The expected line is assembled from the
    /// production `Display` impl, not a copied literal.
    #[serial_test::serial]
    #[test]
    fn t16b_f5_stats_text_staleness_line_is_the_pre_heal_snapshot() {
        let bin = skim_bin_path();
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let root_str = root.to_string_lossy().to_string();
        let cache = tempfile::TempDir::new().unwrap();
        let cache_str = cache.path().to_string_lossy().to_string();

        create_real_git_repo(
            root,
            &[
                ("feat: add x", &[("src/x.rs", "fn ex() {}")]),
                ("feat: add y", &[("src/y.rs", "fn why() {}")]),
            ],
        );

        let (ok, _, stderr) =
            run_skim_search_capture(&bin, &cache_str, &["--rebuild", "--root", &root_str]);
        assert!(ok, "T-16b setup: --rebuild must succeed; stderr:\n{stderr}");

        let search_cache = only_search_cache_dir(cache.path());
        std::fs::remove_file(search_cache.join("index.skpost")).unwrap();

        let (ok, stdout, stderr) =
            run_skim_search_capture(&bin, &cache_str, &["--stats", "--root", &root_str]);
        assert!(
            ok,
            "T-16b/AC-14: --stats (text) must exit 0 after self-healing; stderr:\n{stderr}"
        );
        let expected = format!(
            "staleness     : {}",
            staleness::StalenessCheck::NoStoredHead
        );
        assert!(
            stdout.contains(&expected),
            "T-16b: --stats text output must carry {expected:?}; got stdout:\n{stdout}"
        );
    }

    /// T-16(d) / AD-414-23: on a root that was **never** indexed, `--stats` does
    /// not build one — it prints the `{error, cache_dir}` object and exits 1.
    ///
    /// This is #413's AC21 contract, unchanged by #414, and it is why
    /// `StalenessCheck::NoIndex`'s `Display` form `"no index"` is not among the
    /// values the `staleness` key can take: `run_stats` short-circuits the exact
    /// condition that produces it. CLAUDE.md, the CHANGELOG and the cmd-search
    /// knowledge base previously documented `"no index"` as observable here;
    /// measured false and corrected alongside this test.
    ///
    /// Discriminating in both directions: if `--stats` ever starts building a
    /// missing index, the exit code, the object shape and the "no index.skidx was
    /// created" assertion all fail together.
    #[serial_test::serial]
    #[test]
    fn t16d_ad414_23_cold_start_stats_reports_no_index_error_and_builds_nothing() {
        let bin = skim_bin_path();
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path();
        let root_str = root.to_string_lossy().to_string();
        let cache = tempfile::TempDir::new().unwrap();
        let cache_str = cache.path().to_string_lossy().to_string();

        // A healthy repository — the ONLY thing missing is the index, so the
        // no-index arm is the sole possible cause of the response.
        create_real_git_repo(root, &[("feat: add a", &[("src/a.rs", "fn alpha() {}")])]);

        // --- JSON mode ---
        let (ok, stdout, stderr) = run_skim_search_capture(
            &bin,
            &cache_str,
            &["--stats", "--json", "--root", &root_str],
        );
        assert!(
            !ok,
            "T-16d/#413 AC21: cold-start --stats --json must exit 1; stdout:\n{stdout}"
        );
        let obj: serde_json::Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("T-16d: stdout must be one JSON object ({e}): {stdout}"));
        let map = obj
            .as_object()
            .expect("T-16d: stdout must be a JSON object");
        assert_eq!(
            map.len(),
            2,
            "T-16d/#413 AC21: the object is exactly {{error, cache_dir}}; got {obj}"
        );
        assert_eq!(
            map.get("error").and_then(|v| v.as_str()),
            Some("no index found"),
            "T-16d: the error string is part of the pinned contract; got {obj}"
        );
        assert!(
            map.contains_key("cache_dir"),
            "T-16d: cache_dir answers 'where would it go?'; got {obj}"
        );
        assert!(
            map.get("staleness").is_none() && map.get("file_count").is_none(),
            "T-16d/AD-414-23: no stats are gathered on this arm, so neither \
             'staleness' nor 'file_count' may appear; got {obj}"
        );
        assert!(
            !stderr.contains("no index found"),
            "T-16d/AC-23: in --json mode the single object carries the error on \
             STDOUT; it must not be duplicated onto stderr. Got stderr:\n{stderr}"
        );

        // --- text mode ---
        let (ok, _, stderr) =
            run_skim_search_capture(&bin, &cache_str, &["--stats", "--root", &root_str]);
        assert!(
            !ok,
            "T-16d/#413 AC21: cold-start --stats (text) must exit 1"
        );
        assert!(
            stderr.contains("no index found") && stderr.contains("skim search --build"),
            "T-16d: text mode must name the condition and the remedy; got stderr:\n{stderr}"
        );

        // AD-414-23: --stats reports on an index, it never creates one.
        let search_dir = cache.path().join("search");
        let built: Vec<std::path::PathBuf> = std::fs::read_dir(&search_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .flat_map(|e| std::fs::read_dir(e.path()).into_iter().flatten())
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.file_name().is_some_and(|n| n == "index.skidx"))
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            built.is_empty(),
            "T-16d/AD-414-23: --stats must not build a missing index; found {built:?}"
        );
    }
}
