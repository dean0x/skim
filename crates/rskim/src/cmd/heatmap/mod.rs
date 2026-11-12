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

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::analytics::{CommandType, RecordingContext};

use args::{parse_args, print_help};
use excludes::{build_exclude_set, should_exclude};
use git_source::{CliGitSource, GitDataSource};
use insights::{build_insights_result, compute_insights, keep_code_findings};
use metrics::{
    CouplingResult, ModuleRoots, compute_authors, compute_churn, compute_coupling,
    compute_encapsulation, compute_fix_after_touch, compute_stability, module_of, without_quotes,
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
            eprintln!("skim heatmap: {e}");
            eprintln!("Run `skim heatmap --help` for usage.");
            return Ok(ExitCode::FAILURE);
        }
    };

    let git_source = CliGitSource::new();

    // Resolve --diff to concrete file list (borrows raw_args mutably)
    if let Some(exit) = resolve_diff_files(&git_source, &mut raw_args)? {
        return Ok(exit);
    }

    // Move raw_args into run_with_source for resolution and execution
    run_with_source(&git_source, raw_args, analytics)
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
            eprintln!("skim heatmap: no files changed vs '{base}'");
            Ok(Some(ExitCode::FAILURE))
        }
        Ok(files) => {
            // git diff output is repo-root-relative, so resolve paths against
            // the repo root to avoid cwd-dependent failures. The warning gates
            // nothing: `args.diff_files` still receives the full diff list, and
            // apply_file_scope filters the output by set intersection.
            let root = git_source.get_repo_root().unwrap_or_default();
            if let Some(warning) = diff_not_file_warning(std::path::Path::new(&root), &files) {
                eprintln!("skim heatmap: warning: {warning}");
            }
            args.diff_files = files;
            Ok(None)
        }
        Err(e) => {
            eprintln!("skim heatmap: {e}");
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
/// `walk::list_tracked_files` and `temporal_build::rel_is_regular_file`.
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
        [only] => Some(format!("'{only}' is not a regular file on current branch")),
        _ => Some(format!(
            "{} changed paths are not regular files on current branch: {}",
            not_files.len(),
            named_paths(&not_files)
        )),
    }
}

/// A path from git output as the relative path that it names, for a test
/// on disk or against the exclude patterns, or `None` when its quoted form
/// does not decode. git writes a name with unusual bytes in double quotes,
/// with C escapes and 3-digit octal escapes (the form of
/// `card::git_quoted`); a test on the quoted text would name no file.
fn disk_path(path: &str) -> Option<std::path::PathBuf> {
    let (inner, quoted) = without_quotes(path);
    if !quoted {
        return Some(std::path::PathBuf::from(path));
    }
    path_from_bytes(c_unescaped(inner)?)
}

/// The bytes of the text inside git's quotes, or `None` for an escape that
/// git does not write. Each pass reads at least one byte, so the loop ends
/// after at most `inner.len()` passes.
fn c_unescaped(inner: &str) -> Option<Vec<u8>> {
    let mut rest = inner.bytes();
    let mut out = Vec::with_capacity(inner.len());
    while let Some(b) = rest.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        let byte = match rest.next()? {
            b'a' => 0x07,
            b'b' => 0x08,
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => 0x0b,
            b'f' => 0x0c,
            b'r' => b'\r',
            b'"' => b'"',
            b'\\' => b'\\',
            // Three octal digits, the first 0-3, so the value fits in a byte.
            first @ b'0'..=b'3' => {
                let mut value = first - b'0';
                for _ in 0..2 {
                    let digit = rest.next().filter(|d| (b'0'..=b'7').contains(d))?;
                    value = value * 8 + (digit - b'0');
                }
                value
            }
            _ => return None,
        };
        out.push(byte);
    }
    Some(out)
}

#[cfg(unix)]
fn path_from_bytes(bytes: Vec<u8>) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Some(std::ffi::OsString::from_vec(bytes).into())
}

/// Off Unix a path is text: bytes that are not UTF-8 name no file.
#[cfg(not(unix))]
fn path_from_bytes(bytes: Vec<u8>) -> Option<std::path::PathBuf> {
    String::from_utf8(bytes).ok().map(Into::into)
}

