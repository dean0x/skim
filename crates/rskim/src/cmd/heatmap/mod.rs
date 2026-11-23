//! `skim heatmap` subcommand — git history risk/coupling analysis.
//!
//! Computes 6 metrics from git log history:
//! 1. Churn: commit frequency per file
//! 2. Coupling: files that change together (blast radius)
//! 3. Stability: composite score (churn + recency + volatility)
//! 4. Author diversity: bus-factor risk detection
//! 5. Fix-after-touch: proximity-based bug-introduction risk
//! 6. Module encapsulation: cross-boundary coupling health

mod args;
mod card;
mod excludes;
mod git_source;
mod insights;
mod metrics;
mod noise;
mod output;
mod types;
mod view;
mod window;

use std::collections::HashSet;
use std::io::{self, Write};
use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::analytics::{CommandType, RecordingContext};

use args::{parse_args, print_help};
use excludes::Excludes;
use git_source::{
    CliGitSource, GitDataSource, HeadEntry, HeadTree, NoToplevel, ParsedLog, disk_path,
};
use insights::{build_insights_result, compute_insights, keep_code_findings};
use metrics::{
    CouplingResult, ModuleRoots, compute_authors, compute_churn, compute_coupling,
    compute_encapsulation, compute_fix_after_touch, compute_stability, module_of,
};
use noise::{COUPLING_MAX_FILES, MIN_PAIR_SUPPORT, MODULE_MIN_COMMITS};
use output::{
    render_cards_text, render_insights_json, render_insights_text, render_json, render_text,
};
use types::{
    CommitInfo, FileMetrics, FileTargets, HeatmapArgs, HeatmapConfig, HeatmapResult, ResolvedWindow,
};
use window::{build_window_info, format_epoch, resolve_config};

// ============================================================================
// Entry point
// ============================================================================

/// Heatmap does not stop when stderr is closed.
fn note(message: std::fmt::Arguments<'_>) {
    let _ = crate::cmd::execution::write_line_to_stderr(&message.to_string());
}

fn debug_note(message: std::fmt::Arguments<'_>) {
    if crate::debug::is_debug_enabled() {
        note(message);
    }
}

/// Run the `skim heatmap` subcommand.
pub(crate) fn run(
    args: &[String],
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print_help();
        return Ok(ExitCode::SUCCESS);
    }

    let mut raw_args = match parse_args(args) {
        Ok(a) => a,
        Err(e) => {
            note(format_args!("skim heatmap: {e}"));
            note(format_args!("Run `skim heatmap --help` for usage."));
            return Ok(ExitCode::FAILURE);
        }
    };

    let git_source = match card_source(&raw_args.files) {
        Ok((source, files)) => {
            raw_args.files = files;
            source
        }
        Err(e) => {
            note(format_args!("skim heatmap: {e}"));
            return Ok(ExitCode::FAILURE);
        }
    };

    // Resolve --diff to concrete file list (borrows raw_args mutably)
    if let Some(exit) = resolve_diff_files(&git_source, &mut raw_args)? {
        return Ok(exit);
    }

    // Move raw_args into run_with_source for resolution and execution
    run_with_source(&git_source, raw_args, analytics)
}

/// The git source for the file cards, and the file arguments to use with it.
///
/// A path below the cwd repository by text keeps that repository and the
/// text rules of [`card::normalise_target`], as before. Only a path outside
/// it, or any path when the cwd is in no repository, asks git for the
/// repository that holds it (see [`locate_card`]). All files must be in one
/// repository: two repositories in one call are refused, so the cards
/// always share one history. When the files are in another repository, git
/// runs there.
///
/// With `GIT_DIR` or `GIT_WORK_TREE` set, git does not look for the
/// repository above a folder. On a platform other than unix, the text rules
/// expect `/`. In these cases, and with a cwd that is not a UTF-8 path, the
/// cwd repository stays the only one, as before.
fn card_source(files: &[String]) -> anyhow::Result<(CliGitSource, Vec<String>)> {
    let here = CliGitSource::new();
    if files.is_empty() || !cfg!(unix) || git_repo_env_set() {
        return Ok((here, files.to_vec()));
    }
    let cwd = std::env::current_dir().ok();
    let Some(cwd) = cwd.as_deref().and_then(std::path::Path::to_str) else {
        return Ok((here, files.to_vec()));
    };
    let cwd_repo = here.get_repo_root().ok().zip(here.show_prefix().ok());

    let mut folders = std::collections::HashMap::new();
    let mut first: Option<(&str, String)> = None;
    let mut args = Vec::with_capacity(files.len());
    for arg in files {
        let (root, pass) = locate_card(&here, cwd, cwd_repo.as_ref(), &mut folders, arg)?;
        match &first {
            Some((first_arg, first_root)) if *first_root != root => anyhow::bail!(
                "'{}' and '{}' are in different repositories. Give the files of one repository in each call.",
                card::shown_path(first_arg),
                card::shown_path(arg)
            ),
            Some(_) => {}
            None => first = Some((arg, root)),
        }
        args.push(pass);
    }

    match first {
        Some((_, root)) if cwd_repo.is_none_or(|(cwd_root, _)| cwd_root != root) => {
            Ok((CliGitSource::in_dir(root), args))
        }
        _ => Ok((here, args)),
    }
}

/// The repository root of one file argument, and the argument to pass on.
///
/// A path below the cwd repository by text passes on as typed. Else git
/// runs in the nearest folder on disk at or above the absolute path (no
/// file is opened), and the absolute path passes on: it maps to the same
/// repo-relative path from any folder. `folders` keeps the root of each
/// folder, so files in one folder start one git process. Each error names
/// the argument as typed.
fn locate_card(
    here: &CliGitSource,
    cwd: &str,
    cwd_repo: Option<&(String, String)>,
    folders: &mut std::collections::HashMap<String, String>,
    arg: &str,
) -> anyhow::Result<(String, String)> {
    if let Some((root, prefix)) = cwd_repo {
        match card::normalise_target(arg, root, prefix) {
            Ok(_) => return Ok((root.clone(), arg.to_owned())),
            Err(card::TargetError::Outside(_)) => {}
            Err(e) => return Err(e.into()),
        }
    }
    let path = card::absolute_arg(arg, cwd);
    let shown = card::shown_path(arg);
    let Some(folder) = git_source::nearest_folder(&path) else {
        anyhow::bail!("'{shown}' is in no git repository");
    };
    let root = if let Some(root) = folders.get(folder) {
        root.clone()
    } else {
        let root = here.toplevel_in(folder).map_err(|e| match e {
            NoToplevel::NoRepository => anyhow::anyhow!("'{shown}' is in no git repository"),
            NoToplevel::Git(message) => anyhow::anyhow!("'{shown}': {message}"),
        })?;
        folders.insert(folder.to_owned(), root.clone());
        root
    };
    card::target_below(&path, &root, arg)?;
    Ok((root, path))
}

/// True when `GIT_DIR` or `GIT_WORK_TREE` is set and not empty. Git hooks
/// set `GIT_DIR`.
fn git_repo_env_set() -> bool {
    ["GIT_DIR", "GIT_WORK_TREE"]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
}

/// Resolve `--diff` to a concrete file list, mutating `args.diff_files`.
/// These paths come from git and get no card: they scope the default view.
///
/// Returns `Some(ExitCode::FAILURE)` for any early-exit condition so that `run()`
/// can propagate it directly. Returns `Ok(None)` when resolution succeeded and the
/// caller should continue.
///
/// Path correctness: git diff output is repo-root-relative, so deleted-file
/// detection uses [`CliGitSource::get_repo_root`] to build absolute paths rather
/// than relying on cwd. The `is_git_repo()` guard is intentionally omitted — if
/// the cwd is not inside a repository, `fetch_diff_files` will return an error
/// with a clear message, and `prepare_commits` already handles the non-repo case.
fn resolve_diff_files(
    git_source: &CliGitSource,
    args: &mut HeatmapArgs,
) -> anyhow::Result<Option<ExitCode>> {
    let Some(base) = args.diff_base.take() else {
        return Ok(None);
    };

    match git_source.fetch_diff_files(&base) {
        Ok(files) if files.is_empty() => {
            note(format_args!("skim heatmap: no files changed vs '{base}'"));
            Ok(Some(ExitCode::FAILURE))
        }
        Ok(mut files) => {
            // Check the decoded path. Keep Git's spelling as the history key.
            let total = files.len();
            files.retain(|f| disk_path(f).is_some_and(|p| super::is_repo_relative_safe(&p)));
            let rejected = total - files.len();
            if rejected > 0 {
                let noun = if rejected == 1 { "path" } else { "paths" };
                note(format_args!(
                    "skim heatmap: warning: dropped {rejected} invalid changed {noun}"
                ));
            }
            if files.is_empty() {
                note(format_args!("skim heatmap: no valid changed paths"));
                return Ok(Some(ExitCode::FAILURE));
            }

            // Check the accepted paths for the warning, without changing the scope.
            let root = match git_source.get_repo_root() {
                Ok(root) => root,
                Err(e) => {
                    note(format_args!("skim heatmap: {e}"));
                    return Ok(Some(ExitCode::FAILURE));
                }
            };
            if let Some(warning) = diff_not_file_warning(std::path::Path::new(&root), &files) {
                note(format_args!("skim heatmap: warning: {warning}"));
            }
            args.diff_files = files;
            Ok(None)
        }
        Err(e) => {
            note(format_args!("skim heatmap: {e}"));
            Ok(Some(ExitCode::FAILURE))
        }
    }
}

