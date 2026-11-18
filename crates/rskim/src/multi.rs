//! Multi-file processing: glob patterns, directory traversal, and parallel execution.
//!
//! Orchestrates [`crate::process::process_file`] over multiple inputs using rayon
//! for parallelism. Uses the `ignore` crate (from ripgrep) for directory walking,
//! which respects `.gitignore`, `.ignore`, and `.git/info/exclude` by default.

use globset::GlobBuilder;
use ignore::WalkBuilder;
use rayon::prelude::*;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use rskim_core::{
    ElidedSide, Language, Mode, TruncationCountSource, detect_language_from_path,
    elision_marker_line, truncate_with_count_source,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::cascade::{RungWalk, serde_rungs, walk_rungs};
use crate::output::ELISION_HINT;
use crate::process::{
    ProcessOptions, ProcessResult, process_file, report_token_stats, run_budget_applies,
};
use crate::run_budget::{Body, Piece, PlannedBody, RUN_SUM_STOP_FACTOR, plan_cut};

/// Options for multi-file processing
#[derive(Debug, Clone)]
pub(crate) struct MultiFileOptions {
    pub(crate) process: ProcessOptions,
    pub(crate) no_header: bool,
    pub(crate) jobs: Option<usize>,
    pub(crate) no_ignore: bool,
    pub(crate) analytics_enabled: bool,
    pub(crate) session_id: Option<String>,
}

/// Glob metacharacters recognised by skim.
///
/// Used for detecting glob patterns in user input and for splitting the
/// static directory prefix from the glob suffix in [`glob_walk_root`].
/// Defined once here and re-used in `main.rs::looks_like_file_or_glob`.
pub(crate) const GLOB_METACHARACTERS: &[char] = &['*', '?', '[', '{'];

/// Check if path contains glob pattern characters
pub(crate) fn has_glob_pattern(path: &str) -> bool {
    path.contains(GLOB_METACHARACTERS)
}

/// Validate glob pattern to prevent path traversal attacks.
///
/// Absolute Unix paths (`/Users/…`) and Windows drive paths (`C:/…`) are
/// intentionally **allowed** — AI agents and shell users routinely pass fully
/// qualified paths. Only genuinely dangerous patterns are rejected:
///
/// - `..` traversal (escapes the intended subtree)
/// - Windows UNC network paths (`\\server\share`) — not local paths
fn validate_glob_pattern(pattern: &str) -> anyhow::Result<()> {
    // Reject Windows UNC paths (e.g., "\\server\share") — network paths, not local
    if pattern.starts_with("\\\\") {
        anyhow::bail!(
            "Glob pattern cannot use UNC network paths\n\
             Pattern: {}\n\
             Use a local path like '/Users/foo/src/**/*.ts' instead",
            pattern
        );
    }

    // Reject patterns containing .. (parent directory traversal)
    if pattern.contains("..") {
        anyhow::bail!(
            "Glob pattern cannot contain '..' (parent directory traversal)\n\
             Pattern: {}\n\
             This prevents accessing files outside the intended directory",
            pattern
        );
    }

    Ok(())
}

/// Configure an `ignore::WalkBuilder` with gitignore/hidden-file settings.
///
/// When `no_ignore` is false (default), the walker respects `.gitignore`,
/// global gitignore, `.git/info/exclude`, `.ignore` files, and skips hidden
/// files/directories. When true, all ignore rules are disabled.
fn configure_walker(builder: &mut WalkBuilder, no_ignore: bool) {
    let respect_ignore = !no_ignore;
    builder
        .hidden(respect_ignore)
        .git_ignore(respect_ignore)
        .git_global(respect_ignore)
        .git_exclude(respect_ignore)
        .ignore(respect_ignore)
        .parents(respect_ignore)
        .require_git(false)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b));
}

/// Extract the static directory prefix and glob override pattern from a user
/// glob pattern.
///
/// The walker needs a root directory to start from and an override pattern
/// to filter files. We split on `/`, taking leading segments that contain no
/// glob metacharacters ([`GLOB_METACHARACTERS`]), and join them as the root.
/// The remainder becomes the override pattern.
///
/// Absolute Unix paths are handled correctly: the leading empty segment from
/// `"/Users/foo/src/**/*.ts".split('/')` is treated as a static segment, so
/// the computed root includes the leading `/`.
///
/// # Examples
///
/// ```text
/// "src/**/*.ts"             -> ("src",            "**/*.ts")
/// "*.ts"                    -> (".",               "*.ts")
/// "src/utils/**/*.ts"       -> ("src/utils",       "**/*.ts")
/// "**/*.ts"                 -> (".",               "**/*.ts")
/// "src/*.rs"                -> ("src",             "*.rs")
/// "/Users/foo/src/**/*.ts"  -> ("/Users/foo/src",  "**/*.ts")
/// "/**/*.ts"                -> ("/",               "**/*.ts")
/// ```
fn glob_walk_root(pattern: &str) -> (&str, &str) {
    let segments: Vec<&str> = pattern.split('/').collect();
    let mut static_count = 0;

    for segment in &segments {
        if segment.contains(GLOB_METACHARACTERS) {
            break;
        }
        static_count += 1;
    }

    if static_count == 0 {
        // First segment itself contains glob chars (e.g. "**/*.ts", "*.ts")
        (".", pattern)
    } else if static_count == segments.len() {
        // All segments are static (no glob metacharacters). Treat the
        // entire pattern as a root with a match-everything glob. This is
        // defensive -- callers are expected to verify glob chars exist
        // before calling, but we must not panic on unexpected input.
        (pattern, "**")
    } else {
        // Find the byte offset where the glob portion starts.
        // For absolute paths like "/Users/foo/src/**/*.ts":
        //   segments = ["", "Users", "foo", "src", "**", "*.ts"]
        //   static_count = 4  (segments 0..4 have no glob chars)
        //   root = "/Users/foo/src" (len=14)
        //
        // The formula: sum of static segment lengths + (static_count - 1) separators
        // gives the end index of the last static segment in the original string.
        // For absolute paths the leading "" segment has len=0, so the leading "/" is
        // captured as the separator between segment 0 and segment 1.
        let root_end: usize = segments[..static_count]
            .iter()
            .map(|s| s.len())
            .sum::<usize>()
            + static_count
            - 1; // account for the '/' separators between segments

        // Edge case: absolute path whose first glob char appears right after the
        // leading slash (e.g. "/**/*.ts").
        //   segments = ["", "**", "*.ts"], static_count = 1
        //   root_end = 0 + 1 - 1 = 0  =>  pattern[..0] = ""  (wrong — should be "/")
        // Handle by checking if root_end would produce an empty slice for an absolute
        // path: in that case return "/" as the root.
        let root = if root_end == 0 && pattern.starts_with('/') {
            "/"
        } else {
            &pattern[..root_end]
        };

        let rest = &pattern[root_end + 1..]; // skip the '/' separator
        (root, rest)
    }
}

