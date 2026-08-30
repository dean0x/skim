//! Diff rendering — AST-aware and raw hunk output.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::Path;

use rskim_core::Language;

use super::ast::{build_changed_lines, find_changed_node_ranges, is_container_node};
use super::source::get_file_source;
use super::types::{ChangedNodeRange, DiffHunk, FileDiff, ModeRenderContext};
use super::{DiffMode, MAX_AST_FILE_SIZE};
use crate::output::canonical::DiffFileStatus;

thread_local! {
    /// Per-thread parser cache — avoids creating a new tree-sitter Parser for every file.
    /// Each thread in the rayon pool gets its own `HashMap` of parsers keyed by language.
    static PARSERS: RefCell<HashMap<Language, rskim_core::Parser>> = RefCell::new(HashMap::new());
}

/// Per-file monotonic emitted-line cursor for changed-line de-duplication.
///
/// When two adjacent `ChangedNodeRange` items share one hunk (e.g. a
/// doc-comment edit immediately followed by a signature edit, both covered by
/// one `@@` block), `render_node_with_hunks` is called for each range and each
/// call re-walks the shared hunk via `emit_hunk_patch_lines_clipped`.  The
/// shared changed lines would be emitted twice — once per range call.
///
/// The cursor is created **per-file** as part of [`RenderState`] and threaded
/// as `&mut` into every emission site, so line numbers restart correctly when a
/// new file is rendered.  C1c widened its reach: it used to guard only
/// `emit_hunk_patch_lines_clipped`, leaving the container header, the closing
/// brace and full mode's unchanged-node bodies free to re-emit a line the hunk
/// walk had already written.
///
/// **Skip rule:**
/// - `+` / context line: skip when `patch_new_line <= cursor.last_new`.
/// - `-` line: skip when `patch_old_line <= cursor.last_old`
///   (`-` lines don't advance `new_line`, so they need the old-line axis).
/// - After emitting, update BOTH cursor fields.
///
/// **Scope:** never shared across files — `FileDiff` line numbers restart
/// from 1 per file, so a shared global cursor would incorrectly skip lines
/// in files 2, 3, … whose line numbers fall below the previous file's cursor.
#[derive(Debug, Default, Clone, Copy)]
struct EmittedCursor {
    /// Last new-file line number emitted (`+` or context ` ` line).
    /// Skip any `+`/context patch line whose `patch_new_line <= last_new`.
    last_new: usize,
    /// Last old-file line number emitted (`-` line).
    /// Skip any `-` patch line whose `patch_old_line <= last_old`.
    last_old: usize,
}

/// Line-emission axis for the post-render verifier (C1b / PF-025).
///
/// The verifier (`verify_ast_render`) tracks every rendered line as
/// `(Axis, file_line_number)` to enforce three invariants the ADR-001
/// net-savings size guard cannot catch — it fires only on OVER-emission,
/// never on silent UNDER-emission or content substitution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    /// New-file axis: `+` (added) and ` ` (context) lines, plus breadcrumbs.
    New,
    /// Old-file axis: `-` (removed) lines.
    Old,
}

/// The prefix character a rendered line carries.
///
/// C1d — marker fidelity.  The three original verifier checks ask only whether
/// a line NUMBER appears in the emission trace; none of them inspects the
/// prefix.  Every measured `added-as-context` case emitted the correct number
/// with an unconditional context prefix, so uniqueness, monotonicity and
/// coverage all passed while the render told the reader that brand-new code was
/// pre-existing.  Recording the marker alongside the number is what closes that
/// hole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    /// `+` — added on the new side.
    Added,
    /// `-` — removed from the old side.
    Removed,
    /// ` ` — unchanged: a context line, a breadcrumb, or an out-of-hunk source line.
    Context,
}

/// One rendered line, recorded for the post-render verifier (`verify_ast_render`).
type Emission = (Axis, usize, Marker);

/// The invariant a render violated, named so the debug-gated raw-fallback
/// banner says *which* check fired rather than only that one did.
///
/// A bare boolean made every rejection look alike, which is the wrong shape for
/// a guard whose whole job is to distinguish corruption classes — the C1d
/// marker-mismatch case in particular is indistinguishable from a coverage
/// failure without it.  Unit tests assert the specific variant, so a check that
/// silently starts catching a different class than it was written for fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerifyFailure {
    /// The same line number was emitted twice on one axis.
    DuplicateLine { axis: Axis, line: usize },
    /// New-side line numbers went backward.
    BackwardJump { previous: usize, line: usize },
    /// A `+` or `-` hunk line never reached the reader (#317, ADR-003).
    UncoveredChange { axis: Axis, line: usize },
    /// A line was emitted with a prefix the diff contradicts (C1d).
    MarkerMismatch {
        axis: Axis,
        line: usize,
        marker: Marker,
    },
}

impl std::fmt::Display for VerifyFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DuplicateLine { axis, line } => {
                write!(f, "line {line} emitted twice on the {axis:?} axis")
            }
            Self::BackwardJump { previous, line } => {
                write!(
                    f,
                    "new-side line numbers went backward ({previous} -> {line})"
                )
            }
            Self::UncoveredChange { axis, line } => {
                write!(
                    f,
                    "{axis:?}-axis changed line {line} never reached the reader"
                )
            }
            Self::MarkerMismatch { axis, line, marker } => {
                write!(
                    f,
                    "{axis:?}-axis line {line} rendered as {marker:?}, which the diff contradicts"
                )
            }
        }
    }
}

/// Per-file line classification derived from the diff hunks.
///
/// Two consumers, deliberately kept separate:
///
/// 1. the structure/full render, which must stamp a SOURCE line (a container
///    header, a closing brace, an unchanged-node body line) with the marker the
///    diff actually gives it rather than an unconditional context prefix;
/// 2. `verify_ast_render`, which rebuilds its own copy from the same hunks so
///    the check stays an independent backstop instead of a tautology over the
///    renderer's own input.
///
/// Rebuilding costs one extra O(patch lines) pass per file — cheap next to
/// parsing, and the price of keeping the guard one-directional (it can only
/// reject a render, never bless one).
struct HunkLineMarkers {
    /// New-side line numbers carrying a `+` prefix.
    added: HashSet<usize>,
    /// Old-side line numbers carrying a `-` prefix.
    removed: HashSet<usize>,
}

impl HunkLineMarkers {
    /// Build in one pass over every patch line, with both sets pre-sized from
    /// the total patch length (no growth reallocation on the hot path).
    fn from_hunks(hunks: &[DiffHunk<'_>]) -> Self {
        let patch_lines: usize = hunks.iter().map(|h| h.patch_lines.len()).sum();
        let mut added = HashSet::with_capacity(patch_lines / 2 + 1);
        let mut removed = HashSet::with_capacity(patch_lines / 4 + 1);
        for hunk in hunks {
            let mut cur_new = hunk.new_start;
            let mut cur_old = hunk.old_start;
            for patch_line in &hunk.patch_lines {
                match patch_line.as_bytes().first() {
                    Some(b'+') => {
                        added.insert(cur_new);
                        cur_new += 1;
                    }
                    Some(b'-') => {
                        removed.insert(cur_old);
                        cur_old += 1;
                    }
                    Some(b' ') => {
                        cur_new += 1;
                        cur_old += 1;
                    }
                    // `\ No newline at end of file` — no line number, no delta.
                    _ => {}
                }
            }
        }
        Self { added, removed }
    }

    /// Marker for a new-side SOURCE line: `+` when the diff added it, ` ` otherwise.
    ///
    /// Lines outside every hunk window are unchanged by definition and
    /// correctly classify as context.
    fn new_side(&self, line: usize) -> Marker {
        if self.added.contains(&line) {
            Marker::Added
        } else {
            Marker::Context
        }
    }
}

/// Immutable per-file inputs shared by every line-emission site.
///
/// Bundled so the emission helpers stay under the argument-count limit and so a
/// new input (the `markers` table added by C1d) reaches every site at once
/// instead of being threaded past some of them.
struct EmitInputs<'a> {
    hunks: &'a [DiffHunk<'a>],
    source_lines: &'a [&'a str],
    ln_width: usize,
    markers: &'a HunkLineMarkers,
}

impl<'a> EmitInputs<'a> {
    /// Derive the inputs for a file from its hunks and resolved source.
    fn new(
        hunks: &'a [DiffHunk<'a>],
        source_lines: &'a [&'a str],
        ln_width: usize,
        markers: &'a HunkLineMarkers,
    ) -> Self {
        Self {
            hunks,
            source_lines,
            ln_width,
            markers,
        }
    }
}

/// Mutable per-file render state for the structure/full path.
///
/// Bundles the two things every emission site must touch: the monotonic cursor
/// (so a line can be emitted neither twice nor out of order) and the emission
/// trace the verifier consumes.  Before C1c the container header and closing
/// brace bypassed both — they wrote a hard-coded context line and neither
/// consulted nor advanced the cursor, which is precisely the "breadcrumb never
/// advances the cursor" bug the Default path already fixed in `3fb0fd3`.
#[derive(Debug, Default)]
struct RenderState {
    cursor: EmittedCursor,
    emissions: Vec<Emission>,
}