/// One warning for the `--diff` paths that are not regular files under
/// `root`, or `None` when each one is. It names the first
/// [`WARNING_NAMES`] in diff order, as git wrote them, and counts the
/// others. A large diff thus adds one line, not one per path.
///
/// It tests `is_file()`, not `exists()`, so a path that is now a folder or a
/// dangling link gets the warning, as a deleted path does. A path that is
/// absolute or goes out of `root` (`..`, a drive prefix) is not tested: the
/// shared containment rule of `super::is_repo_relative_safe` applies, as for
/// the git index paths of `skim search`.
fn diff_not_file_warning(root: &std::path::Path, files: &[String]) -> Option<String> {
    let not_files: Vec<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|f| {
            disk_path(f)
                .is_some_and(|rel| super::is_repo_relative_safe(&rel) && !root.join(&rel).is_file())
        })
        .collect();
    match not_files.as_slice() {
        [] => None,
        [only] => Some(format!(
            "{} is not a regular file on current branch",
            warning_name(only)
        )),
        _ => Some(format!(
            "{} changed paths are not regular files on current branch: {}",
            not_files.len(),
            named_paths(&not_files)
        )),
    }
}

/// Kinds of run warning: a shallow clone, more than one window flag, an
/// unresolved `--last`, skipped git log data, git log output at its
/// limit, HEAD data, commits too large to pair and `--diff` files not in
/// the window. Each kind adds at most one line in either JSON output.
const WARNING_KINDS: usize = 8;

/// True when `warnings` has at most [`WARNING_KINDS`] lines and no line
/// twice.
fn one_warning_per_kind(warnings: &[String]) -> bool {
    let distinct: std::collections::HashSet<&str> = warnings.iter().map(String::as_str).collect();
    warnings.len() <= WARNING_KINDS && distinct.len() == warnings.len()
}

fn skipped_log_warning(parsed: &ParsedLog) -> Option<String> {
    let groups = [
        (parsed.malformed, "malformed git log record"),
        (parsed.unpaired, "unpaired git log record"),
        (parsed.unsafe_paths, "unsafe path"),
    ];
    let counts: Vec<_> = groups
        .into_iter()
        .filter(|&(count, _)| count > 0)
        .map(|(count, noun)| {
            let plural = if count == 1 { "" } else { "s" };
            format!("{count} {noun}{plural}")
        })
        .collect();
    (!counts.is_empty()).then(|| format!("Skipped {}.", counts.join(", ")))
}

/// On an early exit there is no result view to carry this warning.
fn emit_skipped_log_warning(warning: Option<&str>) {
    if let Some(warning) = warning {
        note(format_args!("skim heatmap: {warning}"));
    }
}

/// Bundled output of [`prepare_commits`] — everything needed to call [`compute_heatmap`].
struct PreparedCommits {
    commits: Vec<CommitInfo>,
    /// Resolved, immutable configuration produced by [`resolve_config`].
    config: HeatmapConfig,
    window: ResolvedWindow,
    now_epoch: u64,
    repo_root: String,
    warnings: Vec<String>,
    /// Package roots for module labels; empty means the top-folder rule.
    module_roots: ModuleRoots,
    head_tree: Option<HeadTree>,
    excludes: Excludes,
}

/// Execute Steps 1–4 of the heatmap pipeline (git I/O + exclusions).
///
/// Consumes `args` by value — after this call `HeatmapArgs` is replaced by the
/// resolved `HeatmapConfig` stored in `PreparedCommits`.
///
/// Returns `Ok(None)` for early exits (not a git repo, no commits, or all
/// commits excluded in a ranked view). Callers treat `None` as failure.
/// Returns `Ok(Some(PreparedCommits))` when the pipeline should proceed to
/// metric computation.
fn prepare_commits(
    source: &dyn GitDataSource,
    mut args: HeatmapArgs,
) -> anyhow::Result<Option<PreparedCommits>> {
    // Step 1: Validate git environment
    if !source.is_git_repo() {
        note(format_args!("skim heatmap: Not a git repository"));
        return Ok(None);
    }

    let repo_root = match source.get_repo_root() {
        Ok(r) => r,
        Err(e) => {
            note(format_args!("skim heatmap: {e}"));
            return Ok(None);
        }
    };

    // Step 1b: file arguments become repo-relative card targets, before any
    // history is read. A path outside the root ends the run with no card.
    if !args.files.is_empty() {
        match card_targets(source, &repo_root, &args.files) {
            Ok(targets) => args.files = targets,
            Err(e) => {
                note(format_args!("skim heatmap: {e}"));
                return Ok(None);
            }
        }
    }

    let mut warnings: Vec<String> = Vec::new();

    if source.detect_shallow_clone() {
        warnings.push(
            "Shallow clone detected — history may be incomplete, metrics may be skewed."
                .to_string(),
        );
    }

    // Extract debug flag before moving args into resolve_config
    let debug = args.debug;

    if debug {
        debug_note(format_args!(
            "[skim:heatmap] repo root: {}",
            card::shown_path(&repo_root)
        ));
    }

    // Resolve and display the window from the same clock snapshot.
    let now_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Step 2: Resolve effective config (presets and --last). Consumes `args`.
    let (config, window) = resolve_config(args, source, &mut warnings, now_epoch)?;

    if debug {
        let mode = if window.dual_mode {
            "dual"
        } else if window.last_n.is_some() {
            "count"
        } else {
            "time"
        };
        debug_note(format_args!("[skim:heatmap] window mode: {mode}"));
        if let Some(since) = config.since {
            debug_note(format_args!(
                "[skim:heatmap] since epoch: {since} ({})",
                format_epoch(since)
            ));
        }
    }

    // Step 3: Fetch commits
    let parsed = match source.fetch_commits(&config) {
        Ok(p) => p,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not installed") || msg.contains("not in PATH") {
                note(format_args!("skim heatmap: {msg}"));
            } else {
                note(format_args!("skim heatmap: failed to fetch git log: {msg}"));
            }
            return Ok(None);
        }
    };

    let skipped_warning = skipped_log_warning(&parsed);
    if let Some(oldest) = parsed.cut_oldest {
        // Author timestamps are ASCII digits only, so they are never negative.
        warnings.push(format!(
            "git log output stopped at the {} MiB output limit; the oldest commit counted is from {}, and older history is not counted. Use --path, --since or --last for a full count.",
            crate::runner::MAX_OUTPUT_BYTES / (1024 * 1024),
            format_epoch(u64::try_from(oldest).unwrap_or(0)),
        ));
    }
    let raw_commits = parsed.commits;

    if debug {
        debug_note(format_args!(
            "[skim:heatmap] raw commits fetched: {}",
            raw_commits.len()
        ));
    }

    if raw_commits.is_empty() {
        emit_skipped_log_warning(skipped_warning.as_deref());
        note(format_args!("skim heatmap: No commits found in repository"));
        return Ok(None);
    }

    // Step 4: Apply exclusions
    let excludes = Excludes::new(config.no_exclude, &config.extra_excludes)?;
    let raw_commit_count = raw_commits.len();
    let mut commits = raw_commits;
    for commit in &mut commits {
        commit
            .changed_files
            .retain(|f| excludes.excluded_by(&f.path.to_string_lossy()).is_none());
    }
    // Remove commits that are now file-less after exclusion
    commits.retain(|c| !c.changed_files.is_empty());

    if debug {
        debug_note(format_args!(
            "[skim:heatmap] commits after exclusion: {} ({} excluded)",
            commits.len(),
            raw_commit_count - commits.len()
        ));
    }

    // A card can report its filter even when no commits remain.
    if commits.is_empty() && !matches!(config.targets, FileTargets::Cards(_)) {
        emit_skipped_log_warning(skipped_warning.as_deref());
        note(format_args!(
            "skim heatmap: No analyzable files after exclusions"
        ));
        return Ok(None);
    }
    warnings.extend(skipped_warning);

    let (module_roots, head_tree) =
        load_head_data(source, &repo_root, config.path.as_deref(), &mut warnings);

    Ok(Some(PreparedCommits {
        commits,
        config,
        window,
        now_epoch,
        repo_root,
        warnings,
        module_roots,
        head_tree,
        excludes,
    }))
}

/// Normalise the file arguments against the repository root, by text only,
/// in argument order and each path once. The cwd prefix comes from git; the
/// files themselves are never opened.
fn card_targets(
    source: &dyn GitDataSource,
    repo_root: &str,
    files: &[String],
) -> anyhow::Result<Vec<String>> {
    let prefix = source.show_prefix()?;
    let mut seen = std::collections::HashSet::new();
    let mut targets = Vec::with_capacity(files.len());
    for arg in files {
        let target = card::normalise_target(arg, repo_root, &prefix)?;
        if seen.insert(target.clone()) {
            targets.push(target);
        }
    }
    Ok(targets)
}