/// Format a hint about `--no-ignore` when gitignore filtering is active.
fn no_ignore_hint(no_ignore: bool) -> &'static str {
    if no_ignore {
        ""
    } else {
        "\nHint: Files may be excluded by .gitignore. Use --no-ignore to include all files."
    }
}

type Counter<'a> = dyn Fn(&str) -> Option<usize> + Sync + 'a;
type Transform<'a> = dyn Fn(&Path, ProcessOptions) -> anyhow::Result<ProcessResult> + Sync + 'a;

struct RunFile<'a> {
    path: &'a Path,
    prefix: String,
    pass_1: &'a ProcessResult,
}

#[derive(Clone, Copy)]
enum RungKind {
    Probe,
    CutBase,
}

struct RungSet {
    pieces: Vec<Piece>,
    differing: Vec<bool>,
    kept_pass_1: Vec<bool>,
    complete: bool,
    is_empty: bool,
}

struct RunSelection {
    output: String,
    bodies: Vec<PlannedBody>,
    mode: Mode,
    requested: Mode,
    differing: Vec<bool>,
    kept_pass_1: Vec<bool>,
    omitted: usize,
}

impl RunSelection {
    fn changed(&self) -> bool {
        self.mode != self.requested || self.omitted > 0 || self.bodies.iter().any(|b| b.cut)
    }
}

impl RungSet {
    fn fitting(&self, n: usize, count: &Counter<'_>) -> Option<String> {
        self.complete
            .then(|| crate::run_budget::set_fits(&self.pieces, n, count))
            .flatten()
    }

    fn accept(self, output: String, mode: Mode, requested: Mode) -> RunSelection {
        RunSelection {
            output,
            bodies: self
                .pieces
                .into_iter()
                .map(|p| PlannedBody {
                    output: match p.body {
                        Body::Whole(s) | Body::Fixed(s) => s,
                        Body::PreCut { kept, marker, .. } => kept + &marker,
                    },
                    cut: false,
                })
                .collect(),
            mode,
            requested,
            differing: self.differing,
            kept_pass_1: self.kept_pass_1,
            omitted: 0,
        }
    }
}

