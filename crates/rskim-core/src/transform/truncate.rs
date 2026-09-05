//! AST-aware truncation for --max-lines support
//!
//! ARCHITECTURE: Truncates transformed output to a maximum number of lines
//! using priority-based selection that respects AST node boundaries.
//! Types and signatures are kept over imports, which are kept over bodies.
//! Omission markers are inserted between gaps using language-appropriate comment syntax.

use crate::transform::literal_scan;
use crate::transform::utils::{ElidedSide, elision_marker_line, score_node_kind};
use crate::{Language, Result};
use std::borrow::Cow;
use std::ops::Range;

// ============================================================================
// NodeSpan: Maps transformed output line ranges to AST node kinds
// ============================================================================

/// A span mapping transformed output line ranges to their AST node kind
///
/// ARCHITECTURE: Built during transformation, consumed during truncation.
/// Each span represents a contiguous block of output lines that belong to
/// a single AST node (e.g., a function signature, a type definition).
#[derive(Debug, Clone)]
pub(crate) struct NodeSpan {
    /// Line range in the transformed output (0-indexed, exclusive end)
    pub transformed_range: Range<usize>,
    /// tree-sitter node kind string (for priority scoring)
    pub node_kind: &'static str,
}

impl NodeSpan {
    /// Create a new NodeSpan
    pub fn new(transformed_range: Range<usize>, node_kind: &'static str) -> Self {
        Self {
            transformed_range,
            node_kind,
        }
    }

    /// Number of lines this span covers
    fn line_count(&self) -> usize {
        self.transformed_range
            .end
            .saturating_sub(self.transformed_range.start)
    }
}

// ============================================================================
// Core truncation algorithm
// ============================================================================

/// Truncate transformed output to at most `max_lines` lines using AST-aware
/// priority scoring
///
/// Algorithm:
/// 1. If output fits within budget, return unchanged
/// 2. Score each span by node kind priority
/// 3. Sort by priority desc, then position asc (tie-break)
/// 4. Greedily select spans that fit within budget (minus marker overhead)
/// 5. Re-sort selected spans by position for reading order
/// 6. Build output with omission markers between gaps
///
/// # Arguments
/// * `text` - The transformed output text
/// * `spans` - NodeSpan mappings from the transform pipeline
/// * `language` - For language-appropriate omission marker syntax
/// * `max_lines` - Maximum number of output lines
/// * `hint` - Optional remedy clause appended to elision markers (B5 / ADR-011).
///   `None` keeps the library CLI-agnostic; the CLI passes `"SKIM_PASSTHROUGH=1
///   for full output"` via `TransformConfig::elision_hint`.
///
/// # Returns
/// Truncated text that never exceeds `max_lines` lines
pub(crate) fn truncate_to_lines<F: FnOnce() -> usize>(
    text: &str,
    spans: &[NodeSpan],
    language: Language,
    max_lines: usize,
    hint: Option<&str>,
    source_line_count_fn: F,
) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();

    // If output fits, return unchanged — source_line_count_fn is NOT called
    // (performance-3: the source pass is only needed for the marker text, which
    // is only emitted when truncation actually happens).
    if lines.len() <= max_lines {
        return Ok(text.to_string());
    }

    // Truncation is needed: evaluate the lazy count exactly once, now.
    let source_line_count = Some(source_line_count_fn());

    // If no spans provided, fall back to simple line truncation.
    // (The spans-empty path was previously the first check to avoid a redundant
    // lines().collect(); after the performance-3 reorder, the collect already
    // happened. The double traversal inside simple_line_truncate is acceptable
    // for this fallback path, which is reached only by unusual inputs.)
    if spans.is_empty() {
        return simple_line_truncate(text, language, max_lines, hint, source_line_count);
    }

    // Filter out empty spans and spans beyond the actual line count
    let valid_spans: Vec<&NodeSpan> = spans
        .iter()
        .filter(|s| s.line_count() > 0 && s.transformed_range.start < lines.len())
        .collect();

    if valid_spans.is_empty() {
        return simple_line_truncate(text, language, max_lines, hint, source_line_count);
    }

    // Fast-path: single span starting at line 0 — no inter-span gaps, no priority
    // re-ordering needed. Delegate to simple_line_truncate which gives N content
    // lines + 1 marker as line N+1 (E4). Handles pseudo, minimal, and full mode,
    // all of which emit NodeSpan::new(0..line_count, "source_file").
    // E3: pass source_line_count so the marker states omitted SOURCE lines.
    if valid_spans.len() == 1 && valid_spans[0].transformed_range.start == 0 {
        return simple_line_truncate(text, language, max_lines, hint, source_line_count);
    }

    // Score and sort spans: priority desc, position asc (tie-break)
    let mut scored: Vec<(u8, &NodeSpan)> = valid_spans
        .iter()
        .map(|span| (score_node_kind(span.node_kind), *span))
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| {
            a.1.transformed_range
                .start
                .cmp(&b.1.transformed_range.start)
        })
    });

    // Step 1: Greedy select by priority (content lines only, NO marker reserve)
    let mut selected: Vec<(u8, &NodeSpan)> = Vec::new();
    let mut lines_used: usize = 0;

    for &(priority, span) in &scored {
        let clamped_end = span.transformed_range.end.min(lines.len());
        let clamped_lines = clamped_end.saturating_sub(span.transformed_range.start);

        if clamped_lines == 0 {
            continue;
        }

        if lines_used + clamped_lines <= max_lines {
            selected.push((priority, span));
            lines_used += clamped_lines;
        } else if selected.is_empty() {
            // Fallback: if no span fits, take highest-priority span (output builder clamps)
            selected.push((priority, span));
            break;
        }
    }

    // Step 2: Sort selected by position for marker counting
    selected.sort_by_key(|(_, s)| s.transformed_range.start);

    // Step 3: Count actual markers from position-sorted set
    let selected_spans: Vec<&NodeSpan> = selected.iter().map(|(_, s)| *s).collect();
    let mut markers = count_markers(&selected_spans, lines.len());

    // Step 4: Trim — drop lowest-priority spans until content + markers <= max_lines.
    //
    // The marker occupies one of the N lines (#317 / ADR-016: `--max-lines N` ≡
    // `head -N`).  All markers (leading, gap, trailing) count against the N budget.
    //
    // Performance note: This loop is O(n^2) where n = number of selected spans.
    // Vec::remove() is O(n) and count_markers() rescans the selection each iteration.
    // This is acceptable because n is bounded by the number of top-level AST nodes,
    // which is typically tens to low hundreds even for large files.
    let trim_limit = max_lines;
    while lines_used + markers > trim_limit && selected.len() > 1 {
        // Find the span with lowest priority (tie-break: drop highest position first)
        let Some(drop_idx) = selected
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                a.0.cmp(&b.0).then_with(|| {
                    // Among equal priority, drop highest position first
                    b.1.transformed_range
                        .start
                        .cmp(&a.1.transformed_range.start)
                })
            })
            .map(|(idx, _)| idx)
        else {
            break; // unreachable: selected.len() > 1 guarantees Some
        };

        let (_, dropped_span) = selected.remove(drop_idx);
        let dropped_lines = dropped_span
            .transformed_range
            .end
            .min(lines.len())
            .saturating_sub(dropped_span.transformed_range.start);
        // rust-13: the subtraction is safe only because the fallback push at
        // the bottom of the greedy-select loop adds a span without incrementing
        // lines_used, and the trim loop requires selected.len() > 1. Decouple
        // the two facts with a checked_sub so a future edit cannot silently
        // underflow.
        debug_assert!(
            lines_used >= dropped_lines,
            "invariant: lines_used ({lines_used}) >= dropped_lines ({dropped_lines})"
        );
        lines_used = lines_used.checked_sub(dropped_lines).unwrap_or(0);

        // Recalculate markers with updated selection
        let selected_spans: Vec<&NodeSpan> = selected.iter().map(|(_, s)| *s).collect();
        markers = count_markers(&selected_spans, lines.len());
    }

    // Extract just the spans (already position-sorted from Step 2)
    let selected: Vec<&NodeSpan> = selected.into_iter().map(|(_, s)| s).collect();

    if selected.is_empty() {
        return simple_line_truncate(text, language, max_lines, hint, source_line_count);
    }

    // Build output with omission markers between gaps.
    // Each marker carries an exact count of omitted lines (ADR-011 class 1).
    // Hint (B5) is appended to every marker when present.
    let make_marker =
        |omitted: usize| elision_marker_line(Some(language), omitted, ElidedSide::Truncated, hint);

    // Cow: content lines borrow from `lines`, markers are owned Strings.
    let mut result_lines: Vec<Cow<'_, str>> = Vec::with_capacity(max_lines + 1);
    let mut last_end: usize = 0;
    let mut content_count: usize = 0;

    // Leading marker: content before the first selected span
    if selected[0].transformed_range.start > 0 {
        let omitted = selected[0].transformed_range.start;
        result_lines.push(Cow::Owned(make_marker(omitted)));
    }

    for span in &selected {
        let start = span.transformed_range.start;
        let end = span.transformed_range.end.min(lines.len());

        // Gap marker between spans — count the skipped output lines.
        //
        // Fold rule (ADR-011 class 1): if adding the gap marker would leave no budget
        // for both span content AND the trailing marker, fold the gap + the span's own
        // lines into a SINGLE combined marker and skip the span.  Without this, the
        // build emits gap_marker + trailing_marker with no content in between — two
        // consecutive elision markers that confuse agents.  The fold keeps it to one
        // marker and still discloses every omitted line.
        //
        // With N-total semantics (marker counts against budget), after the gap marker
        // we need ≥2 remaining slots: at least 1 for content and 1 for the trailing
        // marker.  Fold when remaining_after_gap <= 1 (0 or 1 slot left).
        let remaining_after_gap = max_lines.saturating_sub(result_lines.len() + 1);
        if start > last_end && last_end > 0 {
            if remaining_after_gap <= 1 {
                // Fold: a single marker covers the gap AND the full span content.
                let omitted = end.saturating_sub(last_end); // gap lines + span lines
                result_lines.push(Cow::Owned(make_marker(omitted)));
                // Advance past the entire span so trailing marker is not double-counted.
                last_end = end;
                continue; // skip content-addition loop for this span
            }
            let omitted = start - last_end;
            result_lines.push(Cow::Owned(make_marker(omitted)));
        }

        // Add lines from this span. Reserve 1 slot for the trailing marker so that
        // content + trailing marker never exceeds max_lines (#317 / ADR-016).
        let remaining_budget = max_lines
            .saturating_sub(result_lines.len())
            .saturating_sub(1);
        let span_end = end.min(start + remaining_budget);

        for line_idx in start..span_end {
            if line_idx < lines.len() {
                result_lines.push(Cow::Borrowed(lines[line_idx]));
                content_count += 1;
            }
        }

        // Track where we actually stopped emitting (span_end, not the full span end).
        // This ensures the trailing marker fires when the span was clamped.
        last_end = span_end;
    }

    // Safety: if no content lines fit (e.g. max_lines=1 with a leading marker consuming
    // the only slot), fall through to simple_line_truncate which always emits at least
    // 1 content line.  Without this guard the output would be pure elision markers with
    // zero visible code — confusing and unhelpful for agents.
    if content_count == 0 && !lines.is_empty() {
        return simple_line_truncate(text, language, max_lines, hint, source_line_count);
    }

    // Trailing marker: content after the last emitted line
    if last_end < lines.len() {
        let omitted = lines.len() - last_end;
        result_lines.push(Cow::Owned(make_marker(omitted)));
    }

    // Safety cap: total output (content + all markers) must not exceed max_lines
    // (#317 / ADR-016: `--max-lines N` ≡ `head -N`).  The trim step above is the
    // primary enforcement; this truncate is the last-resort guard.
    result_lines.truncate(max_lines);

    let mut output = result_lines.join("\n");
    // Preserve trailing newline if original had one
    if text.ends_with('\n') {
        output.push('\n');
    }

    Ok(output)
}