const HEAD_UNAVAILABLE: &str = "HEAD data is not available. Hotspots rank by commits only.";

/// Keep package roots from a complete listing even if Git cannot count lines.
fn load_head_data(
    source: &dyn GitDataSource,
    repo_root: &str,
    path: Option<&str>,
    warnings: &mut Vec<String>,
) -> (ModuleRoots, Option<HeadTree>) {
    let no_lazy_fetch = source.detect_partial_clone(repo_root);
    let Ok(mut entries) = source.list_tracked_files(repo_root, no_lazy_fetch) else {
        warnings.push(HEAD_UNAVAILABLE.into());
        return (ModuleRoots::default(), None);
    };
    let roots = ModuleRoots::from_tracked_files(entries.keys());
    let Ok(counts) = source.count_head_lines(repo_root, path, no_lazy_fetch) else {
        warnings.push(HEAD_UNAVAILABLE.into());
        return (roots, None);
    };
    let mut unmatched = 0;
    for (path, count) in counts {
        match entries.get_mut(&path) {
            Some(HeadEntry::Regular(lines)) => *lines = Some(count),
            Some(HeadEntry::Other) => (),
            None => unmatched += 1,
        }
    }
    if unmatched > 0 {
        warnings.push(format!(
            "HEAD data: {unmatched} line-count rows have no tree entry."
        ));
    }
    (roots, Some(entries))
}

fn trace_head_data(head_tree: Option<&HeadTree>) {
    let totals = match head_tree {
        Some(entries) => {
            let mut regular = 0;
            let mut counted = 0;
            for entry in entries.values() {
                if let HeadEntry::Regular(lines) = entry {
                    regular += 1;
                    counted += usize::from(lines.is_some());
                }
            }
            format!(
                "{} entries, {regular} regular files, {counted} line counts",
                entries.len()
            )
        }
        None => "unavailable".to_string(),
    };
    debug_note(format_args!("[skim:heatmap] HEAD data: {totals}"));
}

/// Orchestration with injected data source (enables testing).
///
/// Takes `HeatmapArgs` by value — ownership moves into [`prepare_commits`] which
/// consumes it via [`resolve_config`], producing the resolved `HeatmapConfig`
/// stored in `PreparedCommits`.
///
/// All git I/O is routed through `source` — infra checks (repo detection, root,
/// shallow clone, commit count) and the commit fetch all use the same trait object.
#[allow(clippy::disallowed_methods)] // Heatmap analysis output; skim-generated, locked handle for atomic multi-line write
fn run_with_source(
    source: &dyn GitDataSource,
    args: HeatmapArgs,
    analytics: &crate::analytics::AnalyticsConfig,
) -> anyhow::Result<ExitCode> {
    let PreparedCommits {
        commits,
        config,
        window,
        now_epoch,
        repo_root,
        warnings,
        module_roots,
        head_tree,
        excludes,
    } = match prepare_commits(source, args)? {
        Some(p) => p,
        None => return Ok(ExitCode::FAILURE),
    };

    if config.debug {
        trace_head_data(head_tree.as_ref());
    }

    // Steps 5-9: Compute metrics and assemble result
    let start_time = std::time::Instant::now();
    let result = compute_heatmap(
        &commits,
        &config,
        &window,
        now_epoch,
        repo_root,
        warnings,
        &module_roots,
    );
    let (mut result, at_head) = keep_at_head(result, head_tree.as_ref(), &config.targets);
    result.not_at_head = at_head.not_at_head;
    let spec = view::ViewSpec::from_config(&config);

    // File targets scope the result after the full metric computation, so
    // the coupling of a target keeps its partners outside the scope.
    let cards = match &config.targets {
        FileTargets::All => None,
        FileTargets::Diff(files) => {
            warn_missing_targets(&mut result, files, &at_head.dropped);
            apply_file_scope(&mut result, files, &module_roots);
            None
        }
        FileTargets::Cards(paths) => {
            let cards = scope_cards(
                &mut result,
                &commits,
                paths,
                spec.lists(),
                &module_roots,
                &excludes,
            );
            // A history-only target can give a card, but not a ranked row.
            result.files.retain(|f| !at_head.dropped.contains(&f.path));
            Some(cards)
        }
    };
    // The warning list is complete here, and both JSON outputs give it as is.
    debug_assert!(
        one_warning_per_kind(&result.warnings),
        "more than one warning of a kind: {:?}",
        result.warnings
    );

    // Step 10: Render
    let elapsed = start_time.elapsed();
    let mut stdout = io::stdout().lock();
    let label = write_output(&mut stdout, &result, &config, &spec, cards.as_ref())?;

    // Step 11: Fire-and-forget analytics
    record_heatmap_analytics(analytics, label, elapsed);

    Ok(ExitCode::SUCCESS)
}

/// Write the output of one run to `out` and return its analytics label.
///
/// A run with file arguments writes its cards, and `--insights` gives the
/// same cards, text or JSON. Else `--insights` writes the ranked findings of
/// the full result, capped at 20 or `--top`. Else the ranked view writes the
/// default view or the `--detail` section.
fn write_output(
    out: &mut impl Write,
    result: &HeatmapResult,
    config: &HeatmapConfig,
    spec: &view::ViewSpec,
    cards: Option<&view::Section<card::Card>>,
) -> anyhow::Result<&'static str> {
    if let Some(cards) = cards {
        if config.format_json {
            let view = view::build_view(result, spec);
            writeln!(out, "{}", render_json(result, &view, Some(cards))?)?;
        } else {
            write!(out, "{}", render_cards_text(result, cards, spec.lists()))?;
        }
        return Ok("skim heatmap");
    }

    if config.insights {
        let (ranked, left_out) = keep_code_findings(compute_insights(result), *spec);
        let insights_result =
            build_insights_result(result, ranked, spec.lists().insights, left_out);
        if config.format_json {
            writeln!(out, "{}", render_insights_json(&insights_result)?)?;
        } else {
            write!(out, "{}", render_insights_text(result, &insights_result))?;
        }
        return Ok("skim heatmap --insights");
    }

    // The view sorts, gates and caps every section for both outputs.
    let view = view::build_view(result, spec);
    if config.format_json {
        writeln!(out, "{}", render_json(result, &view, None)?)?;
    } else {
        write!(out, "{}", render_text(result, &view))?;
    }
    Ok("skim heatmap")
}

/// Fire-and-forget analytics recording for heatmap commands.
fn record_heatmap_analytics(
    analytics: &crate::analytics::AnalyticsConfig,
    command: &str,
    elapsed: std::time::Duration,
) {
    let rec = RecordingContext {
        enabled: analytics.enabled,
        command_type: CommandType::Heatmap,
        parse_tier: None,
        session_id: analytics.session_id.as_deref(),
    };
    crate::analytics::try_record_command(
        rec,
        String::new(),
        String::new(),
        command.to_string(),
        elapsed,
    );
}

// ============================================================================
// File-scope display filter (Step 5-alt)
// ============================================================================

/// Names in a counted warning. The warning counts paths that it does not name.
const WARNING_NAMES: usize = 3;

/// Path text in all heatmap views shows the same form.
fn warning_name(path: &str) -> String {
    format!("'{}'", card::shown_path(path))
}

/// The first [`WARNING_NAMES`] of `names` in single quotes, then
/// `" and N more"` for the others.
fn named_paths(names: &[&str]) -> String {
    let shown: Vec<String> = names
        .iter()
        .take(WARNING_NAMES)
        .map(|name| warning_name(name))
        .collect();
    join_warning_names(&shown, names.len())
}

fn join_warning_names(shown: &[String], total: usize) -> String {
    let unnamed = total.saturating_sub(shown.len());
    let rest = if unnamed > 0 {
        format!(" and {unnamed} more")
    } else {
        String::new()
    };
    format!("{}{rest}", shown.join(", "))
}

#[derive(Default)]
struct AtHead {
    not_at_head: usize,
    dropped: HashSet<String>,
}

/// Apply HEAD status after the metrics so module totals and stability keep
/// the full history. Keep explicit card targets until their cards are built.
#[must_use]
fn keep_at_head(
    mut result: HeatmapResult,
    head_tree: Option<&HeadTree>,
    targets: &FileTargets,
) -> (HeatmapResult, AtHead) {
    let Some(head_tree) = head_tree else {
        return (result, AtHead::default());
    };
    let card_paths: HashSet<String> = match targets {
        FileTargets::Cards(paths) => card::history_paths(paths, &result.files)
            .into_iter()
            .collect(),
        FileTargets::All | FileTargets::Diff(_) => HashSet::new(),
    };
    let mut at_head = AtHead::default();
    result.files.retain_mut(|file| {
        match head_tree.get(&file.path) {
            Some(HeadEntry::Regular(Some(lines))) => {
                file.lines = *lines;
                return true;
            }
            // History and grep share the same path scope. An in-scope, nonempty
            // file with no text count is binary, but it still exists at HEAD.
            Some(HeadEntry::Regular(None)) => (),
            Some(HeadEntry::Other) | None => at_head.not_at_head += 1,
        }
        at_head.dropped.insert(file.path.clone());
        card_paths.contains(&file.path)
    });
    result
        .coupling_graph
        .retain(|edge| !at_head.dropped.contains(&edge.a) && !at_head.dropped.contains(&edge.b));
    for file in &mut result.files {
        file.blast_radius
            .retain(|p| !at_head.dropped.contains(&p.path));
    }
    (result, at_head)
}