#[cfg(test)]
fn pass_one_set(files: &[RunFile<'_>], count: &Counter<'_>) -> RungSet {
    RungSet {
        pieces: files
            .iter()
            .map(|f| Piece {
                prefix: f.prefix.clone(),
                body: Body::Fixed(f.pass_1.output.clone()),
                cut_language: None,
                count: count(&(f.prefix.clone() + &f.pass_1.output)).unwrap_or(usize::MAX),
                floor: usize::MAX,
            })
            .collect(),
        differing: files.iter().map(|f| f.pass_1.view_differs).collect(),
        kept_pass_1: vec![false; files.len()],
        complete: true,
        is_empty: false,
    }
}

/// The index is in all results, not just successes. A failed first file still
/// leaves the separator before the next header, as in the streaming path.
fn piece_prefix(idx: usize, path: &Path, show_headers: bool) -> String {
    if show_headers {
        format!("{}// {}\n", if idx > 0 { "\n" } else { "" }, path.display())
    } else {
        String::new()
    }
}

/// Write the exact string whose joined count passed. Do not assemble it again.
fn write_run(writer: &mut impl Write, candidate: &str) -> io::Result<()> {
    writer.write_all(candidate.as_bytes())
}

fn note_probe_count(cost: usize, n: usize, sum: &AtomicUsize, stop: &AtomicBool) {
    let added = cost.min(n.saturating_add(1));
    let previous = sum.fetch_add(added, Ordering::Relaxed);
    if cost > n || previous.saturating_add(added) > n.saturating_mul(RUN_SUM_STOP_FACTOR) {
        stop.store(true, Ordering::Relaxed);
    }
}

fn make_piece(
    prefix: String,
    output: String,
    language: Option<Language>,
    fixed: bool,
    count: &Counter<'_>,
) -> Piece {
    let cost = count(&(prefix.clone() + &output)).unwrap_or(usize::MAX);
    let floor = if fixed {
        cost
    } else {
        let marker = elision_marker_line(
            language,
            output.lines().count(),
            ElidedSide::Truncated,
            None,
        );
        cost.min(count(&(prefix.clone() + &marker)).unwrap_or(usize::MAX))
    };
    Piece {
        prefix,
        body: if fixed {
            Body::Fixed(output)
        } else {
            Body::Whole(output)
        },
        cut_language: language,
        count: cost,
        floor,
    }
}

/// A pre-cut is transient. Only the plain transform above it can enter cache.
fn precut_piece(mut piece: Piece, n: usize, count: &Counter<'_>) -> Piece {
    if let (Body::Whole(text), Some(language)) = (&piece.body, piece.cut_language) {
        let body_count = count(text).unwrap_or(usize::MAX);
        if body_count > n {
            let total_view_lines = text.lines().count();
            let mut kept = truncate_with_count_source(
                text,
                language,
                n,
                |s| count(s).unwrap_or(usize::MAX),
                Some(body_count),
                Some(ELISION_HINT),
                TruncationCountSource::ViewLines,
            )
            .unwrap_or_else(|_| {
                elision_marker_line(
                    Some(language),
                    total_view_lines,
                    ElidedSide::Truncated,
                    None,
                )
            });
            let marked_count = count(&kept).unwrap_or(usize::MAX);
            piece.count = count(&(piece.prefix.clone() + &kept)).unwrap_or(usize::MAX);
            let marker_start = kept
                .strip_suffix('\n')
                .unwrap_or(&kept)
                .rfind('\n')
                .map_or(0, |i| i + 1);
            let marker = kept.split_off(marker_start);
            piece.body = Body::PreCut {
                kept,
                marker,
                total_view_lines,
                marked_count,
            };
        }
    }
    piece
}

/// Keep pass 1, at most two probes (about 2N tokens each plus one in-flight
/// body per worker), one cut base (at most N per cuttable body), and one transient
/// file per worker. Fixed bodies keep their all-or-nothing pass-1 size.
#[allow(clippy::too_many_arguments)]
fn run_rung(
    files: &[RunFile<'_>],
    mode: Mode,
    kind: RungKind,
    n: usize,
    opts: ProcessOptions,
    transform: &Transform<'_>,
    count: &Counter<'_>,
) -> RungSet {
    let stop = AtomicBool::new(false);
    let sum = AtomicUsize::new(0);
    let workers: Vec<_> = files
        .par_iter()
        .map(|file| {
            let language = opts
                .explicit_lang
                .or_else(|| detect_language_from_path(file.path));
            let mut rung_opts = opts;
            rung_opts.mode =
                if language.is_some_and(|l| l.is_serde_based()) && serde_rungs(mode).len() == 1 {
                    Mode::Structure
                } else {
                    mode
                };
            rung_opts.trunc.token_budget = None;
            rung_opts.show_stats = false;
            let rung = language.and_then(|_| transform(file.path, rung_opts).ok());
            let emptied = rung.as_ref().is_some_and(|pr| {
                !file.pass_1.output.trim().is_empty() && pr.output.trim().is_empty()
            });
            let kept_pass_1 = rung.is_none() || emptied;
            let fixed = language.is_none()
                || rung.is_none()
                || opts.trunc.max_lines.is_some()
                || opts.trunc.last_lines.is_some();
            let (output, differs) = match rung {
                Some(pr) if !emptied => (pr.output, pr.view_differs),
                _ => (file.pass_1.output.clone(), file.pass_1.view_differs),
            };
            let is_empty = file.pass_1.output.trim().is_empty() || emptied;
            // Every worker transforms even after the stop, so emptiness is complete
            // and independent of the pool size. Dropped bodies cannot pass fitting.
            if matches!(kind, RungKind::Probe) && stop.load(Ordering::Relaxed) {
                return (None, differs, kept_pass_1, is_empty);
            }
            let mut piece = make_piece(file.prefix.clone(), output, language, fixed, count);
            if matches!(kind, RungKind::Probe) {
                note_probe_count(piece.count, n, &sum, &stop);
                if stop.load(Ordering::Relaxed) {
                    return (None, differs, kept_pass_1, is_empty);
                }
            } else {
                piece = precut_piece(piece, n, count);
            }
            (Some(piece), differs, kept_pass_1, is_empty)
        })
        .collect();
    let is_empty = workers.iter().all(|(_, _, _, empty)| *empty);
    let mut set = RungSet {
        pieces: Vec::with_capacity(files.len()),
        differing: Vec::with_capacity(files.len()),
        kept_pass_1: Vec::with_capacity(files.len()),
        complete: true,
        is_empty,
    };
    for (piece, differs, kept, _) in workers {
        if let Some(piece) = piece {
            set.pieces.push(piece);
        } else {
            set.complete = false;
        }
        set.differing.push(differs);
        set.kept_pass_1.push(kept);
    }
    set
}

fn select_run(
    files: &[RunFile<'_>],
    pass_1: RungSet,
    n: usize,
    opts: ProcessOptions,
    transform: &Transform<'_>,
    count: &Counter<'_>,
) -> anyhow::Result<RunSelection> {
    if let Some(output) = pass_1.fitting(n, count) {
        return Ok(pass_1.accept(output, opts.mode, opts.mode));
    }
    drop(pass_1);
    let mut accepted = None;
    let walked = walk_rungs(
        &opts.mode.cascade_from_here()[1..],
        &opts.trunc,
        |cfg| {
            Ok(Some(run_rung(
                files,
                cfg.mode,
                RungKind::Probe,
                n,
                opts,
                transform,
                count,
            )))
        },
        |set| set.is_empty,
        |_, set| {
            accepted = set.fitting(n, count);
            accepted.is_some()
        },
    )?;
    let (mode, base) = match walked {
        RungWalk::Fits { mode, rung } => {
            // The fit callback stores the exact counted string.
            let output =
                accepted.ok_or_else(|| anyhow::anyhow!("Run fit has no accepted output"))?;
            return Ok(rung.accept(output, mode, opts.mode));
        }
        RungWalk::NoneFits { mode, rung } if rung.complete => (mode, rung),
        RungWalk::NoneFits { mode, .. } => (
            mode,
            run_rung(files, mode, RungKind::CutBase, n, opts, transform, count),
        ),
        RungWalk::AllEmpty | RungWalk::NoOutput => (
            opts.mode,
            run_rung(
                files,
                opts.mode,
                RungKind::CutBase,
                n,
                opts,
                transform,
                count,
            ),
        ),
    };
    let plan = plan_cut(&base.pieces, n, count);
    Ok(RunSelection {
        output: plan.output,
        bodies: plan.bodies,
        omitted: plan.omitted,
        mode,
        requested: opts.mode,
        differing: base.differing,
        kept_pass_1: base.kept_pass_1,
    })
}

type FileResults<'a> = Vec<(&'a PathBuf, anyhow::Result<ProcessResult>)>;

fn final_results<'a>(
    results: FileResults<'a>,
    selected: &mut RunSelection,
    opts: ProcessOptions,
    count: &Counter<'_>,
) -> FileResults<'a> {
    let changed = selected.changed();
    let mut shown = selected.bodies.iter_mut().enumerate();
    let results: FileResults<'_> = results
        .into_iter()
        .filter_map(|(path, result)| {
            let mut pr = match result {
                Ok(pr) => pr,
                Err(e) => return Some((path, Err(e))),
            };
            let (idx, body) = shown.next()?;
            pr.output = std::mem::take(&mut body.output);
            pr.view_differs = selected.differing[idx] || body.cut;
            if changed {
                // Cached counts describe a plain rung, not the shown body.
                pr.transformed_tokens = None;
                pr.served = None;
            }
            Some((path, Ok(pr)))
        })
        .collect();
    if changed && opts.show_stats {
        results
            .into_par_iter()
            .map(|(path, result)| {
                (
                    path,
                    result.map(|mut pr| {
                        pr.transformed_tokens = count(&pr.output);
                        pr
                    }),
                )
            })
            .collect()
    } else {
        results
    }
}

struct RunReport {
    selection: RunSelection,
    budget: usize,
    total: usize,
    last_shown: Option<PathBuf>,
    first_omitted: Option<PathBuf>,
}

/// All token work for pass 1, rungs, cuts and final stats stays in this pool.
/// Analytics without stats still tokenizes after the stdout flush, in its
/// registered background thread.
fn prepare_results<'a>(
    paths: &'a [PathBuf],
    opts: ProcessOptions,
    show_headers: bool,
    count: &Counter<'_>,
) -> anyhow::Result<(FileResults<'a>, Option<RunReport>)> {
    let budget = opts
        .trunc
        .token_budget
        .filter(|_| run_budget_applies(&opts));
    let mut processed: Vec<_> = paths
        .par_iter()
        .enumerate()
        .map(|(idx, path)| {
            let result = process_file(path, opts);
            let piece = budget.and_then(|_| {
                result.as_ref().ok().map(|pr| {
                    make_piece(
                        piece_prefix(idx, path, show_headers),
                        pr.output.clone(),
                        None,
                        true,
                        count,
                    )
                })
            });
            (path, result, piece)
        })
        .collect();
    let Some(n) = budget else {
        return Ok((
            processed
                .into_iter()
                .map(|(path, result, _)| (path, result))
                .collect(),
            None,
        ));
    };
    let pieces = processed
        .iter_mut()
        .filter_map(|(_, _, piece)| piece.take())
        .collect();
    let files: Vec<_> = processed
        .iter()
        .enumerate()
        .filter_map(|(idx, (path, result, _))| {
            result.as_ref().ok().map(|pr| RunFile {
                path,
                prefix: piece_prefix(idx, path, show_headers),
                pass_1: pr,
            })
        })
        .collect();
    let pass_1 = RungSet {
        pieces,
        differing: files.iter().map(|f| f.pass_1.view_differs).collect(),
        kept_pass_1: vec![false; files.len()],
        complete: true,
        is_empty: false,
    };
    let mut selection = select_run(&files, pass_1, n, opts, &process_file, count)?;
    let total = files.len();
    let shown = selection.bodies.len();
    let last_shown = shown
        .checked_sub(1)
        .and_then(|i| files.get(i))
        .map(|f| f.path.to_path_buf());
    let first_omitted = files.get(shown).map(|f| f.path.to_path_buf());
    let results = final_results(
        processed
            .into_iter()
            .map(|(path, result, _)| (path, result))
            .collect(),
        &mut selection,
        opts,
        count,
    );
    Ok((
        results,
        Some(RunReport {
            selection,
            budget: n,
            total,
            last_shown,
            first_omitted,
        }),
    ))
}

