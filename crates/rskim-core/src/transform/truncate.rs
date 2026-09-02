//! AST-aware truncation for --max-lines support
//!
//! ARCHITECTURE: Truncates transformed output to a maximum number of lines
//! using priority-based selection that respects AST node boundaries.
//! Types and signatures are kept over imports, which are kept over bodies.
//! Omission markers are inserted between gaps using language-appropriate comment syntax.

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
pub(crate) fn truncate_to_lines(
    text: &str,
    spans: &[NodeSpan],
    language: Language,
    max_lines: usize,
    hint: Option<&str>,
    source_line_count: Option<usize>,
) -> Result<String> {
    // If no spans provided, fall back to simple line truncation immediately
    // to avoid a redundant lines().collect() (simple_line_truncate does its own)
    if spans.is_empty() {
        return simple_line_truncate(text, language, max_lines, hint, source_line_count);
    }

    let lines: Vec<&str> = text.lines().collect();

    // If output fits, return unchanged
    if lines.len() <= max_lines {
        return Ok(text.to_string());
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
    // The marker occupies one of the N lines (#317 / ADR-002: `--max-lines N` ≡
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
        lines_used -= dropped_lines;

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
        // content + trailing marker never exceeds max_lines (#317 / ADR-002).
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
    // (#317 / ADR-002: `--max-lines N` ≡ `head -N`).  The trim step above is the
    // primary enforcement; this truncate is the last-resort guard.
    result_lines.truncate(max_lines);

    let mut output = result_lines.join("\n");
    // Preserve trailing newline if original had one
    if text.ends_with('\n') {
        output.push('\n');
    }

    Ok(output)
}

/// Simple line truncation for serde-based languages (JSON, YAML) or fallback
///
/// Emits the first `max_lines - 1` content lines then appends an omission marker
/// as the `max_lines`-th line.  Total output is at most `max_lines` lines, keeping
/// `--max-lines N` equivalent to `head -N` (#317 / ADR-002).
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
pub(crate) fn simple_line_truncate(
    text: &str,
    language: Language,
    max_lines: usize,
    hint: Option<&str>,
    source_line_count: Option<usize>,
) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();

    if lines.len() <= max_lines {
        return Ok(text.to_string());
    }

    // Reserve 1 slot for the marker so that `--max-lines N` ≡ `head -N`:
    // at most N total lines (#317 / ADR-002).  content_lines = N-1; the marker
    // occupies the Nth slot.
    //
    // N=1 is the one irreconcilable case, and it resolves in favour of BOTH
    // obligations at the cost of one line. Reserving the slot would leave zero
    // content (a bare marker is useless as a code view); dropping the marker
    // would be silent loss, which #317 forbids and ADR-011 class 1 makes
    // unconditional. So N=1 alone emits 1 content line + 1 marker = 2 lines.
    // Every N > 1 holds the bound exactly: N-1 content + 1 marker = N.
    // E3: use source-space line count when provided; fall back to output-space.
    let content_lines = if max_lines > 1 {
        max_lines - 1
    } else {
        max_lines
    };
    let total = source_line_count.unwrap_or(lines.len());
    let omitted = total.saturating_sub(content_lines);
    let marker = elision_marker_line(Some(language), omitted, ElidedSide::Truncated, hint);

    // Take first content_lines lines, then append marker (total = max_lines,
    // except the documented N=1 case which yields 2).
    let mut result: Vec<&str> = lines[..content_lines].to_vec();
    result.push(&marker);

    let mut output = result.join("\n");
    if text.ends_with('\n') {
        output.push('\n');
    }

    Ok(output)
}