/// Compute the minimum column width needed to display any line number in `hunks`.
///
/// Returns at least 1 so empty diffs still produce a consistent format.
///
/// Uses integer arithmetic (`checked_ilog10`) instead of heap-allocating a
/// `String` to count decimal digits (PF-014 perf fix).
fn line_number_width(hunks: &[DiffHunk<'_>]) -> usize {
    let max_line = hunks
        .iter()
        .map(|h| {
            let old_end = h.old_start + h.old_count;
            let new_end = h.new_start + h.new_count;
            old_end.max(new_end)
        })
        .max()
        .unwrap_or(0);
    // checked_ilog10 returns None for 0; fall back to 1 in that case.
    max_line
        .checked_ilog10()
        .map(|e| e as usize + 1)
        .unwrap_or(1)
}

/// Render a single file diff with AST-aware context.
///
/// For supported languages: shows changed AST nodes in hunk-scoped view
/// (breadcrumb + hunk lines), preserving `+`/`-` markers from the patch.
///
/// For unsupported languages or parse failures: falls back to raw hunks.
///
/// `diff_mode` controls how unchanged nodes are rendered:
/// - `Default`: Only changed nodes.
/// - `Structure`: Changed + unchanged nodes as signatures.
/// - `Full`: Changed + unchanged nodes in full.
pub(in crate::cmd::git) fn render_diff_file(
    file_diff: &FileDiff<'_>,
    global_flags: &[String],
    args: &[String],
    diff_mode: DiffMode,
    skip_ast: bool,
    is_show: bool,
) -> String {
    let mut output = String::new();

    // File header: renames show "old -> new (renamed)", others show "path (status)"
    if let (DiffFileStatus::Renamed, Some(old)) = (&file_diff.status, &file_diff.old_path) {
        let _ = writeln!(
            output,
            "{} \u{2192} {} ({})",
            old, file_diff.path, file_diff.status
        );
    } else {
        let _ = writeln!(output, "{} ({})", file_diff.path, file_diff.status);
    }

    // Binary files
    if file_diff.status == DiffFileStatus::Binary {
        let _ = writeln!(output, "Binary file differs");
        return output;
    }

    // No hunks means nothing to show
    if file_diff.hunks.is_empty() {
        return output;
    }

    // Compute line number column width from this file's hunks.
    let ln_width = line_number_width(&file_diff.hunks);

    // Added/deleted files: show all patch lines verbatim (no AST overlay needed)
    if file_diff.status == DiffFileStatus::Deleted || file_diff.status == DiffFileStatus::Added {
        return render_raw_hunks(file_diff, &output, ln_width);
    }

    // When AST is skipped (e.g., beyond MAX_AST_FILE_COUNT), render raw hunks.
    if skip_ast {
        return render_raw_hunks(file_diff, &output, ln_width);
    }

    // Determine language for parser lookup — serde-based formats (JSON, YAML,
    // TOML) have no tree-sitter grammar, so fall back to raw hunks.
    let Some(lang) =
        Language::from_path(Path::new(&file_diff.path)).filter(|l| !l.is_serde_based())
    else {
        return render_raw_hunks(file_diff, &output, ln_width);
    };

    // Obtain a cached parser from the thread-local pool and attempt AST rendering.
    let ast_result = PARSERS.with_borrow_mut(|cache| {
        if let std::collections::hash_map::Entry::Vacant(e) = cache.entry(lang)
            && let Ok(p) = rskim_core::Parser::new(lang)
        {
            e.insert(p);
        }
        let parser = cache.get_mut(&lang)?;
        try_ast_render(
            file_diff,
            global_flags,
            args,
            diff_mode,
            parser,
            ln_width,
            is_show,
        )
    });

    match ast_result {
        Some(ast_output) => {
            output.push_str(&ast_output);
            output
        }
        None => render_raw_hunks(file_diff, &output, ln_width),
    }
}

/// Check that every context and added patch line in `hunks` matches the
/// corresponding line in `source_lines`.
///
/// Returns `true` when all checks pass (source is consistent with the diff),
/// `false` when any check fails (source is from the wrong revision).
///
/// ## Line categories
/// - `+` (added) and ` ` (context) lines: content after the leading marker
///   must equal `source_lines[new_line - 1]`. `new_line` advances by 1.
/// - `-` (removed) lines: not present in the new-side file; no check, no
///   advance of `new_line`.
/// - `\` (no-newline marker, e.g. `\ No newline at end of file`): not a
///   real content line; no check, no advance.
///
/// ## Edge cases
/// - `new_start == 0`: git only emits this for a hunk with `new_count == 0`
///   (a pure deletion), which by definition contains no `+`/` ` lines — so the
///   indexing branch is never reached and the hunk passes vacuously.  Note that
///   `checked_sub(1)` returning `None` means **fail**, not pass: a `+`/` ` line
///   at `new_line == 0` would be an impossible state and is rejected.
/// - Past-end: `source_lines.get(i)` returns `None` → check fails → `false`.
///
/// The check is a one-directional backstop: it can only *reject* a source, never
/// bless a wrong one into rendering more than it already would.  A false negative
/// (rejecting a valid source, e.g. because a CRLF or trailing-whitespace
/// difference perturbs the comparison) costs only the AST breadcrumbs — the
/// caller falls back to raw hunks, which is always safe.
fn source_matches_diff(source_lines: &[&str], hunks: &[DiffHunk<'_>]) -> bool {
    for hunk in hunks {
        let mut new_line = hunk.new_start;
        for patch_line in &hunk.patch_lines {
            match patch_line.as_bytes().first() {
                Some(b'-') => {
                    // Removed line — not in new file; skip check, don't advance new_line.
                }
                Some(b'+') | Some(b' ') => {
                    let content = patch_line.get(1..).unwrap_or("");
                    let matches = new_line
                        .checked_sub(1)
                        .and_then(|i| source_lines.get(i))
                        .is_some_and(|s| *s == content);
                    if !matches {
                        return false;
                    }
                    new_line += 1;
                }
                _ => {
                    // `\ No newline at end of file` or empty — no delta, no check.
                }
            }
        }
    }
    true
}

/// Attempt AST-aware rendering for a modified/renamed file.
///
/// Returns `Some(rendered)` on success, `None` when the file cannot be
/// processed via tree-sitter (file too large, parse failure, or no
/// overlapping AST nodes).
///
/// Language validation and serde-based filtering happen in the caller
/// (`render_diff_file`), so `parser` is guaranteed to match the file's
/// language.
fn try_ast_render(
    file_diff: &FileDiff<'_>,
    global_flags: &[String],
    args: &[String],
    diff_mode: DiffMode,
    parser: &mut rskim_core::Parser,
    ln_width: usize,
    is_show: bool,
) -> Option<String> {
    let source = match get_file_source(&file_diff.path, global_flags, args, is_show) {
        Ok(s) => s,
        Err(e) => {
            // Source fetch failed; the reader still gets the full raw hunks via the
            // caller's fallback — nothing is lost.  ADR-011: no-loss raw-fallback →
            // debug-gated banner.
            crate::debug_log!("skim: AST fallback for {}: {e}", file_diff.path);
            return None;
        }
    };

    // Skip AST for files > 100KB
    if source.len() > MAX_AST_FILE_SIZE {
        return None;
    }

    let tree = parser.parse(&source).ok()?;

    let changed_ranges = find_changed_node_ranges(&tree, &file_diff.hunks);
    if changed_ranges.is_empty() {
        return None;
    }

    let source_lines: Vec<&str> = source.lines().collect();

    // Correctness backstop (ADR-011: no-loss raw fallback → debug-gated banner).
    //
    // Verify that the resolved source actually corresponds to the diff by
    // checking every context (' ') and added ('+') patch line against the
    // corresponding line in source_lines. A mismatch means the wrong revision
    // was loaded (e.g. working-tree file diverged from the committed blob) and
    // AST breadcrumbs would be fabricated from unrelated content.
    //
    // This check catches what the ADR-001 net-savings size guard cannot: a
    // corrupt render may be *smaller* than raw (fewer context lines) and
    // therefore passes the size guard even though it shows wrong content.
    if !source_matches_diff(&source_lines, &file_diff.hunks) {
        crate::debug_log!(
            "[skim] git diff AST: source revision mismatch for {}; falling back to raw hunks",
            file_diff.path
        );
        return None;
    }
    let mut output = String::new();

    // C1b/C1d: collect emissions for the post-render verifier so we can detect
    // content-substitution corruption that the ADR-001 net-savings guard cannot
    // catch (it fires only on OVER-emission, never on silent omission or on a
    // line rendered with the wrong marker).
    let mut state = RenderState {
        cursor: EmittedCursor::default(),
        emissions: Vec::with_capacity(file_diff.hunks.iter().map(|h| h.patch_lines.len()).sum()),
    };
    let markers = HunkLineMarkers::from_hunks(&file_diff.hunks);

    // B2: EmitInputs is built once and shared by both branches so
    // render_default_scoped can route breadcrumbs through emit_source_line.
    let inputs = EmitInputs::new(&file_diff.hunks, &source_lines, ln_width, &markers);

    if diff_mode != DiffMode::Default {
        let changed_lines = build_changed_lines(&file_diff.hunks);
        let ctx = ModeRenderContext {
            changed_ranges: &changed_ranges,
            changed_lines: &changed_lines,
            source: &source,
            diff_mode,
        };
        render_with_unchanged_context(&mut output, &tree, &ctx, &inputs, parser, &mut state);
    } else {
        // Default mode: hunk-scoped render — breadcrumb + hunk lines only.
        // This replaces the old render_changed_only → render_node_with_hunks path
        // that emitted the ENTIRE enclosing node body, producing 2-5x bloat vs raw
        // for small changes inside large functions (ADR-001).
        render_default_scoped(&mut output, &changed_ranges, &inputs, &mut state);
    }

    // C1e: the verifier now guards EVERY mode, not just Default.  `structure`
    // and `full` route through `render_with_unchanged_context`, which was never
    // covered and shipped duplicate lines and added-as-context renders.
    //
    // ADR-011 class-2: no-loss raw fallback — the reader still gets the full raw
    // hunks, so the banner is debug-gated (`crate::debug_log!` → zero stderr
    // bytes without `SKIM_DEBUG`).
    if let Err(failure) = verify_ast_render(&state.emissions, &file_diff.hunks) {
        crate::debug_log!(
            "[skim] git diff AST verifier: {failure} in {}; \
             falling back to raw hunks (ADR-011 class-2: no-loss)",
            file_diff.path
        );
        return None;
    }

    Some(output)
}

/// Render only changed nodes (full-node-body mode — used by tests only).
///
/// This function is kept for tests that validate the de-duplication cursor
/// (`EmittedCursor`) and per-container header/close-brace logic in isolation.
/// Production code uses `render_default_scoped` (hunk-scoped, ADR-001 safe).
///
/// For nested nodes (inside a class/struct), emits the parent declaration
/// header line before the changed child node.
#[cfg(test)]
fn render_changed_only(
    output: &mut String,
    changed_ranges: &[ChangedNodeRange],
    hunks: &[DiffHunk<'_>],
    source_lines: &[&str],
    ln_width: usize,
) {
    // Track which parent headers we have already emitted
    let mut emitted_parent_headers: HashSet<usize> = HashSet::new();

    // Pre-compute the last range index for each parent header to avoid O(N^2)
    // scanning on every iteration.
    let mut last_index_for_parent: HashMap<usize, usize> = HashMap::new();
    for (idx, range) in changed_ranges.iter().enumerate() {
        if let Some(ref ctx) = range.parent_context {
            last_index_for_parent.insert(ctx.header_line, idx);
        }
    }

    // Per-file monotonic cursor — prevents duplicate changed lines when two
    // adjacent ranges share one hunk.  Created here (per-file) so it resets
    // correctly for each FileDiff without leaking across file boundaries.
    let markers = HunkLineMarkers::from_hunks(hunks);
    let inputs = EmitInputs::new(hunks, source_lines, ln_width, &markers);
    let mut state = RenderState::default();

    for (idx, range) in changed_ranges.iter().enumerate() {
        // Emit parent header if this is a nested node
        if let Some(ref ctx) = range.parent_context
            && emitted_parent_headers.insert(ctx.header_line)
        {
            emit_source_line(output, ctx.header_line, &inputs, &mut state);
        }

        // Clip the render range to exclude parent boundary lines that are
        // emitted separately (header above, close brace below).  When a
        // grandchild node starts on the same line as the container header
        // (e.g. a class-body node at `{` on line 1) or ends on the same
        // line as the closing brace, render_node_with_hunks would otherwise
        // re-emit those lines as unchanged context, producing duplicates.
        let (effective_start, effective_end) = if let Some(ref ctx) = range.parent_context {
            let start = if range.start == ctx.header_line {
                range.start + 1
            } else {
                range.start
            };
            let end = if range.end == ctx.close_line {
                range.end.saturating_sub(1)
            } else {
                range.end
            };
            (start, end)
        } else {
            (range.start, range.end)
        };

        if effective_start <= effective_end {
            render_node_with_hunks(output, effective_start, effective_end, &inputs, &mut state);
        }

        // Emit parent closing brace if this is the last child with this parent
        if let Some(ref ctx) = range.parent_context {
            let is_last = last_index_for_parent
                .get(&ctx.header_line)
                .is_some_and(|&last_idx| last_idx == idx);
            if is_last {
                emit_source_line(output, ctx.close_line, &inputs, &mut state);
            }
        }
    }
}

/// Render default mode with hunk-scoped output (ADR-001 compliance, #R2).
///
/// Implements a **single positional walk** over all hunks (C1a fix):
///   1. Pre-computes a breadcrumb schedule: for each changed AST range, the
///      first hunk where `breadcrumb_line < hunk.new_start` gets a scheduled
///      breadcrumb.  This constraint ensures the breadcrumb's line is always
///      OUTSIDE the hunk window — the hunk never re-emits it.
///   2. Walks hunks in document order.  Before each hunk, emits any scheduled
///      breadcrumbs.  Then walks ALL hunk patch lines (in-node AND orphan) in
///      one pass.
///
/// **Three bugs eliminated by this design:**
///   - Bug 1 (duplicate context): breadcrumb no longer emitted mid-hunk (only
///     strictly before), so the hunk cannot re-emit the same line as context.
///   - Bug 2 (out-of-order orphan): orphan lines appear during the single walk,
///     in document order, not after all range processing.
///   - Bug 3 (`+` as context): `+` lines at the function header position are
///     now emitted by the hunk itself (not the breadcrumb), preserving the
///     correct `+` prefix.
///
/// **Coverage guarantee (#317, ADR-003):** every patch line in every hunk is
/// visited exactly once during the single walk — no clipping, no skip pass.
/// In-node and orphan lines are treated identically.
///
/// **Emissions tracking (C1b/C1d):** every emitted line is pushed to
/// `emissions` for the post-render verifier (`verify_ast_render`) as
/// `(axis, line, marker)`: `+` and context lines on the New axis, `-` lines on
/// the Old axis.  The `\` marker carries no line number and no delta, so it is
/// not tracked.
fn render_default_scoped(
    output: &mut String,
    changed_ranges: &[ChangedNodeRange],
    inputs: &EmitInputs<'_>,
    state: &mut RenderState,
) {
    // -------------------------------------------------------------------------
    // Phase 1 — breadcrumb schedule
    //
    // For each changed range, find the first overlapping hunk H where
    // `breadcrumb_line < H.new_start`.  That constraint guarantees the
    // breadcrumb's line is strictly before the hunk's window, so the hunk can
    // never visit it and we never need cursor-based de-duplication.
    //
    // breadcrumb_line → earliest hunk_idx at which to emit it.
    // -------------------------------------------------------------------------
    let mut schedule: HashMap<usize, usize> = HashMap::new();

    for range in changed_ranges {
        // `breadcrumb_line` is always >= 1 (tree-sitter row + 1, see comments
        // in `render_changed_only`).  Use `checked_sub` defensively.
        let breadcrumb_line = range
            .parent_context
            .as_ref()
            .map_or(range.start, |p| p.header_line);

        // Skip to the first hunk whose new-range ends at/after range.start.
        let first = inputs.hunks.partition_point(|h| {
            h.new_start.saturating_add(h.new_count.saturating_sub(1)) < range.start
        });

        // Walk overlapping hunks to find the first one where
        // `breadcrumb_line < hunk.new_start`.
        let emit_before = inputs.hunks[first..]
            .iter()
            .enumerate()
            .take_while(|(_, h)| h.new_start <= range.end)
            .find(|(_, h)| breadcrumb_line < h.new_start)
            .map(|(i, _)| first + i);

        if let Some(hunk_idx) = emit_before {
            // Keep the earliest hunk index for this breadcrumb line so that two
            // ranges with the same breadcrumb schedule it before the first one.
            schedule
                .entry(breadcrumb_line)
                .and_modify(|v| *v = (*v).min(hunk_idx))
                .or_insert(hunk_idx);
        }
    }

    // Build a per-hunk list of breadcrumb lines, sorted for deterministic output.
    // hunk_crumbs[i] = breadcrumb lines to emit before hunk i, in source order.
    let mut hunk_crumbs: Vec<Vec<usize>> = vec![Vec::new(); inputs.hunks.len()];
    for (&bl, &hi) in &schedule {
        hunk_crumbs[hi].push(bl);
    }
    for crumbs in &mut hunk_crumbs {
        crumbs.sort_unstable();
    }

    // -------------------------------------------------------------------------
    // Phase 2 — single positional walk (C1a fix, B2 breadcrumb routing fix)
    //
    // Walk hunks in document order. Before each hunk, emit any scheduled
    // breadcrumbs (guaranteed breadcrumb_line < hunk.new_start, so no
    // overlap with the hunk's patch lines). Then walk ALL patch lines.
    //
    // B2 fix: breadcrumbs now route through `emit_source_line`, which is the
    // ONLY valid emission path (file-wrapper-fidelity KB anti-pattern).  This
    // fixes two defects that existed in the old direct-writeln! path (#512):
    //   1. The EmittedCursor was not consulted — `emit_source_line` updates
    //      state.cursor.last_new so the cursor participates in tracking.
    //      Breadcrumbs sorted by schedule construction + hunk order are
    //      monotonically increasing, so the cursor never incorrectly blocks.
    //   2. Marker::Context was hard-coded — `emit_source_line` looks up the
    //      real marker via inputs.markers.new_side(), so an added (`+`) header
    //      line is rendered with the `+` prefix instead of a misleading space.
    //      The old code relied on the C1d verifier to catch the corruption and
    //      bail to raw hunks; that bail is now avoidable.
    // -------------------------------------------------------------------------
    for (hunk_idx, hunk) in inputs.hunks.iter().enumerate() {
        // --- Breadcrumbs (B2: routed through emit_source_line) ---
        for &breadcrumb_line in &hunk_crumbs[hunk_idx] {
            // emit_source_line consults state.cursor.last_new for monotonic
            // de-duplication and updates it on success — no separate HashSet
            // needed.  The schedule already maps each breadcrumb_line to
            // exactly one hunk, so no duplicate breadcrumb can appear here.
            emit_source_line(output, breadcrumb_line, inputs, state);
        }

        // --- Hunk patch lines (single pass, no clipping) ---
        //
        // Every line — in-node and orphan alike — is emitted here.
        // The old design split these across a range loop (in-node) and a
        // trailing orphan pass (out-of-node), causing Bug 2 (out-of-order).
        let mut cur_new = hunk.new_start;
        let mut cur_old = hunk.old_start;
        for patch_line in &hunk.patch_lines {
            let (nd, od, marker) =
                emit_patch_line(output, patch_line, cur_new, cur_old, inputs.ln_width);
            record_patch_emission(&mut state.emissions, cur_new, cur_old, marker);
            cur_new += nd;
            cur_old += od;
        }
    }
}

/// Render changed nodes with unchanged nodes as context (structure/full mode).
///
/// Walks all top-level AST nodes. Changed nodes get full patch rendering;
/// unchanged nodes are rendered as signatures (structure mode) or in full
/// (full mode).
///
/// `parser` is threaded through for reuse by `render_unchanged_node` in
/// structure mode, avoiding per-node parser re-creation.
///
/// `state` carries the per-file cursor and the emission trace.  Every emission
/// site below routes through it (C1c): tree-sitter node spans OVERLAP — a Rust
/// `line_comment` token includes its trailing newline, so node N spans rows
/// `[N, N+1]` and adjacent comment nodes share a line — and before C1c each
/// site wrote directly to `output`, so the shared line was emitted once per
/// node.
fn render_with_unchanged_context(
    output: &mut String,
    tree: &tree_sitter::Tree,
    ctx: &ModeRenderContext<'_>,
    inputs: &EmitInputs<'_>,
    parser: &mut rskim_core::Parser,
    state: &mut RenderState,
) {
    let root = tree.root_node();
    let mut walker = root.walk();

    // Next line not yet accounted for by a rendered node.  Drives the orphan
    // gap fill below (ADR-003 coverage).
    let mut next_line = 1usize;

    for child in root.children(&mut walker) {
        let node_start = child.start_position().row + 1;
        let node_end = child.end_position().row + 1;

        render_orphan_gap(output, next_line, node_start, ctx, inputs, state);
        next_line = next_line.max(node_end + 1);

        // Check if this top-level node contains any changed range.
        //
        // changed_ranges is sorted by start (AST children are visited in
        // document order), so partition_point skips all ranges that end
        // before this node. We then scan forward only while range.start
        // is within the node boundary — O(log R + matches) instead of O(R).
        let first = ctx.changed_ranges.partition_point(|r| r.start < node_start);
        let has_changes = ctx.changed_ranges[first..].iter().any(|r| {
            if r.start > node_end {
                return false;
            }
            // B3: overlap test (was containment).  partition_point guarantees
            // r.start >= node_start; the early-exit above guarantees
            // r.start <= node_end.  Any range reaching here overlaps this node.
            // The old `r.end <= node_end` containment guard and the now-dead
            // parent_context fallback are both removed — they were redundant
            // given that tree-sitter always positions grandchildren inside their
            // parent's span, but the containment form would silently skip a
            // range that starts inside the node but ends past it.
            true
        });

        if has_changes {
            // This node contains changes — render with full patch detail.
            // If it's a container, render parent header + changed children + context children.
            if is_container_node(&child) {
                render_container_with_mode(output, &child, ctx, inputs, parser, state);
            } else {
                // Non-container changed node: render with patch
                render_node_with_hunks(output, node_start, node_end, inputs, state);
            }
        } else {
            // Unchanged node: render at mode level
            render_unchanged_node(output, &child, ctx, inputs, parser, state);
        }
    }

    // Trailing gap: deletions at EOF sit past the last AST node.
    let last_hunk_line = inputs
        .hunks
        .iter()
        .map(|h| h.new_start.max(h.new_start + h.new_count.saturating_sub(1)))
        .max()
        .unwrap_or(0);
    render_orphan_gap(
        output,
        next_line,
        last_hunk_line.saturating_add(1),
        ctx,
        inputs,
        state,
    );
}

/// Render the lines in `[start, before_line)` that belong to no AST node.
///
/// ADR-003 coverage.  The structure/full walk visits top-level AST nodes, but a
/// hunk can touch a line no node owns — a blank line between two declarations
/// (the measured `92417dc9` case: line 362, a `+` blank between the new
/// `struct`'s closing brace and the new `impl`), or a trailing deletion at EOF.
/// Those orphan lines are exactly the silent-omission class ADR-003 names, and
/// because omitting them makes the output SMALLER the ADR-001 net-savings guard
/// is structurally blind to it.  `render_default_scoped` avoids the problem by
/// walking hunks rather than nodes; this is the node-walk's equivalent.
///
/// The gap is rendered only when it actually intersects a changed line.  A gap
/// of untouched blank lines carries no coverage obligation, and emitting it
/// would bloat structure mode for no fidelity gain.
fn render_orphan_gap(
    output: &mut String,
    start: usize,
    before_line: usize,
    ctx: &ModeRenderContext<'_>,
    inputs: &EmitInputs<'_>,
    state: &mut RenderState,
) {
    let Some(end) = before_line.checked_sub(1) else {
        return;
    };
    if start > end {
        return;
    }
    if ctx.changed_lines.range(start..=end).next().is_none() {
        return;
    }
    render_node_with_hunks(output, start, end, inputs, state);
}

/// Render a container node (class/struct/impl/trait/enum) with mode-aware
/// member rendering.
///
/// **C1c — the root-cause fix.** The header and closing-brace emissions used to
/// write `" {:>ln_width$} {line}"` unconditionally and neither consulted nor
/// advanced the cursor.  Both now route through [`emit_source_line`], so:
///
/// - a header line that the diff marks `+` renders as `+`, not as pre-existing
///   context (the measured `92417dc9` case, where a wholly new `struct` *and*
///   `impl` block read as "only the derive was added");
/// - a header line a hunk walk already emitted is not emitted a second time
///   (the measured `6f8edd82` case, where line 63 appeared as `+63` then ` 63`).
fn render_container_with_mode(
    output: &mut String,
    node: &tree_sitter::Node<'_>,
    ctx: &ModeRenderContext<'_>,
    inputs: &EmitInputs<'_>,
    parser: &mut rskim_core::Parser,
    state: &mut RenderState,
) {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;

    // Emit parent header — cursor-gated and marker-stamped (C1c).
    emit_source_line(output, node_start, inputs, state);

    // Walk the container's members.
    //
    // A direct child that begins AND ends on the header line is a declaration
    // fragment (`impl`, the type name, the opening brace) already covered by
    // the header emission above.  A direct child that begins on the header line
    // but extends past it is the container BODY — `declaration_list` (Rust),
    // `field_declaration_list`, `class_body` (TS/JS), `block` (Python).  The old
    // `child_start == node_start { continue; }` rule skipped that body along
    // with the fragments, so a changed container rendered as nothing but its
    // header and closing brace and every member vanished; because dropping
    // content makes output SMALLER, the ADR-001 net-savings guard waved it
    // through (the blindspot recorded in the file-wrapper-fidelity knowledge
    // base and in ADR-003).
    //
    // Descend exactly one level into the body.  Bounded: no recursion, so
    // traversal cost stays linear in the container's members (PF-020).
    let mut child_cursor = node.walk();
    for child in node.children(&mut child_cursor) {
        let child_start = child.start_position().row + 1;
        let child_end = child.end_position().row + 1;

        if child_start == node_start && child_end <= node_start {
            continue; // header-line declaration fragment
        }

        if child_start == node_start {
            // The container body — render its members, not the body node itself.
            //
            // Note: the B3 gap-fill (interleaving render_orphan_gap between
            // members) was reverted here because it caused a structure-mode
            // rendering regression.  When the body's direct children include
            // separator tokens (`,` in Rust field_declaration_list), the gap-fill
            // iterated over them and called render_unchanged_node, which applied
            // the structure transform to a bare `,` token, writing ` ,\n` and
            // also splitting field text from its comma.  The stated defect (an
            // orphan blank line being dropped) did not reproduce on any tested
            // fixture before B3.  See Phase B-repair commit for full analysis.
            let mut body_cursor = child.walk();
            for member in child.children(&mut body_cursor) {
                render_container_member(output, node, &member, ctx, inputs, parser, state);
            }
            continue;
        }

        render_container_member(output, node, &child, ctx, inputs, parser, state);
    }

    // Emit closing brace — cursor-gated, so a member that already rendered the
    // brace line (the body's own `}` token) does not produce a duplicate.
    if node_end > node_start {
        emit_source_line(output, node_end, inputs, state);
    }
}

/// Render one member of a container: with patch detail when it matches a
/// changed range, at mode level otherwise.
///
/// Members lying entirely on the container's header or closing-brace line are
/// skipped — those two lines are emitted by `render_container_with_mode` itself.
fn render_container_member(
    output: &mut String,
    container: &tree_sitter::Node<'_>,
    member: &tree_sitter::Node<'_>,
    ctx: &ModeRenderContext<'_>,
    inputs: &EmitInputs<'_>,
    parser: &mut rskim_core::Parser,
    state: &mut RenderState,
) {
    let container_start = container.start_position().row + 1;
    let container_end = container.end_position().row + 1;
    let member_start = member.start_position().row + 1;
    let member_end = member.end_position().row + 1;

    if member_end <= container_start {
        return; // fragment lying entirely on the header line
    }
    if member_start >= container_end {
        return; // fragment lying entirely on the closing-brace line
    }

    // Classify by line overlap, not by an exact `changed_ranges` match.
    //
    // `find_changed_node_ranges` records container children — for a Rust `impl`
    // that is the single `declaration_list` spanning the whole body, never the
    // individual methods.  Matching members against those ranges therefore
    // classified EVERY member as unchanged, and an unchanged member renders only
    // its new-side source lines, so any `-` line inside it silently vanished.
    // `changed_lines` is the same set `find_changed_node_ranges` tests against,
    // applied at the depth the members actually live at.
    //
    // `BTreeSet::range` is O(log N + matches), so this stays cheap per member.
    let member_changed = ctx
        .changed_lines
        .range(member_start..=member_end)
        .next()
        .is_some();

    if member_changed {
        render_node_with_hunks(output, member_start, member_end, inputs, state);
    } else {
        render_unchanged_node(output, member, ctx, inputs, parser, state);
    }
}

/// Render an unchanged node at the appropriate mode level.
///
/// In structure mode, reuses the provided `parser` for transformation
/// instead of creating a new parser per node.
///
/// Full mode renders line numbers using `ln_width` for alignment.
/// Structure mode emits synthetic transformed text — no line numbers
/// since the lines don't correspond 1-to-1 with source positions.
fn render_unchanged_node(
    output: &mut String,
    node: &tree_sitter::Node<'_>,
    ctx: &ModeRenderContext<'_>,
    inputs: &EmitInputs<'_>,
    parser: &mut rskim_core::Parser,
    state: &mut RenderState,
) {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;

    match ctx.diff_mode {
        DiffMode::Full => {
            // Show unchanged nodes in full with line numbers.  Every line goes
            // through `emit_source_line` (C1c) because tree-sitter node spans
            // overlap: a Rust `line_comment` token includes its trailing
            // newline, so consecutive `//!` comments produce nodes [1,2], [2,3],
            // … and the old direct write emitted the shared line once per node
            // (the measured `d7407d6c` case, where every module-doc line from
            // the second onward appeared twice).
            for line_num in node_start..=node_end {
                emit_source_line(output, line_num, inputs, state);
            }
        }
        DiffMode::Structure => {
            // Show unchanged nodes as structure (signatures).
            //
            // Structure output is synthetic (transformed) text — line numbers
            // are omitted because the lines do not correspond 1-to-1 with real
            // source positions.  Nothing is recorded in `state` for the same
            // reason: an emission carries a line NUMBER, and these lines have
            // none.  Recording them under a fabricated number would make the
            // verifier reject correct renders (or index out of range).
            let node_text = node.utf8_text(ctx.source.as_bytes()).unwrap_or_default();

            // Transform using the reused parser (avoids per-node parser creation)
            let config = rskim_core::TransformConfig::with_mode(rskim_core::Mode::Structure);
            match parser.transform(node_text, &config) {
                Ok(transformed) => {
                    for line in transformed.lines() {
                        let _ = writeln!(output, " {line}");
                    }
                }
                Err(_) => {
                    // Fall back to showing just the first line (declaration)
                    if let Some(line) = inputs.source_lines.get(node_start - 1) {
                        let _ = writeln!(output, " {line}");
                    }
                }
            }
        }
        DiffMode::Default => {
            // Default mode: unchanged nodes are omitted (handled by caller)
        }
    }
}

/// Emit patch lines from a single hunk, clipped to the node's line range
/// `[node_start, node_end]`.
///
/// A single git hunk can span multiple AST nodes (one `@@` block covering both
/// an interface ending at line 8 and a class starting at line 10).  Without
/// clipping, every node that overlaps the hunk would emit ALL of the hunk's
/// patch lines, producing duplicate output across adjacent nodes.
///
/// Clipping rules:
/// - Skip lines before `node_start`: advance counters without emitting.
///   Removed lines (`-`, new_delta == 0) are skipped if `patch_new_line`
///   hasn't yet reached `node_start`.
/// - Stop after `node_end`: break once `patch_new_line > node_end`.
///
/// De-duplication (`cursor`): when two adjacent changed ranges share a hunk,
/// this function is called once per range.  The second call starts with the
/// same `hunk.new_start` / `hunk.old_start` and would re-emit lines already
/// output by the first call.  The cursor (`&mut EmittedCursor`) prevents that:
/// - `+` / ` ` (context) line: skip when `patch_new_line <= cursor.last_new`.
/// - `-` line: skip when `patch_old_line <= cursor.last_old`.
///
/// Both axes are updated after each emission.
///
/// Returns the final `patch_new_line` value so the caller can advance
/// `current_new_line` past the lines consumed by this hunk.
fn emit_hunk_patch_lines_clipped(
    output: &mut String,
    hunk: &DiffHunk<'_>,
    node_start: usize,
    node_end: usize,
    inputs: &EmitInputs<'_>,
    state: &mut RenderState,
) -> usize {
    let ln_width = inputs.ln_width;
    let mut patch_new_line = hunk.new_start;
    let mut patch_old_line = hunk.old_start;

    for patch_line in &hunk.patch_lines {
        // Stop once we've passed the node's end boundary.
        if patch_new_line > node_end {
            break;
        }

        let (new_delta, old_delta) = match patch_line.as_bytes().first() {
            Some(b'+') => (1usize, 0usize),
            Some(b'-') => (0, 1),
            Some(b' ') => (1, 1),
            _ => (0, 0),
        };

        // Skip lines that fall before the node's start (hunk started earlier
        // in the file).
        if patch_new_line < node_start {
            patch_new_line += new_delta;
            patch_old_line += old_delta;
            continue;
        }

        // De-duplication: skip lines already emitted by a prior range call.
        // `+` / context lines are identified by the new-file axis; `-` lines
        // (which do not advance new_line) by the old-file axis.
        let already_emitted = match patch_line.as_bytes().first() {
            Some(b'-') => patch_old_line <= state.cursor.last_old,
            _ => patch_new_line <= state.cursor.last_new,
        };
        if already_emitted {
            patch_new_line += new_delta;
            patch_old_line += old_delta;
            continue;
        }

        let (nd, od, marker) =
            emit_patch_line(output, patch_line, patch_new_line, patch_old_line, ln_width);

        // Update the emitted cursor after each emit so subsequent calls in the
        // same file skip the lines we just wrote.
        if nd > 0 {
            state.cursor.last_new = state.cursor.last_new.max(patch_new_line);
        }
        if od > 0 {
            state.cursor.last_old = state.cursor.last_old.max(patch_old_line);
        }
        record_patch_emission(&mut state.emissions, patch_new_line, patch_old_line, marker);

        patch_new_line += nd;
        patch_old_line += od;
    }

    patch_new_line
}

/// Render a node region with hunk patch lines overlaid, including line numbers.
///
/// Line number assignment:
/// - `+` (added) lines use the new-file line number; `current_new_line` advances.
/// - `-` (removed) lines use the old-file line number; `current_old_line` advances.
/// - ` ` (context) lines use the new-file line number; both counters advance.
/// - `\` (no-newline marker) has no line number.
/// - Unchanged source lines between hunks use the new-file line number.
///
/// `cursor` is a per-file monotonic cursor that prevents duplicate changed-line
/// output when two adjacent ranges share a hunk.  Created once per file by the
/// caller and threaded here; updated via `emit_hunk_patch_lines_clipped`.
fn render_node_with_hunks(
    output: &mut String,
    node_start: usize,
    node_end: usize,
    inputs: &EmitInputs<'_>,
    state: &mut RenderState,
) {
    // Hunks are sorted by new_start (they come from git's sequential output).
    // Use partition_point to skip hunks that end before node_start, then
    // take_while to stop once the hunk starts after node_end — O(log H + matches).
    let first = inputs.hunks.partition_point(|h| {
        h.new_start.saturating_add(h.new_count.saturating_sub(1)) < node_start
    });
    let relevant_hunks: Vec<&DiffHunk<'_>> = inputs.hunks[first..]
        .iter()
        .take_while(|h| h.new_start <= node_end)
        .collect();

    if relevant_hunks.is_empty() {
        // No hunks overlap — show as unchanged context with new-file line numbers
        for line_num in node_start..=node_end {
            emit_source_line(output, line_num, inputs, state);
        }
        return;
    }

    let mut current_new_line = node_start;

    for hunk in &relevant_hunks {
        // Output unchanged source lines before this hunk's position.
        // Context lines: use new-file line number.
        while current_new_line < hunk.new_start && current_new_line <= node_end {
            emit_source_line(output, current_new_line, inputs, state);
            current_new_line += 1;
        }

        // Old-line cursor starts at the hunk boundary.
        // The patch-line cursor starts at hunk.new_start so that skip logic
        // correctly advances past pre-node lines when the hunk begins before
        // node_start.
        current_new_line =
            emit_hunk_patch_lines_clipped(output, hunk, node_start, node_end, inputs, state);
    }

    // Output remaining unchanged source lines to end of node
    while current_new_line <= node_end {
        emit_source_line(output, current_new_line, inputs, state);
        current_new_line += 1;
    }
}

/// Emit a NEW-side SOURCE line, stamped with the marker the diff gives it and
/// gated by the per-file cursor.
///
/// Every source-derived emission in the structure/full path routes through this
/// function: the container header, the container's closing brace, the unchanged
/// lines `render_node_with_hunks` fills around hunks, and full mode's
/// unchanged-node bodies.  It enforces exactly the two properties the old
/// hard-coded `" {:>ln_width$} {line}"` writes lacked (C1c):
///
/// - **cursor participation** — a line at or behind `cursor.last_new` has
///   already been rendered, by a hunk walk or by an overlapping AST node, so it
///   is skipped; it can be neither duplicated nor emitted out of order;
/// - **marker fidelity** — a line the diff marks `+` renders as `+`, never as
///   pre-existing context (C1d).
///
/// Out-of-range line numbers are ignored rather than panicking: node spans come
/// from tree-sitter and hunk numbers from git, and the two can disagree at the
/// end of a truncated file.
fn emit_source_line(
    output: &mut String,
    line_no: usize,
    inputs: &EmitInputs<'_>,
    state: &mut RenderState,
) {
    if line_no <= state.cursor.last_new {
        return;
    }
    let Some(line) = line_no
        .checked_sub(1)
        .and_then(|idx| inputs.source_lines.get(idx))
    else {
        return;
    };

    let ln_width = inputs.ln_width;
    let marker = inputs.markers.new_side(line_no);
    if marker == Marker::Added {
        let _ = writeln!(output, "+{line_no:>ln_width$} {line}");
    } else {
        let _ = writeln!(output, " {line_no:>ln_width$} {line}");
    }

    state.cursor.last_new = line_no;
    state.emissions.push((Axis::New, line_no, marker));
}

/// Record one patch-line emission on the axis its marker implies.
///
/// `Marker::Removed` lives on the Old axis; `Added` and `Context` on the New
/// axis.  `None` is the `\ No newline at end of file` marker, which carries no
/// line number and is therefore not tracked.
fn record_patch_emission(
    emissions: &mut Vec<Emission>,
    new_line: usize,
    old_line: usize,
    marker: Option<Marker>,
) {
    match marker {
        Some(Marker::Removed) => emissions.push((Axis::Old, old_line, Marker::Removed)),
        Some(m) => emissions.push((Axis::New, new_line, m)),
        None => {}
    }
}

/// Emit a single patch line with its line number, updating the line counters.
///
/// Returns `(new_line_delta, old_line_delta, marker)` — the amount each counter
/// should advance after this line, and the prefix the line was written with.
/// Most callers immediately add the deltas back; splitting the counters out of
/// this function avoids passing `&mut` through the hot path.
///
/// The marker is returned rather than re-derived by the caller so the emission
/// trace records what was actually WRITTEN, not what the caller assumed — the
/// distinction C1d's marker-fidelity check depends on.
///
/// `\` (no-newline marker) and unknown prefixes are written verbatim with no
/// line number, contribute zero delta to either counter, and yield `None`.
fn emit_patch_line(
    output: &mut String,
    patch_line: &str,
    current_new_line: usize,
    current_old_line: usize,
    ln_width: usize,
) -> (usize, usize, Option<Marker>) {
    // Use get(1..) instead of &s[1..] for defensive byte-slice safety (PF-020).
    // The prefixes +/-/space are single-byte ASCII so this is always correct,
    // but get(1..) avoids a panic if the string is somehow empty.
    let rest = patch_line.get(1..).unwrap_or("");
    match patch_line.as_bytes().first() {
        Some(b'+') => {
            let _ = writeln!(output, "+{:>ln_width$} {}", current_new_line, rest);
            (1, 0, Some(Marker::Added))
        }
        Some(b'-') => {
            let _ = writeln!(output, "-{:>ln_width$} {}", current_old_line, rest);
            (0, 1, Some(Marker::Removed))
        }
        Some(b' ') => {
            let _ = writeln!(output, " {:>ln_width$} {}", current_new_line, rest);
            (1, 1, Some(Marker::Context))
        }
        _ => {
            // `\` (no-newline marker) or unexpected prefix — emit verbatim, no line number
            let _ = writeln!(output, "{patch_line}");
            (0, 0, None)
        }
    }
}

/// Post-render correctness verifier (C1b, extended by C1d/C1e).
///
/// Checks four invariants that the ADR-001 net-savings size guard cannot catch
/// (it fires only on OVER-emission, never on silent UNDER-emission or content
/// substitution — see ADR-003):
///
/// 1. **Per-axis uniqueness** — no line number appears twice on the same axis.
/// 2. **New-axis monotonicity** — new-side numbers never jump backward.
/// 3. **Coverage** — every `+` hunk line appears in the New-axis trace; every
///    `-` hunk line appears in the Old-axis trace.
/// 4. **Marker fidelity** — a line rendered `+` is a `+` line in the hunks, a
///    line rendered ` ` is not, and a line rendered `-` is a `-` line.
///
/// Check 4 is the one that catches the dominant corruption class.  Checks 1-3
/// ask only whether a line NUMBER appears in the trace and never inspect the
/// prefix, so an `added-as-context` render — the correct number with a
/// hard-coded context marker — passes all three while telling the reader that
/// brand-new code is pre-existing.  It is derived independently from `hunks`
/// rather than reusing the table the renderer stamped markers from, so the
/// check cannot degenerate into a tautology.
///
/// Applied to EVERY diff mode (C1e), not just `Default`: `structure` and `full`
/// route through `render_with_unchanged_context`, which was the uncovered path
/// where the corruption lived.  Inter-node gaps in those modes are harmless —
/// they produce MISSING numbers, never duplicates, and monotonic is not the
/// same as contiguous.
///
/// Returns `true` when all invariants hold (render is correct).
/// Returns `false` on any violation — the caller falls back to `render_raw_hunks`,
/// which is always safe (ADR-011 class-2 no-loss fallback; banner is
/// `crate::debug_log!`-gated → zero stderr bytes without `SKIM_DEBUG`).
///
/// **PF-025 lesson:** every check here was proven against a known-corrupt input
/// before adoption — duplicate line, backward jump, dropped `+` line, and wrong
/// marker each have a unit test that constructs the corruption and asserts
/// rejection.  Do not adopt candidate guards that pass on known-corrupt data.
///
/// Cost: three linear passes over `emissions` plus one over the patch lines, with
/// both `HashSet`s pre-sized — O(n) with no growth reallocation on the hot path.
fn verify_ast_render(emissions: &[Emission], hunks: &[DiffHunk<'_>]) -> Result<(), VerifyFailure> {
    // Pre-populate seen sets (used for both uniqueness and coverage checks).
    let mut new_seen: HashSet<usize> = HashSet::with_capacity(emissions.len());
    let mut old_seen: HashSet<usize> = HashSet::with_capacity(emissions.len() / 4 + 1);

    // (1) Per-axis uniqueness — reject the first duplicate.
    for &(axis, line, _) in emissions {
        let fresh = match axis {
            Axis::New => new_seen.insert(line),
            Axis::Old => old_seen.insert(line),
        };
        if !fresh {
            return Err(VerifyFailure::DuplicateLine { axis, line });
        }
    }

    // (2) New-axis monotonicity — reject any backward jump.
    let mut prev_new: usize = 0;
    for &(axis, line, _) in emissions {
        if axis == Axis::New {
            if line < prev_new {
                return Err(VerifyFailure::BackwardJump {
                    previous: prev_new,
                    line,
                });
            }
            prev_new = line;
        }
    }

    // (3) Coverage — every `+` and `-` hunk line must appear in the trace.
    // Context ` ` lines are not checked (they aren't tracked on the Old axis).
    for hunk in hunks {
        let mut cur_new = hunk.new_start;
        let mut cur_old = hunk.old_start;
        for patch_line in &hunk.patch_lines {
            match patch_line.as_bytes().first() {
                Some(b'+') => {
                    if !new_seen.contains(&cur_new) {
                        return Err(VerifyFailure::UncoveredChange {
                            axis: Axis::New,
                            line: cur_new,
                        });
                    }
                    cur_new += 1;
                }
                Some(b'-') => {
                    if !old_seen.contains(&cur_old) {
                        return Err(VerifyFailure::UncoveredChange {
                            axis: Axis::Old,
                            line: cur_old,
                        });
                    }
                    cur_old += 1;
                }
                Some(b' ') => {
                    cur_new += 1;
                    cur_old += 1;
                }
                _ => {} // `\` no-newline marker — no advance, no check
            }
        }
    }

    // (4) Marker fidelity — independently re-derive what the diff says about
    // each line and require the emitted prefix to agree.
    let markers = HunkLineMarkers::from_hunks(hunks);
    for &(axis, line, marker) in emissions {
        let agrees = match (axis, marker) {
            (Axis::New, Marker::Added) => markers.added.contains(&line),
            (Axis::New, Marker::Context) => !markers.added.contains(&line),
            (Axis::Old, Marker::Removed) => markers.removed.contains(&line),
            // A `-` on the new axis, or a `+`/` ` on the old axis, is not a
            // shape any emitter produces.  Reject rather than silently accept:
            // an unexpected pairing means the emission bookkeeping itself drifted.
            _ => false,
        };
        if !agrees {
            return Err(VerifyFailure::MarkerMismatch { axis, line, marker });
        }
    }

    Ok(())
}

/// Render raw diff hunks as fallback (no AST awareness), with line numbers.
///
/// Tracks old and new line counters across all hunks in the file, emitting
/// the appropriate file line number after each prefix character.
fn render_raw_hunks(file_diff: &FileDiff<'_>, header: &str, ln_width: usize) -> String {
    let mut output = header.to_string();
    for hunk in &file_diff.hunks {
        let mut current_new_line = hunk.new_start;
        let mut current_old_line = hunk.old_start;
        for line in &hunk.patch_lines {
            let (new_delta, old_delta, _) = emit_patch_line(
                &mut output,
                line,
                current_new_line,
                current_old_line,
                ln_width,
            );
            current_new_line += new_delta;
            current_old_line += old_delta;
        }
    }
    output
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::canonical::{DiffFileEntry, DiffResult};

    // ========================================================================
    // Render output tests (#103)
    // ========================================================================

    #[test]
    fn test_render_binary_file() {
        let file_diff = FileDiff {
            path: "assets/logo.png".to_string(),
            old_path: None,
            status: DiffFileStatus::Binary,
            hunks: vec![],
        };
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default, false, false);
        assert!(rendered.contains("logo.png"));
        assert!(rendered.contains("binary"));
        assert!(rendered.contains("Binary file differs"));
    }

    #[test]
    fn test_render_added_file() {
        let file_diff = FileDiff {
            path: "src/new.ts".to_string(),
            old_path: None,
            status: DiffFileStatus::Added,
            hunks: vec![DiffHunk {
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: 2,
                patch_lines: vec!["+const x = 1;", "+const y = 2;"],
            }],
        };
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default, false, false);
        assert!(rendered.contains("added"), "header should show 'added'");
        assert!(
            rendered.contains("const x = 1;"),
            "should contain added line content"
        );
        // Line numbers are prepended: format is `+{ln} {content}`
        assert!(
            rendered.contains("+1 const x = 1;") || rendered.contains("+ 1 const x = 1;"),
            "added lines should have line numbers; got: {rendered}"
        );
    }

    #[test]
    fn test_render_deleted_file() {
        let file_diff = FileDiff {
            path: "src/old.ts".to_string(),
            old_path: None,
            status: DiffFileStatus::Deleted,
            hunks: vec![DiffHunk {
                old_start: 1,
                old_count: 2,
                new_start: 0,
                new_count: 0,
                patch_lines: vec!["-const x = 1;", "-const y = 2;"],
            }],
        };
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default, false, false);
        assert!(rendered.contains("deleted"), "header should show 'deleted'");
        assert!(
            rendered.contains("const x = 1;"),
            "should contain deleted line content"
        );
        // Line numbers are prepended: format is `-{ln} {content}`
        assert!(
            rendered.contains("-1 const x = 1;") || rendered.contains("- 1 const x = 1;"),
            "deleted lines should have line numbers; got: {rendered}"
        );
    }

    #[test]
    fn test_render_renamed_file_header() {
        let file_diff = FileDiff {
            path: "src/utils/format.ts".to_string(),
            old_path: Some("src/utils/helpers.ts".to_string()),
            status: DiffFileStatus::Renamed,
            hunks: vec![],
        };
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default, false, false);
        assert!(rendered.contains("helpers.ts"), "should show old path");
        assert!(rendered.contains("format.ts"), "should show new path");
        assert!(rendered.contains("renamed"), "header should show 'renamed'");
    }

    // ========================================================================
    // DiffResult output type tests (#103)
    // ========================================================================

    #[test]
    fn test_diff_result_display() {
        let entries = vec![
            DiffFileEntry {
                path: "src/main.rs".to_string(),
                status: DiffFileStatus::Modified,
                changed_regions: 2,
            },
            DiffFileEntry {
                path: "src/lib.rs".to_string(),
                status: DiffFileStatus::Added,
                changed_regions: 1,
            },
        ];
        let result = DiffResult::new(entries, "test rendered output".to_string());
        assert_eq!(result.files_changed, 2);
        assert_eq!(result.to_string(), "test rendered output");
    }

    #[test]
    fn test_diff_result_serde_roundtrip() {
        let entries = vec![DiffFileEntry {
            path: "src/main.rs".to_string(),
            status: DiffFileStatus::Modified,
            changed_regions: 1,
        }];
        let original = DiffResult::new(entries, "rendered output".to_string());
        let json = serde_json::to_string(&original).unwrap();
        let mut deserialized: DiffResult = serde_json::from_str(&json).unwrap();
        deserialized.ensure_rendered();
        // After deserialization+ensure_rendered, it should have some output
        assert!(!deserialized.as_ref().is_empty());
    }

    // ========================================================================
    // Thread-local PARSERS cache tests
    // ========================================================================

    /// Validates that the thread-local parser cache does not corrupt state
    /// across sequential renders of the same language.
    ///
    /// If the cached parser retained stale incremental-parse state from the
    /// first call, the second render would produce wrong output. Correct
    /// output from both calls proves the cache reuse path is safe.
    #[test]
    fn test_parser_cache_reuse_does_not_corrupt_output() {
        // Both diffs are for TypeScript files — the second render must reuse
        // the same parser instance (already in the thread-local cache after
        // the first call) and still produce correct output.
        let file_diff_a = FileDiff {
            path: "src/foo.ts".to_string(),
            old_path: None,
            status: DiffFileStatus::Added,
            hunks: vec![DiffHunk {
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: 1,
                patch_lines: vec!["+const FOO = 1;"],
            }],
        };
        let file_diff_b = FileDiff {
            path: "src/bar.ts".to_string(),
            old_path: None,
            status: DiffFileStatus::Added,
            hunks: vec![DiffHunk {
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: 1,
                patch_lines: vec!["+const BAR = 2;"],
            }],
        };

        let out_a = render_diff_file(&file_diff_a, &[], &[], DiffMode::Default, false, false);
        let out_b = render_diff_file(&file_diff_b, &[], &[], DiffMode::Default, false, false);

        // Each output should contain only its own added line, not content
        // from the other file — proving cache reuse doesn't bleed state.
        assert!(
            out_a.contains("foo.ts"),
            "first render should reference foo.ts"
        );
        assert!(
            out_a.contains("FOO = 1;"),
            "first render should contain its patch line content"
        );
        assert!(
            out_b.contains("bar.ts"),
            "second render should reference bar.ts"
        );
        assert!(
            out_b.contains("BAR = 2;"),
            "second render should contain its patch line content"
        );
        assert!(
            !out_a.contains("BAR"),
            "first render must not bleed second file content"
        );
        assert!(
            !out_b.contains("FOO"),
            "second render must not bleed first file content"
        );
    }

    // ========================================================================
    // MAX_AST_FILE_COUNT / skip_ast tests (#103 review batch-7)
    // ========================================================================

    #[test]
    fn test_render_diff_file_skip_ast_uses_raw_hunks() {
        // When skip_ast is true, render_diff_file should produce raw patch
        // lines instead of attempting AST-aware rendering.
        let file_diff = FileDiff {
            path: "src/foo.rs".to_string(),
            old_path: None,
            status: DiffFileStatus::Modified,
            hunks: vec![DiffHunk {
                old_start: 1,
                old_count: 3,
                new_start: 1,
                new_count: 4,
                patch_lines: vec![" fn main() {", "+    println!(\"hello\");", " }"],
            }],
        };

        let output = render_diff_file(
            &file_diff,
            &[],
            &[],
            DiffMode::Structure,
            true, // skip_ast
            false,
        );

        // Should contain file header
        assert!(
            output.contains("src/foo.rs (modified)"),
            "expected file header, got: {output}"
        );
        // Should contain raw patch line content with line number prefix
        assert!(
            output.contains("println!(\"hello\");"),
            "expected raw patch line content, got: {output}"
        );
        // Line number format: `+{ln} {content}`
        assert!(
            output.contains("+2     println!(\"hello\");")
                || output.contains("+ 2     println!(\"hello\");"),
            "expected line-numbered patch line, got: {output}"
        );
    }

    // ========================================================================
    // Line number tests (Workstream 1)
    // ========================================================================

    #[test]
    fn test_render_raw_hunks_shows_line_numbers() {
        // render_raw_hunks (fallback path) should prefix each line with its
        // file line number, right-aligned to the width of the largest line.
        let file_diff = FileDiff {
            path: "src/old.ts".to_string(),
            old_path: None,
            status: DiffFileStatus::Deleted,
            hunks: vec![DiffHunk {
                old_start: 10,
                old_count: 3,
                new_start: 0,
                new_count: 0,
                // Three removed lines starting at old line 10
                patch_lines: vec!["-const a = 1;", "-const b = 2;", "-const c = 3;"],
            }],
        };
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default, false, false);
        // Removed lines should use old-file line numbers (10, 11, 12)
        assert!(
            rendered.contains("-10 const a = 1;"),
            "first removed line should carry line 10; got:\n{rendered}"
        );
        assert!(
            rendered.contains("-11 const b = 2;"),
            "second removed line should carry line 11; got:\n{rendered}"
        );
        assert!(
            rendered.contains("-12 const c = 3;"),
            "third removed line should carry line 12; got:\n{rendered}"
        );
    }

    #[test]
    fn test_render_raw_hunks_multi_hunk_line_tracking() {
        // Two hunks in a single added file — line numbers must restart from each
        // hunk's new_start and not bleed across hunk boundaries.
        let file_diff = FileDiff {
            path: "src/mod.ts".to_string(),
            old_path: None,
            status: DiffFileStatus::Added,
            hunks: vec![
                DiffHunk {
                    old_start: 0,
                    old_count: 0,
                    new_start: 1,
                    new_count: 2,
                    patch_lines: vec!["+const A = 1;", "+const B = 2;"],
                },
                DiffHunk {
                    old_start: 0,
                    old_count: 0,
                    new_start: 10,
                    new_count: 1,
                    patch_lines: vec!["+const C = 3;"],
                },
            ],
        };
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default, false, false);
        // First hunk: lines 1 and 2
        assert!(
            rendered.contains("+1 const A = 1;") || rendered.contains("+ 1 const A = 1;"),
            "first hunk line 1; got:\n{rendered}"
        );
        assert!(
            rendered.contains("+2 const B = 2;") || rendered.contains("+ 2 const B = 2;"),
            "first hunk line 2; got:\n{rendered}"
        );
        // Second hunk: line 10
        assert!(
            rendered.contains("+10 const C = 3;") || rendered.contains("+10  const C = 3;"),
            "second hunk line 10; got:\n{rendered}"
        );
    }

    #[test]
    fn test_line_number_width_helper() {
        // Empty hunks → minimum width 1
        assert_eq!(line_number_width(&[]), 1);
        // Single-digit max
        assert_eq!(
            line_number_width(&[DiffHunk {
                old_start: 1,
                old_count: 3,
                new_start: 1,
                new_count: 3,
                patch_lines: vec![]
            }]),
            1
        );
        // Three-digit max (old_end = 100 + 10 = 110)
        assert_eq!(
            line_number_width(&[DiffHunk {
                old_start: 100,
                old_count: 10,
                new_start: 1,
                new_count: 5,
                patch_lines: vec![]
            }]),
            3
        );
    }

    /// When a single git hunk spans multiple AST nodes (e.g. one `@@` block covering
    /// both an interface ending at line 8 and a class starting at line 10), patch lines
    /// must be clipped to each node's [start, end] range.
    ///
    /// Without the fix, the interface node (lines 1-8) would emit patch lines for lines
    /// 4-16+, and the class node (lines 10-20) would emit the same patch lines again.
    #[test]
    fn test_render_node_with_hunks_clips_to_node_boundaries_when_hunk_spans_two_nodes() {
        // interface Foo {         ← line 1
        //   a: string;            ← line 2
        //   b: string;            ← line 3
        //   c: string;            ← line 4  (changed in hunk)
        // }                       ← line 5
        //                         ← line 6 (blank)
        // class Bar {             ← line 7
        //   x: number;            ← line 8  (changed in hunk)
        //   y: number;            ← line 9
        // }                       ← line 10
        let source_lines: Vec<&str> = vec![
            "interface Foo {",
            "  a: string;",
            "  b: string;",
            "  c: string;",
            "}",
            "",
            "class Bar {",
            "  x: number;",
            "  y: number;",
            "}",
        ];

        // Single hunk that spans both containers (lines 4-8 in the new file).
        let hunks = vec![DiffHunk {
            old_start: 4,
            old_count: 5,
            new_start: 4,
            new_count: 5,
            patch_lines: vec![
                "-  c: string;",
                "+  c: boolean;",
                " }",
                " ",
                " class Bar {",
                "-  x: number;",
                "+  x: boolean;",
            ],
        }];

        // Two changed ranges: interface body (lines 1-5) and class body (lines 7-10).
        let changed_ranges = vec![
            super::super::types::ChangedNodeRange {
                start: 1,
                end: 5,
                parent_context: Some(super::super::types::ParentContext {
                    header_line: 1,
                    close_line: 5,
                }),
            },
            super::super::types::ChangedNodeRange {
                start: 7,
                end: 10,
                parent_context: Some(super::super::types::ParentContext {
                    header_line: 7,
                    close_line: 10,
                }),
            },
        ];

        let mut output = String::new();
        render_changed_only(&mut output, &changed_ranges, &hunks, &source_lines, 2);

        // Each container header must appear exactly once.
        let foo_count = output
            .lines()
            .filter(|l| l.contains("interface Foo {"))
            .count();
        assert_eq!(
            foo_count, 1,
            "interface Foo header must appear exactly once; got {foo_count}:\n{output}"
        );

        let bar_count = output.lines().filter(|l| l.contains("class Bar {")).count();
        assert_eq!(
            bar_count, 1,
            "class Bar header must appear exactly once; got {bar_count}:\n{output}"
        );

        // Each changed line must appear exactly once (not duplicated across nodes).
        let c_bool_count = output.lines().filter(|l| l.contains("c: boolean;")).count();
        assert_eq!(
            c_bool_count, 1,
            "c: boolean must appear exactly once; got {c_bool_count}:\n{output}"
        );

        let x_bool_count = output.lines().filter(|l| l.contains("x: boolean;")).count();
        assert_eq!(
            x_bool_count, 1,
            "x: boolean must appear exactly once; got {x_bool_count}:\n{output}"
        );

        // The class Bar change must NOT appear in the interface section and vice versa.
        // We verify by checking that the interface node does not contain "x: boolean".
        // (The output is a single string but we can check ordering of appearances.)
        let foo_pos = output.find("interface Foo {").unwrap();
        let bar_pos = output.find("class Bar {").unwrap();
        let c_bool_pos = output.find("c: boolean").unwrap();
        let x_bool_pos = output.find("x: boolean").unwrap();

        assert!(
            c_bool_pos < bar_pos,
            "c: boolean must appear before class Bar section:\n{output}"
        );
        assert!(
            x_bool_pos > foo_pos,
            "x: boolean must appear after interface Foo section starts:\n{output}"
        );
        assert!(
            x_bool_pos > c_bool_pos,
            "x: boolean must appear after c: boolean (class Bar comes after interface Foo):\n{output}"
        );
    }

    // ========================================================================
    // Changed-line de-duplication tests (#6.1)
    //
    // Root cause: when two adjacent ChangedNodeRange items share one hunk
    // (the hunk spans both nodes), render_node_with_hunks was called once per
    // range and each call re-emitted the shared changed lines — producing each
    // `+`/`-` line twice.
    //
    // Fix: per-file EmittedCursor threaded through render_node_with_hunks and
    // emit_hunk_patch_lines_clipped.  Changed lines are emitted exactly once.
    // ========================================================================

    /// Two adjacent changed ranges share one hunk (doc-comment edit + signature edit).
    /// Each `+`/`-` line must appear exactly once; context lines intact.
    ///
    /// This is the canonical reproduction shape from Phase 0:
    ///   hunk covers lines 3-6; range A is [3,4], range B is [5,6].
    #[test]
    fn test_dedup_adjacent_ranges_sharing_one_hunk() {
        // Source (new file after patch):
        //   line 1: unchanged preamble
        //   line 2: unchanged preamble
        //   line 3: /** doc comment */ (changed — part of range A)
        //   line 4: /** end doc */      (changed — part of range A)
        //   line 5: fn compute(       (changed — part of range B)
        //   line 6: ) -> u64 {        (changed — part of range B)
        //   line 7: }
        let source_lines: Vec<&str> = vec![
            "// unchanged preamble",
            "// unchanged preamble 2",
            "/// doc comment",
            "/// end doc",
            "fn compute(",
            ") -> u64 {",
            "}",
        ];

        // One hunk that spans both ranges (lines 3-6 in the new file).
        // The old file had two lines replaced (3→1 doc, 5→5 fn).
        let hunks = vec![DiffHunk {
            old_start: 3,
            old_count: 4,
            new_start: 3,
            new_count: 4,
            patch_lines: vec![
                "-/// old doc comment",
                "-/// old end doc",
                "+/// doc comment",
                "+/// end doc",
                "-fn compute_old(",
                "-) -> u32 {",
                "+fn compute(",
                "+) -> u64 {",
            ],
        }];

        // Two adjacent ranges sharing the hunk.
        let changed_ranges = vec![
            super::super::types::ChangedNodeRange {
                start: 3,
                end: 4,
                parent_context: None,
            },
            super::super::types::ChangedNodeRange {
                start: 5,
                end: 6,
                parent_context: None,
            },
        ];

        let mut output = String::new();
        render_changed_only(&mut output, &changed_ranges, &hunks, &source_lines, 1);

        // Each ADDED (+) changed line must appear exactly once.
        // Count lines starting with `+` that contain the target content.
        // (Removed `-` lines may also contain "doc comment" etc. — filter by prefix.)
        let added_doc_count = output
            .lines()
            .filter(|l| l.starts_with('+') && l.contains("doc comment"))
            .count();
        assert_eq!(
            added_doc_count, 1,
            "added 'doc comment' line must appear exactly once; got {added_doc_count}:\n{output}"
        );

        let added_doc_end_count = output
            .lines()
            .filter(|l| l.starts_with('+') && l.contains("end doc"))
            .count();
        assert_eq!(
            added_doc_end_count, 1,
            "added 'end doc' line must appear exactly once; got {added_doc_end_count}:\n{output}"
        );

        let added_fn_count = output
            .lines()
            .filter(|l| l.starts_with('+') && l.contains("fn compute("))
            .count();
        assert_eq!(
            added_fn_count, 1,
            "added 'fn compute(' must appear exactly once; got {added_fn_count}:\n{output}"
        );

        let added_ret_count = output
            .lines()
            .filter(|l| l.starts_with('+') && l.contains(") -> u64 {"))
            .count();
        assert_eq!(
            added_ret_count, 1,
            "added ') -> u64 {{' must appear exactly once; got {added_ret_count}:\n{output}"
        );
    }

    /// Nested-container variant: two children inside the same parent container
    /// share one hunk.  Each changed line must be emitted exactly once.
    #[test]
    fn test_dedup_two_children_in_same_container_share_hunk() {
        // class Foo {           ← line 1 (parent header)
        //   fn a(&self) {}     ← line 2 (changed — child A)
        //   fn b(&self) {}     ← line 3 (changed — child B)
        // }                    ← line 4 (parent close)
        let source_lines: Vec<&str> =
            vec!["class Foo {", "  fn a(&self) {}", "  fn b(&self) {}", "}"];

        // Single hunk covering lines 2 and 3 (both children).
        let hunks = vec![DiffHunk {
            old_start: 2,
            old_count: 2,
            new_start: 2,
            new_count: 2,
            patch_lines: vec![
                "-  fn a(&self) -> i32 {}",
                "+  fn a(&self) {}",
                "-  fn b(&self) -> i32 {}",
                "+  fn b(&self) {}",
            ],
        }];

        // Two child ranges inside the same parent container (lines 1-4).
        let changed_ranges = vec![
            super::super::types::ChangedNodeRange {
                start: 2,
                end: 2,
                parent_context: Some(super::super::types::ParentContext {
                    header_line: 1,
                    close_line: 4,
                }),
            },
            super::super::types::ChangedNodeRange {
                start: 3,
                end: 3,
                parent_context: Some(super::super::types::ParentContext {
                    header_line: 1,
                    close_line: 4,
                }),
            },
        ];

        let mut output = String::new();
        render_changed_only(&mut output, &changed_ranges, &hunks, &source_lines, 1);

        // Container header appears once.
        let header_count = output.lines().filter(|l| l.contains("class Foo {")).count();
        assert_eq!(
            header_count, 1,
            "class Foo header must appear once; got {header_count}:\n{output}"
        );

        // Each ADDED (+) changed child line appears exactly once.
        // Count only added lines to distinguish from removed variants.
        let fn_a_count = output
            .lines()
            .filter(|l| l.starts_with('+') && l.contains("fn a("))
            .count();
        assert_eq!(
            fn_a_count, 1,
            "added 'fn a' must appear exactly once; got {fn_a_count}:\n{output}"
        );

        let fn_b_count = output
            .lines()
            .filter(|l| l.starts_with('+') && l.contains("fn b("))
            .count();
        assert_eq!(
            fn_b_count, 1,
            "added 'fn b' must appear exactly once; got {fn_b_count}:\n{output}"
        );
    }

    /// Pure-deletion hunk (all `-`, no `+` lines): removed lines deduplicate correctly.
    ///
    /// `-` lines are tracked on the old-line axis (`cursor.last_old`).
    /// A second range call covering the same old lines must NOT re-emit them.
    #[test]
    fn test_dedup_pure_deletion_hunk() {
        // Source (new file — the deleted lines are gone):
        //   line 1: fn keep()
        //   line 2: fn also_keep()
        let source_lines: Vec<&str> = vec!["fn keep()", "fn also_keep()"];

        // Pure-deletion hunk: removes two old lines at old positions 1-2,
        // new_count == 0 so new_start stays at 1 (or 0, per git convention).
        // We use new_start = 1 to ensure the skip-before-node-start logic
        // does not interfere with the dedup check.
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_count: 2,
            new_start: 1,
            new_count: 0,
            patch_lines: vec!["-fn deleted_a()", "-fn deleted_b()"],
        }];

        // Two adjacent ranges that both "include" the deleted lines
        // (deletion hunk straddles their boundary).
        let changed_ranges = vec![
            super::super::types::ChangedNodeRange {
                start: 1,
                end: 1,
                parent_context: None,
            },
            super::super::types::ChangedNodeRange {
                start: 2,
                end: 2,
                parent_context: None,
            },
        ];

        let mut output = String::new();
        render_changed_only(&mut output, &changed_ranges, &hunks, &source_lines, 1);

        // Each deleted line must appear at most once.
        let del_a_count = output.lines().filter(|l| l.contains("deleted_a")).count();
        assert!(
            del_a_count <= 1,
            "fn deleted_a must appear at most once; got {del_a_count}:\n{output}"
        );

        let del_b_count = output.lines().filter(|l| l.contains("deleted_b")).count();
        assert!(
            del_b_count <= 1,
            "fn deleted_b must appear at most once; got {del_b_count}:\n{output}"
        );
    }

    // ========================================================================
    // render_default_scoped unit tests (F1 — hunk-scoped ADR-001 rendering)
    // ========================================================================

    /// Default mode: a small change inside a large node emits breadcrumb + hunk
    /// lines only — NOT the entire node body.  This is the core AC-F1.2 check:
    /// the old render_changed_only → render_node_with_hunks path would emit every
    /// source line from node_start to node_end; the new path emits only the
    /// breadcrumb + patch lines.
    #[test]
    fn test_render_default_scoped_breadcrumb_and_hunk_only_not_full_body() {
        // A 10-line "function" with a change at line 5.
        // The hunk covers only lines 4-6.  The old path would emit lines 1-10;
        // the new path emits only the breadcrumb (line 1) + hunk lines (4-6).
        let source_lines: Vec<&str> = vec![
            "fn big_function() {", // line 1 — breadcrumb
            "    let a = 1;",      // line 2
            "    let b = 2;",      // line 3
            "    let c = 3;",      // line 4 (context in hunk)
            "    let d = 4;",      // line 5 (changed)
            "    let e = 5;",      // line 6 (context in hunk)
            "    let f = 6;",      // line 7
            "    let g = 7;",      // line 8
            "    let h = 8;",      // line 9
            "}",                   // line 10
        ];

        // Hunk: changes line 5 (one line), with one context line above and below.
        let hunks = vec![DiffHunk {
            old_start: 4,
            old_count: 3,
            new_start: 4,
            new_count: 3,
            patch_lines: vec![
                " let c = 3;",
                "-    let d = 4;",
                "+    let d = 99;",
                " let e = 5;",
            ],
        }];

        // Single range covering the whole function (lines 1-10).
        let changed_ranges = vec![super::super::types::ChangedNodeRange {
            start: 1,
            end: 10,
            parent_context: None,
        }];

        let markers = HunkLineMarkers::from_hunks(&hunks);
        let inputs = EmitInputs::new(&hunks, &source_lines, 2, &markers);
        let mut state = RenderState::default();
        let mut output = String::new();
        render_default_scoped(&mut output, &changed_ranges, &inputs, &mut state);

        // Breadcrumb must appear.
        assert!(
            output.contains("big_function"),
            "breadcrumb line must appear in output:\n{output}"
        );

        // Changed line must appear.
        assert!(
            output.contains("let d = 99;"),
            "changed line must appear in output:\n{output}"
        );

        // Lines outside the hunk window must NOT appear (no full-body bloat).
        assert!(
            !output.contains("let g = 7;"),
            "line 8 (outside hunk) must NOT appear — no full-body emission:\n{output}"
        );
        assert!(
            !output.contains("let h = 8;"),
            "line 9 (outside hunk) must NOT appear — no full-body emission:\n{output}"
        );

        // Output must be shorter than a full 10-line node body.
        let line_count = output.lines().count();
        assert!(
            line_count < 10,
            "hunk-scoped output ({line_count} lines) must be shorter than full 10-line node body:\n{output}"
        );
    }

    /// Orphan hunk (EOF deletion): a hunk whose changed lines fall outside all
    /// AST node ranges must be rendered as raw hunk lines (AC-F1.3 / plan point 3).
    ///
    /// Without this, the old path silently dropped orphan hunks, making skim output
    /// smaller than raw in a way that the ADR-001 guardrail could never catch (the
    /// guardrail only fires when skim > raw, not when content is silently omitted).
    #[test]
    fn test_render_default_scoped_orphan_eof_deletion_rendered() {
        // Source (new file after deletion): just the function, trailing blank removed.
        let source_lines: Vec<&str> = vec!["fn foo() {}", ""];

        // Orphan hunk: removes a trailing blank line at old line 3.
        // new_start = 3, new_count = 0 (pure deletion past end of file).
        let hunks = vec![DiffHunk {
            old_start: 3,
            old_count: 1,
            new_start: 3,
            new_count: 0,
            patch_lines: vec!["-"], // deleted blank line
        }];

        // No changed ranges — the deletion at old line 3 has no AST node.
        let changed_ranges: Vec<super::super::types::ChangedNodeRange> = vec![];

        let markers = HunkLineMarkers::from_hunks(&hunks);
        let inputs = EmitInputs::new(&hunks, &source_lines, 1, &markers);
        let mut state = RenderState::default();
        let mut output = String::new();
        render_default_scoped(&mut output, &changed_ranges, &inputs, &mut state);

        // The orphan deletion line must appear in the output.
        assert!(
            !output.is_empty(),
            "EOF deletion orphan hunk must not be silently dropped:\n'{output}'"
        );
        // The deleted blank line marker must appear.
        assert!(
            output.contains('-'),
            "orphan deletion line must contain '-' prefix:\n{output}"
        );
    }

    /// Regression (#317 / ADR-003): edit-inside-last-node + trailing deletions
    /// past the node's closing brace, in ONE hunk, must NOT drop the deletions.
    ///
    /// The prior per-hunk `hunk_attributed` boolean marked the whole hunk done
    /// after clipping the patch to `fn a`'s `[1,3]` boundary, so the orphan loop
    /// skipped it and the two blank-line deletions at old lines 4-5 (OUTSIDE
    /// every AST node) vanished from the output. The ADR-001 net-savings guard
    /// cannot catch this — it fires only on OVER-emission, never UNDER-emission.
    #[test]
    fn test_render_default_scoped_trailing_deletion_in_shared_hunk_not_dropped() {
        // New file after patch:
        //   line 1: fn a() {
        //   line 2:     return 42;   (edited — was `return 0;`)
        //   line 3: }
        //   line 4: fn b() {}
        // Old file had two blank lines between `}` (old line 3) and `fn b`
        // (old line 6); they are deleted in the SAME hunk that edits line 2.
        let source_lines: Vec<&str> = vec!["fn a() {", "    return 42;", "}", "fn b() {}"];

        let hunks = vec![DiffHunk {
            old_start: 1,
            old_count: 6,
            new_start: 1,
            new_count: 4,
            patch_lines: vec![
                " fn a() {",
                "-    return 0;",
                "+    return 42;",
                " }",
                "-", // deleted blank (old line 4) — OUTSIDE fn a's [1,3]
                "-", // deleted blank (old line 5) — OUTSIDE fn a's [1,3]
                " fn b() {}",
            ],
        }];

        // Only `fn a` (new lines 1-3) is a changed node. The two blank-line
        // deletions map (via build_changed_lines) to new-file position 4, which
        // matches no AST node → orphan lines inside an otherwise-attributed hunk.
        let changed_ranges = vec![super::super::types::ChangedNodeRange {
            start: 1,
            end: 3,
            parent_context: None,
        }];

        let markers = HunkLineMarkers::from_hunks(&hunks);
        let inputs = EmitInputs::new(&hunks, &source_lines, 1, &markers);
        let mut state = RenderState::default();
        let mut output = String::new();
        render_default_scoped(&mut output, &changed_ranges, &inputs, &mut state);

        // The edited in-node line must appear (attributed emission).
        assert!(
            output.contains("return 42;"),
            "edited in-node line must appear:\n{output}"
        );

        // Every `-` line from the raw hunk must survive: the edited-line deletion
        // PLUS both trailing blank-line deletions = 3 removed lines. The old code
        // dropped the two trailing ones (only 1 `-` line survived).
        let removed_lines = output.lines().filter(|l| l.starts_with('-')).count();
        assert!(
            removed_lines >= 3,
            "expected the in-node deletion + 2 trailing blank deletions (>=3 '-' lines); \
             got {removed_lines}:\n{output}"
        );

        // The trailing deletions carry old-file line numbers 4 and 5.
        assert!(
            output.contains("-4"),
            "trailing blank deletion at old line 4 must appear:\n{output}"
        );
        assert!(
            output.contains("-5"),
            "trailing blank deletion at old line 5 must appear:\n{output}"
        );
    }

    /// Multiple hunks in one node → ONE breadcrumb, all hunks emitted.
    /// This is the line-doubling regression guard (AC-F1.3).
    #[test]
    fn test_render_default_scoped_multiple_hunks_one_breadcrumb() {
        // A class with three methods; two hunks changing methods 1 and 3.
        let source_lines: Vec<&str> = vec![
            "class MyClass {",    // line 1 (breadcrumb for all children)
            "  fn method_a() {}", // line 2 (changed by hunk 1)
            "  fn method_b() {}", // line 3
            "  fn method_c() {}", // line 4 (changed by hunk 2)
            "}",                  // line 5
        ];

        let hunks = vec![
            DiffHunk {
                old_start: 2,
                old_count: 1,
                new_start: 2,
                new_count: 1,
                patch_lines: vec!["-  fn method_a() {}", "+  fn method_a() -> u32 {}"],
            },
            DiffHunk {
                old_start: 4,
                old_count: 1,
                new_start: 4,
                new_count: 1,
                patch_lines: vec!["-  fn method_c() {}", "+  fn method_c() -> u32 {}"],
            },
        ];

        // Two child ranges, same parent container (lines 1-5).
        let changed_ranges = vec![
            super::super::types::ChangedNodeRange {
                start: 2,
                end: 2,
                parent_context: Some(super::super::types::ParentContext {
                    header_line: 1,
                    close_line: 5,
                }),
            },
            super::super::types::ChangedNodeRange {
                start: 4,
                end: 4,
                parent_context: Some(super::super::types::ParentContext {
                    header_line: 1,
                    close_line: 5,
                }),
            },
        ];

        let markers = HunkLineMarkers::from_hunks(&hunks);
        let inputs = EmitInputs::new(&hunks, &source_lines, 1, &markers);
        let mut state = RenderState::default();
        let mut output = String::new();
        render_default_scoped(&mut output, &changed_ranges, &inputs, &mut state);

        // Breadcrumb must appear exactly once.
        let breadcrumb_count = output.lines().filter(|l| l.contains("MyClass {")).count();
        assert_eq!(
            breadcrumb_count, 1,
            "container breadcrumb must appear exactly once; got {breadcrumb_count}:\n{output}"
        );

        // Both changed methods must appear in output.
        assert!(
            output.contains("method_a() -> u32"),
            "first hunk change must appear:\n{output}"
        );
        assert!(
            output.contains("method_c() -> u32"),
            "second hunk change must appear:\n{output}"
        );
    }

    /// No-newline marker (`\ No newline at end of file`) survives round-trip.
    /// This is the AC-F1.3 edge case for special patch lines.
    #[test]
    fn test_render_default_scoped_no_newline_marker_preserved() {
        let source_lines: Vec<&str> = vec!["fn foo() {}", "    return 1;"];

        let hunks = vec![DiffHunk {
            old_start: 2,
            old_count: 1,
            new_start: 2,
            new_count: 1,
            patch_lines: vec![
                "-    return 1;",
                r"\ No newline at end of file",
                "+    return 42;",
                r"\ No newline at end of file",
            ],
        }];

        let changed_ranges = vec![super::super::types::ChangedNodeRange {
            start: 1,
            end: 2,
            parent_context: None,
        }];

        let markers = HunkLineMarkers::from_hunks(&hunks);
        let inputs = EmitInputs::new(&hunks, &source_lines, 1, &markers);
        let mut state = RenderState::default();
        let mut output = String::new();
        render_default_scoped(&mut output, &changed_ranges, &inputs, &mut state);

        // The no-newline markers must appear verbatim.
        assert!(
            output.contains("No newline at end of file"),
            "no-newline marker must be preserved:\n{output}"
        );

        // The changed content must also appear.
        assert!(
            output.contains("return 42;"),
            "changed content must appear:\n{output}"
        );
    }

    /// Non-overlapping multi-node diff: each node has its OWN hunk.
    ///
    /// Note: render_changed_only (default mode) does NOT emit unchanged context lines
    /// between independent changed ranges — only lines within each range's
    /// [effective_start, effective_end] bounds are rendered.  So `fn beta()` (between
    /// range 1 and range 3) does NOT appear in the output; this is correct behaviour.
    /// The test validates that changed lines from BOTH ranges appear, proving the cursor
    /// does not incorrectly suppress lines from the second independent range.
    #[test]
    fn test_dedup_non_overlapping_ranges_each_emitted_once() {
        // Two independent ranges with their own hunks (no shared lines).
        let source_lines: Vec<&str> = vec![
            "fn alpha()",
            "fn beta()", // unchanged — NOT rendered in default mode
            "fn gamma()",
            "fn delta()",
        ];

        let hunks = vec![
            DiffHunk {
                old_start: 1,
                old_count: 1,
                new_start: 1,
                new_count: 1,
                patch_lines: vec!["-fn alpha_old()", "+fn alpha()"],
            },
            DiffHunk {
                old_start: 3,
                old_count: 1,
                new_start: 3,
                new_count: 1,
                patch_lines: vec!["-fn gamma_old()", "+fn gamma()"],
            },
        ];

        let changed_ranges = vec![
            super::super::types::ChangedNodeRange {
                start: 1,
                end: 1,
                parent_context: None,
            },
            super::super::types::ChangedNodeRange {
                start: 3,
                end: 3,
                parent_context: None,
            },
        ];

        let mut output = String::new();
        render_changed_only(&mut output, &changed_ranges, &hunks, &source_lines, 1);

        // Both changed lines must appear exactly once (not suppressed by cursor).
        let alpha_count = output.lines().filter(|l| l.contains("fn alpha()")).count();
        assert_eq!(
            alpha_count, 1,
            "fn alpha must appear exactly once; got {alpha_count}:\n{output}"
        );

        let gamma_count = output.lines().filter(|l| l.contains("fn gamma()")).count();
        assert_eq!(
            gamma_count, 1,
            "fn gamma must appear exactly once; got {gamma_count}:\n{output}"
        );

        // Verify that old (removed) lines from each hunk are NOT duplicated.
        let alpha_old_count = output
            .lines()
            .filter(|l| l.contains("fn alpha_old"))
            .count();
        assert!(
            alpha_old_count <= 1,
            "fn alpha_old must appear at most once; got {alpha_old_count}:\n{output}"
        );

        let gamma_old_count = output
            .lines()
            .filter(|l| l.contains("fn gamma_old"))
            .count();
        assert!(
            gamma_old_count <= 1,
            "fn gamma_old must appear at most once; got {gamma_old_count}:\n{output}"
        );
    }

    // =========================================================================
    // source_matches_diff unit tests
    // =========================================================================

    fn make_hunk<'a>(new_start: usize, patch_lines: Vec<&'a str>) -> DiffHunk<'a> {
        DiffHunk {
            old_start: 1,
            old_count: 1,
            new_start,
            new_count: patch_lines.len(),
            patch_lines,
        }
    }

    #[test]
    fn test_source_matches_diff_matching_returns_true() {
        // Source has three lines; patch has one context + one added line.
        let source_lines = vec!["fn original() {}", "fn added_in_commit2() {}", "};"];
        let hunk = make_hunk(1, vec![" fn original() {}", "+fn added_in_commit2() {}"]);
        assert!(
            source_matches_diff(&source_lines, &[hunk]),
            "matching source and diff should return true"
        );
    }

    #[test]
    fn test_source_matches_diff_mismatch_returns_false() {
        // Source has working-tree content that differs from the diff.
        let source_lines = vec!["fn completely_different() {}"];
        let hunk = make_hunk(1, vec![" fn original() {}"]);
        assert!(
            !source_matches_diff(&source_lines, &[hunk]),
            "mismatched source and diff should return false"
        );
    }

    #[test]
    fn test_source_matches_diff_pure_deletion_returns_true() {
        // A pure-deletion hunk: new_start = 0, only '-' lines; nothing to check.
        let source_lines: Vec<&str> = vec![];
        let hunk = make_hunk(0, vec!["-fn removed() {}"]);
        assert!(
            source_matches_diff(&source_lines, &[hunk]),
            "pure-deletion hunk (new_start=0) should return true"
        );
    }

    #[test]
    fn test_source_matches_diff_new_start_one_context_line() {
        // new_start = 1: first context line should match source_lines[0].
        let source_lines = vec!["fn alpha() {}"];
        let hunk = make_hunk(1, vec![" fn alpha() {}"]);
        assert!(
            source_matches_diff(&source_lines, &[hunk]),
            "context line at new_start=1 should match source_lines[0]"
        );
    }

    #[test]
    fn test_source_matches_diff_past_end_of_source_returns_false() {
        // Patch claims a context line at line 5 but source only has 2 lines.
        let source_lines = vec!["line1", "line2"];
        let hunk = make_hunk(5, vec![" line5"]);
        assert!(
            !source_matches_diff(&source_lines, &[hunk]),
            "context line past end of source should return false"
        );
    }

    #[test]
    fn test_source_matches_diff_no_newline_marker_skipped() {
        // '\ No newline at end of file' must not advance new_line or fail.
        let source_lines = vec!["fn last() {}"]; // one line, no trailing newline
        let hunk = make_hunk(1, vec![" fn last() {}", r"\ No newline at end of file"]);
        assert!(
            source_matches_diff(&source_lines, &[hunk]),
            r"'\ No newline at end of file' marker must be skipped"
        );
    }

    // =========================================================================
    // C1a — render_default_scoped single-positional-walk tests (TDD: failing first)
    //
    // These tests document the THREE bugs in the old two-pass design:
    //   Bug 1 (Duplicate context line): breadcrumb didn't update EmittedCursor,
    //     so the hunk's context line at `breadcrumb_line` was emitted twice.
    //   Bug 2 (Out-of-order orphan): the trailing orphan pass ran AFTER the
    //     range loop, placing orphan hunk lines AFTER later range hunk lines.
    //   Bug 3 (+ line as context): a `+` line at `breadcrumb_line` was emitted
    //     as context (` ` format) by the breadcrumb before the hunk could emit
    //     it correctly as `+`.
    //
    // All three are fixed by the single positional walk: breadcrumbs are only
    // emitted when `breadcrumb_line < hunk.new_start`, so the hunk never revisits
    // the breadcrumb position, and orphan hunks are emitted in document order.
    // =========================================================================

    /// Canonical C1a reproducer — doc-comment insertion above a function.
    ///
    /// When a doc comment is inserted at line 1 and fn compute() is at line 2,
    /// the hunk covers lines 1-4 (new_start=1). The range for fn compute is [2,4]
    /// giving breadcrumb_line=2.
    ///
    /// OLD code: breadcrumb emits " 2 fn compute() {" (cursor NOT updated), then
    /// `emit_hunk_patch_lines_clipped` clips to [2,4] and re-emits " 2 fn compute()"
    /// because cursor.last_new==0. Result: fn compute() appears TWICE.
    ///
    /// NEW code: breadcrumb_line=2, hunk.new_start=1 → 2 < 1 is false → no
    /// separate breadcrumb; hunk emits line 2 as context exactly once.
    #[test]
    fn test_c1a_doc_comment_insertion_no_duplicate_context_line() {
        let source_lines: Vec<&str> = vec![
            "/// New doc comment", // line 1 (added by hunk)
            "fn compute() {",      // line 2 — the AST range starts here
            "    42",              // line 3
            "}",                   // line 4
        ];

        // Hunk: insert doc comment at line 1; fn compute context starts at 1.
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_count: 3,
            new_start: 1,
            new_count: 4,
            patch_lines: vec!["+/// New doc comment", " fn compute() {", "     42", " }"],
        }];

        // Range covers fn compute (line 2 through 4); breadcrumb_line = 2.
        let changed_ranges = vec![super::super::types::ChangedNodeRange {
            start: 2,
            end: 4,
            parent_context: None,
        }];

        let markers = HunkLineMarkers::from_hunks(&hunks);
        let inputs = EmitInputs::new(&hunks, &source_lines, 1, &markers);
        let mut state = RenderState::default();
        let mut output = String::new();
        render_default_scoped(&mut output, &changed_ranges, &inputs, &mut state);

        // fn compute must appear exactly once (Bug 1: old code emits it twice).
        let fn_count = output.lines().filter(|l| l.contains("fn compute")).count();
        assert_eq!(
            fn_count, 1,
            "fn compute() must appear exactly once (Bug 1 — breadcrumb duplicate);\
             \ngot {fn_count} occurrences in:\n{output}"
        );

        // Doc comment must appear as ADDED (`+` prefix), not duplicated.
        let added_doc = output
            .lines()
            .filter(|l| l.starts_with('+') && l.contains("doc comment"))
            .count();
        assert_eq!(
            added_doc, 1,
            "doc comment must appear exactly once as added (+):\n{output}"
        );

        // Verifier must accept the new render.
        assert!(
            verify_ast_render(&state.emissions, &hunks).is_ok(),
            "render must pass verifier:\n{output}"
        );
    }

    /// C1a Bug 2 — orphan hunk out-of-order (the trailing orphan pass ran AFTER
    /// the range loop, reversing document order between an orphan and a range hunk).
    ///
    /// Orphan hunk: comment update at lines 2-3 (no AST node).
    /// Range hunk: function body change at lines 8-10.
    ///
    /// OLD code: range loop emits lines 8-10 first; orphan pass appends lines 2-3
    /// AFTER → output is backward (lines 8-10 before lines 2-3).
    ///
    /// NEW code: single hunk walk → H0 (lines 2-3) emitted first, H1 (lines 8-10)
    /// emitted second → correct document order.
    #[test]
    fn test_c1a_orphan_hunk_before_range_emitted_in_document_order() {
        // Ten-line file; only lines 8-10 are in the AST range.
        let source_lines: Vec<&str> = vec![
            "// file header", // line 1
            "// comment A",   // line 2
            "// comment B",   // line 3
            "",               // line 4
            "",               // line 5
            "",               // line 6
            "",               // line 7
            "fn foo() {",     // line 8
            "    let x = 0;", // line 9
            "}",              // line 10
        ];

        let hunks = vec![
            // H0: orphan edit of comment, lines 2-3 (no AST range covers this).
            DiffHunk {
                old_start: 2,
                old_count: 2,
                new_start: 2,
                new_count: 2,
                patch_lines: vec!["-// comment A", "+// comment A (updated)"],
            },
            // H1: edit inside fn foo, lines 8-10.
            DiffHunk {
                old_start: 8,
                old_count: 3,
                new_start: 8,
                new_count: 3,
                patch_lines: vec![" fn foo() {", "-    let x = 0;", "+    let x = 1;"],
            },
        ];

        // Only fn foo (lines 8-10) is in the changed AST range.
        let changed_ranges = vec![super::super::types::ChangedNodeRange {
            start: 8,
            end: 10,
            parent_context: None,
        }];

        let markers = HunkLineMarkers::from_hunks(&hunks);
        let inputs = EmitInputs::new(&hunks, &source_lines, 2, &markers);
        let mut state = RenderState::default();
        let mut output = String::new();
        render_default_scoped(&mut output, &changed_ranges, &inputs, &mut state);

        // The orphan comment update must appear BEFORE fn foo (Bug 2 check).
        let comment_pos = output
            .find("comment A (updated)")
            .expect("updated comment must appear in output");
        let fn_pos = output
            .find("fn foo()")
            .expect("fn foo must appear in output");
        assert!(
            comment_pos < fn_pos,
            "Bug 2: orphan hunk (lines 2-3) must precede range hunk (lines 8-10)\
             \nin output:\n{output}"
        );

        // Both changes must be present.
        assert!(
            output.contains("let x = 1;"),
            "fn foo body change must appear:\n{output}"
        );

        // Verifier must accept the render.
        assert!(
            verify_ast_render(&state.emissions, &hunks).is_ok(),
            "render must pass verifier:\n{output}"
        );
    }

    /// C1a Bug 3 — a `+` (added) line at `breadcrumb_line` was emitted in
    /// CONTEXT format (` ` prefix) by the breadcrumb, then the hunk tried to
    /// emit it again as `+` — resulting in the wrong prefix surviving.
    ///
    /// Scenario: a new function `fn inserted()` is added at line 1.  The AST
    /// range is [1,3].  breadcrumb_line = 1 = hunk.new_start.
    ///
    /// OLD code: breadcrumb emits " 1 fn inserted()" (context, WRONG); cursor NOT
    /// updated; hunk clips to [1,3] and re-emits "+1 fn inserted()" (added, right)
    /// — so the function appears as BOTH context and added.
    ///
    /// NEW code: breadcrumb_line=1, hunk.new_start=1 → 1 < 1 is false → no separate
    /// breadcrumb; hunk emits "+1 fn inserted()" exactly once (correct).
    #[test]
    fn test_c1a_added_line_at_range_start_emitted_as_added_not_context() {
        let source_lines: Vec<&str> = vec![
            "fn inserted() {", // line 1 — newly added
            "    42",          // line 2
            "}",               // line 3
        ];

        // All lines are `+` (newly added function).
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_count: 0,
            new_start: 1,
            new_count: 3,
            patch_lines: vec!["+fn inserted() {", "+    42", "+}"],
        }];

        let changed_ranges = vec![super::super::types::ChangedNodeRange {
            start: 1,
            end: 3,
            parent_context: None,
        }];

        let markers = HunkLineMarkers::from_hunks(&hunks);
        let inputs = EmitInputs::new(&hunks, &source_lines, 1, &markers);
        let mut state = RenderState::default();
        let mut output = String::new();
        render_default_scoped(&mut output, &changed_ranges, &inputs, &mut state);

        // fn inserted must appear exactly once (no breadcrumb + hunk double-emission).
        let count = output.lines().filter(|l| l.contains("fn inserted")).count();
        assert_eq!(
            count, 1,
            "fn inserted() must appear exactly once (Bug 3 — + line as context):\n{output}"
        );

        // It must appear as ADDED (`+` prefix), NOT as context (` ` prefix).
        let as_added = output
            .lines()
            .filter(|l| l.starts_with('+') && l.contains("fn inserted"))
            .count();
        assert_eq!(
            as_added, 1,
            "fn inserted() must appear with `+` prefix, not as context:\n{output}"
        );

        let as_context = output
            .lines()
            .filter(|l| l.starts_with(' ') && l.contains("fn inserted"))
            .count();
        assert_eq!(
            as_context, 0,
            "fn inserted() must NOT appear as context (` ` prefix):\n{output}"
        );

        // Verifier must accept the render.
        assert!(
            verify_ast_render(&state.emissions, &hunks).is_ok(),
            "render must pass verifier:\n{output}"
        );
    }

    // =========================================================================
    // C1b — verify_ast_render unit tests (PF-025: test against known-corrupt input)
    //
    // PF-025 lesson: "Any guard you write must be tested against known-corrupt
    // input before it is trusted." Each test below constructs a deliberately
    // corrupt emission trace and proves the verifier rejects it.
    // =========================================================================

    /// Verifier rejects a trace with a duplicate New-axis line number.
    #[test]
    fn test_c1b_verifier_rejects_duplicate_new_axis_line() {
        // Deliberately corrupt: line 5 appears twice on the New axis.
        let emissions: Vec<Emission> = vec![
            (Axis::New, 3, Marker::Context),
            (Axis::New, 4, Marker::Context),
            (Axis::New, 5, Marker::Context), // first occurrence
            (Axis::New, 5, Marker::Context), // DUPLICATE — corrupt
            (Axis::New, 6, Marker::Context),
        ];
        assert!(
            matches!(
                verify_ast_render(&emissions, &[]),
                Err(VerifyFailure::DuplicateLine { .. })
            ),
            "duplicate New-axis line must be rejected (per-axis uniqueness invariant)"
        );
    }

    /// Verifier rejects a trace with a duplicate Old-axis line number.
    #[test]
    fn test_c1b_verifier_rejects_duplicate_old_axis_line() {
        let hunks = vec![DiffHunk {
            old_start: 10,
            old_count: 1,
            new_start: 10,
            new_count: 0,
            patch_lines: vec!["-gone"],
        }];
        let emissions: Vec<Emission> = vec![
            (Axis::Old, 10, Marker::Removed),
            (Axis::Old, 10, Marker::Removed), // DUPLICATE — corrupt
        ];
        assert!(
            matches!(
                verify_ast_render(&emissions, &hunks),
                Err(VerifyFailure::DuplicateLine { .. })
            ),
            "duplicate Old-axis line must be rejected"
        );
    }

    /// Verifier rejects a trace where New-axis numbers jump backward.
    #[test]
    fn test_c1b_verifier_rejects_backward_jump_on_new_axis() {
        // Deliberately corrupt: line 7 appears after line 15 → backward jump.
        let emissions: Vec<Emission> = vec![
            (Axis::New, 5, Marker::Context),
            (Axis::New, 15, Marker::Context),
            (Axis::New, 7, Marker::Context), // BACKWARD JUMP — corrupt
        ];
        assert!(
            matches!(
                verify_ast_render(&emissions, &[]),
                Err(VerifyFailure::BackwardJump { .. })
            ),
            "backward jump on New axis must be rejected (monotonicity invariant)"
        );
    }

    /// Verifier rejects a trace where a `+` hunk line is absent.
    ///
    /// This is the PF-025 scenario: a verifier that only checks subsequence
    /// order passes while a `+` line is silently missing from the render. This
    /// verifier catches it via the coverage check.
    #[test]
    fn test_c1b_verifier_rejects_dropped_added_line() {
        let hunks = vec![DiffHunk {
            old_start: 4,
            old_count: 1,
            new_start: 4,
            new_count: 2,
            patch_lines: vec![" context4", "+added5", " context6"],
        }];
        // Trace: context4 and context6 emitted; `+added5` at new_line=5 is MISSING.
        let emissions: Vec<Emission> = vec![
            (Axis::New, 4, Marker::Context), // context4
            // Missing: (Axis::New, 5, Marker::Added) for +added5 — deliberately corrupt
            (Axis::New, 6, Marker::Context), // context6
        ];
        assert!(
            matches!(
                verify_ast_render(&emissions, &hunks),
                Err(VerifyFailure::UncoveredChange { .. })
            ),
            "missing `+` line must be detected by coverage invariant (PF-025)"
        );
    }

    /// Verifier rejects a trace where a `-` hunk line is absent.
    #[test]
    fn test_c1b_verifier_rejects_dropped_removed_line() {
        let hunks = vec![DiffHunk {
            old_start: 4,
            old_count: 2,
            new_start: 4,
            new_count: 1,
            patch_lines: vec!["-removed_a", "-removed_b", "+added"],
        }];
        // Trace: only the added line; both `-` lines missing — deliberately corrupt.
        let emissions: Vec<Emission> = vec![(Axis::New, 4, Marker::Added)];
        assert!(
            matches!(
                verify_ast_render(&emissions, &hunks),
                Err(VerifyFailure::UncoveredChange { .. })
            ),
            "missing `-` line must be detected by coverage invariant"
        );
    }

    /// Verifier accepts a valid render (positive control — PF-025 requires
    /// proving the guard does not false-positive on good data).
    #[test]
    fn test_c1b_verifier_accepts_correct_render() {
        let hunks = vec![DiffHunk {
            old_start: 4,
            old_count: 2,
            new_start: 4,
            new_count: 2,
            patch_lines: vec![" context4", "-removed5", "+added5", " context6"],
        }];
        // Correct trace: context4 (New,4), removed5 (Old,5), added5 (New,5), context6 (New,6).
        let emissions: Vec<Emission> = vec![
            (Axis::New, 4, Marker::Context),
            (Axis::Old, 5, Marker::Removed),
            (Axis::New, 5, Marker::Added),
            (Axis::New, 6, Marker::Context),
        ];
        assert!(
            verify_ast_render(&emissions, &hunks).is_ok(),
            "correct render must be accepted by verifier"
        );
    }

    /// Verifier trivially accepts empty emissions with no hunks.
    #[test]
    fn test_c1b_verifier_accepts_empty_trace_no_hunks() {
        assert!(
            verify_ast_render(&[], &[]).is_ok(),
            "empty trace with no hunks must pass all invariants vacuously"
        );
    }

    /// ADR-011 class-2 pin: the verifier fallback is a no-loss raw-fallback and
    /// must be gated behind `SKIM_DEBUG`. This test verifies the four verifier
    /// invariants independently, proving each alone can trigger rejection —
    /// which is what would cause `try_ast_render` to call `crate::debug_log!`
    /// (the class-2 gated banner) and return `None` for the raw-fallback.
    ///
    /// The `crate::debug_log!` macro writes zero bytes to stderr without
    /// `SKIM_DEBUG=1`, satisfying ADR-011 class-2 for no-loss raw fallbacks.
    #[test]
    fn test_c1b_each_invariant_triggers_rejection_independently() {
        // Invariant 1: uniqueness — duplicate triggers rejection.
        let dup = vec![
            (Axis::New, 1, Marker::Context),
            (Axis::New, 1, Marker::Context),
        ];
        assert!(
            matches!(
                verify_ast_render(&dup, &[]),
                Err(VerifyFailure::DuplicateLine { .. })
            ),
            "uniqueness invariant must reject duplicate"
        );

        // Invariant 2: monotonicity — backward jump triggers rejection.
        let backward = vec![
            (Axis::New, 10, Marker::Context),
            (Axis::New, 5, Marker::Context),
        ];
        assert!(
            matches!(
                verify_ast_render(&backward, &[]),
                Err(VerifyFailure::BackwardJump { .. })
            ),
            "monotonicity invariant must reject backward jump"
        );

        // Invariant 3: coverage — missing `+` triggers rejection.
        let hunk = DiffHunk {
            old_start: 1,
            old_count: 0,
            new_start: 1,
            new_count: 1,
            patch_lines: vec!["+added"],
        };
        let missing_plus: Vec<Emission> = vec![]; // +added at new_line=1 missing
        assert!(
            matches!(
                verify_ast_render(&missing_plus, std::slice::from_ref(&hunk)),
                Err(VerifyFailure::UncoveredChange { .. })
            ),
            "coverage invariant must reject missing `+` line"
        );

        // Invariant 4: marker fidelity — right number, wrong prefix.
        let wrong_marker = vec![(Axis::New, 1, Marker::Context)];
        assert!(
            matches!(
                verify_ast_render(&wrong_marker, &[hunk]),
                Err(VerifyFailure::MarkerMismatch { .. })
            ),
            "marker-fidelity invariant must reject an added line rendered as context"
        );
    }

    // =========================================================================
    // C1d — marker-fidelity unit tests (the dominant corruption class)
    //
    // Every `added-as-context` case measured on the corpus emitted the CORRECT
    // line number with an unconditional context prefix, so the uniqueness,
    // monotonicity and coverage checks all pass on it.  Each test below proves
    // that: it first asserts checks 1-3 accept the corrupt trace (by showing the
    // marker-corrected trace passes and the numbers are identical), then that
    // check 4 rejects it.
    // =========================================================================

    /// Added line rendered as pre-existing context — the `92417dc9` shape,
    /// where a wholly new `struct` and `impl` block read as "only the derive
    /// was added".
    #[test]
    fn test_c1d_rejects_added_line_rendered_as_context() {
        let hunks = vec![DiffHunk {
            old_start: 6,
            old_count: 0,
            new_start: 7,
            new_count: 2,
            patch_lines: vec!["+#[derive(Default)]", "+pub struct Options {"],
        }];

        // The marker-correct trace over the SAME line numbers is accepted —
        // which is exactly why checks 1-3 cannot see the corruption.
        let honest: Vec<Emission> =
            vec![(Axis::New, 7, Marker::Added), (Axis::New, 8, Marker::Added)];
        assert!(
            verify_ast_render(&honest, &hunks).is_ok(),
            "positive control: the honest render must be accepted"
        );

        // Same numbers, same order, no duplicates, full coverage — only the
        // marker on line 8 is wrong.  Checks 1-3 are blind to this.
        let corrupt: Vec<Emission> = vec![
            (Axis::New, 7, Marker::Added),
            (Axis::New, 8, Marker::Context), // CORRUPT: line 8 is a `+` line
        ];
        assert!(
            matches!(
                verify_ast_render(&corrupt, &hunks),
                Err(VerifyFailure::MarkerMismatch { .. })
            ),
            "an added line rendered as context must be rejected (C1d marker fidelity)"
        );
    }

    /// Context line rendered as added — the mirror-image lie, which would tell
    /// the reader that untouched code is new.
    #[test]
    fn test_c1d_rejects_context_line_rendered_as_added() {
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_count: 2,
            new_start: 1,
            new_count: 3,
            patch_lines: vec![" fn keep() {", "+    added();", " }"],
        }];
        let corrupt: Vec<Emission> = vec![
            (Axis::New, 1, Marker::Added), // CORRUPT: line 1 is context
            (Axis::New, 2, Marker::Added),
            (Axis::New, 3, Marker::Context),
        ];
        assert!(
            matches!(
                verify_ast_render(&corrupt, &hunks),
                Err(VerifyFailure::MarkerMismatch { .. })
            ),
            "a context line rendered as added must be rejected"
        );
    }

    /// Removed marker on a line the diff does not remove.
    #[test]
    fn test_c1d_rejects_removed_marker_on_non_removed_line() {
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_count: 2,
            new_start: 1,
            new_count: 1,
            patch_lines: vec![" keep", "-gone"],
        }];
        let corrupt: Vec<Emission> = vec![
            (Axis::New, 1, Marker::Context),
            (Axis::Old, 1, Marker::Removed), // CORRUPT: old line 1 is context, 2 is removed
            (Axis::Old, 2, Marker::Removed),
        ];
        assert!(
            matches!(
                verify_ast_render(&corrupt, &hunks),
                Err(VerifyFailure::MarkerMismatch { .. })
            ),
            "a removed marker on a context line must be rejected"
        );
    }

    /// Axis/marker pairings no emitter produces are rejected rather than
    /// silently accepted — an unexpected pairing means the emission bookkeeping
    /// itself drifted.
    #[test]
    fn test_c1d_rejects_impossible_axis_marker_pairings() {
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_count: 2,
            new_start: 1,
            new_count: 1,
            patch_lines: vec!["-gone", "-also_gone", "+new"],
        }];
        // The honest trace is the control: both `-` lines on the Old axis, the
        // `+` line on the New axis.  Each corrupt variant below swaps in one
        // impossible pairing at a line number the honest trace does not already
        // hold, so uniqueness and coverage stay satisfied and only the pairing
        // check can fire.
        let honest: Vec<Emission> = vec![
            (Axis::Old, 1, Marker::Removed),
            (Axis::Old, 2, Marker::Removed),
            (Axis::New, 1, Marker::Added),
        ];
        assert!(
            verify_ast_render(&honest, &hunks).is_ok(),
            "positive control: the honest trace must be accepted"
        );

        for (index, bad) in [
            (Axis::Old, 2usize, Marker::Added),
            (Axis::Old, 2usize, Marker::Context),
            (Axis::New, 1usize, Marker::Removed),
        ]
        .into_iter()
        .enumerate()
        {
            let mut corrupt = honest.clone();
            // Replace the honest emission that shares the bad one's axis+line.
            let slot = if bad.0 == Axis::New { 2 } else { 1 };
            corrupt[slot] = bad;
            assert!(
                matches!(
                    verify_ast_render(&corrupt, &hunks),
                    Err(VerifyFailure::MarkerMismatch { .. })
                ),
                "case {index}: impossible axis/marker pairing {bad:?} must be rejected"
            );
        }
    }

    /// The Default-mode breadcrumb is written in context format.  When the
    /// scheduled breadcrumb line is in fact a `+` line, marker fidelity is the
    /// only check that fires — proving C1e's extension of check 4 to Default
    /// mode is load-bearing and not decorative.
    #[test]
    fn test_c1d_default_mode_breadcrumb_on_added_line_is_rejected() {
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_count: 0,
            new_start: 1,
            new_count: 1,
            patch_lines: vec!["+impl Widget {"],
        }];
        // A breadcrumb emitted in context format for a line the diff added.
        let corrupt: Vec<Emission> = vec![(Axis::New, 1, Marker::Context)];

        // Checks 1-3 all pass: one emission, no duplicate, monotonic, and the
        // `+` line's NUMBER is present in the trace.
        let honest: Vec<Emission> = vec![(Axis::New, 1, Marker::Added)];
        assert!(
            verify_ast_render(&honest, &hunks).is_ok(),
            "positive control: the same number with the right marker is accepted"
        );
        assert!(
            matches!(
                verify_ast_render(&corrupt, &hunks),
                Err(VerifyFailure::MarkerMismatch { .. })
            ),
            "a Default-mode breadcrumb on an added line must be rejected"
        );
    }

    /// C1a + C1b integration: render_default_scoped single walk produces a trace
    /// that passes the verifier for a mixed hunk (orphan + in-node lines).
    ///
    /// Validates that the new code path threads emissions correctly and the
    /// verifier accepts the output (regression guard against accidentally
    /// breaking the emission tracking in the new single-walk implementation).
    #[test]
    fn test_c1a_c1b_integration_single_walk_passes_verifier() {
        // Simple function change: one context + one removed + one added + one context.
        let source_lines: Vec<&str> = vec!["fn foo() {", "    new_val", "}"];
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_count: 3,
            new_start: 1,
            new_count: 3,
            patch_lines: vec![" fn foo() {", "-    old_val", "+    new_val", " }"],
        }];
        let changed_ranges = vec![super::super::types::ChangedNodeRange {
            start: 1,
            end: 3,
            parent_context: None,
        }];

        let markers = HunkLineMarkers::from_hunks(&hunks);
        let inputs = EmitInputs::new(&hunks, &source_lines, 1, &markers);
        let mut state = RenderState::default();
        let mut output = String::new();
        render_default_scoped(&mut output, &changed_ranges, &inputs, &mut state);

        assert!(
            verify_ast_render(&state.emissions, &hunks).is_ok(),
            "single-walk render must pass all three verifier invariants;\
             \nemissions: {:?}\noutput:\n{output}",
            state.emissions,
        );

        // Sanity check on content.
        assert!(
            output.contains("new_val"),
            "changed content must appear:\n{output}"
        );
        assert!(
            output.contains("old_val"),
            "removed content must appear:\n{output}"
        );
    }
}