/// Process multiple files with parallel processing via rayon.
///
/// Used by glob, directory, and explicit multi-file inputs. Handles parallel
/// execution, error aggregation, and accumulated token statistics.
///
/// Precondition: `paths` must be non-empty. Callers should validate and
/// produce a descriptive error (with `--no-ignore` hint) before calling.
#[allow(clippy::disallowed_methods)] // Multi-file streaming output; may span thousands of files, streaming handle required
fn process_files(paths: Vec<PathBuf>, options: MultiFileOptions) -> anyhow::Result<()> {
    debug_assert!(
        !paths.is_empty(),
        "BUG: process_files called with empty paths"
    );
    let total_paths = paths.len();
    let process_options = options.process;
    let show_headers = !options.no_header && paths.len() > 1;
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(options.jobs.unwrap_or(0))
        .build()?;
    let count = |s: &str| crate::tokens::count_tokens(s).ok();
    let (results, run) =
        pool.install(|| prepare_results(&paths, process_options, show_headers, &count))?;

    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());

    let mut success_count = 0;
    let mut error_count = 0;
    let mut guardrail_count = 0usize;
    let mut view_differs_count = 0usize;
    let mut total_original_tokens = 0usize;
    let mut total_transformed_tokens = 0usize;

    if let Some(run) = &run {
        write_run(&mut writer, &run.selection.output)?;
    }
    for (idx, (path, result)) in results.iter().enumerate() {
        match result {
            Ok(process_result) => {
                if run.is_none() {
                    write!(
                        writer,
                        "{}{}",
                        piece_prefix(idx, path, show_headers),
                        process_result.output
                    )?;
                }
                success_count += 1;

                if process_result.guardrail_triggered {
                    guardrail_count += 1;
                }

                if process_result.view_differs {
                    view_differs_count += 1;
                }

                if let (Some(orig), Some(trans)) = (
                    process_result.original_tokens,
                    process_result.transformed_tokens,
                ) {
                    total_original_tokens += orig;
                    total_transformed_tokens += trans;
                }
            }
            Err(e) => {
                eprintln!("Error processing {}: {}", path.display(), e);
                error_count += 1;
            }
        }
    }

    writer.flush()?;

    let processed_successes = run.as_ref().map_or(success_count, |r| r.total);
    if processed_successes == 0 {
        anyhow::bail!("All {} file(s) failed to process", error_count);
    }

    if error_count > 0 {
        eprintln!(
            "\nProcessed {} file(s) successfully, {} failed",
            processed_successes, error_count
        );
    }

    if guardrail_count > 0 {
        let total = success_count + error_count;
        crate::debug_log!(
            "[skim:guardrail] triggered on {}/{} files",
            guardrail_count,
            total
        );
    }

    // One unconditional notice discloses a changed view. The run notice
    // replaces the aggregate notice. Neither is charged to a batch row.
    let mode_str = options.process.mode.name().to_string();
    if let Some(run) = run.as_ref().filter(|r| r.selection.changed()) {
        let selection = &run.selection;
        let shown = selection.bodies.len();
        let facts = crate::output::RunNoticeFacts {
            budget: run.budget,
            requested: process_options.mode,
            served: selection.mode,
            differing: view_differs_count,
            total: run.total,
            cut: selection
                .bodies
                .iter()
                .zip(&selection.kept_pass_1)
                .filter(|(body, kept)| body.cut && !**kept)
                .count(),
            omitted: selection.omitted,
            kept_pass_1: selection.kept_pass_1[..shown]
                .iter()
                .filter(|&&kept| kept)
                .count(),
            last_shown: run.last_shown.as_deref(),
            first_omitted: run.first_omitted.as_deref(),
        };
        let notice = crate::output::run_view_marker(&facts);
        let _ = write!(io::stderr().lock(), "{}", notice.line());
    } else if view_differs_count > 0
        && let Some(notice) = crate::output::emitted_notice_cost(
            crate::output::rewrite_origin().as_deref(),
            &mode_str,
            view_differs_count,
            total_paths,
        )
    {
        eprint!("{}", notice.line());
    }

    if options.process.show_stats && total_original_tokens > 0 {
        let suffix = format!(" across {} file(s)", success_count);
        report_token_stats(
            Some(total_original_tokens),
            Some(total_transformed_tokens),
            &suffix,
        );
    }

    // Record per-file analytics rows (Phase A2 — fixes PF-001 for multi/glob/dir).
    //
    // Capture project_path and common fields on the MAIN thread before any
    // background spawn. Consume `results` by value so output Strings MOVE into
    // the rows without cloning.
    //
    // Behaviour change (intentional): N per-file rows are emitted for BOTH plain
    // and --show-stats, replacing the single aggregate row.  Err entries (files
    // that failed to process) are skipped — they already produced stderr output.
    if options.analytics_enabled {
        let cwd = std::env::current_dir()
            .unwrap_or_default()
            .display()
            .to_string();

        // EVERY ROW OF A BATCH IS UNMEASURED, and that is the honest answer
        // rather than a missing one.
        //
        // `aggregate_notice` above is ONE line of stderr for the whole RUN.
        // `notice_tokens` and `notice_bytes` are PER-FILE columns and
        // `token_savings` has no invocation dimension to hang a run-scoped
        // measurement on, so there is no per-file number here that was ever
        // emitted. Every encoding that produces one is a fabrication:
        //
        // - charge every differing row: the run is billed N times for one
        //   emission;
        // - split it N ways: per-file costs nobody emitted, which do not even
        //   sum back cleanly under integer division;
        // - charge the FIRST differing row via `Option::take` — what this did.
        //   The run TOTAL was right, and the per-file attribution was not: after
        //   a 40-file run one arbitrary file carried the entire disclosure cost
        //   and 39 carried a measured ZERO, with nothing in any row marking the
        //   regime (architecture-04).
        //
        // So the rows record `notice_measured: false`, which
        // `analytics::file_op_notice_cost` maps to SQL NULL in BOTH columns —
        // "not measured in this regime". BOTH matters:
        // `AnalyticsDb::DELIVERED_ROW` selects on `notice_tokens IS NOT NULL`
        // and `delivered_unmeasured_rows` counts `notice_bytes IS NOT NULL AND
        // notice_tokens IS NULL`, so NULLing only the tokens would move the
        // whole batch into a counter that exists to expose untokenisable
        // DISCLOSURES. Two NULLs leave the batch out of both, which is where a
        // run-scoped measurement belongs until the schema can carry one.
        //
        // What this gives up, stated plainly: the batch's real disclosure cost
        // is now recorded NOWHERE, where before it was recorded once and
        // mis-attributed. It is not silently zeroed — `notice: None` on every
        // row would have done that, and a measured zero DELETES the cost from
        // the delivered series and biases the headline UP (PF-036's favourable
        // direction), which is worse than the mis-attribution it would fix. The
        // rows still feed the continuity series through `raw_tokens` /
        // `compressed_tokens`, and still record `served`.
        let rows: Vec<crate::analytics::FileOpRow> = results
            .into_iter()
            .filter_map(|(path, result)| {
                let pr = result.ok()?; // skip Err entries
                let served = pr.served;
                let counts = match (pr.original_tokens, pr.transformed_tokens) {
                    (Some(raw), Some(comp)) => {
                        // --show-stats (or count-carrying cache hit): counts already known.
                        crate::analytics::FileCounts::Known {
                            raw,
                            compressed: comp,
                        }
                    }
                    _ => {
                        // Plain run / cold cache: tokenize off the main thread.
                        // F14: if the file is deleted/changed between here and the
                        // background re-read, read_source returns Err → row silently
                        // skipped; sibling rows still recorded.
                        crate::analytics::FileCounts::Tokenize {
                            raw: crate::analytics::RawSource::Reread(path.clone()),
                            compressed: pr.output,
                        }
                    }
                };
                Some(crate::analytics::FileOpRow {
                    counts,
                    original_cmd: format!("skim {}", path.display()),
                    language: pr.language.map(|l| l.as_str().to_string()),
                    parse_tier: pr.parse_tier.map(str::to_string),
                    // No per-file disclosure exists to carry, and the row says
                    // so rather than claiming a zero — see the block above.
                    notice: None,
                    notice_measured: false,
                    served,
                })
            })
            .collect();

        let common = crate::analytics::FileOpCommon {
            mode: Some(mode_str),
            project_path: cwd,
            session_id: options.session_id.clone(),
        };

        crate::analytics::record_file_ops(options.analytics_enabled, rows, common);
    }

    Ok(())
}