/// Simple last-line truncation: keeps only the last N lines of output
///
/// Emits a truncation marker followed by the last `n - 1` content lines.
/// Total output is at most `n` lines, keeping `--max-lines N` equivalent to
/// `head -N` (#317 / ADR-002).
/// Uses language-appropriate comment syntax.
///
/// `hint` is appended to the marker when `Some` (B5 / ADR-011 class 1 remedy clause).
///
/// # Source-space counts (E3)
///
/// When `source_line_count` is `Some(k)`, the marker reports `k - (n - 1)` lines
/// above — the count in **source** space. `None` falls back to
/// `text.lines().count() - (n - 1)`.
pub(crate) fn simple_last_line_truncate(
    text: &str,
    language: Language,
    n: usize,
    hint: Option<&str>,
    source_line_count: Option<usize>,
) -> Result<String> {
    let total = text.lines().count();

    if total <= n {
        return Ok(text.to_string());
    }

    // Reserve 1 slot for the marker so total output = n lines (#317 / ADR-002:
    // `--max-lines N` ≡ `head -N`).  content_lines = n-1; the marker is the first slot.
    // E3: use source-space line count when provided; fall back to output-space.
    let content_lines = n.saturating_sub(1);
    let source_total = source_line_count.unwrap_or(total);
    let omitted = source_total.saturating_sub(content_lines);
    let marker = elision_marker_line(Some(language), omitted, ElidedSide::Above, hint);

    // Skip to the tail without collecting all lines into a Vec
    let mut result: Vec<&str> = Vec::with_capacity(n + 1);
    result.push(&marker);
    result.extend(text.lines().skip(total - content_lines));

    let mut output = result.join("\n");
    if text.ends_with('\n') {
        output.push('\n');
    }

    Ok(output)
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
pub(crate) fn truncate_to_token_budget<F>(
    text: &str,
    language: Language,
    token_budget: usize,
    count_tokens: F,
    known_token_count: Option<usize>,
    elision_hint: Option<&str>,
) -> Result<String>
where
    F: Fn(&str) -> usize,
{
    // Fast path: if text already fits, return unchanged. When the caller
    // already knows the token count from the cascade loop, this avoids a
    // redundant full-text tokenization.
    let full_count = known_token_count.unwrap_or_else(|| count_tokens(text));
    debug_assert!(
        known_token_count.is_none() || known_token_count == Some(count_tokens(text)),
        "known_token_count ({:?}) does not match actual count ({})",
        known_token_count,
        count_tokens(text),
    );
    if full_count <= token_budget {
        return Ok(text.to_string());
    }

    let lines: Vec<&str> = text.lines().collect();

    // Edge case: empty input
    if lines.is_empty() {
        return Ok(String::new());
    }

    // B5: elision_hint must be captured by the closure to append the remedy clause.
    let make_marker = |truncated_count: usize| {
        elision_marker_line(
            Some(language),
            truncated_count,
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
        let marker = make_marker(lines.len() - mid);
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

    // Build final output from pre-joined string
    let marker = make_marker(lines.len() - best);

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
    let mut output = if best > 0 {
        let content_slice = &joined[..byte_end[best - 1]];
        let mut s = String::with_capacity(content_slice.len() + 1 + marker.len() + 1);
        s.push_str(content_slice);
        s.push('\n');
        s.push_str(&marker);
        s
    } else {
        elision_marker_line(Some(language), lines.len(), ElidedSide::Truncated, None)
    };

    if text.ends_with('\n') {
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 10, None, None).unwrap();
        assert_eq!(result, text);
    }

    #[test]
    fn test_no_truncation_when_exact_budget() {
        let text = "line 1\nline 2\nline 3\n";
        let spans = vec![NodeSpan::new(0..3, "source_file")];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, None).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, None).unwrap();
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
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, None).unwrap();
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
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, None).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 4, None, None).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::Python, 2, None, None).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::Markdown, 3, None, None).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, None).unwrap();
        let line_count = result.lines().count();
        assert!(
            line_count <= 4,
            "Expected at most 4 lines (3 content + 1 marker), got {}",
            line_count
        );
    }

    #[test]
    fn test_simple_line_truncate() {
        // N-total semantics: marker counts against the N budget (b5507ad / ADR-002).
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
        let text = "type A = string\nfunction foo() {}\n";
        let spans = vec![
            NodeSpan::new(0..1, "type_alias_declaration"),
            NodeSpan::new(1..2, "function_declaration"),
        ];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 1, None, None).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 5, None, None).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 5, None, None).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, None).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, None).unwrap();
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
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 0, None, None).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 4, None, None).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 4, None, None).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 5, None, None).unwrap();
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
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3, None, None).unwrap();

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
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 2, None, None).unwrap();

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
            truncate_to_token_budget(text, Language::TypeScript, 100, word_count, None, None)
                .unwrap();
        assert_eq!(result, text);
    }

    #[test]
    fn test_token_budget_truncates_when_over_budget() {
        let text = "word1 word2\nword3 word4\nword5 word6\nword7 word8\n";
        // Budget of 10 words: should truncate since text has 8 content words
        // plus marker words
        let result =
            truncate_to_token_budget(text, Language::TypeScript, 6, word_count, None, None)
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
            truncate_to_token_budget(text, Language::TypeScript, 5, word_count, None, None)
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
            truncate_to_token_budget(text, Language::TypeScript, 5, word_count, None, None)
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
            truncate_to_token_budget(text, Language::TypeScript, 4, word_count, None, None)
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
            truncate_to_token_budget(text, Language::TypeScript, 10, word_count, None, None)
                .unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_token_budget_very_small_budget() {
        // Budget of 1: marker (~5 word-tokens) exceeds budget.
        // After ADR-011 / #317 fix: always emit the marker, never return empty string.
        let text = "line one\nline two\nline three\n";
        let result =
            truncate_to_token_budget(text, Language::TypeScript, 1, word_count, None, None)
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
            truncate_to_token_budget(text, Language::Python, 5, word_count, None, None).unwrap();
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
            truncate_to_token_budget(text, Language::TypeScript, 5, word_count, None, None)
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
            truncate_to_token_budget(text, Language::TypeScript, 6, word_count, Some(known), None)
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
        )
        .unwrap();
        assert_eq!(result, text, "Fast-path should return text unchanged");
        // In debug builds the debug_assert! calls count_tokens(text) once.
        // The fast-path unwrap_or_else should NOT call it (known_token_count is Some).
        // So we expect at most 1 call (from debug_assert), not 2.
        let calls = call_count.get();
        assert!(
            calls <= 1,
            "count_tokens should not be called via unwrap_or_else when known_token_count is Some \
             (expected <= 1 full-text call from debug_assert, got {})",
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
            )
            .unwrap();
            let result_some = truncate_to_token_budget(
                text,
                Language::TypeScript,
                budget,
                word_count,
                Some(word_count(text)),
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
        // N-total semantics: marker counts against the N budget (b5507ad / ADR-002).
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
        // N-total semantics: marker counts against the N budget (b5507ad / ADR-002).
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
        // N-total semantics: marker counts against the N budget (b5507ad / ADR-002).
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
        // N-total semantics: marker counts against the N budget (b5507ad / ADR-002).
        // Old comment said "1 content + 1 marker = 2 total." The correct tally:
        // content_lines = n-1 = 0, omitted = total - content_lines = 3-0 = 3.
        // With n=1, the only slot is the marker itself (no content fits).
        let text = "line 1\nline 2\nline 3\n";
        let result = simple_last_line_truncate(text, Language::TypeScript, 1, None, None).unwrap();
        let result_lines: Vec<&str> = result.lines().collect();
        assert_eq!(
            result_lines.len(),
            1,
            "Only marker fits (n=1, N-total): 1 total, got {:?}",
            result_lines
        );
        assert!(
            result_lines[0].contains("... (3 lines above)"),
            "got {:?}",
            result_lines[0]
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
}