/// Kinds of run warning: a shallow clone, more than one window flag, an
/// unresolved `--last`, an unknown `--window` preset, malformed git log
/// records, git log output at its limit, commits too large to pair and
/// `--diff` files not in the window. Each kind adds at most one line, so a
/// run has at most this many warnings in either JSON output.
const WARNING_KINDS: usize = 8;

/// True when `warnings` has at most [`WARNING_KINDS`] lines and no line
/// twice.
fn one_warning_per_kind(warnings: &[String]) -> bool {
    let distinct: std::collections::HashSet<&str> = warnings.iter().map(String::as_str).collect();
    warnings.len() <= WARNING_KINDS && distinct.len() == warnings.len()
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
}

/// Execute Steps 1–4 of the heatmap pipeline (git I/O + exclusions).
///
/// Consumes `args` by value — after this call `HeatmapArgs` is replaced by the
/// resolved `HeatmapConfig` stored in `PreparedCommits`.
///
/// Returns `Ok(None)` for all early-exit conditions (not a git repo, no commits,
/// all commits excluded). Callers treat `None` as a `ExitCode::FAILURE` signal.
/// Returns `Ok(Some(PreparedCommits))` when the pipeline should proceed to
/// metric computation.
fn prepare_commits(
    source: &dyn GitDataSource,
    mut args: HeatmapArgs,
) -> anyhow::Result<Option<PreparedCommits>> {
    // Step 1: Validate git environment
    if !source.is_git_repo() {
        eprintln!("skim heatmap: Not a git repository");
        return Ok(None);
    }

    let repo_root = match source.get_repo_root() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("skim heatmap: {e}");
            return Ok(None);
        }
    };

    // Step 1b: file arguments become repo-relative card targets, before any
    // history is read. A path outside the root ends the run with no card.
    if !args.files.is_empty() {
        match card_targets(source, &repo_root, &args.files) {
            Ok(targets) => args.files = targets,
            Err(e) => {
                eprintln!("skim heatmap: {e}");
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
        eprintln!("[skim:heatmap] repo root: {repo_root}");
    }

    // Capture a single clock snapshot for all window-resolution helpers so that
    // preset_to_since_secs, resolve_config, and build_window_info all use the
    // same value (temporal consistency).
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
        eprintln!("[skim:heatmap] window mode: {mode}");
        if let Some(since) = config.since {
            eprintln!(
                "[skim:heatmap] since epoch: {since} ({})",
                format_epoch(since)
            );
        }
    }

    // Step 3: Fetch commits
    let parsed = match source.fetch_commits(&config) {
        Ok(p) => p,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not installed") || msg.contains("not in PATH") {
                eprintln!("skim heatmap: {msg}");
            } else {
                eprintln!("skim heatmap: failed to fetch git log: {msg}");
            }
            return Ok(None);
        }
    };

    if parsed.malformed > 0 {
        let noun = if parsed.malformed == 1 {
            "record"
        } else {
            "records"
        };
        warnings.push(format!(
            "Skipped {} malformed git log {noun} (bad hash or timestamp); their file changes are not counted.",
            parsed.malformed
        ));
    }
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
        eprintln!("[skim:heatmap] raw commits fetched: {}", raw_commits.len());
    }

    if raw_commits.is_empty() {
        eprintln!("skim heatmap: No commits found in repository");
        return Ok(None);
    }

    // Step 4: Apply exclusions
    let exclude_set = build_exclude_set(config.no_exclude, &config.extra_excludes);
    let raw_commit_count = raw_commits.len();
    let mut commits = raw_commits;
    for commit in &mut commits {
        commit
            .changed_files
            .retain(|f| !should_exclude(&f.path.to_string_lossy(), &exclude_set));
    }
    // Remove commits that are now file-less after exclusion
    commits.retain(|c| !c.changed_files.is_empty());

    if debug {
        eprintln!(
            "[skim:heatmap] commits after exclusion: {} ({} excluded)",
            commits.len(),
            raw_commit_count - commits.len()
        );
    }

    if commits.is_empty() {
        eprintln!("skim heatmap: No analyzable files after exclusions");
        return Ok(None);
    }

    let module_roots = load_module_roots(source, &repo_root);

    Ok(Some(PreparedCommits {
        commits,
        config,
        window,
        now_epoch,
        repo_root,
        warnings,
        module_roots,
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

/// Read the package roots from the tracked-file list of `repo_root`.
///
/// The list is optional input: when it is not available, modules use the
/// top-folder rule and the run continues.
fn load_module_roots(source: &dyn GitDataSource, repo_root: &str) -> ModuleRoots {
    match source.list_tracked_files(repo_root) {
        Ok(Some(paths)) => ModuleRoots::from_tracked_files(&paths),
        Ok(None) => ModuleRoots::default(),
        Err(e) => {
            crate::debug_log!(
                "[skim:heatmap] tracked-file list not available ({e}); modules use the top-folder rule"
            );
            ModuleRoots::default()
        }
    }
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
    } = match prepare_commits(source, args)? {
        Some(p) => p,
        None => return Ok(ExitCode::FAILURE),
    };

    // Steps 5-9: Compute metrics and assemble result
    let start_time = std::time::Instant::now();
    let mut result = compute_heatmap(
        &commits,
        &config,
        &window,
        now_epoch,
        repo_root,
        warnings,
        &module_roots,
    );
    let spec = view::ViewSpec::from_config(&config);

    // File targets scope the result after the full metric computation, so
    // the coupling of a target keeps its partners outside the scope.
    let cards = match &config.targets {
        FileTargets::All => None,
        FileTargets::Diff(files) => {
            warn_missing_targets(&mut result, files);
            apply_file_scope(&mut result, files, &module_roots);
            None
        }
        FileTargets::Cards(paths) => {
            // A card says itself when its file is not in the window, so
            // there is no warning for it.
            let targets = card::history_paths(paths, &result.files);
            let cards = card::build_cards(&commits, &result, &targets, spec.lists());
            apply_file_scope(&mut result, &targets, &module_roots);
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

/// Names in a counted warning about `--diff` paths. The warning counts the
/// paths that it does not name.
const WARNING_NAMES: usize = 3;

/// The first [`WARNING_NAMES`] of `names` in single quotes, then
/// `" and N more"` for the others.
fn named_paths(names: &[&str]) -> String {
    let shown: Vec<String> = names
        .iter()
        .take(WARNING_NAMES)
        .map(|name| format!("'{name}'"))
        .collect();
    let unnamed = names.len().saturating_sub(WARNING_NAMES);
    let rest = if unnamed > 0 {
        format!(" and {unnamed} more")
    } else {
        String::new()
    };
    format!("{}{rest}", shown.join(", "))
}

/// Add one warning for the `--diff` files that are not in the window's
/// history. It counts each missing file once and names the first
/// [`WARNING_NAMES`] in argument order (a set order would change
/// between runs). A large diff thus adds one warning, not one per file.
fn warn_missing_targets(result: &mut HeatmapResult, files: &[String]) {
    use std::collections::HashSet;

    let result_paths: HashSet<&str> = result.files.iter().map(|f| f.path.as_str()).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    let missing: Vec<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|target| !result_paths.contains(target) && seen.insert(target))
        .collect();
    let warning = match missing.as_slice() {
        [] => return,
        [only] => format!("targeted file '{only}' not found in git history"),
        _ => format!(
            "{} targeted files not found in git history: {}",
            missing.len(),
            named_paths(&missing)
        ),
    };
    result.warnings.push(warning);
}

/// Filter heatmap results to only include targeted files.
///
/// Filtering happens AFTER metric computation to preserve coupling accuracy.
/// Coupling graph retains edges where at least one endpoint is targeted
/// (blast radius view). Modules are kept when they hold a targeted file, by
/// the same [`module_of`] mapping the module metric uses.
fn apply_file_scope(result: &mut HeatmapResult, files: &[String], roots: &ModuleRoots) {
    use std::collections::HashSet;

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

    if large_commits_skipped > 0 {
        let noun = if large_commits_skipped == 1 {
            "commit"
        } else {
            "commits"
        };
        warnings.push(format!(
            "coupling: {large_commits_skipped} {noun} with more than {COUPLING_MAX_FILES} files not paired"
        ));
    }

    if config.debug {
        let elapsed = t0.elapsed();
        eprintln!(
            "[skim:heatmap] metrics computed in {:.1}ms — {} files, {} coupling edges, {} modules",
            elapsed.as_secs_f64() * 1000.0,
            churn_map.len(),
            coupling_graph.len(),
            modules.len(),
        );
    }

    // Window totals for the view gates: the fix-risk base rate and the
    // bus-factor author minimum.
    let fix_commits_total = commits
        .iter()
        .filter(|c| rskim_search::is_fix_commit(&c.message))
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
        patterns.extend(config.extra_excludes.iter().cloned());
        patterns
    };

    // Step 9: Build result
    HeatmapResult {
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

    // -----------------------------------------------------------------------
    // apply_file_scope
    // -----------------------------------------------------------------------

    fn make_test_result() -> HeatmapResult {
        use types::{
            AuthorMetrics, ChurnMetrics, CouplingEdge, FileMetrics, FixRiskMetrics, ModuleHealth,
            WindowInfo,
        };
        HeatmapResult {
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
        warn_missing_targets(&mut result, &["nonexistent.rs".to_string()]);
        assert_eq!(
            result.warnings,
            ["targeted file 'nonexistent.rs' not found in git history"]
        );
    }

    #[test]
    fn test_warn_missing_targets_gives_one_counted_warning_in_argument_order() {
        let names = ["z.rs", "a.rs", "m.rs", "z.rs", "k.rs", "b.rs"];
        let targets: Vec<String> = names.iter().map(|n| (*n).to_string()).collect();
        let mut result = make_test_result();
        result.warnings.clear();
        warn_missing_targets(&mut result, &targets);
        assert_eq!(
            result.warnings,
            ["5 targeted files not found in git history: 'z.rs', 'a.rs', 'm.rs' and 2 more"]
        );
    }

    #[test]
    fn test_warn_missing_targets_names_all_of_a_few_files() {
        let targets = ["x.rs".to_string(), "y.rs".to_string(), "x.rs".to_string()];
        let mut result = make_test_result();
        result.warnings.clear();
        warn_missing_targets(&mut result, &targets);
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
        warn_missing_targets(&mut result, &present);
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
        Listed(Vec<String>),
        Unavailable,
        Fails,
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
        shallow: bool,
    }

    impl StubSource {
        /// One commit with one file, plus `malformed` skipped records.
        fn with_malformed(malformed: usize) -> Self {
            Self {
                commits: vec![stub_commit(0, &["src/main.rs"])],
                malformed,
                cut_oldest: None,
                tracked: TrackedStub::Unavailable,
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
            })
        }
        fn list_tracked_files(&self, _repo_root: &str) -> anyhow::Result<Option<Vec<String>>> {
            match &self.tracked {
                TrackedStub::Listed(paths) => Ok(Some(paths.clone())),
                TrackedStub::Unavailable => Ok(None),
                TrackedStub::Fails => Err(anyhow::anyhow!("simulated ls-tree failure")),
            }
        }
        fn show_prefix(&self) -> anyhow::Result<String> {
            Ok(String::new())
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
            vec![
                "Skipped 3 malformed git log records (bad hash or timestamp); their file changes are not counted."
            ]
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
            vec![
                "Skipped 1 malformed git log record (bad hash or timestamp); their file changes are not counted."
            ]
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
        };
        // Two window flags, and the stub has fewer than 5 commits.
        let args = HeatmapArgs {
            last_n: Some(5),
            window_preset: Some("unknown".to_string()),
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
        warn_missing_targets(&mut result, &["gone.rs".to_string()]);

        // `--last` comes before `--window`, so the unknown-preset kind
        // cannot show with an unresolved `--last`: all other kinds show.
        let distinct: std::collections::HashSet<&String> = result.warnings.iter().collect();
        assert_eq!(
            result.warnings.len(),
            WARNING_KINDS - 1,
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
            malformed: 0,
            cut_oldest: None,
            tracked,
            shallow: false,
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
        TrackedStub::Listed(paths.iter().map(|p| p.to_string()).collect())
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
    fn test_modules_use_top_folder_when_tracked_list_fails() {
        assert_eq!(
            module_paths_with(TrackedStub::Fails),
            vec!["crates", "packages"]
        );
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
            malformed: 0,
            cut_oldest: None,
            tracked: TrackedStub::Unavailable,
            shallow: false,
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