/// Process a list of explicitly specified file arguments.
///
/// Each argument may be:
/// - A glob pattern (contains `*`, `?`, `[`, or `{`) — expanded via [`process_glob`] logic
/// - A directory path — files are collected recursively via [`collect_files_from_directory`]
/// - A plain file path — added directly
///
/// All resolved paths are collected into a single `Vec<PathBuf>` and processed
/// together. This enables `skim file1.ts file2.ts` and mixed forms like
/// `skim 'src/**/*.ts' extra.py`.
pub(crate) fn process_explicit_files(
    args: &[String],
    options: MultiFileOptions,
) -> anyhow::Result<()> {
    process_files(resolve_explicit_paths(args, options.no_ignore)?, options)
}

/// Serve the explicit list as raw bytes, in the same path order as the transform.
/// Directory and glob arguments use the same collectors and ignore rules.
/// Write errors keep their source so a closed stdout pipe exits 141.
#[allow(clippy::disallowed_methods)] // File-list output needs one stdout lock for the raw byte stream
pub(crate) fn passthrough_explicit_files(args: &[String], no_ignore: bool) -> anyhow::Result<()> {
    let paths = resolve_explicit_paths(args, no_ignore)?;
    let stdout = io::stdout();
    let mut out = stdout.lock();
    for path in paths {
        let contents = std::fs::read(&path)
            .map_err(|e| anyhow::anyhow!("passthrough read {}: {e}", path.display()))?;
        out.write_all(&contents)?;
    }
    Ok(())
}

/// Resolve a mixed explicit list once for both transformed and raw output.
/// Keep partial-failure warnings, the all-fail error, path order and deduplication.
fn resolve_explicit_paths(args: &[String], no_ignore: bool) -> anyhow::Result<Vec<PathBuf>> {
    debug_assert!(
        !args.is_empty(),
        "BUG: resolve_explicit_paths called with empty args"
    );
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for arg in args {
        if has_glob_pattern(arg) {
            // Glob expansion — validate then walk
            match expand_glob_to_paths(arg, no_ignore) {
                Ok(mut matched) => paths.append(&mut matched),
                Err(e) => errors.push(format!("{arg}: {e}")),
            }
        } else {
            let path = PathBuf::from(arg);
            if path.is_dir() {
                let mut dir_files = collect_files_from_directory(&path, no_ignore);
                if dir_files.is_empty() {
                    errors.push(format!(
                        "No files found in directory '{}'{}",
                        path.display(),
                        no_ignore_hint(no_ignore)
                    ));
                } else {
                    paths.append(&mut dir_files);
                }
            } else if path.exists() {
                paths.push(path);
            } else {
                errors.push(format!("File not found: '{}'", path.display()));
            }
        }
    }

    if !errors.is_empty() && paths.is_empty() {
        // All arguments failed — report the first error as the primary message,
        // then list the rest.
        let mut msg = errors.remove(0);
        for e in errors {
            msg.push('\n');
            msg.push_str(&e);
        }
        anyhow::bail!("{}", msg);
    }

    // Partial failures: warn on stderr but continue with resolved paths.
    for e in &errors {
        eprintln!("Warning: {e}");
    }

    if paths.is_empty() {
        anyhow::bail!(
            "No files found for the given arguments{}",
            no_ignore_hint(no_ignore)
        );
    }

    // Deduplicate: a plain file arg and a glob arg may both resolve to the
    // same path (e.g. `skim explicit.ts '*.ts'`). Sort first so dedup is O(n).
    paths.sort();
    paths.dedup();

    Ok(paths)
}

/// Expand a glob pattern to a list of matching paths.
///
/// Separated from [`process_glob`] so it can be used within
/// [`process_explicit_files`] without going through the full single-glob
/// pipeline (which calls `process_files` directly).
fn expand_glob_to_paths(pattern: &str, no_ignore: bool) -> anyhow::Result<Vec<PathBuf>> {
    validate_glob_pattern(pattern)?;

    let (walk_root, glob_pattern) = glob_walk_root(pattern);

    let glob = GlobBuilder::new(glob_pattern)
        .literal_separator(false)
        .build()
        .map_err(|e| anyhow::anyhow!("Invalid glob pattern '{}': {}", pattern, e))?;
    let matcher = glob.compile_matcher();

    let mut builder = WalkBuilder::new(walk_root);
    configure_walker(&mut builder, no_ignore);

    let paths: Vec<PathBuf> = builder
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_some_and(|ft| ft.is_file()))
        .filter(|entry| {
            entry
                .path()
                .strip_prefix(walk_root)
                .ok()
                .is_some_and(|rel| matcher.is_match(rel))
        })
        .map(|entry| entry.into_path())
        .collect();

    if paths.is_empty() {
        anyhow::bail!(
            "No files found: pattern '{}'{}",
            pattern,
            no_ignore_hint(no_ignore)
        );
    }

    Ok(paths)
}