/// Marker spelling for a cut that could not be moved out of a multi-line
/// construct (#511).
///
/// `base` picks the direction wording: [`ElidedSide::Truncated`] for the
/// `--max-lines` head window, [`ElidedSide::Above`] for the `--last-lines` tail
/// window. `language` picks the construct — Markdown's multi-line construct is
/// a fenced code block, every other language's is a string literal.
///
/// Already-converted `*Inside*` variants pass through unchanged so that a
/// double call (e.g. from a fixpoint loop that re-enters the fail-safe) does
/// not silently relabel `TruncatedInsideFence` as `TruncatedInsideLiteral`.
/// The catch-all `(_, _)` arms that accepted ANY `ElidedSide` have been
/// removed; only the two unconverted inputs (`Truncated`, `Above`) are
/// actually converted here (rust-9).
const fn cut_inside_side(base: ElidedSide, language: Language) -> ElidedSide {
    match (base, language) {
        (ElidedSide::Above, Language::Markdown) => ElidedSide::AboveInsideFence,
        (ElidedSide::Above, _) => ElidedSide::AboveInsideLiteral,
        (ElidedSide::Truncated, Language::Markdown) => ElidedSide::TruncatedInsideFence,
        (ElidedSide::Truncated, _) => ElidedSide::TruncatedInsideLiteral,
        // Pass already-converted *Inside* variants through unchanged.
        // The language arm is required for exhaustiveness but not meaningful.
        (already, _) => already,
    }
}

/// Simple line truncation for serde-based languages (JSON, YAML) or fallback
///
/// Emits the first `max_lines - 1` content lines then appends an omission marker
/// as the `max_lines`-th line.  Total output is at most `max_lines` lines, keeping
/// `--max-lines N` equivalent to `head -N` (#317 / ADR-016).
///
/// ADR-016 owns this arithmetic: `--max-lines N` yields **N lines total, marker
/// included**, with one documented exception at `N = 1` (below). ADR-002 owns
/// the unrelated degrade-to-lossless-passthrough rule and was cited here by
/// mistake.
///
/// `hint` is appended to the marker when `Some` (B5 / ADR-011 class 1 remedy clause).
///
/// # Source-space counts (E3)
///
/// When `source_line_count` is `Some(k)`, the marker reports `k - (max_lines - 1)` lines
/// omitted — the count in **source** space (how many original source lines the agent
/// cannot see), regardless of how many lines the transformed output contains.
/// `None` falls back to `text.lines().count() - (max_lines - 1)` (output-space count).
/// For serde paths, the caller (E5) passes `Some(source.lines().count())` because
/// the serde transform restructures text so the output has far fewer lines than
/// the source.
///
/// # Literal boundaries (#511)
///
/// The cut never lands *inside* a multi-line string literal (or a Markdown
/// fenced code block). Text cut mid-literal stops being what it looks like: the
/// tail of a template literal reads as code, and a half-emitted fence turns the
/// rest of a document into a code block that swallows the marker itself.
///
/// The window is pulled **back** to just before the opening line — never forward
/// past the closer, which would break the `--max-lines N` ≡ `head -N` bound
/// (#317 / ADR-016). When the literal opens on the first retained line there is
/// nothing to pull back to, and the output takes the degenerate shape: the
/// marker FIRST (so it is outside the literal and still readable), then the raw
/// cut, with the marker naming the cut it could not avoid.
pub fn simple_line_truncate(
    text: &str,
    language: Language,
    max_lines: usize,
    hint: Option<&str>,
    source_line_count: Option<usize>,
) -> Result<String> {
    // performance-8: count first so the early-return path pays O(N) scan
    // instead of O(N) scan + O(N) allocation. Mirrors truncate_to_lines:84-86.
    if text.lines().count() <= max_lines {
        return Ok(text.to_string());
    }
    let lines: Vec<&str> = text.lines().collect();

    // Reserve 1 slot for the marker so that `--max-lines N` ≡ `head -N`:
    // at most N total lines (#317 / ADR-016).  content_lines = N-1; the marker
    // occupies the Nth slot.
    //
    // N=1 is the one irreconcilable case, and it resolves in favour of BOTH
    // obligations at the cost of one line. Reserving the slot would leave zero
    // content (a bare marker is useless as a code view); dropping the marker
    // would be silent loss, which #317 forbids and ADR-011 class 1 makes
    // unconditional. So N=1 alone emits 1 content line + 1 marker = 2 lines.
    // Every N > 1 holds the bound exactly: N-1 content + 1 marker = N.
    // E3: use source-space line count when provided; fall back to output-space.
    let mut content_lines = if max_lines > 1 {
        max_lines - 1
    } else {
        max_lines
    };

    // #511: pull the cut back out of a multi-line literal / Markdown fence.
    //
    // The scan is a single forward pass over `text`, computed once. The snap
    // runs as a **bounded fixpoint** (architecture-5): a line that closes one
    // literal and immediately opens another (e.g. Python `""" + """`) records
    // `open_after[i] = Some(i)` while `open_after[i-1] = Some(k < i)`.
    // A single snap to `content_lines = i` leaves the new last retained line
    // `i-1` still inside the previous literal. Iterating until `open_after` is
    // `None` guarantees the cut always lands in clean state.
    //
    // Bound: the loop decreases `content_lines` by at least 1 each iteration
    // (guarded by `open < content_lines`), so it runs at most `lines.len()`
    // times — which satisfies CLAUDE.md's "every loop has an explicit bound".
    //
    // `content_lines == 0` (only reachable via `max_lines == 0`) has no last
    // retained line to ask about, so the loop body is skipped entirely.
    let mut side = ElidedSide::Truncated;
    if content_lines > 0 {
        let scan = literal_scan::scan(text, language);
        for _ in 0..lines.len() {
            let Some(last_retained) = content_lines.checked_sub(1) else {
                break;
            };
            match scan.open_after(last_retained) {
                // The cut lands inside a literal: snap back to just before the
                // opener. `open` is the opener's 0-based index and also the
                // count of clean lines before it. Continue the loop: the new
                // last retained line may itself end inside an earlier literal.
                // `open < content_lines` ensures we always decrease — defending
                // against a scanner bug that returns an opener at or past the
                // current cut.
                Some(open) if open > 0 && open < content_lines => content_lines = open,
                // FAIL-SAFE: snapping back would leave zero content lines (the
                // literal opens on the first retained line). Keep the raw cut
                // and switch to the degenerate shape (marker goes first).
                Some(_) => {
                    side = cut_inside_side(ElidedSide::Truncated, language);
                    break;
                }
                // Clean after last_retained — the cut is safe.
                None => break,
            }
        }
    }
    let degenerate = side != ElidedSide::Truncated;

    let total = source_line_count.unwrap_or(lines.len());
    let omitted = total.saturating_sub(content_lines);
    let marker = elision_marker_line(Some(language), omitted, side, hint);

    // Take first content_lines lines, then append marker (total = max_lines,
    // except the documented N=1 case which yields 2).
    //
    // Degenerate case: the marker goes FIRST instead. Appended, it would land
    // inside the unterminated literal it is reporting — commented out by the
    // literal's own syntax, invisible to the reader who most needs it.
    let mut result: Vec<&str> = Vec::with_capacity(content_lines + 1);
    if degenerate {
        result.push(&marker);
    }
    result.extend_from_slice(&lines[..content_lines]);
    if !degenerate {
        result.push(&marker);
    }

    let mut output = result.join("\n");
    if text.ends_with('\n') {
        output.push('\n');
    }

    Ok(output)
}

/// Simple last-line truncation: keeps only the last N lines of output
///
/// Emits a truncation marker followed by the last `n - 1` content lines.
/// Total output is at most `n` lines, so `--last-lines N` bounds the WHOLE
/// output, marker included (#317 / ADR-016).
/// Uses language-appropriate comment syntax.
///
/// `hint` is appended to the marker when `Some` (B5 / ADR-011 class 1 remedy clause).
///
/// # Source-space counts (E3)
///
/// When `source_line_count` is `Some(k)`, the marker reports `k - content_lines`
/// lines above — the count in **source** space. `None` falls back to
/// `text.lines().count() - content_lines`.
///
/// # Literal boundaries (#511, E7.3)
///
/// See [`simple_last_line_truncate_with_start`]: the retained window never
/// *begins* inside a multi-line string literal or a Markdown fenced code block.
pub fn simple_last_line_truncate(
    text: &str,
    language: Language,
    n: usize,
    hint: Option<&str>,
    source_line_count: Option<usize>,
) -> Result<String> {
    simple_last_line_truncate_with_start(text, language, n, hint, source_line_count)
        .map(|(output, _start)| output)
}

/// [`simple_last_line_truncate`], also returning the 0-based index of the first
/// retained line of `text`.
///
/// # Why the start is returned
///
/// PF-019: `Language::transform_passthrough_with_line_map` labels every
/// retained line for `-n` by rebuilding the window arithmetically. Once the
/// window can *move* (below), a start recomputed independently there silently
/// mislabels every line of the output. The truncator is the single authority
/// on where the window begins, so it hands the index to the map builder rather
/// than leaving two sites to agree by coincidence.
///
/// The index is in `text`'s own line space ([`str::lines`] semantics), and is
/// `0` whenever no truncation happened (`total <= n`).
///
/// # Literal boundaries (#511, E7.3)
///
/// The mirror of [`simple_line_truncate`]'s pull-back. A tail window that
/// *begins* inside a multi-line string literal (or a Markdown fenced code
/// block) shows the reader literal body dressed as code, and — for Markdown —
/// leaves an orphan closing fence that turns the rest of the document into one
/// runaway code block.
///
/// The window moves **forward**, past the construct's closer, so it only ever
/// shrinks; moving backward would grow it past the `N` bound (#317 / ADR-016).
/// The boundary tested is `open_after(start - 1)`: the window starts clean iff
/// the line before it ended clean, which also drops a closing delimiter left
/// orphaned on the first retained line.
///
/// When the construct has no closer before end of file, or moving forward would
/// leave zero content lines, the raw cut stays and the (already leading) marker
/// names the cut it could not avoid — the same fail-safe as the head window, so
/// a scanner bug can never breach the `N` bound or empty a content-bearing
/// output.
pub fn simple_last_line_truncate_with_start(
    text: &str,
    language: Language,
    n: usize,
    hint: Option<&str>,
    source_line_count: Option<usize>,
) -> Result<(String, usize)> {
    let total = text.lines().count();

    if total <= n {
        return Ok((text.to_string(), 0));
    }

    // Reserve 1 slot for the marker so total output = n lines (#317 / ADR-016:
    // `--last-lines N` bounds the whole output).  content_lines = n-1; the
    // marker is the first slot.
    //
    // N=1 carve-out (ADR-016 tail mirror): spending the only slot on the marker
    // returns a view with no code, which violates the no-silent-loss rule.
    // N=1 therefore emits 1 content line + 1 marker = 2 total lines, exactly
    // as `simple_line_truncate` does for `--max-lines 1`.
    let content_count = if n > 1 { n - 1 } else { n };
    let mut start = total.saturating_sub(content_count);
    let mut side = ElidedSide::Above;

    // #511: one forward pass over `text`, run ONCE per truncation call.
    // `start == total` (n == 0, not reachable via CLI) retains no content line,
    // so there is no window boundary to ask about and the scan is skipped.
    let boundary = start.checked_sub(1).filter(|_| start < total);
    if let Some(previous) = boundary {
        let scan = literal_scan::scan(text, language);
        if scan.open_after(previous).is_some() {
            // `close_line(previous)`, not `close_line(start)`. close_line(i)
            // is documented as the first line AFTER `i` that ends clean, and
            // is meaningful whether or not line `i` is itself inside a
            // literal.  Asked from `previous` it names the construct's closing
            // line; asked from `start` it would skip a closer sitting on the
            // first retained line and drop one more line than necessary.
            match scan.close_line(previous) {
                // Forward, past the closer. Only ever shrinks the window, so
                // the N bound holds without re-checking it.
                Some(close) if close + 1 < total => start = close + 1,
                // FAIL-SAFE: no closer before EOF, or moving forward would
                // leave zero content lines. Keep the raw cut and disclose it.
                _ => side = cut_inside_side(ElidedSide::Above, language),
            }
        }
    }

    // Recomputed from the (possibly moved) start, in the counting spaces the
    // pre-#511 code used: content in output space, `omitted` against the
    // source-space total when the caller supplied one (E3/E5).
    let content_lines = total.saturating_sub(start);
    let source_total = source_line_count.unwrap_or(total);
    let omitted = source_total.saturating_sub(content_lines);
    let marker = elision_marker_line(Some(language), omitted, side, hint);

    // Skip to the tail without collecting all lines into a Vec
    let mut result: Vec<&str> = Vec::with_capacity(content_lines + 1);
    result.push(&marker);
    result.extend(text.lines().skip(start));

    let mut output = result.join("\n");
    if text.ends_with('\n') {
        output.push('\n');
    }

    Ok((output, start))
}