/// Add one warning for the `--diff` files that are not in the window's
/// history. It counts each missing file once and names the first
/// [`WARNING_NAMES`] in argument order (a set order would change
/// between runs). A large diff thus adds one warning, not one per file.
fn warn_missing_targets(result: &mut HeatmapResult, files: &[String], dropped: &HashSet<String>) {
    let result_paths: HashSet<&str> = result.files.iter().map(|f| f.path.as_str()).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let missing: Vec<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|target| {
            !result_paths.contains(target) && !dropped.contains(*target) && seen.insert(target)
        })
        .collect();
    let warning = match missing.as_slice() {
        [] => return,
        [only] => format!(
            "targeted file {} not found in git history",
            warning_name(only)
        ),
        _ => format!(
            "{} targeted files not found in git history: {}",
            missing.len(),
            named_paths(&missing)
        ),
    };
    result.warnings.push(warning);
}

/// Scope the view to file cards and name only shown targets in the coupling warning.
fn scope_cards(
    result: &mut HeatmapResult,
    commits: &[CommitInfo],
    paths: &[String],
    caps: view::ListCaps,
    roots: &ModuleRoots,
    excludes: &Excludes,
) -> view::Section<card::Card> {
    let targets = card::history_paths(paths, &result.files);
    let (cards, counts) = card::build_cards(commits, result, &targets, caps, excludes);
    debug_assert_eq!(cards.rows.len(), counts.len());
    let skipped = commits
        .iter()
        .filter(|c| metrics::is_large_commit(c))
        .count();
    let touches: Vec<_> = targets
        .iter()
        .zip(counts)
        .filter(|&(_, count)| count > 0)
        .map(|(path, count)| (path.as_str(), count))
        .collect();
    scope_coupling_warning(&mut result.warnings, skipped, &touches);
    apply_file_scope(result, &targets, roots);
    cards
}

/// `None` for `touches` gives the global line. A card view only reports it
/// when one or more shown targets had a large commit.
fn coupling_warning(skipped: usize, touches: Option<&[(&str, usize)]>) -> Option<String> {
    if skipped == 0 {
        return None;
    }
    let noun = if skipped == 1 { "commit" } else { "commits" };
    let global =
        format!("coupling: {skipped} {noun} with more than {COUPLING_MAX_FILES} files not paired");
    match touches {
        None => Some(global),
        Some([]) => None,
        Some(touches) => {
            let shown: Vec<_> = touches
                .iter()
                .take(WARNING_NAMES)
                .map(|(path, count)| {
                    let verb = if *count == 1 { "touches" } else { "touch" };
                    format!("{count} {verb} {}", card::shown_path(path))
                })
                .collect();
            Some(format!(
                "{global} ({})",
                join_warning_names(&shown, touches.len())
            ))
        }
    }
}

/// Replace or remove the exact global line without changing the warning order.
fn scope_coupling_warning(warnings: &mut Vec<String>, skipped: usize, touches: &[(&str, usize)]) {
    if let Some(global) = coupling_warning(skipped, None)
        && let Some(index) = warnings.iter().position(|line| *line == global)
    {
        if let Some(scoped) = coupling_warning(skipped, Some(touches)) {
            warnings[index] = scoped;
        } else {
            warnings.remove(index);
        }
    }
}

/// Filter heatmap results to only include targeted files.
///
/// Filtering happens AFTER metric computation to preserve coupling accuracy.
/// Coupling graph retains edges where at least one endpoint is targeted
/// (blast radius view). Modules are kept when they hold a targeted file, by
/// the same [`module_of`] mapping the module metric uses.
fn apply_file_scope(result: &mut HeatmapResult, files: &[String], roots: &ModuleRoots) {
    let target_set: HashSet<&str> = files.iter().map(|s| s.as_str()).collect();

    // Filter files
    result
        .files
        .retain(|f| target_set.contains(f.path.as_str()));

    // Filter coupling graph: keep edges where at least one endpoint is targeted
    result
        .coupling_graph
        .retain(|e| target_set.contains(e.a.as_str()) || target_set.contains(e.b.as_str()));

    // Filter modules: keep only the modules that hold a targeted file.
    let target_modules: HashSet<String> =
        files.iter().filter_map(|f| module_of(f, roots)).collect();
    result.modules.retain(|m| target_modules.contains(&m.path));

    result.file_targets = Some(files.to_vec());
}

// ============================================================================
// Pure metric computation (Steps 5-9)
// ============================================================================