/// Process multiple files matched by glob pattern.
///
/// Uses `ignore::WalkBuilder` for directory walking (respects `.gitignore`
/// and hidden file rules by default), then filters entries with a
/// `globset::GlobMatcher` to match the user's glob pattern. This ensures
/// gitignore rules are applied *before* glob matching, so gitignored files
/// are excluded even when the glob would otherwise match them.
pub(crate) fn process_glob(pattern: &str, options: MultiFileOptions) -> anyhow::Result<()> {
    let paths = expand_glob_to_paths(pattern, options.no_ignore)?;
    process_files(paths, options)
}

/// Collect all supported files from a directory recursively.
///
/// Uses `ignore::WalkBuilder` to walk the directory tree, respecting
/// `.gitignore` and hidden file rules. Filters for supported extensions
/// using `Language::from_path()`.
///
/// Walk errors (e.g. permission-denied on individual entries) are
/// intentionally dropped via `filter_map(|e| e.ok())`. A single
/// unreadable file should not abort traversal of an entire directory
/// tree -- this matches ripgrep/fd behavior.
fn collect_files_from_directory(dir: &Path, no_ignore: bool) -> Vec<PathBuf> {
    let mut builder = WalkBuilder::new(dir);
    configure_walker(&mut builder, no_ignore);

    builder
        .build()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_some_and(|ft| ft.is_file()))
        .filter(|entry| Language::from_path(entry.path()).is_some())
        .map(|entry| entry.into_path())
        .collect()
}

/// Process all supported files in a directory recursively
pub(crate) fn process_directory(dir: &Path, options: MultiFileOptions) -> anyhow::Result<()> {
    let paths = collect_files_from_directory(dir, options.no_ignore);

    if paths.is_empty() {
        anyhow::bail!(
            "No files found: directory '{}'{}",
            dir.display(),
            no_ignore_hint(options.no_ignore)
        );
    }

    process_files(paths, options)
}

/// Collect skim-supported file paths from a directory for passthrough mode.
///
/// Mirrors `collect_files_from_directory` but is `pub(crate)` so the passthrough
/// gate in `process_single_arg` can enumerate files without running the transform
/// pipeline.  Returns an empty `Vec` when the directory is empty or has no
/// skim-supported files (the caller is responsible for handling that case).
pub(crate) fn collect_passthrough_paths_dir(dir: &Path, no_ignore: bool) -> Vec<PathBuf> {
    collect_files_from_directory(dir, no_ignore)
}