/// Count the number of omission markers needed for a position-sorted selection
///
/// Counts:
/// - Leading marker: if the first span doesn't start at line 0
/// - Gap markers: for each gap between adjacent spans
/// - Trailing marker: if the last span doesn't reach the end of the output
///
/// # Arguments
/// * `selected` - Position-sorted slice of selected spans
/// * `total_lines` - Total number of lines in the original output
fn count_markers(selected: &[&NodeSpan], total_lines: usize) -> usize {
    if selected.is_empty() {
        return 0;
    }

    let mut count = 0;

    // Leading marker
    if selected[0].transformed_range.start > 0 {
        count += 1;
    }

    // Gap markers between adjacent selected spans
    for i in 1..selected.len() {
        let prev_end = selected[i - 1].transformed_range.end.min(total_lines);
        let curr_start = selected[i].transformed_range.start;
        if curr_start > prev_end {
            count += 1;
        }
    }

    // Trailing marker (early return above guarantees non-empty)
    let last_end = selected[selected.len() - 1]
        .transformed_range
        .end
        .min(total_lines);
    if last_end < total_lines {
        count += 1;
    }

    count
}

// ============================================================================
// Token budget truncation (dependency-injected token counting)
// ============================================================================

/// Internal implementation of token-budget truncation.
///
/// Public API surface is [`crate::truncate_to_token_budget`].
///
/// `elision_hint` is appended to the truncation marker when `Some`
/// (B5 / ADR-011 class 1 remedy clause). `None` keeps the library CLI-agnostic.
///
/// # Source-space counts (reliability-8)
///
/// When `source_line_count` is `Some(k)`, the marker reports `k - best`
/// lines omitted — the count in **source** space. `None` falls back to
/// `text.lines().count() - best` (output-space count). Callers should pass
/// the original source-file line count so that a stacked `--tokens N --max-lines M`
/// invocation correctly reports how many source lines the agent cannot see,
/// rather than mis-counting the synthetic marker line from the `--max-lines` pass.
///
/// # Literal boundaries (#511)
///
/// The binary search converges on a line count, not on a syntactic boundary, so
/// the converged cut is afterwards pulled **back** out of any multi-line string
/// literal (or Markdown fenced code block) it landed in — the same rule
/// [`simple_line_truncate`] applies to `--max-lines`. The pull-back only ever
/// removes content lines, so the budget the search established still holds and
/// no candidate is re-counted.
pub(crate) fn truncate_to_token_budget<F>(
    text: &str,
    language: Language,
    token_budget: usize,
    count_tokens: F,
    known_token_count: Option<usize>,
    elision_hint: Option<&str>,
    source_line_count: Option<usize>,
) -> Result<String>
where
    F: Fn(&str) -> usize,
{
    // Fast path: if text already fits, return unchanged. When the caller
    // already knows the token count from the cascade loop, this avoids a
    // redundant full-text tokenization.
    let full_count = known_token_count.unwrap_or_else(|| count_tokens(text));
    // performance-12: the debug_assert that called count_tokens(text) again
    // here defeated the optimisation that known_token_count exists to provide
    // (re-tokenising the full text on every call in debug mode). The invariant
    // is covered by cascade integration tests instead.
    if full_count <= token_budget {
        return Ok(text.to_string());
    }

    let lines: Vec<&str> = text.lines().collect();

    // Edge case: empty input
    if lines.is_empty() {
        return Ok(String::new());
    }

    // reliability-8: use source_line_count (the true source size) when
    // provided so the marker is accurate in source space.  Falls back to
    // lines.len() when None — same as the previous behaviour.
    let source_total = source_line_count.unwrap_or(lines.len());

    // B5: elision_hint must be captured by the closure to append the remedy clause.
    // The marker count is `source_total - kept` — source-space omitted lines.
    let make_marker = |kept: usize| {
        elision_marker_line(
            Some(language),
            source_total.saturating_sub(kept),
            ElidedSide::Truncated,
            elision_hint,
        )
    };

    // Pre-join once and build byte-offset index to avoid O(N log N)
    // allocation churn from per-iteration `lines[..mid].join("\n")`.
    let joined = lines.join("\n");
    let mut byte_end: Vec<usize> = Vec::with_capacity(lines.len());
    let mut pos: usize = 0;
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            pos += 1; // \n separator
        }
        pos += line.len();
        byte_end.push(pos);
    }

    // #511: one forward pass over `text`, run ONCE — before the search, never
    // per candidate. The binary search is free to probe cuts that land inside a
    // literal; only the converged `best` is snapped, below.
    let scan = literal_scan::scan(text, language);

    // Binary search for max content lines that fit within budget (including marker).
    // Invariant: best is the largest number of content lines whose candidate
    // (content + omission marker) fits within token_budget.
    let mut lo: usize = 1;
    let mut hi: usize = lines.len();
    let mut best: usize = 0;

    while lo <= hi {
        let mid = lo + (hi - lo) / 2;

        // Build candidate: mid content lines + omission marker
        // Slice from pre-joined string instead of per-iteration join
        let marker = make_marker(mid);
        let content_slice = &joined[..byte_end[mid - 1]];
        let mut candidate = String::with_capacity(content_slice.len() + 1 + marker.len());
        candidate.push_str(content_slice);
        candidate.push('\n');
        candidate.push_str(&marker);

        if count_tokens(&candidate) <= token_budget {
            best = mid;
            lo = mid + 1;
        } else {
            hi = mid - 1;
        }
    }

    // #511: pull the converged cut back out of a multi-line literal / Markdown
    // fence, exactly as `simple_line_truncate` does for `--max-lines`.  `open`
    // is the opener's 0-based index and therefore also the count of lines
    // before it, so `best` only ever DECREASES — the candidate shrinks and the
    // budget invariant established by the search still holds, with no
    // re-counting.
    //
    // rust-4: guard the snap so it never reduces `best` to 0 when the binary
    // search found content lines that fit (best > 0). A top-of-file literal
    // (open == 0) means the cut landed on the first retained line with no
    // clean line before it; keeping `best` unchanged and letting the content +
    // marker path below handle it is preferable to serving a marker-only output
    // with zero content lines.
    let open_at_cut = best
        .checked_sub(1)
        .and_then(|last_retained| scan.open_after(last_retained));
    let snapped_to_zero = open_at_cut == Some(0);
    if let Some(open) = open_at_cut {
        if open > 0 {
            best = open;
        }
        // When open == 0, keep best unchanged; snapped_to_zero is true so the
        // compact_side below carries the cut-inside disclosure.
    }

    // Build final output from pre-joined string
    let marker = make_marker(best);

    // ADR-011 class 1 / #317: elision markers are unconditional — never suppress them
    // even when the marker alone exceeds the token budget. Returning an empty string
    // here would be silent total data loss. The token budget is advisory; the marker
    // always wins.
    //
    // When best==0 (no content fits even with the full marker included), use the
    // compact marker form — drop the `— SKIM_PASSTHROUGH=1 for full output` hint to
    // minimise token cost. The disclosure obligation is still met: the reader sees
    // that content was elided and the exact count of truncated lines. The hint is a
    // remedy clause, not the disclosure itself (ADR-011 / #317).
    //
    // #511: when best==0 is the *result of snapping* (rather than a budget too
    // small for any content at all), the reader is also being told the cut it
    // could not avoid — the compact marker carries the `; cut inside …` clause.
    // The hint stays dropped: the clause is disclosure, the hint is remedy.
    let compact_side = if snapped_to_zero {
        cut_inside_side(ElidedSide::Truncated, language)
    } else {
        ElidedSide::Truncated
    };
    let mut output = if best > 0 {
        let content_slice = &joined[..byte_end[best - 1]];
        let mut s = String::with_capacity(content_slice.len() + 1 + marker.len() + 1);
        s.push_str(content_slice);
        s.push('\n');
        s.push_str(&marker);
        s
    } else {
        elision_marker_line(Some(language), source_total, compact_side, None)
    };

    // documentation-24: always append a trailing newline to the compact
    // marker (`best == 0`) so downstream pipeline consumers receive a
    // complete text line regardless of whether the input had a final newline.
    // The non-compact path (`best > 0`) mirrors the input's trailing newline
    // behaviour — consistent with the rest of the function.
    if best == 0 {
        // Compact marker: unconditionally terminate as a full line.
        if !output.ends_with('\n') {
            output.push('\n');
        }
    } else if text.ends_with('\n') {
        output.push('\n');
    }

    Ok(output)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)] // Unwrapping and panics are acceptable in tests
mod tests {
    use super::*;