/// Compute all six risk metrics from commits and assemble a `HeatmapResult`.
///
/// No git I/O — all repository data is pre-fetched by callers (Steps 1-4 in
/// `run_with_source`). Accepting `now_epoch` as a parameter (instead of calling
/// `SystemTime::now()` here) keeps metric computation deterministic and testable.
///
/// Debug timing is emitted to stderr when `config.debug` is enabled.
fn compute_heatmap(
    commits: &[CommitInfo],
    config: &HeatmapConfig,
    window: &ResolvedWindow,
    now_epoch: u64,
    repository: String,
    mut warnings: Vec<String>,
    module_roots: &ModuleRoots,
) -> HeatmapResult {
    let t0 = Instant::now();

    // Step 5: Compute metrics
    // Phase 1: churn must run first — max_churn feeds into stability.
    let churn_map = compute_churn(commits);
    let max_churn = churn_map.values().map(|m| m.commits).max().unwrap_or(1);

    // Phase 2: remaining 5 metrics are independent of each other — run in parallel.
    let fix_window = config.fix_window;
    let coupling_threshold = config.coupling_threshold;
    let (
        (stability_map, author_map),
        (
            (
                fix_risk_map,
                CouplingResult {
                    blast_radius: blast_radius_map,
                    graph: coupling_graph,
                    large_commits_skipped,
                },
            ),
            modules,
        ),
    ) = rayon::join(
        || {
            rayon::join(
                || compute_stability(commits, max_churn, now_epoch),
                || compute_authors(commits),
            )
        },
        || {
            rayon::join(
                || {
                    rayon::join(
                        || compute_fix_after_touch(commits, fix_window),
                        || compute_coupling(commits, coupling_threshold, MIN_PAIR_SUPPORT),
                    )
                },
                || compute_encapsulation(commits, MODULE_MIN_COMMITS, module_roots),
            )
        },
    );

    warnings.extend(coupling_warning(large_commits_skipped, None));

    if config.debug {
        let elapsed = t0.elapsed();
        debug_note(format_args!(
            "[skim:heatmap] metrics computed in {:.1}ms — {} files, {} coupling edges, {} modules",
            elapsed.as_secs_f64() * 1000.0,
            churn_map.len(),
            coupling_graph.len(),
            modules.len(),
        ));
    }

    // Window totals for the view gates: the fix-risk base rate and the
    // bus-factor author minimum.
    let fix_commits_total = commits
        .iter()
        .filter(|c| metrics::is_fix_commit(&c.message))
        .count();
    let window_authors = commits
        .iter()
        .map(|c| c.author.as_str())
        .collect::<std::collections::HashSet<&str>>()
        .len();

    // Step 6: Assemble FileMetrics. The ranked view sorts every section, so
    // the files stay in no particular order here.
    // `churn_map` already contains every path seen across all commits; no need
    // to rebuild a separate HashSet from the commit list.
    let file_metrics: Vec<FileMetrics> = churn_map
        .into_iter()
        .map(|(path, churn)| {
            let stability_score = stability_map.get(&path).copied().unwrap_or(100);
            let authors = author_map.get(&path).cloned().unwrap_or_default();
            let fix_risk = fix_risk_map.get(&path).cloned().unwrap_or_default();
            let blast_radius = blast_radius_map.get(&path).cloned().unwrap_or_default();

            FileMetrics {
                lines: 0,
                path,
                churn,
                stability_score,
                authors,
                fix_risk,
                blast_radius,
            }
        })
        .collect();

    // Step 7: Build window info
    let window_info = build_window_info(window, config.since, commits.len(), now_epoch);

    // Step 8: Get excluded patterns for output
    let excluded_patterns: Vec<String> = if config.no_exclude {
        Vec::new()
    } else {
        let capacity = excludes::DEFAULT_EXCLUDES.len() + config.extra_excludes.len();
        let mut patterns = Vec::with_capacity(capacity);
        patterns.extend(excludes::DEFAULT_EXCLUDES.iter().map(|s| s.to_string()));
        patterns.extend(config.extra_excludes.iter().map(|g| g.glob().into()));
        patterns
    };

    // Step 9: Build result
    HeatmapResult {
        not_at_head: 0,
        generated_at: format_epoch(now_epoch),
        repository,
        window: window_info,
        files: file_metrics,
        modules,
        coupling_graph,
        excluded_patterns,
        warnings,
        file_targets: None,
        fix_commits_total,
        window_authors,
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Heatmap does not stop when stderr is closed. The shared runner cap
    /// marker is outside this check. Heatmap uses `run_stdout_degrade` instead.
    #[test]
    fn test_heatmap_stderr_source_contract() {
        let direct_stderr = regex::Regex::new(r"\b(?:eprint|eprintln|debug_log)\s*!").unwrap();
        let runner_with_marker = regex::Regex::new(r"\.\s*run(?:_with_env)?\s*\(").unwrap();
        for (file, source) in [
            ("args.rs", include_str!("args.rs")),
            ("card.rs", include_str!("card.rs")),
            ("excludes.rs", include_str!("excludes.rs")),
            ("git_source.rs", include_str!("git_source.rs")),
            ("insights.rs", include_str!("insights.rs")),
            ("metrics.rs", include_str!("metrics.rs")),
            ("mod.rs", include_str!("mod.rs")),
            ("noise.rs", include_str!("noise.rs")),
            ("output.rs", include_str!("output.rs")),
            ("types.rs", include_str!("types.rs")),
            ("view.rs", include_str!("view.rs")),
            ("window.rs", include_str!("window.rs")),
        ] {
            let production = source.split("\n#[cfg(test)]").next().unwrap();
            assert!(!direct_stderr.is_match(production), "{file}: stderr macro");
            if file == "git_source.rs" {
                assert!(
                    !runner_with_marker.is_match(production),
                    "{file}: runner call"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // apply_file_scope
    // -----------------------------------------------------------------------

    fn make_test_result() -> HeatmapResult {
        use types::{
            AuthorMetrics, ChurnMetrics, CouplingEdge, FileMetrics, FixRiskMetrics, ModuleHealth,
            WindowInfo,
        };
        HeatmapResult {
            not_at_head: 0,
            generated_at: "2025-01-01".to_string(),
            repository: "test".to_string(),
            window: WindowInfo {
                mode: "90d".to_string(),
                since: "2024-10-01".to_string(),
                until: "2025-01-01".to_string(),
                commits_analyzed: 10,
                effective_strategy: None,
            },
            files: vec![
                FileMetrics {
                    lines: 0,
                    path: "src/main.rs".to_string(),
                    churn: ChurnMetrics {
                        commits: 5,
                        rate: 0.5,
                    },
                    stability_score: 42,
                    authors: AuthorMetrics::default(),
                    fix_risk: FixRiskMetrics::default(),
                    blast_radius: vec![],
                },
                FileMetrics {
                    lines: 0,
                    path: "src/lib.rs".to_string(),
                    churn: ChurnMetrics {
                        commits: 3,
                        rate: 0.3,
                    },
                    stability_score: 60,
                    authors: AuthorMetrics::default(),
                    fix_risk: FixRiskMetrics::default(),
                    blast_radius: vec![],
                },
                FileMetrics {
                    lines: 0,
                    path: "tests/test.rs".to_string(),
                    churn: ChurnMetrics {
                        commits: 2,
                        rate: 0.2,
                    },
                    stability_score: 80,
                    authors: AuthorMetrics::default(),
                    fix_risk: FixRiskMetrics::default(),
                    blast_radius: vec![],
                },
            ],
            modules: vec![
                ModuleHealth {
                    path: "src".to_string(),
                    encapsulation_pct: 80.0,
                    files_count: 2,
                    total_commits: 8,
                    cross_boundary_commits: 1,
                },
                ModuleHealth {
                    path: "tests".to_string(),
                    encapsulation_pct: 90.0,
                    files_count: 1,
                    total_commits: 2,
                    cross_boundary_commits: 0,
                },
            ],
            coupling_graph: vec![
                CouplingEdge {
                    a: "src/main.rs".to_string(),
                    b: "src/lib.rs".to_string(),
                    confidence: 0.8,
                    support: 4,
                },
                CouplingEdge {
                    a: "tests/test.rs".to_string(),
                    b: "src/lib.rs".to_string(),
                    confidence: 0.6,
                    support: 3,
                },
            ],
            excluded_patterns: vec![],
            warnings: vec![],
            file_targets: None,
            fix_commits_total: 2,
            window_authors: 1,
        }
    }

    #[test]
    fn test_apply_file_scope_filters_files() {
        let mut result = make_test_result();
        apply_file_scope(
            &mut result,
            &["src/main.rs".to_string()],
            &ModuleRoots::default(),
        );
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].path, "src/main.rs");
    }

    #[test]
    fn test_apply_file_scope_filters_coupling() {
        let mut result = make_test_result();
        apply_file_scope(
            &mut result,
            &["src/main.rs".to_string()],
            &ModuleRoots::default(),
        );
        // Edge with src/main.rs kept, edge without any target dropped
        assert_eq!(result.coupling_graph.len(), 1);
        assert_eq!(result.coupling_graph[0].a, "src/main.rs");
    }

    #[test]
    fn test_apply_file_scope_filters_modules() {
        let mut result = make_test_result();
        apply_file_scope(
            &mut result,
            &["src/main.rs".to_string()],
            &ModuleRoots::default(),
        );
        assert_eq!(result.modules.len(), 1);
        assert_eq!(result.modules[0].path, "src");
    }

    #[test]
    fn test_warn_missing_targets_names_the_missing_diff_file() {
        let mut result = make_test_result();
        result.warnings.clear();
        warn_missing_targets(
            &mut result,
            &["nonexistent.rs".to_string()],
            &HashSet::new(),
        );
        assert_eq!(
            result.warnings,
            ["targeted file 'nonexistent.rs' not found in git history"]
        );
    }

    #[test]
    fn test_warn_missing_targets_gives_one_counted_warning_in_argument_order() {
        let names = [
            "z\u{202e}.rs",
            "a.rs",
            "m.rs",
            "z\u{202e}.rs",
            "k.rs",
            "b.rs",
        ];
        let targets: Vec<String> = names.iter().map(|n| (*n).to_string()).collect();
        let mut result = make_test_result();
        result.warnings.clear();
        warn_missing_targets(&mut result, &targets, &HashSet::new());
        assert_eq!(
            result.warnings,
            ["5 targeted files not found in git history: 'z�.rs', 'a.rs', 'm.rs' and 2 more"]
        );
    }

    #[test]
    fn test_warn_missing_targets_names_all_of_a_few_files() {
        let targets = ["x.rs".to_string(), "y.rs".to_string(), "x.rs".to_string()];
        let mut result = make_test_result();
        result.warnings.clear();
        warn_missing_targets(&mut result, &targets, &HashSet::new());
        assert_eq!(
            result.warnings,
            ["2 targeted files not found in git history: 'x.rs', 'y.rs'"]
        );
    }

    #[test]
    fn test_warn_missing_targets_adds_nothing_when_all_are_in_history() {
        let mut result = make_test_result();
        result.warnings.clear();
        let present: Vec<String> = result.files.iter().map(|f| f.path.clone()).collect();
        warn_missing_targets(&mut result, &present, &HashSet::new());
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    // -----------------------------------------------------------------------
    // --diff paths that are not regular files
    // -----------------------------------------------------------------------

    /// A root with the regular files `kept.rs` and `docs/café.md`.
    fn diff_root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("kept.rs"), "").expect("write kept.rs");
        std::fs::create_dir(dir.path().join("docs")).expect("create docs");
        std::fs::write(dir.path().join("docs/caf\u{e9}.md"), "").expect("write docs file");
        dir
    }

    fn strings(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_string()).collect()
    }

    #[test]
    fn test_diff_not_file_warning_counts_the_paths_in_one_line() {
        let root = diff_root();
        let files = strings(&["kept.rs", "a.rs", "b.rs", "c.rs", "d.rs", "e.rs"]);
        assert_eq!(
            diff_not_file_warning(root.path(), &files).as_deref(),
            Some(
                "5 changed paths are not regular files on current branch: 'a.rs', 'b.rs', 'c.rs' and 2 more"
            )
        );
    }

    #[test]
    fn test_diff_not_file_warning_names_all_of_a_few_paths() {
        let root = diff_root();
        let files = strings(&["a.rs", "kept.rs", "docs"]);
        assert_eq!(
            diff_not_file_warning(root.path(), &files).as_deref(),
            Some("2 changed paths are not regular files on current branch: 'a.rs', 'docs'")
        );
    }

    #[test]
    fn test_diff_not_file_warning_names_one_path_alone() {
        let root = diff_root();
        let files = strings(&["kept.rs", "gone.rs"]);
        assert_eq!(
            diff_not_file_warning(root.path(), &files).as_deref(),
            Some("'gone.rs' is not a regular file on current branch")
        );
    }

    #[test]
    fn test_diff_not_file_warning_finds_a_quoted_path_on_disk() {
        let root = diff_root();
        let quoted = card::git_quoted("docs/caf\u{e9}.md", true);
        assert_eq!(quoted, r#""docs/caf\303\251.md""#);
        assert_eq!(
            diff_not_file_warning(root.path(), &[quoted, "kept.rs".to_string()]),
            None
        );
    }

    #[test]
    fn test_diff_not_file_warning_names_a_quoted_path_as_git_wrote_it() {
        let root = diff_root();
        let files = strings(&[r#""docs/gon\303\251.md""#]);
        assert_eq!(
            diff_not_file_warning(root.path(), &files).as_deref(),
            Some(r#"'"docs/gon\303\251.md"' is not a regular file on current branch"#)
        );
    }

    #[test]
    fn test_diff_not_file_warning_skips_a_path_outside_the_root() {
        let root = diff_root();
        let files = strings(&["../outside.rs", r#""../caf\303\251.rs""#, "/etc/none.rs"]);
        assert_eq!(diff_not_file_warning(root.path(), &files), None);
    }

    #[test]
    fn test_disk_path_decodes_each_escape_that_git_writes() {
        let paths = [
            "plain.rs",
            "a\tb\nc\rd.rs",
            "bell\u{7}back\u{8}vt\u{b}ff\u{c}.rs",
            r#"say "hi".rs"#,
            r"back\slash.rs",
            "caf\u{e9}/\u{65e5}.rs",
            "del\u{7f}\u{1}.rs",
        ];
        for path in paths {
            let quoted = card::git_quoted(path, true);
            assert_eq!(
                disk_path(&quoted),
                Some(std::path::PathBuf::from(path)),
                "{quoted}"
            );
        }
    }

    #[test]
    fn test_disk_path_refuses_an_escape_that_git_does_not_write() {
        for quoted in [
            r#""a\qb""#,
            r#""a\""#,
            r#""a\3""#,
            r#""a\400""#,
            r#""a\08x""#,
        ] {
            assert_eq!(disk_path(quoted), None, "{quoted}");
        }
    }

    #[test]
    fn test_apply_file_scope_sets_file_targets() {
        let mut result = make_test_result();
        apply_file_scope(
            &mut result,
            &["src/main.rs".to_string()],
            &ModuleRoots::default(),
        );
        assert_eq!(result.file_targets, Some(vec!["src/main.rs".to_string()]));
    }

    /// Regression test for deeply-nested file targets.
    ///
    /// When a targeted file lives multiple levels deep (e.g.
    /// `src/cmd/heatmap/mod.rs`), the module filter must match it against the
    /// top-level module `"src"` — not the immediate parent `"src/cmd/heatmap"`.
    /// The previous `rsplit_once('/')` implementation extracted the deepest
    /// parent directory and would incorrectly drop the `"src"` module.
    #[test]
    fn test_apply_file_scope_filters_modules_deeply_nested() {
        use types::{AuthorMetrics, ChurnMetrics, FileMetrics, FixRiskMetrics, ModuleHealth};
        let mut result = make_test_result();
        // Replace the file list with a single deeply-nested path
        result.files = vec![FileMetrics {
            lines: 0,
            path: "src/cmd/heatmap/mod.rs".to_string(),
            churn: ChurnMetrics {
                commits: 1,
                rate: 0.1,
            },
            stability_score: 50,
            authors: AuthorMetrics::default(),
            fix_risk: FixRiskMetrics::default(),
            blast_radius: vec![],
        }];
        // Add a matching deeply-nested module entry to the modules list
        result.modules.push(ModuleHealth {
            path: "src/cmd".to_string(),
            encapsulation_pct: 70.0,
            files_count: 1,
            total_commits: 1,
            cross_boundary_commits: 0,
        });

        apply_file_scope(
            &mut result,
            &["src/cmd/heatmap/mod.rs".to_string()],
            &ModuleRoots::default(),
        );

        // The top-level "src" module must be retained; "tests" must be dropped;
        // the "src/cmd" entry (not a valid top-level module) must also be dropped.
        let module_paths: Vec<&str> = result.modules.iter().map(|m| m.path.as_str()).collect();
        assert!(
            module_paths.contains(&"src"),
            "expected 'src' module to be retained for deeply-nested target, got: {module_paths:?}"
        );
        assert!(
            !module_paths.contains(&"tests"),
            "expected 'tests' module to be dropped, got: {module_paths:?}"
        );
    }

    #[test]
    fn test_apply_file_scope_keeps_module_of_manifest_folder() {
        use types::ModuleHealth;
        let mut result = make_test_result();
        result.modules = ["crates/a", "crates/b", "crates"]
            .iter()
            .map(|p| ModuleHealth {
                path: p.to_string(),
                encapsulation_pct: 100.0,
                files_count: 1,
                total_commits: 3,
                cross_boundary_commits: 0,
            })
            .collect();
        let roots = ModuleRoots::from_tracked_files(&[
            "crates/a/Cargo.toml".to_string(),
            "crates/b/Cargo.toml".to_string(),
        ]);

        apply_file_scope(&mut result, &["crates/a/src/lib.rs".to_string()], &roots);

        let module_paths: Vec<&str> = result.modules.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(module_paths, vec!["crates/a"]);
    }

    // -----------------------------------------------------------------------
    // prepare_commits — malformed git log records, tracked-file list
    // -----------------------------------------------------------------------

    /// What the stub's `list_tracked_files` gives.
    enum TrackedStub {
        Listed(HeadTree),
        Unavailable,
    }

    /// Stub `GitDataSource`: `fetch_commits` returns `commits`, the
    /// configured count of malformed records and `cut_oldest`;
    /// `list_tracked_files` follows `tracked`; `detect_shallow_clone`
    /// gives `shallow`.
    struct StubSource {
        commits: Vec<CommitInfo>,
        malformed: usize,
        cut_oldest: Option<i64>,
        tracked: TrackedStub,
        counts: Option<git_source::HeadCounts>,
        count_calls: std::cell::RefCell<Vec<(String, Option<String>)>>,
        shallow: bool,
    }

    impl StubSource {
        /// One commit with one file, plus `malformed` skipped records.
        fn with_malformed(malformed: usize) -> Self {
            Self {
                commits: vec![stub_commit(0, &["src/main.rs"])],
                malformed,
                cut_oldest: None,
                tracked: listed(&["src/main.rs"]),
                counts: Some(Vec::new()),
                count_calls: Default::default(),
                shallow: false,
            }
        }
    }

    fn stub_commit(n: usize, files: &[&str]) -> CommitInfo {
        CommitInfo {
            hash: format!("{n:040x}"),
            author: "Alice".to_string(),
            timestamp: 1_700_000_000 + n as i64,
            message: "feat: one".to_string(),
            changed_files: files
                .iter()
                .map(|p| types::FileChangeInfo {
                    path: std::path::PathBuf::from(p),
                    additions: 1,
                    deletions: 0,
                })
                .collect(),
        }
    }

    impl GitDataSource for StubSource {
        fn is_git_repo(&self) -> bool {
            true
        }
        fn get_repo_root(&self) -> anyhow::Result<String> {
            Ok("/stub/repo".to_string())
        }
        fn detect_shallow_clone(&self) -> bool {
            self.shallow
        }
        fn fetch_commit_count_since(&self, _n: usize) -> anyhow::Result<Option<u64>> {
            Ok(None)
        }
        fn fetch_commits(&self, _config: &HeatmapConfig) -> anyhow::Result<git_source::ParsedLog> {
            Ok(git_source::ParsedLog {
                commits: self.commits.clone(),
                malformed: self.malformed,
                cut_oldest: self.cut_oldest,
                ..ParsedLog::default()
            })
        }
        fn detect_partial_clone(&self, _repo_root: &str) -> bool {
            false
        }
        fn list_tracked_files(
            &self,
            _repo_root: &str,
            _no_lazy_fetch: bool,
        ) -> anyhow::Result<HeadTree> {
            match &self.tracked {
                TrackedStub::Listed(entries) => Ok(entries.clone()),
                TrackedStub::Unavailable => anyhow::bail!("simulated ls-tree failure"),
            }
        }
        fn count_head_lines(
            &self,
            repo_root: &str,
            path: Option<&str>,
            _no_lazy_fetch: bool,
        ) -> anyhow::Result<git_source::HeadCounts> {
            self.count_calls
                .borrow_mut()
                .push((repo_root.into(), path.map(str::to_owned)));
            self.counts
                .clone()
                .ok_or_else(|| anyhow::anyhow!("simulated grep failure"))
        }
        fn show_prefix(&self) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }

    #[test]
    fn test_head_data_uses_one_scoped_count_and_joins_only_regular_entries() {
        let source = StubSource {
            tracked: TrackedStub::Listed(HeadTree::from([
                ("src/main.rs".into(), HeadEntry::Regular(None)),
                ("src/link.rs".into(), HeadEntry::Other),
                ("src/binary.rs".into(), HeadEntry::Regular(None)),
            ])),
            counts: Some(vec![
                ("src/main.rs".into(), 12),
                ("src/link.rs".into(), 3),
                ("src/missing.rs".into(), 7),
                ("src/gone.rs".into(), 1),
            ]),
            ..StubSource::with_malformed(0)
        };
        let prepared = prepare_commits(
            &source,
            HeatmapArgs {
                path: Some("src".into()),
                ..HeatmapArgs::default()
            },
        )
        .unwrap()
        .expect("the run continues");
        assert_eq!(
            *source.count_calls.borrow(),
            [("/stub/repo".into(), Some("src".into()))]
        );
        let tree = prepared.head_tree.unwrap();
        assert_eq!(tree["src/main.rs"], HeadEntry::Regular(Some(12)));
        assert_eq!(tree["src/link.rs"], HeadEntry::Other);
        assert_eq!(tree["src/binary.rs"], HeadEntry::Regular(None));
        assert_eq!(
            prepared.warnings,
            ["HEAD data: 2 line-count rows have no tree entry."]
        );
    }

    #[test]
    fn test_head_data_falls_back_once_after_either_failure() {
        for (tracked, count_calls) in [(TrackedStub::Unavailable, 0), (listed(&["src/main.rs"]), 1)]
        {
            let source = StubSource {
                tracked,
                counts: None,
                ..StubSource::with_malformed(0)
            };
            let prepared = prepare_commits(&source, HeatmapArgs::default())
                .unwrap()
                .expect("the run continues");
            assert_eq!(source.count_calls.borrow().len(), count_calls);
            assert!(prepared.head_tree.is_none());
            assert_eq!(prepared.warnings, [HEAD_UNAVAILABLE]);
            assert_eq!(prepared.commits.len(), 1);
        }
    }

    fn malformed_warnings(prepared: &PreparedCommits) -> Vec<&String> {
        prepared
            .warnings
            .iter()
            .filter(|w| w.contains("malformed git log"))
            .collect()
    }

    #[test]
    fn test_prepare_commits_adds_one_warning_with_malformed_count() {
        let source = StubSource::with_malformed(3);
        let prepared = prepare_commits(&source, HeatmapArgs::default())
            .unwrap()
            .expect("one good commit remains");
        assert_eq!(
            malformed_warnings(&prepared),
            vec!["Skipped 3 malformed git log records."]
        );
        assert_eq!(prepared.commits.len(), 1);
    }

    #[test]
    fn test_prepare_commits_uses_the_singular_noun_for_one_malformed_record() {
        let source = StubSource::with_malformed(1);
        let prepared = prepare_commits(&source, HeatmapArgs::default())
            .unwrap()
            .expect("one good commit remains");
        assert_eq!(
            malformed_warnings(&prepared),
            vec!["Skipped 1 malformed git log record."]
        );
    }

    #[test]
    fn test_log_warning_names_each_nonzero_count() {
        let parsed = ParsedLog {
            malformed: 1,
            unpaired: 2,
            unsafe_paths: 3,
            ..ParsedLog::default()
        };
        assert_eq!(
            skipped_log_warning(&parsed).as_deref(),
            Some("Skipped 1 malformed git log record, 2 unpaired git log records, 3 unsafe paths.")
        );
    }

    #[test]
    fn test_prepare_commits_no_malformed_warning_when_count_is_zero() {
        let source = StubSource::with_malformed(0);
        let prepared = prepare_commits(&source, HeatmapArgs::default())
            .unwrap()
            .expect("one good commit remains");
        assert!(malformed_warnings(&prepared).is_empty());
    }

    fn cut_warnings(prepared: &PreparedCommits) -> Vec<&String> {
        prepared
            .warnings
            .iter()
            .filter(|w| w.contains("output limit"))
            .collect()
    }

    #[test]
    fn test_prepare_commits_adds_one_warning_when_git_log_output_stopped_early() {
        let source = StubSource {
            cut_oldest: Some(1_700_000_000),
            ..StubSource::with_malformed(0)
        };
        let prepared = prepare_commits(&source, HeatmapArgs::default())
            .unwrap()
            .expect("one good commit remains");
        assert_eq!(
            cut_warnings(&prepared),
            vec![
                "git log output stopped at the 64 MiB output limit; the oldest commit counted is from 2023-11-14, and older history is not counted. Use --path, --since or --last for a full count."
            ]
        );
        assert_eq!(prepared.commits.len(), 1);
    }

    #[test]
    fn test_prepare_commits_no_output_limit_warning_for_full_output() {
        let prepared = prepare_commits(&StubSource::with_malformed(0), HeatmapArgs::default())
            .unwrap()
            .expect("one good commit remains");
        assert!(cut_warnings(&prepared).is_empty());
    }

    #[test]
    fn test_a_run_with_every_warning_kind_gives_one_line_for_each_kind() {
        // One commit too large to pair, in a shallow clone, with malformed
        // records and git log output at its limit.
        let paths: Vec<String> = (0..=COUPLING_MAX_FILES)
            .map(|i| format!("src/f{i:02}.rs"))
            .collect();
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let source = StubSource {
            commits: vec![stub_commit(0, &paths)],
            malformed: 1,
            cut_oldest: Some(1_700_000_000),
            tracked: TrackedStub::Unavailable,
            shallow: true,
            ..StubSource::with_malformed(0)
        };
        // Two window flags, and the stub has fewer than 5 commits.
        let args = HeatmapArgs {
            last_n: Some(5),
            window_preset: Some("all".to_string()),
            ..HeatmapArgs::default()
        };
        let p = prepare_commits(&source, args)
            .unwrap()
            .expect("the run continues");
        let mut result = compute_heatmap(
            &p.commits,
            &p.config,
            &p.window,
            p.now_epoch,
            p.repo_root,
            p.warnings,
            &p.module_roots,
        );
        warn_missing_targets(&mut result, &["gone.rs".to_string()], &HashSet::new());

        assert_eq!(
            result
                .warnings
                .iter()
                .filter(|warning| warning.starts_with("HEAD data"))
                .count(),
            1,
            "{:?}",
            result.warnings
        );
        let distinct: std::collections::HashSet<&String> = result.warnings.iter().collect();
        assert_eq!(
            result.warnings.len(),
            WARNING_KINDS,
            "{:?}",
            result.warnings
        );
        assert_eq!(
            distinct.len(),
            result.warnings.len(),
            "{:?}",
            result.warnings
        );
    }

    /// `MODULE_MIN_COMMITS` commits each in `crates/a`, `crates/b` and
    /// `packages/x`, so every module reaches the module support minimum.
    fn package_commits() -> Vec<CommitInfo> {
        (0..MODULE_MIN_COMMITS)
            .flat_map(|i| {
                [
                    stub_commit(3 * i, &["crates/a/src/lib.rs"]),
                    stub_commit(3 * i + 1, &["crates/b/src/lib.rs"]),
                    stub_commit(3 * i + 2, &["packages/x/index.js"]),
                ]
            })
            .collect()
    }

    /// Sorted module paths of a full run on `package_commits` with `tracked`.
    fn module_paths_with(tracked: TrackedStub) -> Vec<String> {
        let source = StubSource {
            commits: package_commits(),
            tracked,
            ..StubSource::with_malformed(0)
        };
        let p = prepare_commits(&source, HeatmapArgs::default())
            .unwrap()
            .expect("the run continues");
        let result = compute_heatmap(
            &p.commits,
            &p.config,
            &p.window,
            p.now_epoch,
            p.repo_root,
            p.warnings,
            &p.module_roots,
        );
        let mut paths: Vec<String> = result.modules.into_iter().map(|m| m.path).collect();
        paths.sort();
        paths
    }

    fn listed(paths: &[&str]) -> TrackedStub {
        TrackedStub::Listed(
            paths
                .iter()
                .map(|&p| (p.into(), HeadEntry::Regular(None)))
                .collect(),
        )
    }

    #[test]
    fn test_modules_follow_tracked_manifest_folders() {
        let tracked = listed(&[
            "Cargo.toml",
            "crates/a/Cargo.toml",
            "crates/a/src/lib.rs",
            "crates/b/Cargo.toml",
            "crates/b/src/lib.rs",
            "packages/x/index.js",
            "packages/x/package.json",
        ]);
        assert_eq!(
            module_paths_with(tracked),
            vec!["crates/a", "crates/b", "packages/x"]
        );
    }

    #[test]
    fn test_modules_with_only_root_manifest_use_top_folder() {
        let tracked = listed(&["Cargo.toml", "package.json", "crates/a/src/lib.rs"]);
        assert_eq!(module_paths_with(tracked), vec!["crates", "packages"]);
    }

    #[test]
    fn test_modules_use_top_folder_when_tracked_list_is_unavailable() {
        assert_eq!(
            module_paths_with(TrackedStub::Unavailable),
            vec!["crates", "packages"]
        );
    }

    // -----------------------------------------------------------------------
    // Window totals and the view spec
    // -----------------------------------------------------------------------

    fn authored_commit(n: usize, author: &str, message: &str) -> CommitInfo {
        CommitInfo {
            author: author.to_string(),
            message: message.to_string(),
            ..stub_commit(n, &["src/main.rs"])
        }
    }

    /// A full run on `commits` with default args, up to the result.
    fn result_of(commits: Vec<CommitInfo>) -> HeatmapResult {
        let source = StubSource {
            commits,
            ..StubSource::with_malformed(0)
        };
        let p = prepare_commits(&source, HeatmapArgs::default())
            .unwrap()
            .expect("the run continues");
        compute_heatmap(
            &p.commits,
            &p.config,
            &p.window,
            p.now_epoch,
            p.repo_root,
            p.warnings,
            &p.module_roots,
        )
    }

    #[test]
    fn test_compute_heatmap_counts_window_fix_commits_and_authors() {
        let result = result_of(vec![
            authored_commit(0, "Alice", "fix: crash on start"),
            authored_commit(1, "Bob", "feat: add flag"),
            authored_commit(2, "Alice", "Fix the parser"),
            authored_commit(3, "Carol", "docs: prefix"),
            authored_commit(4, "Alice", "feat: more"),
        ]);
        assert_eq!(result.fix_commits_total, 2);
        assert_eq!(result.window_authors, 3);
    }

    #[test]
    fn test_compute_heatmap_one_author_window_counts_one_author() {
        let result = result_of(vec![
            authored_commit(0, "Alice", "feat: a"),
            authored_commit(1, "Alice", "feat: b"),
        ]);
        assert_eq!(result.window_authors, 1);
        assert_eq!(result.fix_commits_total, 0);
    }

    #[test]
    fn test_compute_heatmap_large_commit_warning_uses_the_plural_for_two() {
        let names: Vec<String> = (0..=COUPLING_MAX_FILES)
            .map(|i| format!("src/f{i}.rs"))
            .collect();
        let files: Vec<&str> = names.iter().map(String::as_str).collect();
        let result = result_of(vec![stub_commit(0, &files), stub_commit(1, &files)]);
        let coupling: Vec<&String> = result
            .warnings
            .iter()
            .filter(|w| w.starts_with("coupling:"))
            .collect();
        assert_eq!(
            coupling,
            ["coupling: 2 commits with more than 50 files not paired"]
        );
    }

    fn card_view(
        commits: Vec<CommitInfo>,
        args: HeatmapArgs,
    ) -> (HeatmapResult, view::Section<card::Card>, view::ListCaps) {
        let source = StubSource {
            commits,
            ..StubSource::with_malformed(0)
        };
        let p = prepare_commits(&source, args).unwrap().expect("card view");
        let caps = view::ViewSpec::from_config(&p.config).lists();
        let FileTargets::Cards(paths) = &p.config.targets else {
            panic!("expected file targets");
        };
        let mut result = compute_heatmap(
            &p.commits,
            &p.config,
            &p.window,
            p.now_epoch,
            p.repo_root,
            p.warnings,
            &p.module_roots,
        );
        let cards = scope_cards(
            &mut result,
            &p.commits,
            paths,
            caps,
            &p.module_roots,
            &p.excludes,
        );
        (result, cards, caps)
    }

    #[test]
    fn test_card_warning_names_three_targets_in_argument_order() {
        let names: Vec<String> = (0..51).map(|i| format!("src/f{i}.rs")).collect();
        let files: Vec<&str> = names.iter().map(String::as_str).collect();
        let (result, _, _) = card_view(
            vec![stub_commit(0, &files), stub_commit(1, &files)],
            HeatmapArgs {
                files: ["src/f4.rs", "src/f2.rs", "src/f1.rs", "src/f3.rs"]
                    .map(String::from)
                    .into(),
                ..HeatmapArgs::default()
            },
        );
        assert_eq!(
            result.warnings,
            [
                "coupling: 2 commits with more than 50 files not paired (2 touch src/f4.rs, 2 touch src/f2.rs, 2 touch src/f1.rs and 1 more)"
            ]
        );
    }

    #[test]
    fn test_card_warning_replaces_the_exact_global_line_in_place() {
        let mut warnings = [
            "coupling: other detail",
            "coupling: 1 commit with more than 50 files not paired",
            "Shallow clone detected",
        ]
        .map(String::from)
        .to_vec();
        scope_coupling_warning(&mut warnings, 1, &[("src/core.rs", 1)]);
        assert_eq!(
            warnings,
            [
                "coupling: other detail",
                "coupling: 1 commit with more than 50 files not paired (1 touches src/core.rs)",
                "Shallow clone detected",
            ]
        );
    }

    #[test]
    fn test_card_warning_omits_targets_past_the_cap() {
        let names: Vec<String> = (0..51).map(|i| format!("src/f{i}.rs")).collect();
        let files: Vec<&str> = names.iter().map(String::as_str).collect();
        let (result, _, _) = card_view(
            vec![stub_commit(0, &files), stub_commit(1, &["src/solo.rs"])],
            HeatmapArgs {
                files: ["src/solo.rs", "a.rs", "b.rs", "c.rs", "d.rs", "src/f0.rs"]
                    .map(String::from)
                    .into(),
                ..HeatmapArgs::default()
            },
        );
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn test_excluded_card_names_the_filter_and_preserves_the_window_count() {
        let (result, cards, caps) = card_view(
            vec![stub_commit(0, &["package-lock.json"])],
            HeatmapArgs {
                files: vec!["package-lock.json".into()],
                ..HeatmapArgs::default()
            },
        );
        let rendered = render_cards_text(&result, &cards, caps);
        assert_eq!(result.window.commits_analyzed, 0);
        assert_eq!(
            rendered.lines().last(),
            Some("package-lock.json: left out by the default excludes; --no-exclude keeps it")
        );
        let json = serde_json::to_value(&cards.rows[0]).unwrap();
        assert_eq!(json["in_window"], false);
        assert_eq!(json["excluded"], "default");
    }

    #[test]
    fn test_no_exclude_restores_the_lockfile_card() {
        let (result, cards, caps) = card_view(
            vec![stub_commit(0, &["package-lock.json"])],
            HeatmapArgs {
                files: vec!["package-lock.json".into()],
                no_exclude: true,
                ..HeatmapArgs::default()
            },
        );
        let rendered = render_cards_text(&result, &cards, caps);
        assert!(
            rendered
                .lines()
                .any(|line| line == "package-lock.json: 1 commit")
        );
        let json = serde_json::to_value(&cards.rows[0]).unwrap();
        assert_eq!(json["in_window"], true);
        assert!(json.get("excluded").is_none());
    }

    #[test]
    fn test_card_names_the_user_exclude_flag() {
        let (result, cards, caps) = card_view(
            vec![stub_commit(0, &["src/core.rs", "src/extra.rs"])],
            HeatmapArgs {
                files: vec!["src/extra.rs".into()],
                extra_excludes: vec![globset::Glob::new("src/extra.rs").unwrap()],
                ..HeatmapArgs::default()
            },
        );
        let rendered = render_cards_text(&result, &cards, caps);
        assert_eq!(
            rendered.lines().last(),
            Some("src/extra.rs: left out by --exclude; remove the matching patterns to keep it")
        );
        let json = serde_json::to_value(&cards.rows[0]).unwrap();
        assert_eq!(json["excluded"], "user");
    }

    #[test]
    fn test_view_spec_reads_top_only_when_it_was_given() {
        let spec_of = |args: HeatmapArgs| {
            let p = prepare_commits(&StubSource::with_malformed(0), args)
                .unwrap()
                .expect("the run continues");
            view::ViewSpec::from_config(&p.config)
        };
        assert_eq!(
            spec_of(HeatmapArgs::default()),
            view::ViewSpec {
                detail: None,
                top: None,
                include_docs: false,
            }
        );
        let explicit = HeatmapArgs {
            top_n: 3,
            top_explicit: true,
            include_docs: true,
            detail: Some(types::DetailSection::Coupling),
            ..HeatmapArgs::default()
        };
        assert_eq!(
            spec_of(explicit),
            view::ViewSpec {
                detail: Some(types::DetailSection::Coupling),
                top: Some(3),
                include_docs: true,
            }
        );
    }

    #[test]
    fn test_view_spec_keeps_non_code_paths_for_a_non_code_path_scope() {
        let keeps = |path: &str| {
            let args = HeatmapArgs {
                path: Some(path.to_string()),
                ..HeatmapArgs::default()
            };
            let p = prepare_commits(&StubSource::with_malformed(0), args)
                .unwrap()
                .expect("the run continues");
            view::ViewSpec::from_config(&p.config).include_docs
        };
        assert!(keeps("docs"));
        assert!(keeps("./.github/workflows"));
        assert!(keeps("tests/"));
        assert!(!keeps("src"));
        assert!(!keeps("crates/rskim/tests"));
    }

    // -----------------------------------------------------------------------
    // Analytics recording
    // -----------------------------------------------------------------------

    #[test]
    #[serial_test::serial]
    fn test_record_heatmap_analytics_insights_label() {
        use std::time::Duration;
        use tempfile::NamedTempFile;

        let tmp = NamedTempFile::new().unwrap();
        let db_path = tmp.path().to_str().unwrap().to_string();

        let _ = crate::analytics::AnalyticsDb::open(tmp.path()).unwrap();

        // SAFETY: test is single-threaded; no concurrent env reads.
        unsafe { std::env::set_var("SKIM_ANALYTICS_DB", &db_path) };

        let analytics = crate::analytics::AnalyticsConfig {
            enabled: true,
            input_cost_per_mtok: None,
            session_id: None,
        };

        record_heatmap_analytics(
            &analytics,
            "skim heatmap --insights",
            Duration::from_millis(42),
        );
        crate::analytics::flush_pending();

        let db = crate::analytics::AnalyticsDb::open(tmp.path()).unwrap();
        let cmds = db.query_by_original_cmd(None).unwrap();
        assert!(
            cmds.iter()
                .any(|c| c.original_cmd == "skim heatmap --insights"),
            "expected 'skim heatmap --insights' in recorded commands, got: {:?}",
            cmds.iter().map(|c| &c.original_cmd).collect::<Vec<_>>()
        );

        // SAFETY: test is single-threaded; no concurrent env reads.
        unsafe { std::env::remove_var("SKIM_ANALYTICS_DB") };
    }
}