/// Expand a glob pattern to matching file paths for passthrough mode.
///
/// Mirrors `expand_glob_to_paths` but is `pub(crate)` so the passthrough gate
/// in `process_single_arg` can enumerate files without running the transform
/// pipeline.
pub(crate) fn collect_passthrough_paths_glob(
    pattern: &str,
    no_ignore: bool,
) -> anyhow::Result<Vec<PathBuf>> {
    expand_glob_to_paths(pattern, no_ignore)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_options() -> ProcessOptions {
        ProcessOptions {
            mode: Mode::Structure,
            explicit_lang: None,
            use_cache: false,
            show_stats: true,
            trunc: crate::cascade::TruncationOptions {
                token_budget: Some(100),
                ..Default::default()
            },
            line_numbers: false,
            batch: true,
        }
    }

    fn result(text: &str) -> ProcessResult {
        ProcessResult {
            output: text.to_string(),
            original_tokens: Some(1000),
            transformed_tokens: Some(999),
            guardrail_triggered: false,
            served: None,
            parse_tier: Some("full"),
            language: Some(Language::Rust),
            stdin_raw: None,
            view_differs: true,
            bound: None,
        }
    }

    fn run_file<'a>(path: &'a str, pr: &'a ProcessResult) -> RunFile<'a> {
        RunFile {
            path: Path::new(path),
            prefix: String::new(),
            pass_1: pr,
        }
    }

    #[test]
    fn piece_prefix_matches_stream_bytes_after_a_failed_first_file() {
        let mut stream = Vec::new();
        for (idx, path, body) in [(1, "b.rs", "body"), (2, "c.rs", "next\n")] {
            write!(
                stream,
                "{}{}",
                piece_prefix(idx, Path::new(path), true),
                body
            )
            .unwrap();
        }
        assert_eq!(stream, b"\n// b.rs\nbody\n// c.rs\nnext\n");
        assert_eq!(piece_prefix(0, Path::new("a.rs"), true), "// a.rs\n");
        assert_eq!(piece_prefix(9, Path::new("a.rs"), false), "");
    }

    #[test]
    fn write_run_keeps_the_exact_accepted_bytes() {
        let candidate = "\n// b.rs\nfn f() {}";
        let mut bytes = Vec::new();
        write_run(&mut bytes, candidate).unwrap();
        assert_eq!(bytes, candidate.as_bytes());
    }

    #[test]
    fn rung_options_keep_annotations_and_use_one_serde_structure_entry() {
        let pr = result("pass 1\n");
        let files = [run_file("a.json", &pr)];
        let mut opts = run_options();
        opts.line_numbers = true;
        let set = run_rung(
            &files,
            Mode::Types,
            RungKind::Probe,
            100,
            opts,
            &|_, got| {
                assert_eq!(got.mode, Mode::Structure);
                assert!(got.line_numbers && got.batch);
                assert!(!got.show_stats);
                assert_eq!(got.trunc.token_budget, None);
                Ok(result("rung\n"))
            },
            &|s| Some(s.len()),
        );
        assert_eq!(set.pieces.len(), 1);
        assert_eq!(
            crate::run_budget::set_fits(&set.pieces, 100, &|s| Some(s.len())).unwrap(),
            "rung\n"
        );
    }

    #[test]
    fn an_all_empty_rung_leaves_the_last_nonempty_cut_base() {
        let pr = result("structure\n".repeat(30).as_str());
        let files = [run_file("a.rs", &pr)];
        let selected = select_run(
            &files,
            pass_one_set(&files, &|s| Some(s.len())),
            100,
            run_options(),
            &|_, opts| {
                Ok(result(&if opts.mode == Mode::Types {
                    " \n".to_string()
                } else {
                    "signature\n".repeat(20)
                }))
            },
            &|s| Some(s.len()),
        )
        .unwrap();
        assert_eq!(selected.mode, Mode::Signatures);
        assert!(selected.bodies[0].cut);
        assert!(selected.output.contains("truncated"));
        assert!(!selected.output.contains("structure"));
    }

    #[test]
    fn a_partly_empty_rung_keeps_pass_one_and_can_fit() {
        let function = result("function structure\n");
        let constant = result("const C: u8 = 1;\n");
        let files = [run_file("a.rs", &function), run_file("b.rs", &constant)];
        let selected = select_run(
            &files,
            pass_one_set(&files, &|s| Some(s.len())),
            30,
            run_options(),
            &|path, opts| {
                Ok(result(
                    if opts.mode == Mode::Types || path == Path::new("b.rs") {
                        "\n"
                    } else {
                        "fn f();\n"
                    },
                ))
            },
            &|s| Some(s.len()),
        )
        .unwrap();
        assert_eq!(selected.mode, Mode::Signatures);
        assert_eq!(selected.output, "fn f();\nconst C: u8 = 1;\n");
        assert_eq!(selected.kept_pass_1, [false, true]);
        assert!(!selected.bodies.iter().any(|body| body.cut));
    }

    #[test]
    fn rung_errors_and_unknown_paths_keep_fixed_pass_one_bodies() {
        let pr = result("pass 1\n");
        let files = [run_file("a.rs", &pr), run_file("script", &pr)];
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let set = run_rung(
            &files,
            Mode::Types,
            RungKind::CutBase,
            100,
            run_options(),
            &|_, _| {
                calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                anyhow::bail!("rung error")
            },
            &|s| Some(s.len()),
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(
            set.pieces
                .iter()
                .all(|p| matches!(&p.body, Body::Fixed(s) if s == "pass 1\n"))
        );
        assert_eq!(set.kept_pass_1, [true, true]);
        assert!(!set.is_empty);
    }

    #[test]
    fn line_bound_rung_bodies_stay_fixed() {
        let pr = result("pass 1\n");
        let files = [run_file("a.rs", &pr)];
        for last in [false, true] {
            let mut opts = run_options();
            if last {
                opts.trunc.last_lines = Some(3);
            } else {
                opts.trunc.max_lines = Some(3);
            }
            let set = run_rung(
                &files,
                Mode::Types,
                RungKind::CutBase,
                10,
                opts,
                &|_, _| Ok(result("bounded body\n")),
                &|s| Some(s.len()),
            );
            assert!(matches!(&set.pieces[0].body, Body::Fixed(s) if s == "bounded body\n"));
        }
    }

    #[test]
    fn probe_stop_still_transforms_every_file_and_cannot_accept_a_failed_set() {
        let pr = result("pass 1\n");
        let files: Vec<_> = (0..8).map(|_| run_file("a.rs", &pr)).collect();
        for jobs in [1, 4] {
            let calls = std::sync::atomic::AtomicUsize::new(0);
            let set = rayon::ThreadPoolBuilder::new()
                .num_threads(jobs)
                .build()
                .unwrap()
                .install(|| {
                    run_rung(
                        &files,
                        Mode::Types,
                        RungKind::Probe,
                        100,
                        run_options(),
                        &|_, _| {
                            calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            Ok(result(&"a".repeat(70)))
                        },
                        &|s| Some(s.len()),
                    )
                });
            assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 8);
            assert!(!set.complete);
            assert!(!set.is_empty);
            assert!(set.fitting(100, &|s| Some(s.len())).is_none());
        }
    }

    #[test]
    fn probe_sum_caps_each_addition_and_never_wraps_large_counts() {
        let sum = std::sync::atomic::AtomicUsize::new(0);
        let stop = std::sync::atomic::AtomicBool::new(false);
        note_probe_count(usize::MAX, 100, &sum, &stop);
        assert_eq!(sum.load(std::sync::atomic::Ordering::Relaxed), 101);
        assert!(stop.load(std::sync::atomic::Ordering::Relaxed));
    }

    #[test]
    fn cut_base_precuts_count_view_lines_and_keep_one_marker() {
        let pr = result("pass 1\n");
        let files = [run_file("a.rs", &pr)];
        let set = run_rung(
            &files,
            Mode::Types,
            RungKind::CutBase,
            100,
            run_options(),
            &|_, _| Ok(result(&"type A = u64;\n".repeat(100))),
            &|s| Some(s.len()),
        );
        let Body::PreCut {
            kept,
            marker,
            total_view_lines,
            marked_count,
        } = &set.pieces[0].body
        else {
            panic!("expected worker pre-cut")
        };
        assert_eq!(*total_view_lines, 100);
        assert_eq!(marker.lines().count(), 1);
        assert!(marker.contains(&format!("{} lines", 100 - kept.lines().count())));
        assert_eq!(*marked_count, kept.len() + marker.len());
        assert!(*marked_count <= 100);
    }

    #[test]
    fn failed_counts_never_accept_content() {
        let pr = result("pass 1\n");
        let files = [run_file("a.rs", &pr)];
        let selected = select_run(
            &files,
            pass_one_set(&files, &|_| None),
            1,
            run_options(),
            &|_, _| Ok(result("rung\n")),
            &|_| None,
        )
        .unwrap();
        assert!(selected.bodies.is_empty());
        assert_eq!(selected.omitted, 1);
        assert!(selected.output.contains("SKIM_PASSTHROUGH=1"));
    }

    #[test]
    fn fitting_pass_one_keeps_all_bytes_and_does_not_transform() {
        let pr = result("pass 1\n");
        let files = [run_file("a.rs", &pr)];
        let selected = select_run(
            &files,
            pass_one_set(&files, &|s| Some(s.len())),
            100,
            run_options(),
            &|_, _| panic!("fitting pass one must not transform"),
            &|s| Some(s.len()),
        )
        .unwrap();
        assert_eq!(selected.output, "pass 1\n");
        assert_eq!(selected.mode, Mode::Structure);
        assert_eq!(selected.omitted, 0);
        assert!(!selected.changed());
    }

    #[test]
    fn final_results_use_shown_bodies_for_stats_and_keep_errors() {
        let paths = [
            PathBuf::from("a.rs"),
            PathBuf::from("b.rs"),
            PathBuf::from("c.rs"),
        ];
        let input = vec![
            (&paths[0], Ok(result("stale"))),
            (&paths[1], Err(anyhow::anyhow!("invalid UTF-8"))),
            (&paths[2], Ok(result("omitted"))),
        ];
        let mut selected = RunSelection {
            output: "cut\n".to_string(),
            bodies: vec![PlannedBody {
                output: "cut\n".to_string(),
                cut: true,
            }],
            mode: Mode::Types,
            requested: Mode::Structure,
            differing: vec![false, false],
            kept_pass_1: vec![false, false],
            omitted: 1,
        };
        let shown = final_results(input, &mut selected, run_options(), &|s| Some(s.len()));
        assert_eq!(shown.len(), 2);
        let pr = shown[0].1.as_ref().unwrap();
        assert_eq!(pr.output, "cut\n");
        assert_eq!(pr.original_tokens, Some(1000));
        assert_eq!(pr.transformed_tokens, Some(4));
        assert!(pr.view_differs);
        assert_eq!(
            shown[1].1.as_ref().unwrap_err().to_string(),
            "invalid UTF-8"
        );
    }

    #[test]
    fn changed_bodies_clear_cached_analytics_counts_without_stats() {
        let path = PathBuf::from("a.rs");
        let mut selected = RunSelection {
            output: "new".to_string(),
            bodies: vec![PlannedBody {
                output: "new".to_string(),
                cut: false,
            }],
            mode: Mode::Signatures,
            requested: Mode::Structure,
            differing: vec![true],
            kept_pass_1: vec![false],
            omitted: 0,
        };
        let mut opts = run_options();
        opts.show_stats = false;
        let shown = final_results(
            vec![(&path, Ok(result("cached")))],
            &mut selected,
            opts,
            &|_| panic!("analytics counts stay off the main thread"),
        );
        assert_eq!(shown[0].1.as_ref().unwrap().transformed_tokens, None);
        assert_eq!(shown[0].1.as_ref().unwrap().output, "new");
    }

    #[test]
    fn test_has_glob_pattern() {
        assert!(has_glob_pattern("*.ts"));
        assert!(has_glob_pattern("src/**/*.js"));
        assert!(has_glob_pattern("file?.py"));
        assert!(has_glob_pattern("file[123].rs"));
        assert!(has_glob_pattern("*.{js,ts}"));
        assert!(has_glob_pattern("src/{a,b}.ts"));
        assert!(!has_glob_pattern("file.ts"));
        assert!(!has_glob_pattern("src/main.rs"));
    }

    #[test]
    fn test_validate_glob_pattern_accepts_absolute_unix_paths() {
        // Absolute paths are legitimate in AI agent workflows where the full
        // path is known. They must NOT be rejected.
        assert!(validate_glob_pattern("/etc/passwd").is_ok());
        assert!(validate_glob_pattern("/Users/foo/src/**/*.ts").is_ok());
        assert!(validate_glob_pattern("/src/**/*.ts").is_ok());
    }

    #[test]
    fn test_validate_glob_pattern_accepts_windows_drive_paths() {
        // Windows drive-letter paths are absolute local paths, not network
        // paths. Allow them so Windows users can pass absolute paths.
        assert!(validate_glob_pattern("C:\\Users\\*.ts").is_ok());
        assert!(validate_glob_pattern("D:/projects/**/*.rs").is_ok());
    }

    #[test]
    fn test_validate_glob_pattern_rejects_parent_traversal() {
        let result = validate_glob_pattern("../secret/*.ts");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("parent directory traversal"), "got: {msg}");
    }

    #[test]
    fn test_validate_glob_pattern_rejects_embedded_parent_traversal() {
        let result = validate_glob_pattern("src/../../etc/passwd");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("parent directory traversal"), "got: {msg}");
    }

    #[test]
    fn test_validate_glob_pattern_rejects_windows_unc_paths() {
        let result = validate_glob_pattern("\\\\server\\share\\*.ts");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("UNC network paths"), "got: {msg}");
    }

    #[test]
    fn test_validate_glob_pattern_accepts_valid_relative_patterns() {
        assert!(validate_glob_pattern("src/**/*.ts").is_ok());
        assert!(validate_glob_pattern("*.rs").is_ok());
        assert!(validate_glob_pattern("tests/fixtures/*.py").is_ok());
        assert!(validate_glob_pattern("**/*.{js,ts}").is_ok());
    }

    #[test]
    fn test_validate_glob_pattern_accepts_tilde_prefix() {
        // Tilde is not expanded by the ignore crate (treated as literal),
        // so it is safe to allow as a relative pattern component.
        assert!(validate_glob_pattern("~/*.ts").is_ok());
    }

    // ========================================================================
    // glob_walk_root unit tests
    // ========================================================================

    #[test]
    fn test_glob_walk_root_with_prefix() {
        assert_eq!(glob_walk_root("src/**/*.ts"), ("src", "**/*.ts"));
    }

    #[test]
    fn test_glob_walk_root_no_prefix() {
        assert_eq!(glob_walk_root("*.ts"), (".", "*.ts"));
    }

    #[test]
    fn test_glob_walk_root_multi_segment_prefix() {
        assert_eq!(
            glob_walk_root("src/utils/**/*.ts"),
            ("src/utils", "**/*.ts")
        );
    }

    #[test]
    fn test_glob_walk_root_doublestar_start() {
        assert_eq!(glob_walk_root("**/*.ts"), (".", "**/*.ts"));
    }

    #[test]
    fn test_glob_walk_root_single_dir_star() {
        assert_eq!(glob_walk_root("src/*.rs"), ("src", "*.rs"));
    }

    #[test]
    fn test_glob_walk_root_brace_expansion() {
        assert_eq!(glob_walk_root("src/**/*.{js,ts}"), ("src", "**/*.{js,ts}"));
    }

    #[test]
    fn test_glob_walk_root_question_mark() {
        assert_eq!(glob_walk_root("src/file?.ts"), ("src", "file?.ts"));
    }

    #[test]
    fn test_glob_walk_root_bracket() {
        assert_eq!(glob_walk_root("src/file[123].ts"), ("src", "file[123].ts"));
    }

    #[test]
    fn test_glob_walk_root_no_glob_chars_defensive() {
        // Defensive: if no glob chars exist, treat the entire pattern as root
        // with a match-everything glob. This should not happen in practice
        // (callers check has_glob_pattern first), but must not panic.
        assert_eq!(glob_walk_root("src/file.ts"), ("src/file.ts", "**"));
    }

    // ========================================================================
    // glob_walk_root: absolute path tests
    // ========================================================================

    #[test]
    fn test_glob_walk_root_absolute_path() {
        // /Users/foo/src/**/*.ts should split to root=/Users/foo/src, glob=**/*.ts
        assert_eq!(
            glob_walk_root("/Users/foo/src/**/*.ts"),
            ("/Users/foo/src", "**/*.ts")
        );
    }

    #[test]
    fn test_glob_walk_root_absolute_root_glob() {
        // "/**/*.ts" — glob char immediately after leading slash
        // segments = ["", "**", "*.ts"], static_count = 1 (just "")
        // root_end = 0 + 1 - 1 = 0, but pattern starts with '/', so root = "/"
        assert_eq!(glob_walk_root("/**/*.ts"), ("/", "**/*.ts"));
    }

    #[test]
    fn test_glob_walk_root_absolute_single_dir() {
        // "/src/*.rs" -> ("/src", "*.rs")
        assert_eq!(glob_walk_root("/src/*.rs"), ("/src", "*.rs"));
    }
}