    #[test]
    fn test_no_truncation_when_within_budget() {
        let text = "line 1\nline 2\nline 3\n";
        let spans = vec![NodeSpan::new(0..3, "source_file")];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 10, None, || text.lines().count()).unwrap();
        assert_eq!(result, text);
    }

    #[test]
    fn test_no_truncation_when_exact_budget() {
        let text = "line 1\nline 2\nline 3\n";
        let spans = vec![NodeSpan::new(0..3, "source_file")];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, || text.lines().count()).unwrap();
        assert_eq!(result, text);
    }

    #[test]
    fn test_truncation_respects_max_lines() {
        let text = "import foo\ntype A = string\nfunction bar() {}\nfunction baz() {}\nlet x = 1\n";
        let spans = vec![
            NodeSpan::new(0..1, "import_statement"),
            NodeSpan::new(1..2, "type_alias_declaration"),
            NodeSpan::new(2..3, "function_declaration"),
            NodeSpan::new(3..4, "function_declaration"),
            NodeSpan::new(4..5, "expression_statement"),
        ];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, || text.lines().count()).unwrap();
        let line_count = result.lines().count();
        // E4: N content + 1 trailing marker = N+1 total.
        assert!(
            line_count <= 4,
            "Expected at most 4 lines (3 content + 1 trailing marker), got {}: {:?}",
            line_count,
            result
        );
    }

    #[test]
    fn test_priority_ordering_types_over_functions() {
        let text = "function foo() {}\ninterface Bar {}\nfunction baz() {}\n";
        let spans = vec![
            NodeSpan::new(0..1, "function_declaration"),
            NodeSpan::new(1..2, "interface_declaration"),
            NodeSpan::new(2..3, "function_declaration"),
        ];

        // Budget of 3: should prefer interface (priority 5) over functions (priority 4)
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, || text.lines().count()).unwrap();
        assert!(
            result.contains("interface Bar"),
            "Should contain the interface: {:?}",
            result
        );
    }

    #[test]
    fn test_priority_ordering_types_over_imports() {
        let text = "import foo from 'foo'\ntype A = string\nimport bar from 'bar'\n";
        let spans = vec![
            NodeSpan::new(0..1, "import_statement"),
            NodeSpan::new(1..2, "type_alias_declaration"),
            NodeSpan::new(2..3, "import_statement"),
        ];

        // Budget of 3: should prefer type (priority 5) over imports (priority 3)
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, || text.lines().count()).unwrap();
        assert!(
            result.contains("type A"),
            "Should contain the type alias: {:?}",
            result
        );
    }

    #[test]
    fn test_omission_markers_between_gaps() {
        // 5 lines, budget of 4. Selects the two types; the 3 middle expr lines
        // become one gap marker.  After E2 the marker carries an exact count.
        let text = "type A = string\nlet x = 1\nlet y = 2\nlet z = 3\ntype B = number\n";
        let spans = vec![
            NodeSpan::new(0..1, "type_alias_declaration"),
            NodeSpan::new(1..2, "expression_statement"),
            NodeSpan::new(2..3, "expression_statement"),
            NodeSpan::new(3..4, "expression_statement"),
            NodeSpan::new(4..5, "type_alias_declaration"),
        ];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 4, None, || text.lines().count()).unwrap();
        assert!(
            result.contains("lines truncated"),
            "Should contain counted omission marker: {:?}",
            result
        );
    }

    #[test]
    fn test_python_omission_marker() {
        let text = "import os\ndef foo(): pass\ndef bar(): pass\n";
        let spans = vec![
            NodeSpan::new(0..1, "import_statement"),
            NodeSpan::new(1..2, "function_definition"),
            NodeSpan::new(2..3, "function_definition"),
        ];

        let result = truncate_to_lines(text, &spans, Language::Python, 2, None, || text.lines().count()).unwrap();
        assert!(
            result.contains("# ...") && result.contains("lines truncated"),
            "Python should use # for counted omission marker: {:?}",
            result
        );
    }

    #[test]
    fn test_markdown_omission_marker() {
        let text = "# Heading 1\n## Heading 2\n## Heading 3\n## Heading 4\n";
        let spans = vec![
            NodeSpan::new(0..1, "atx_heading"),
            NodeSpan::new(1..2, "atx_heading"),
            NodeSpan::new(2..3, "atx_heading"),
            NodeSpan::new(3..4, "atx_heading"),
        ];

        let result = truncate_to_lines(text, &spans, Language::Markdown, 3, None, || text.lines().count()).unwrap();
        assert!(
            result.contains("<!-- ...") && result.contains("lines truncated"),
            "Markdown should use HTML comment for counted omission marker: {:?}",
            result
        );
    }

    #[test]
    fn test_empty_spans_falls_back_to_simple() {
        // E4.2: simple fallback emits N content lines + 1 marker = N+1 total.
        let text = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        let spans: Vec<NodeSpan> = vec![];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, || text.lines().count()).unwrap();
        let line_count = result.lines().count();
        assert!(
            line_count <= 4,
            "Expected at most 4 lines (3 content + 1 marker), got {}",
            line_count
        );
    }

    #[test]
    fn test_simple_line_truncate() {
        // #511: fixture has no string literal — exact counts unchanged by literal snapping.
        // N-total semantics: marker counts against the N budget (b5507ad / ADR-016).
        // Old comment said "N+1 (marker was extra)." The correct tally:
        // N-1 content + 1 marker = N total.
        // Input 5 lines, max_lines=3 → content_lines=2, omitted=5-2=3, total=3.
        let text = "line 1\nline 2\nline 3\nline 4\nline 5\n";

        let result = simple_line_truncate(text, Language::TypeScript, 3, None, None).unwrap();
        let result_lines: Vec<&str> = result.lines().collect();
        assert_eq!(
            result_lines.len(),
            3,
            "Expected 2 content lines + 1 marker = 3 lines total, got {:?}",
            result_lines
        );
        assert!(result.contains("line 1"));
        assert!(result.contains("line 2"));
        assert!(
            result.contains("// ... (3 lines truncated)"),
            "marker must count omitted lines correctly (5-2=3), got: {result}"
        );
    }

    /// `--max-lines N` = at most N lines TOTAL, marker included (b5507ad).
    /// N=1 is the documented exception: a bare marker is useless as a code view
    /// and dropping the marker would be silent loss, so N=1 yields both.
    #[test]
    fn test_simple_line_truncate_n1() {
        // #511: fixture has no string literal — exact counts unchanged by literal snapping.
        let text = "line 1\nline 2\nline 3\n";
        let result = simple_line_truncate(text, Language::TypeScript, 1, None, None).unwrap();
        let result_lines: Vec<&str> = result.lines().collect();
        assert_eq!(
            result_lines.len(),
            2,
            "N=1 exception: 1 content line + 1 marker, got {:?}",
            result_lines
        );
        assert_eq!(result_lines[0], "line 1", "N=1 must serve a content line");
        assert!(
            result_lines[1].contains("// ... (2 lines truncated)"),
            "N=1 must still disclose the 2 elided lines, got: {:?}",
            result_lines[1]
        );
    }

    /// `--max-lines N` = at most N lines TOTAL, marker included (b5507ad).
    /// N=20 over a 25-line input: 19 content lines + 1 marker covering 6.
    #[test]
    fn test_simple_line_truncate_n20() {
        // #511: fixture has no string literal — exact counts unchanged by literal snapping.
        let lines_25: String = (1..=25).map(|i| format!("line {i}\n")).collect();
        let result = simple_line_truncate(&lines_25, Language::TypeScript, 20, None, None).unwrap();
        let result_lines: Vec<&str> = result.lines().collect();
        assert_eq!(
            result_lines.len(),
            20,
            "N=20: 19 content lines + 1 marker = 20 total, got {:?}",
            result_lines.len()
        );
        assert_eq!(result_lines[0], "line 1");
        assert_eq!(result_lines[18], "line 19");
        assert!(
            result_lines[19].contains("// ... (6 lines truncated)"),
            "marker must say 6 lines truncated (25-19), got: {:?}",
            result_lines[19]
        );
    }

    #[test]
    fn test_simple_line_truncate_no_truncation() {
        let text = "line 1\nline 2\n";

        let result = simple_line_truncate(text, Language::TypeScript, 5, None, None).unwrap();
        assert_eq!(result, text);
    }

    #[test]
    fn test_max_lines_1_returns_one_line() {
        // #511: fixture has no string literal — exact counts unchanged by literal snapping.
        let text = "type A = string\nfunction foo() {}\n";
        let spans = vec![
            NodeSpan::new(0..1, "type_alias_declaration"),
            NodeSpan::new(1..2, "function_declaration"),
        ];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 1, None, || text.lines().count()).unwrap();
        let line_count = result.lines().count();
        // `--max-lines N` = at most N lines TOTAL, marker included (b5507ad).
        // N=1 is the documented exception: a bare marker is useless as a code
        // view, and dropping the marker would be silent loss (#317 / ADR-011
        // class 1), so N=1 alone yields 1 content line + 1 marker.
        assert!(
            line_count <= 2,
            "N=1 exception: at most 1 content line + 1 marker, got {}: {:?}",
            line_count,
            result
        );
        let content = result.lines().filter(|l| !l.contains("truncated")).count();
        assert!(
            content >= 1,
            "N=1 must serve a content line, not a bare marker: {:?}",
            result
        );
    }

    #[test]
    fn test_source_order_preservation() {
        // When multiple high-priority spans are selected, they should appear in
        // their original source order
        let text = "type A = string\ntype B = number\ntype C = boolean\nlet x = 1\nlet y = 2\n";
        let spans = vec![
            NodeSpan::new(0..1, "type_alias_declaration"),
            NodeSpan::new(1..2, "type_alias_declaration"),
            NodeSpan::new(2..3, "type_alias_declaration"),
            NodeSpan::new(3..4, "expression_statement"),
            NodeSpan::new(4..5, "expression_statement"),
        ];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 5, None, || text.lines().count()).unwrap();
        let result_lines: Vec<&str> = result.lines().collect();

        // Types should appear before any omission markers
        let type_a_pos = result_lines.iter().position(|l| l.contains("type A"));
        let type_b_pos = result_lines.iter().position(|l| l.contains("type B"));

        if let (Some(a), Some(b)) = (type_a_pos, type_b_pos) {
            assert!(a < b, "type A should appear before type B in output");
        }
    }

    #[test]
    fn test_multi_line_span_respected() {
        // A span covering multiple lines should be kept as a unit
        let text = "interface Foo {\n  name: string\n  age: number\n}\nlet x = 1\n";
        let spans = vec![
            NodeSpan::new(0..4, "interface_declaration"),
            NodeSpan::new(4..5, "expression_statement"),
        ];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 5, None, || text.lines().count()).unwrap();
        assert!(
            result.contains("interface Foo"),
            "Should contain the interface: {:?}",
            result
        );
        assert!(
            result.contains("name: string"),
            "Should contain interface body: {:?}",
            result
        );
    }

    #[test]
    fn test_trailing_newline_preserved() {
        let text = "line 1\nline 2\nline 3\nline 4\n";
        let spans = vec![
            NodeSpan::new(0..1, "type_alias_declaration"),
            NodeSpan::new(1..4, "expression_statement"),
        ];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, || text.lines().count()).unwrap();
        assert!(
            result.ends_with('\n'),
            "Should preserve trailing newline: {:?}",
            result
        );
    }

    #[test]
    fn test_no_trailing_newline_when_original_lacks_it() {
        let text = "line 1\nline 2\nline 3\nline 4";
        let spans = vec![
            NodeSpan::new(0..1, "type_alias_declaration"),
            NodeSpan::new(1..4, "expression_statement"),
        ];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, || text.lines().count()).unwrap();
        assert!(
            !result.ends_with('\n'),
            "Should not add trailing newline: {:?}",
            result
        );
    }

    #[test]
    fn test_max_lines_zero_with_spans_does_not_panic() {
        // CONTRACT: max_lines=0 is guarded by CLI validation (--max-lines must be >= 1).
        // At the core library level, with_max_lines(0) is accepted without error.
        //
        // For multi-span inputs the output builder computes the trailing marker but
        // result_lines.truncate(max_lines=0) clips it away — only the trailing newline
        // (preserved from the original) remains. max_lines=0 is not a valid production
        // input so this minimal edge behavior is acceptable.
        let text = "type A = string\nfunction foo() {}\n";
        let spans = vec![
            NodeSpan::new(0..1, "type_alias_declaration"),
            NodeSpan::new(1..2, "function_declaration"),
        ];

        // Should not panic — result must be a valid (possibly empty) string.
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 0, None, || text.lines().count()).unwrap();
        let line_count = result.lines().count();
        // truncate(0) removes all entries; only the preserved trailing '\n' survives.
        assert!(
            line_count <= 1,
            "max_lines=0 must produce at most 1 line, got {}: {:?}",
            line_count,
            result
        );
    }

    #[test]
    fn test_simple_line_truncate_max_lines_zero_does_not_panic() {
        // CONTRACT: max_lines=0 at simple_line_truncate level. The function uses
        // saturating_sub(1) so content_lines=0, producing only the marker line.
        // Then truncation to 0 would clip everything. This documents the edge behavior.
        let text = "line 1\nline 2\nline 3\n";

        let result = simple_line_truncate(text, Language::TypeScript, 0, None, None).unwrap();
        let line_count = result.lines().count();
        // saturating_sub(1) => content_lines=0, then push marker => 1 line,
        // but no final truncate(0) call exists in simple_line_truncate
        // so we get 1 line (just the marker). Document this clamping behavior.
        assert!(
            line_count <= 1,
            "simple_line_truncate with max_lines=0 should produce at most 1 line, got {}: {:?}",
            line_count,
            result
        );
    }

    #[test]
    fn test_overlapping_spans_output_within_budget() {
        // Verify that overlapping NodeSpan ranges do not cause the output to exceed
        // the max_lines budget. The truncation algorithm should handle overlapping
        // spans gracefully via the final truncate(max_lines) enforcement.
        let text = "line 0\nline 1\nline 2\nline 3\nline 4\nline 5\n";
        let spans = vec![
            NodeSpan::new(0..3, "type_alias_declaration"), // lines 0-2
            NodeSpan::new(1..4, "type_alias_declaration"), // lines 1-3 (overlaps with first)
            NodeSpan::new(3..6, "function_declaration"),   // lines 3-5 (overlaps with second)
        ];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 4, None, || text.lines().count()).unwrap();
        let line_count = result.lines().count();
        assert!(
            line_count <= 4,
            "Overlapping spans should not cause output to exceed budget of 4 lines, got {}: {:?}",
            line_count,
            result
        );
    }

    #[test]
    fn test_adjacent_spans_output_within_budget() {
        // Adjacent spans (end of one == start of next) should not produce spurious
        // gap markers, and output should stay within budget.
        let text = "line 0\nline 1\nline 2\nline 3\nline 4\nline 5\n";
        let spans = vec![
            NodeSpan::new(0..2, "type_alias_declaration"), // lines 0-1
            NodeSpan::new(2..4, "type_alias_declaration"), // lines 2-3 (adjacent)
            NodeSpan::new(4..6, "function_declaration"),   // lines 4-5 (adjacent)
        ];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 4, None, || text.lines().count()).unwrap();
        let line_count = result.lines().count();
        // E4: N content + 1 trailing marker = N+1 total. Budget is 4 lines of content
        // plus 1 trailing elision marker, so the ceiling is 5.
        assert!(
            line_count <= 5,
            "Adjacent spans should not cause output to exceed budget of 5 lines (4 content + 1 marker), got {}: {:?}",
            line_count,
            result
        );
    }

    // ========================================================================
    // performance-3: lazy source-line count tests
    // ========================================================================

    /// When the transformed output already fits within max_lines, the source-line
    /// count closure must NOT be called — the count is only needed for the marker
    /// text, which is never emitted when truncation does not happen.
    #[test]
    fn test_source_count_not_called_when_output_fits() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let text = "line1\nline2\nline3\n"; // 3 lines
        let spans = vec![NodeSpan::new(0..3, "source_file")];
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = Arc::clone(&call_count);

        // max_lines=10 > 3 output lines → early return fires, closure is skipped.
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 10, None, move || {
            call_count_clone.fetch_add(1, Ordering::SeqCst);
            100 // would be the source line count if called
        })
        .unwrap();

        assert_eq!(result, text, "unchanged output when fits");
        assert_eq!(
            call_count.load(Ordering::SeqCst),
            0,
            "source-line count must NOT be computed when no truncation happens (performance-3)"
        );
    }

    /// When truncation IS needed, the source-line count closure MUST be called
    /// exactly once, and the marker must report counts in SOURCE space (ADR-017).
    #[test]
    fn test_source_count_correct_in_source_space_when_truncating() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        // 5 output lines, all in one span (single-span fast-path → simple_line_truncate).
        let text = "line1\nline2\nline3\nline4\nline5\n";
        let spans = vec![NodeSpan::new(0..5, "source_file")];
        let call_count = Arc::new(AtomicUsize::new(0));
        let call_count_clone = Arc::clone(&call_count);

        // Pretend the source has 20 lines (much larger than the 5-line transform output).
        // max_lines=3: truncation happens; the marker must say 20-2=18 source lines omitted.
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, move || {
            call_count_clone.fetch_add(1, Ordering::SeqCst);
            20 // source line count in source space
        })
        .unwrap();

        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "source-line count must be computed exactly once when truncation happens (performance-3)"
        );
        // content_lines = max_lines-1 = 2; omitted = source_total - content_lines = 20-2 = 18.
        assert!(
            result.contains("18 lines truncated"),
            "marker must report source-space omitted count (ADR-017): got {result:?}"
        );
    }

    // ========================================================================
    // count_markers tests
    // ========================================================================

    #[test]
    fn test_count_markers_empty() {
        let selected: Vec<&NodeSpan> = vec![];
        assert_eq!(count_markers(&selected, 10), 0);
    }

    #[test]
    fn test_count_markers_no_gaps() {
        // Contiguous spans covering the entire output → 0 markers
        let s1 = NodeSpan::new(0..3, "type_alias_declaration");
        let s2 = NodeSpan::new(3..6, "function_declaration");
        let selected: Vec<&NodeSpan> = vec![&s1, &s2];
        assert_eq!(count_markers(&selected, 6), 0);
    }

    #[test]
    fn test_count_markers_with_gaps() {
        // Spans at 0 and 3, total 10 lines → gap between 1..3, trailing 4..10
        let s1 = NodeSpan::new(0..1, "type_alias_declaration");
        let s2 = NodeSpan::new(3..4, "type_alias_declaration");
        let selected: Vec<&NodeSpan> = vec![&s1, &s2];
        // No leading (starts at 0), 1 gap (1..3), 1 trailing (4..10) = 2
        assert_eq!(count_markers(&selected, 10), 2);
    }

    #[test]
    fn test_count_markers_leading_and_trailing() {
        // Span doesn't start at 0 and doesn't reach end
        let s1 = NodeSpan::new(2..4, "function_declaration");
        let selected: Vec<&NodeSpan> = vec![&s1];
        // 1 leading + 1 trailing = 2
        assert_eq!(count_markers(&selected, 10), 2);
    }

    // ========================================================================
    // select-then-trim tests
    // ========================================================================

    #[test]
    fn test_noncontiguous_spans_marker_accounting() {
        // Concrete bug case from the plan:
        // Types at lines 0 and 3, function at line 6, expression lines 1-2/4-5/7-9
        // max_lines=5
        //
        // Old code: would select all 3 (3 content lines within effective_budget=3),
        // then need 3 markers (2 gaps + 1 trailing), totaling 6 > 5. Clipped mid-span.
        //
        // New code: selects all 3, counts 3 markers → 6 > 5, trims function (lowest prio).
        // Result: 2 content + 2 markers = 4 ≤ 5. All content intact.
        let text = "type A\nexpr1\nexpr2\ntype B\nexpr3\nexpr4\nfn foo()\nexpr5\nexpr6\nexpr7\n";
        let spans = vec![
            NodeSpan::new(0..1, "type_alias_declaration"), // line 0: "type A"
            NodeSpan::new(1..2, "expression_statement"),   // line 1
            NodeSpan::new(2..3, "expression_statement"),   // line 2
            NodeSpan::new(3..4, "type_alias_declaration"), // line 3: "type B"
            NodeSpan::new(4..5, "expression_statement"),   // line 4
            NodeSpan::new(5..6, "expression_statement"),   // line 5
            NodeSpan::new(6..7, "function_declaration"),   // line 6: "fn foo()"
            NodeSpan::new(7..8, "expression_statement"),   // line 7
            NodeSpan::new(8..9, "expression_statement"),   // line 8
            NodeSpan::new(9..10, "expression_statement"),  // line 9
        ];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 5, None, || text.lines().count()).unwrap();
        let result_lines: Vec<&str> = result.lines().collect();

        // `--max-lines N` = at most N lines TOTAL, marker included (b5507ad).
        // Markers count against the budget, so trimming stops at 2 content + 2
        // markers = 4 ≤ 5 — exactly the behaviour documented above.
        assert!(
            result_lines.len() <= 5,
            "Output should not exceed the --max-lines=5 budget, got {}: {:?}",
            result_lines.len(),
            result
        );
        // Both type declarations must be present (priority 5).
        assert!(
            result.contains("type A"),
            "Should contain type A (priority 5): {:?}",
            result
        );
        assert!(
            result.contains("type B"),
            "Should contain type B (priority 5): {:?}",
            result
        );
        // fn foo (priority 4) is the lowest-priority span and is trimmed first:
        // under N-total the two type declarations plus their gap markers already
        // consume 4 of the 5 available lines.
        assert!(
            !result.contains("fn foo()"),
            "fn foo (prio 4) should be trimmed before the type declarations: {:?}",
            result
        );
        // Elision markers must be present for the content gaps.
        assert!(
            result.contains("lines truncated"),
            "Should contain omission markers for skipped lines: {:?}",
            result
        );
    }

    #[test]
    fn test_trim_prefers_dropping_low_priority() {
        // 3 spans that fit in content but need markers. Trim should drop lowest priority.
        let text = "type A\nimport B\nfn foo()\nexpr1\n";
        let spans = vec![
            NodeSpan::new(0..1, "type_alias_declaration"), // prio 5
            NodeSpan::new(1..2, "import_statement"),       // prio 3
            NodeSpan::new(2..3, "function_declaration"),   // prio 4
            NodeSpan::new(3..4, "expression_statement"),   // prio 1
        ];

        // max_lines=3: greedy selects type(5)+fn(4)+import(3) = 3 content lines.
        // Trailing marker (expr not selected) brings total to 4 > 3, triggering trim.
        // Import (prio 3) is dropped first, but this creates a gap between type(0..1)
        // and fn(2..3), adding a gap marker. Now 2 content + 2 markers = 4 > 3,
        // so fn is also dropped. Final: type + trailing marker = 2 lines.
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, || text.lines().count()).unwrap();

        // Highest priority (type) must always be preserved
        assert!(
            result.contains("type A"),
            "Should keep highest priority (type): {:?}",
            result
        );
        // Import (prio 3) must never survive when function (prio 4) is dropped
        assert!(
            !result.contains("import B") || result.contains("fn foo()"),
            "Import (prio 3) should be dropped before function (prio 4). Got: {:?}",
            result
        );
        // Output respects E4 budget: N content + 1 trailing marker = N+1 total.
        assert!(result.lines().count() <= 4);
    }

    #[test]
    fn test_trim_tiebreak_drops_last_position() {
        // Two spans with equal priority — should drop the one furthest from start
        let text = "type A\nexpr\ntype B\nexpr2\n";
        let spans = vec![
            NodeSpan::new(0..1, "type_alias_declaration"), // prio 5, pos 0
            NodeSpan::new(1..2, "expression_statement"),   // prio 1
            NodeSpan::new(2..3, "type_alias_declaration"), // prio 5, pos 2
            NodeSpan::new(3..4, "expression_statement"),   // prio 1
        ];

        // Budget tight enough that one type must be dropped
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 2, None, || text.lines().count()).unwrap();

        // If one type was dropped, it should be type B (higher position)
        if result.contains("type A") && !result.contains("type B") {
            // Correct tie-break: dropped higher position
        } else if result.contains("type A") && result.contains("type B") {
            // Both fit — acceptable
        } else {
            panic!(
                "Unexpected tie-break result: expected type B (higher position) to be dropped \
                 before type A, or both to fit. Got: {:?}",
                result
            );
        }
        assert!(result.lines().count() <= 2);
    }

    // ========================================================================
    // truncate_to_token_budget tests
    // ========================================================================

    /// Mock token counter: counts whitespace-separated words
    fn word_count(s: &str) -> usize {
        s.split_whitespace().count()
    }

    #[test]
    fn test_token_budget_no_truncation_when_within_budget() {
        let text = "line one\nline two\nline three\n";
        let result =
            truncate_to_token_budget(text, Language::TypeScript, 100, word_count, None, None, None)
                .unwrap();
        assert_eq!(result, text);
    }

    #[test]
    fn test_token_budget_truncates_when_over_budget() {
        let text = "word1 word2\nword3 word4\nword5 word6\nword7 word8\n";
        // Budget of 10 words: should truncate since text has 8 content words
        // plus marker words
        let result =
            truncate_to_token_budget(text, Language::TypeScript, 6, word_count, None, None, None)
                .unwrap();
        let token_count = word_count(&result);
        assert!(
            token_count <= 6,
            "Output should have at most 6 word-tokens, got {}: {:?}",
            token_count,
            result
        );
    }

    #[test]
    fn test_token_budget_includes_omission_marker() {
        let text = "line one\nline two\nline three\nline four\nline five\n";
        let result =
            truncate_to_token_budget(text, Language::TypeScript, 5, word_count, None, None, None)
                .unwrap();
        assert!(
            result.contains("truncated"),
            "Should contain omission marker: {:?}",
            result
        );
    }

    #[test]
    fn test_token_budget_preserves_trailing_newline() {
        let text = "line one\nline two\nline three\n";
        // Budget of 5: full text is 6 words, marker alone is 5 words ("// ... (3 lines truncated)")
        // so best=0, marker fits, trailing newline from original is preserved
        let result =
            truncate_to_token_budget(text, Language::TypeScript, 5, word_count, None, None, None)
                .unwrap();
        assert!(
            result.ends_with('\n'),
            "Should preserve trailing newline: {:?}",
            result
        );
    }

    #[test]
    fn test_token_budget_no_trailing_newline_when_absent() {
        let text = "line one\nline two\nline three";
        let result =
            truncate_to_token_budget(text, Language::TypeScript, 4, word_count, None, None, None)
                .unwrap();
        assert!(
            !result.ends_with('\n'),
            "Should not add trailing newline: {:?}",
            result
        );
    }

    #[test]
    fn test_token_budget_empty_input() {
        let text = "";
        let result =
            truncate_to_token_budget(text, Language::TypeScript, 10, word_count, None, None, None)
                .unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_token_budget_very_small_budget() {
        // Budget of 1: marker (~5 word-tokens) exceeds budget.
        // After ADR-011 / #317 fix: always emit the marker, never return empty string.
        let text = "line one\nline two\nline three\n";
        let result =
            truncate_to_token_budget(text, Language::TypeScript, 1, word_count, None, None, None)
                .unwrap();
        assert!(
            !result.is_empty(),
            "budget < marker size must still emit the marker, not return empty string: {:?}",
            result
        );
        assert!(
            result.contains("truncated"),
            "emitted output must be the truncation marker: {:?}",
            result
        );
    }

    #[test]
    fn test_token_budget_python_marker_syntax() {
        let text = "def foo(): pass\ndef bar(): pass\ndef baz(): pass\n";
        let result =
            truncate_to_token_budget(text, Language::Python, 5, word_count, None, None, None).unwrap();
        if result.contains("truncated") {
            assert!(
                result.contains("# ..."),
                "Python should use # for omission marker: {:?}",
                result
            );
        }
    }

    #[test]
    fn test_token_budget_marker_only_output() {
        // When budget is big enough for the marker but not for any content lines,
        // only the marker should be returned (zero content lines, best=0).
        // The marker "// ... (3 lines truncated)" is 5 word-tokens.
        let text = "line one\nline two\nline three\n";
        let result =
            truncate_to_token_budget(text, Language::TypeScript, 5, word_count, None, None, None)
                .unwrap();
        assert!(
            result.contains("truncated"),
            "Should contain omission marker: {:?}",
            result
        );
        assert!(
            !result.contains("line one"),
            "Should not contain any content lines: {:?}",
            result
        );
        let token_count = word_count(&result);
        assert!(
            token_count <= 5,
            "Marker-only output should be within budget, got {} tokens: {:?}",
            token_count,
            result
        );
    }

    #[test]
    fn test_token_budget_output_invariant() {
        // The fundamental invariant (ADR-011 / #317): when truncation occurs,
        // the output is NEVER empty. The elision marker is always emitted, even if
        // it alone exceeds the budget (unconditional class 1). The token budget is
        // advisory once the marker is the only remaining content.
        //
        // When the text fits (no truncation), the original is returned unchanged.
        let text =
            "word1 word2 word3\nword4 word5 word6\nword7 word8 word9\nword10 word11 word12\n";
        let full_token_count = word_count(text); // 12 words
        for budget in 1..20 {
            let result = truncate_to_token_budget(
                text,
                Language::TypeScript,
                budget,
                word_count,
                None,
                None,
                None,
            )
            .unwrap();
            // Output must never be empty.
            assert!(
                !result.is_empty(),
                "Budget {}: output must not be empty (silent loss prohibited)",
                budget
            );
            let truncated = budget < full_token_count;
            if truncated {
                // Truncation occurred: marker must be present.
                assert!(
                    result.contains("truncated"),
                    "Budget {}: truncation must emit the elision marker: {:?}",
                    budget,
                    result
                );
                // When over-budget (marker alone > budget), verify no content slipped through.
                let token_count = word_count(&result);
                if token_count > budget {
                    assert!(
                        !result.contains("word1")
                            && !result.contains("word4")
                            && !result.contains("word7")
                            && !result.contains("word10"),
                        "Budget {}: tokens ({}) > budget but content lines present: {:?}",
                        budget,
                        token_count,
                        result
                    );
                }
            } else {
                // No truncation: original returned unchanged.
                assert_eq!(
                    result, text,
                    "Budget {} >= full count ({}): original must be returned unchanged",
                    budget, full_token_count
                );
            }
        }
    }

    // ========================================================================
    // known_token_count tests
    // ========================================================================

    #[test]
    fn test_token_budget_known_count_skips_recount_when_over_budget() {
        // When known_token_count exceeds budget, truncation must still occur
        let text = "word1 word2\nword3 word4\nword5 word6\nword7 word8\n";
        let known = word_count(text); // 8
        let result =
            truncate_to_token_budget(text, Language::TypeScript, 6, word_count, Some(known), None, None)
                .unwrap();
        let token_count = word_count(&result);
        assert!(
            token_count <= 6,
            "With known count over budget, output should be truncated to <= 6 tokens, got {}: {:?}",
            token_count,
            result
        );
        assert!(
            result.contains("truncated"),
            "Should contain omission marker: {:?}",
            result
        );
    }

    #[test]
    fn test_token_budget_known_count_returns_early_when_within_budget() {
        let text = "line one\nline two\nline three\n";
        let actual_count = word_count(text);
        // Track whether count_tokens was called on the full text via unwrap_or_else.
        // The debug_assert! also calls count_tokens(text) for validation, so we use
        // a call-count approach: fast-path should only invoke the counter once (from
        // the debug_assert), not twice (debug_assert + unwrap_or_else).
        let call_count = std::cell::Cell::new(0u32);
        let counting_fn = |s: &str| -> usize {
            if s == text {
                call_count.set(call_count.get() + 1);
            }
            s.split_whitespace().count()
        };
        let result = truncate_to_token_budget(
            text,
            Language::TypeScript,
            100,
            counting_fn,
            Some(actual_count),
            None,
            None,
        )
        .unwrap();
        assert_eq!(result, text, "Fast-path should return text unchanged");
        // performance-12: the debug_assert that called count_tokens(text) again has
        // been removed. The fast-path unwrap_or_else should NOT call it (known_token_count is Some).
        // So we expect at most 0 calls now (the removed debug_assert was the only call site).
        let calls = call_count.get();
        assert!(
            calls <= 1,
            "count_tokens should not be called via unwrap_or_else when known_token_count is Some \
             (expected <= 1 full-text call, got {})",
            calls
        );
    }

    #[test]
    fn test_token_budget_known_count_none_behaves_like_before() {
        // Property test: None produces identical invariant (output tokens <= budget)
        let text =
            "word1 word2 word3\nword4 word5 word6\nword7 word8 word9\nword10 word11 word12\n";
        for budget in 1..20 {
            let result_none = truncate_to_token_budget(
                text,
                Language::TypeScript,
                budget,
                word_count,
                None,
                None,
                None,
            )
            .unwrap();
            let result_some = truncate_to_token_budget(
                text,
                Language::TypeScript,
                budget,
                word_count,
                Some(word_count(text)),
                None,
                None,
            )
            .unwrap();
            assert_eq!(
                result_none, result_some,
                "Budget {}: None and Some(actual_count) should produce identical output",
                budget
            );
        }
    }

    // ========================================================================
    // simple_last_line_truncate tests
    // ========================================================================

    #[test]
    fn test_last_line_no_truncation_when_within_budget() {
        let text = "line 1\nline 2\nline 3\n";
        let result = simple_last_line_truncate(text, Language::TypeScript, 5, None, None).unwrap();
        assert_eq!(result, text);
    }

    #[test]
    fn test_last_line_no_truncation_when_exact() {
        let text = "line 1\nline 2\nline 3\n";
        let result = simple_last_line_truncate(text, Language::TypeScript, 3, None, None).unwrap();
        assert_eq!(result, text);
    }

    #[test]
    fn test_last_line_truncation_keeps_last_lines() {
        // #511: quote-free, fence-free fixture — the retained window never begins
        // inside a literal, so the `--last-lines` mirror leaves these exact
        // positions and counts unchanged.
        // N-total semantics: marker counts against the N budget (b5507ad / ADR-016).
        // Old comment said "N+1 (marker was extra)." The correct tally:
        // 1 marker + (N-1) content = N total.
        // n=3, total=5 → content_lines=2, omitted=5-2=3. Shows lines 4,5 + marker.
        let text = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        let result = simple_last_line_truncate(text, Language::TypeScript, 3, None, None).unwrap();
        let result_lines: Vec<&str> = result.lines().collect();
        assert_eq!(
            result_lines.len(),
            3,
            "1 marker + 2 content = 3 total, got {:?}",
            result_lines
        );
        assert!(
            result_lines[0].contains("... (3 lines above)"),
            "got {:?}",
            result_lines[0]
        );
        assert_eq!(result_lines[1], "line 4");
        assert_eq!(result_lines[2], "line 5");
    }

    #[test]
    fn test_last_line_truncation_preserves_trailing_newline() {
        let text = "line 1\nline 2\nline 3\nline 4\n";
        let result = simple_last_line_truncate(text, Language::TypeScript, 2, None, None).unwrap();
        assert!(
            result.ends_with('\n'),
            "Should preserve trailing newline: {:?}",
            result
        );
    }

    #[test]
    fn test_last_line_truncation_no_trailing_newline() {
        let text = "line 1\nline 2\nline 3\nline 4";
        let result = simple_last_line_truncate(text, Language::TypeScript, 2, None, None).unwrap();
        assert!(
            !result.ends_with('\n'),
            "Should not add trailing newline: {:?}",
            result
        );
    }

    #[test]
    fn test_last_line_truncation_python_marker() {
        // #511: quote-free, fence-free fixture — the retained window never begins
        // inside a literal, so the `--last-lines` mirror leaves these exact
        // positions and counts unchanged.
        // N-total semantics: marker counts against the N budget (b5507ad / ADR-016).
        // Old comment said "Omitted = 3-2 = 1." The correct tally:
        // content_lines = n-1 = 1, omitted = total - content_lines = 3-1 = 2.
        let text = "def foo(): pass\ndef bar(): pass\ndef baz(): pass\n";
        let result = simple_last_line_truncate(text, Language::Python, 2, None, None).unwrap();
        assert!(
            result.contains("# ... (2 lines above)"),
            "Python should use # for marker, omit=2: {:?}",
            result
        );
    }

    #[test]
    fn test_last_line_truncation_markdown_marker() {
        // #511: quote-free, fence-free fixture — the retained window never begins
        // inside a literal, so the `--last-lines` mirror leaves these exact
        // positions and counts unchanged.
        // N-total semantics: marker counts against the N budget (b5507ad / ADR-016).
        // n=2, total=4 → content_lines = n-1 = 1, omitted = 4-1 = 3.
        let text = "# H1\n## H2\n## H3\n## H4\n";
        let result = simple_last_line_truncate(text, Language::Markdown, 2, None, None).unwrap();
        assert!(
            result.contains("<!-- ... (3 lines above) -->"),
            "Markdown should use HTML comment for marker, omit=3: {:?}",
            result
        );
    }

    #[test]
    fn test_last_line_truncation_single_line_budget() {
        // ADR-016 N=1 tail carve-out: spending the only slot on the marker returns
        // a view with no code, which violates the no-silent-loss rule.  The tail
        // mirrors the head: N=1 yields 1 content line + 1 marker = 2 total lines.
        // The content line is the LAST line of `text`; the marker precedes it and
        // discloses the omitted count (source-space: 3 total − 1 kept = 2).
        let text = "line 1\nline 2\nline 3\n";
        let result = simple_last_line_truncate(text, Language::TypeScript, 1, None, None).unwrap();
        let result_lines: Vec<&str> = result.lines().collect();
        assert_eq!(
            result_lines.len(),
            2,
            "N=1 tail carve-out: 1 content line + 1 marker = 2 total, got {:?}",
            result_lines
        );
        assert!(
            result_lines[0].contains("... (2 lines above)"),
            "marker must disclose 2 omitted lines (source-space: 3 − 1 = 2), got {:?}",
            result_lines[0]
        );
        assert_eq!(
            result_lines[1], "line 3",
            "N=1 must retain the last content line, got {:?}",
            result_lines[1]
        );
    }

    // ========================================================================
    // Marker text: where the remedy hint sits relative to the comment suffix
    // ========================================================================

    /// The CLI's remedy clause (`TransformConfig::elision_hint`).
    const HINT: &str = "SKIM_PASSTHROUGH=1 for full output";

    /// Markdown is the only language whose comment has a closing suffix, so the
    /// remedy hint must render INSIDE the HTML comment. Spelling it
    /// `<!-- ... --> — <hint>` leaks the hint into the rendered document as
    /// visible prose.
    ///
    /// Pairs with `test_last_line_truncation_markdown_marker`, which pins the
    /// hint-less spelling byte-identically.
    #[test]
    fn test_markdown_hint_renders_inside_html_comment() {
        // 5 lines, budget 3 -> content_lines = 2, elided = 5 - 2 = 3.
        let text = "# H1\n## H2\n## H3\n## H4\n## H5\n";

        let tail =
            simple_last_line_truncate(text, Language::Markdown, 3, Some(HINT), None).unwrap();
        let tail_marker = tail.lines().next().unwrap();
        assert_eq!(
            tail_marker,
            "<!-- ... (3 lines above) \u{2014} SKIM_PASSTHROUGH=1 for full output -->"
        );

        let head = simple_line_truncate(text, Language::Markdown, 3, Some(HINT), None).unwrap();
        let head_marker = head.lines().next_back().unwrap();
        assert_eq!(
            head_marker,
            "<!-- ... (3 lines truncated) \u{2014} SKIM_PASSTHROUGH=1 for full output -->"
        );

        for marker in [tail_marker, head_marker] {
            assert!(
                marker.ends_with(" -->"),
                "the HTML comment must close last: {marker:?}"
            );
            assert!(
                !marker.contains("--> \u{2014}"),
                "the hint must not escape the HTML comment: {marker:?}"
            );
        }
    }

    /// Control: every non-Markdown language has an empty comment suffix, so moving
    /// the hint inside the suffix must not move a single byte. Green before and
    /// after the refactor.
    #[test]
    fn test_non_markdown_marker_bytes_unchanged_by_hint_placement() {
        // 5 lines, budget 3 -> content_lines = 2, elided = 5 - 2 = 3.
        let text = "line 1\nline 2\nline 3\nline 4\nline 5\n";

        let ts = simple_line_truncate(text, Language::TypeScript, 3, Some(HINT), None).unwrap();
        assert_eq!(
            ts.lines().next_back().unwrap(),
            "// ... (3 lines truncated) \u{2014} SKIM_PASSTHROUGH=1 for full output"
        );

        let py = simple_line_truncate(text, Language::Python, 3, Some(HINT), None).unwrap();
        assert_eq!(
            py.lines().next_back().unwrap(),
            "# ... (3 lines truncated) \u{2014} SKIM_PASSTHROUGH=1 for full output"
        );

        let yaml = simple_line_truncate(text, Language::Yaml, 3, Some(HINT), None).unwrap();
        assert_eq!(
            yaml.lines().next_back().unwrap(),
            "# ... (3 lines truncated) \u{2014} SKIM_PASSTHROUGH=1 for full output"
        );
    }

    // ========================================================================
    // #511 — the cut never lands inside a multi-line literal or Markdown fence
    // ========================================================================

    /// 200 lines of `const v{i} = {i};` filler around two template literals:
    /// one opening on line 38 and closing on line 44 (in reach of a `--max-lines`
    /// cut) and one opening on line 160 and closing on line 167 (in reach of a
    /// `--last-lines` window).
    const TS_MULTILINE_LITERAL: &str =
        include_str!("../../../../tests/fixtures/typescript/multiline_literal.ts");

    /// 45 lines whose module docstring occupies lines 1-40, so a small
    /// `--max-lines` budget cuts inside a literal with no earlier opener to
    /// retreat to.
    const PY_DEGENERATE_LITERAL: &str =
        include_str!("../../../../tests/fixtures/python/degenerate_literal.py");

    /// 60 lines with a fenced code block spanning lines 33-40 — the README
    /// shape that `--max-lines 34` used to cut open.
    const MD_FENCED: &str = include_str!("../../../../tests/fixtures/markdown/fenced.md");

    /// Backticks in `text`. An odd count means a template literal was cut open.
    fn backtick_count(text: &str) -> usize {
        text.bytes().filter(|byte| *byte == b'`').count()
    }

    /// Lines opening or closing a Markdown fence. An odd count means the
    /// document below the cut renders as one runaway code block.
    fn fence_line_count(text: &str) -> usize {
        text.lines()
            .filter(|line| line.trim_start().starts_with("```"))
            .count()
    }

    /// #511: `--max-lines 40` over a fixture whose template literal opens on
    /// line 38 kept lines 1-39 — the opening backtick with no closer, so the
    /// marker and everything after it read as literal text.
    ///
    /// Measured at e48f977 (`skim … --mode full --max-lines 40`): 40 lines,
    /// 1 backtick, `// ... (161 lines truncated)`. Required: the window pulls
    /// back to line 37, giving 37 content lines + marker = 38 lines, 0
    /// backticks, `// ... (163 lines truncated)`.
    ///
    /// 60/80/120 are controls — their cuts fall past the literal's closer, so
    /// they keep the full N-line budget and the literal's two backticks pair up.
    #[test]
    fn test_max_lines_never_cuts_inside_template_literal() {
        // (max_lines, expected total output lines, expected omitted count)
        let cases = [
            (40_usize, 38_usize, 163_usize),
            (60, 60, 141),
            (80, 80, 121),
            (120, 120, 81),
        ];

        for (max_lines, expected_total, expected_omitted) in cases {
            let out = simple_line_truncate(
                TS_MULTILINE_LITERAL,
                Language::TypeScript,
                max_lines,
                None,
                None,
            )
            .unwrap();

            assert!(
                out.lines().count() <= max_lines,
                "--max-lines {max_lines} is a bound; got {} lines",
                out.lines().count()
            );
            assert_eq!(
                out.lines().count(),
                expected_total,
                "--max-lines {max_lines} must yield {expected_total} lines"
            );
            assert_eq!(
                backtick_count(&out) % 2,
                0,
                "--max-lines {max_lines} left an unbalanced template literal:\n{out}"
            );
            assert_eq!(
                out.lines().next_back().unwrap(),
                format!("// ... ({expected_omitted} lines truncated)"),
                "--max-lines {max_lines} must count from the retained window"
            );
        }

        // N=40 is the cut that moves: it stops on the line before the opener.
        let out = simple_line_truncate(TS_MULTILINE_LITERAL, Language::TypeScript, 40, None, None)
            .unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(
            lines[lines.len() - 2],
            "const v37 = 37;",
            "the last content line must precede the literal's opener (line 38)"
        );
        assert_eq!(
            backtick_count(&out),
            0,
            "no backtick survives the cut:\n{out}"
        );
    }

    /// Python's `"""` docstring is multi-line: a cut inside it leaves the head
    /// of a string, and the marker appended after it is swallowed by the string.
    ///
    /// Measured at e48f977 for this input: 9 content lines ending
    /// `docstring line 9`, one unclosed `"""`, `# ... (11 lines truncated)`.
    #[test]
    fn test_max_lines_python_triple_quote() {
        let mut src = String::new();
        for i in 1..=5 {
            src.push_str(&format!("x{i} = {i}\n"));
        }
        src.push_str("DOC = \"\"\"\n"); // line 6 opens
        for i in 7..=10 {
            src.push_str(&format!("docstring line {i}\n"));
        }
        src.push_str("\"\"\"\n"); // line 11 closes
        for i in 12..=20 {
            src.push_str(&format!("y{i} = {i}\n"));
        }
        assert_eq!(src.lines().count(), 20, "fixture shape");

        let out = simple_line_truncate(&src, Language::Python, 10, None, None).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 6, "5 content lines + marker:\n{out}");
        assert_eq!(lines[4], "x5 = 5", "cut moves back before line 6's opener");
        assert_eq!(lines[5], "# ... (15 lines truncated)");
        assert_eq!(
            out.matches("\"\"\"").count(),
            0,
            "no half-open docstring:\n{out}"
        );
    }

    /// Rust raw strings close on `"` followed by the opener's hash run, so a
    /// mid-literal cut leaves `r#"` dangling.
    ///
    /// Measured at e48f977 for this input: 9 content lines ending `select 9
    /// from t;`, one unclosed `r#"`, `// ... (11 lines truncated)`.
    #[test]
    fn test_max_lines_rust_raw_string() {
        let mut src = String::new();
        for i in 1..=5 {
            src.push_str(&format!("const X{i}: usize = {i};\n"));
        }
        src.push_str("const SQL: &str = r#\"\n"); // line 6 opens
        for i in 7..=10 {
            src.push_str(&format!("select {i} from t;\n"));
        }
        src.push_str("\"#;\n"); // line 11 closes
        for i in 12..=20 {
            src.push_str(&format!("const Y{i}: usize = {i};\n"));
        }
        assert_eq!(src.lines().count(), 20, "fixture shape");

        let out = simple_line_truncate(&src, Language::Rust, 10, None, None).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 6, "5 content lines + marker:\n{out}");
        assert_eq!(lines[4], "const X5: usize = 5;");
        assert_eq!(lines[5], "// ... (15 lines truncated)");
        assert!(!out.contains("r#\""), "no half-open raw string:\n{out}");
    }

    /// Degenerate case: the literal opens on line 1, so there is no earlier
    /// line to pull back to. The window stays where the budget put it and the
    /// marker moves to the FRONT — appended, it would sit inside the very
    /// literal it reports and never reach the reader.
    ///
    /// Measured at e48f977 (`--mode full --max-lines 5`): lines 1-4 of the
    /// docstring followed by `# ... (41 lines truncated) — …`, the marker
    /// inside the string.
    #[test]
    fn test_degenerate_literal_emits_marker_first_then_raw_cut() {
        let out =
            simple_line_truncate(PY_DEGENERATE_LITERAL, Language::Python, 5, None, None).unwrap();
        let lines: Vec<&str> = out.lines().collect();

        assert_eq!(
            lines.len(),
            5,
            "exactly N lines: marker + N-1 content:\n{out}"
        );
        assert!(
            lines[0].starts_with("# ..."),
            "the marker must come first:\n{out}"
        );
        assert!(
            lines[0].contains("(41 lines truncated"),
            "count is source total minus the 4 retained lines: {:?}",
            lines[0]
        );
        assert!(
            lines[0].contains("cut inside a string literal"),
            "the marker must name the cut it could not avoid: {:?}",
            lines[0]
        );

        let source: Vec<&str> = PY_DEGENERATE_LITERAL.lines().collect();
        assert_eq!(
            &lines[1..],
            &source[..4],
            "content is the raw head of the file, unmodified"
        );
    }

    /// JSON's scanner row is empty: RFC 8259 forbids a raw newline inside a
    /// string, so no JSON cut can land in one. The cut stays exactly where the
    /// budget puts it even on a line whose quotes look unbalanced to a naive
    /// counter (`"quote": "\""` reads as five quotes).
    #[test]
    fn test_json_literal_scan_is_noop() {
        let text = concat!(
            "{\n",
            r#"  "one": 1,"#,
            "\n",
            r#"  "quote": "\"","#,
            "\n",
            r#"  "three": 3,"#,
            "\n",
            r#"  "four": 4,"#,
            "\n",
            r#"  "five": 5"#,
            "\n",
            "}\n",
        );
        assert_eq!(text.lines().count(), 7, "fixture shape");

        let out = simple_line_truncate(text, Language::Json, 4, None, None).unwrap();
        assert_eq!(
            out,
            concat!(
                "{\n",
                r#"  "one": 1,"#,
                "\n",
                r#"  "quote": "\"","#,
                "\n",
                "// ... (4 lines truncated)\n",
            ),
            "JSON output must be byte-identical to the pre-#511 cut"
        );
    }

    /// YAML block scalars (`|`, `>`) are deliberately NOT modelled: they close
    /// by dedent, so a column-0 marker legally ends the scalar and the document
    /// stays valid YAML. This pins the non-fix — the cut inside `script: |` is
    /// left exactly where the budget put it.
    #[test]
    fn test_yaml_block_scalar_cut_is_not_backed_up() {
        let text =
            "name: demo\nscript: |\n  line one\n  line two\n  line three\nafter: 1\ntail: 2\n";
        assert_eq!(text.lines().count(), 7, "fixture shape");

        let out = simple_line_truncate(text, Language::Yaml, 4, None, None).unwrap();
        assert_eq!(
            out, "name: demo\nscript: |\n  line one\n# ... (4 lines truncated)\n",
            "a block scalar must not pull the cut back"
        );
    }

    /// E7 (#511, Markdown): a fence opening on line 33 used to be cut open by
    /// `--max-lines 34`, turning every line after the marker into code-block
    /// content — including the marker itself.
    ///
    /// Measured at e48f977 (`skim … --mode full --max-lines 34`): 34 lines,
    /// 1 fence line, `<!-- ... (27 lines truncated) — … -->`. Required: 32
    /// content lines + marker = 33, 0 fence lines, `(28 lines truncated)`.
    #[test]
    fn test_markdown_max_lines_34_does_not_cut_inside_fence() {
        let out =
            simple_line_truncate(MD_FENCED, Language::Markdown, 34, Some(HINT), None).unwrap();
        let lines: Vec<&str> = out.lines().collect();

        assert!(
            lines.len() <= 34,
            "--max-lines 34 is a bound: {}",
            lines.len()
        );
        assert_eq!(lines.len(), 33, "32 content lines + marker:\n{out}");
        assert_eq!(
            fence_line_count(&out) % 2,
            0,
            "an odd fence count leaves a runaway code block:\n{out}"
        );
        assert_eq!(
            lines[31], "Prose line 32 with `inline code` that is not a fence.",
            "the last content line must precede the fence opener (line 33)"
        );
        assert_eq!(
            lines[32],
            "<!-- ... (28 lines truncated) \u{2014} SKIM_PASSTHROUGH=1 for full output -->",
            "the hint stays inside the HTML comment"
        );
    }

    /// Degenerate fence: the block opens on line 1, so the marker leads. It is
    /// the only line of the output that is *not* inside the fence, which is
    /// exactly why it cannot be appended.
    #[test]
    fn test_markdown_degenerate_fence_marker_first() {
        let text = "```rust\nfn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\nfn e() {}\nfn f() {}\nfn g() {}\n```\ntail\n";
        assert_eq!(text.lines().count(), 10, "fixture shape");

        let out = simple_line_truncate(text, Language::Markdown, 5, None, None).unwrap();
        let lines: Vec<&str> = out.lines().collect();

        assert_eq!(
            lines.len(),
            5,
            "exactly N lines: marker + N-1 content:\n{out}"
        );
        assert_eq!(
            lines[0],
            "<!-- ... (6 lines truncated; cut inside a code fence) -->"
        );
        assert_eq!(
            &lines[1..],
            &["```rust", "fn a() {}", "fn b() {}", "fn c() {}"][..]
        );
    }

    /// Control: `--max-lines 20` over the same fixture cuts at line 19, far
    /// above the fence, so #511 must not move a single byte.
    #[test]
    fn test_markdown_max_lines_20_is_unchanged() {
        let out =
            simple_line_truncate(MD_FENCED, Language::Markdown, 20, Some(HINT), None).unwrap();
        let lines: Vec<&str> = out.lines().collect();

        assert_eq!(lines.len(), 20, "19 content lines + marker:\n{out}");
        assert_eq!(fence_line_count(&out), 0, "the cut is above the fence");
        assert_eq!(
            lines[18],
            "Prose line 19 with `inline code` that is not a fence."
        );
        assert_eq!(
            lines[19],
            "<!-- ... (41 lines truncated) \u{2014} SKIM_PASSTHROUGH=1 for full output -->"
        );
    }

    // ========================================================================
    // #511 / E7.3 — the `--last-lines` window never BEGINS inside a literal
    // ========================================================================

    /// Mirror of the `--max-lines` pull-back. A tail window that begins inside
    /// a template literal hands the reader literal body dressed as code, with
    /// an orphan backtick where the opener should be.
    ///
    /// Measured at 9058273 (`skim … --mode full --last-lines 40`, against this
    /// fixture's tail literal at lines 160-167): 40 lines, 1 backtick, first
    /// content line `  A --last-lines window that opens in here must move
    /// FORWARD, past` — source line 162, the middle of the literal.
    ///
    /// Required: the window moves FORWARD past the closer on line 167 and so
    /// SHRINKS to 33 content lines + marker = 34.  Forward only: pulling back
    /// would grow the window past the N bound (ADR-016).
    #[test]
    fn test_last_lines_never_starts_inside_literal() {
        let out =
            simple_last_line_truncate(TS_MULTILINE_LITERAL, Language::TypeScript, 40, None, None)
                .unwrap();
        let lines: Vec<&str> = out.lines().collect();

        assert!(
            lines.len() <= 40,
            "--last-lines 40 is a bound; got {} lines",
            lines.len()
        );
        assert_eq!(lines.len(), 34, "marker + 33 content lines:\n{out}");
        assert_eq!(
            backtick_count(&out),
            0,
            "no orphan backtick may survive the cut:\n{out}"
        );
        assert_eq!(
            lines[1], "const v168 = 168;",
            "the window must begin after the literal's closer (line 167)"
        );
        assert_eq!(lines.last().copied(), Some("const v200 = 200;"));
        assert_eq!(
            lines[0],
            format!("// ... ({} lines above)", 200 - (lines.len() - 1)),
            "the count must be recomputed from the moved start"
        );
    }

    /// Degenerate `--last-lines`: the literal has no closing delimiter before
    /// end of file, so there is nothing to move forward past — and moving
    /// forward at all would empty the window. The raw cut stays and the
    /// (already leading) marker names the cut it could not avoid.
    ///
    /// Measured at 9058273 (`--mode full --last-lines 5` on this shape):
    /// `// ... (16 lines above) — …` then four lines of literal body presented
    /// as code, with nothing telling the reader they are inside a string.
    #[test]
    fn test_last_lines_degenerate_literal_to_eof_keeps_raw_cut_with_clause() {
        let mut src = String::new();
        for i in 1..=14 {
            src.push_str(&format!("const a{i} = {i};\n"));
        }
        src.push_str("const unterminated = `\n"); // line 15 opens, never closes
        for i in 16..=20 {
            src.push_str(&format!("  literal line {i}\n"));
        }
        assert_eq!(src.lines().count(), 20, "fixture shape");

        let out = simple_last_line_truncate(&src, Language::TypeScript, 5, None, None).unwrap();
        let lines: Vec<&str> = out.lines().collect();

        assert_eq!(
            lines.len(),
            5,
            "the raw cut is kept: marker + 4 content:\n{out}"
        );
        assert_eq!(
            lines[0], "// ... (16 lines above; cut inside a string literal)",
            "the marker must name the cut it could not avoid"
        );
        assert_eq!(
            &lines[1..],
            &[
                "  literal line 17",
                "  literal line 18",
                "  literal line 19",
                "  literal line 20",
            ][..],
            "content is the raw tail of the file, unmodified"
        );
    }

    /// E7.3 (Markdown): a tail window opening inside a fenced block leaves the
    /// block's CLOSING fence with no opener, so every line after it — the rest
    /// of the document — renders as one runaway code block.
    ///
    /// Measured at 9058273 (`skim … --mode full --last-lines 28`): 28 lines,
    /// 1 fence line, first content line `export function fenced(): void {`
    /// (source line 34, inside the block). Required: the window moves forward
    /// past the closer on line 40, giving 20 content lines + marker = 21 and no
    /// fence line at all.
    #[test]
    fn test_markdown_last_lines_does_not_start_inside_fence() {
        let out =
            simple_last_line_truncate(MD_FENCED, Language::Markdown, 28, Some(HINT), None).unwrap();
        let lines: Vec<&str> = out.lines().collect();

        assert!(
            lines.len() <= 28,
            "--last-lines 28 is a bound; got {} lines",
            lines.len()
        );
        assert_eq!(lines.len(), 21, "marker + 20 content lines:\n{out}");
        assert_eq!(
            fence_line_count(&out),
            0,
            "an orphan closing fence swallows the rest of the document:\n{out}"
        );
        assert_eq!(
            lines[0], "<!-- ... (40 lines above) \u{2014} SKIM_PASSTHROUGH=1 for full output -->",
            "the count must be recomputed from the moved start"
        );
        assert_eq!(
            lines[3], "## Tail section 43",
            "the window must begin after the fence's closer (line 40)"
        );
    }

    // ========================================================================
    // #511 / E6.5 — `--tokens` snaps the converged cut out of a literal
    // ========================================================================

    /// The token binary search converges on a line count, not on a syntactic
    /// boundary, so it happily stops in the middle of a template literal. The
    /// converged `best` is snapped back to the opener afterwards; because that
    /// only ever DECREASES `best`, the budget the search established still
    /// holds without re-counting a single candidate.
    ///
    /// The whole fixture is 15 words, so the budget has to sit below that or the
    /// `full_count <= token_budget` fast path returns the text untouched and
    /// there is no cut to snap. At budget 14 the search picks `best = 3` — two
    /// prose lines, the literal's opening line and the marker
    /// "... (4 lines truncated)", 13 words — leaving one unbalanced backtick with
    /// the marker itself reading as literal text. The snap pulls `best` back to
    /// the opener's index, 2.
    #[test]
    fn test_token_budget_snaps_best_back_out_of_literal() {
        let text = "alpha one\nbeta two\nconst s = `\ninside one\ninside two\n`;\ngamma three\n";
        let budget = 14;

        let out =
            truncate_to_token_budget(text, Language::TypeScript, budget, word_count, None, None, None)
                .unwrap();

        assert_eq!(
            backtick_count(&out),
            0,
            "the cut must not leave a template literal open:\n{out}"
        );
        assert!(
            word_count(&out) <= budget,
            "snapping only shrinks the output, so the budget still holds: {} > {budget}\n{out}",
            word_count(&out)
        );
        assert_eq!(
            out, "alpha one\nbeta two\n// ... (5 lines truncated)\n",
            "the window stops on the line before the literal's opener"
        );
    }

    /// The literal opens on line 1, so the snap has nowhere to retreat to:
    /// rust-4: the snap-to-zero guard keeps at least one content line when the
    /// binary search found a fitting candidate.
    ///
    /// At budget 14 the search picks `best = 1`: the literal's opening line (4
    /// words) plus the hinted marker "... (5 lines truncated) —
    /// SKIM_PASSTHROUGH=1 for full output" (10 words) totals exactly 14. The
    /// cut lands after line 0, and `scan.open_after(0) = Some(0)` (the backtick
    /// on line 0 itself opens the literal that spans the entire file). The OLD
    /// behaviour unconditionally set `best = open = 0`, producing a compact
    /// marker with no content. The FIX (rust-4) guards `if open > 0` so the
    /// snap only fires when it leaves at least one content line intact; when
    /// `open == 0` the guard keeps `best = 1` and the content + regular marker
    /// path is taken instead.
    ///
    /// The literal's body carries three words a line because the hinted marker
    /// alone costs 10 tokens: with a one-word body the whole fixture (9 words)
    /// is cheaper than the opener-plus-marker candidate (14) and the fast-path
    /// return fires before the binary search, making this branch unreachable.
    #[test]
    fn test_token_budget_snap_to_zero_guard_keeps_content() {
        let text = "const banner = `\nalpha one two\nbeta three four\ngamma five six\ndelta seven eight\n`;\n";

        let out =
            truncate_to_token_budget(text, Language::TypeScript, 14, word_count, None, Some(HINT), None)
                .unwrap();

        // rust-4: the guard does NOT snap best to 0; the literal's opening line
        // is the one retained content line.
        assert!(
            out.starts_with("const banner = `\n"),
            "the literal opening line must survive snap-to-zero guard:\n{out}"
        );
        // The non-compact path includes the hint (HINT is the remedy clause).
        assert!(
            out.contains(HINT),
            "the non-compact form must include the remedy hint:\n{out}"
        );
        // 5 source lines were truncated (source_total=6, kept=1).
        assert!(
            out.contains("5 lines"),
            "marker must report 5 truncated lines:\n{out}"
        );
        // Total line count is at most 3 (1 content + 1 marker + optional blank).
        assert!(
            out.lines().count() <= 3,
            "output must be at most 3 lines:\n{out}"
        );
    }
}
