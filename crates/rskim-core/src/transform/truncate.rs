//! AST-aware truncation for --max-lines support
//!
//! ARCHITECTURE: Truncates transformed output to a maximum number of lines
//! using priority-based selection that respects AST node boundaries.
//! Types and signatures are kept over imports, which are kept over bodies.
//! Omission markers are inserted between gaps using language-appropriate comment syntax.

use crate::transform::utils::{get_comment_prefix, get_comment_suffix, score_node_kind};
use crate::{Language, Result};
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
///
/// # Returns
/// Truncated text that never exceeds `max_lines` lines
pub(crate) fn truncate_to_lines(
    text: &str,
    spans: &[NodeSpan],
    language: Language,
    max_lines: usize,
) -> Result<String> {
    // If no spans provided, fall back to simple line truncation immediately
    // to avoid a redundant lines().collect() (simple_line_truncate does its own)
    if spans.is_empty() {
        return simple_line_truncate(text, language, max_lines);
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
        return simple_line_truncate(text, language, max_lines);
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

    // Step 4: Trim — drop lowest-priority spans until content + markers <= max_lines
    //
    // Performance note: This loop is O(n^2) where n = number of selected spans.
    // Vec::remove() is O(n) and count_markers() rescans the selection each iteration.
    // This is acceptable because n is bounded by the number of top-level AST nodes,
    // which is typically tens to low hundreds even for large files.
    while lines_used + markers > max_lines && selected.len() > 1 {
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
        return simple_line_truncate(text, language, max_lines);
    }

    // Build output with omission markers between gaps
    let prefix = get_comment_prefix(language);
    let suffix = get_comment_suffix(language);
    let omission_marker = format!("{} ... (truncated){}", prefix, suffix);

    let mut result_lines: Vec<&str> = Vec::with_capacity(max_lines);
    let mut last_end: usize = 0;

    // Check if there's content before the first selected span
    if selected[0].transformed_range.start > 0 {
        result_lines.push(&omission_marker);
    }

    for span in &selected {
        let start = span.transformed_range.start;
        let end = span.transformed_range.end.min(lines.len());

        // Insert omission marker if there's a gap from previous span
        if start > last_end && last_end > 0 {
            result_lines.push(&omission_marker);
        }

        // Add lines from this span (may need to clamp for the fallback case)
        // Reserve 1 line for a potential trailing omission marker
        let remaining_budget = max_lines.saturating_sub(result_lines.len() + 1);
        let span_end = end.min(start + remaining_budget);

        for line_idx in start..span_end {
            if line_idx < lines.len() {
                result_lines.push(lines[line_idx]);
            }
        }

        last_end = end;
    }

    // Trailing omission marker if there's content after last selected span
    if last_end < lines.len() && result_lines.len() < max_lines {
        result_lines.push(&omission_marker);
    }

    // Final enforcement: never exceed max_lines
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
/// Takes the first N-1 lines plus an omission marker.
pub(crate) fn simple_line_truncate(
    text: &str,
    language: Language,
    max_lines: usize,
) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();

    if lines.len() <= max_lines {
        return Ok(text.to_string());
    }

    let prefix = get_comment_prefix(language);
    let suffix = get_comment_suffix(language);
    let marker = format!(
        "{} ... ({} lines truncated){}",
        prefix,
        lines.len() - max_lines + 1,
        suffix
    );

    // Take first max_lines - 1 lines, then append marker
    let content_lines = max_lines.saturating_sub(1);
    let mut result: Vec<&str> = lines[..content_lines].to_vec();
    result.push(&marker);

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
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_truncation_when_within_budget() {
        let text = "line 1\nline 2\nline 3\n";
        let spans = vec![NodeSpan::new(0..3, "source_file")];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 10).unwrap();
        assert_eq!(result, text);
    }

    #[test]
    fn test_no_truncation_when_exact_budget() {
        let text = "line 1\nline 2\nline 3\n";
        let spans = vec![NodeSpan::new(0..3, "source_file")];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3).unwrap();
        let line_count = result.lines().count();
        assert!(
            line_count <= 3,
            "Expected at most 3 lines, got {}: {:?}",
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
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3).unwrap();
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
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3).unwrap();
        assert!(
            result.contains("type A"),
            "Should contain the type alias: {:?}",
            result
        );
    }

    #[test]
    fn test_omission_markers_between_gaps() {
        // 5 lines, budget of 3
        let text = "type A = string\nlet x = 1\nlet y = 2\nlet z = 3\ntype B = number\n";
        let spans = vec![
            NodeSpan::new(0..1, "type_alias_declaration"),
            NodeSpan::new(1..2, "expression_statement"),
            NodeSpan::new(2..3, "expression_statement"),
            NodeSpan::new(3..4, "expression_statement"),
            NodeSpan::new(4..5, "type_alias_declaration"),
        ];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 4).unwrap();
        assert!(
            result.contains("// ... (truncated)"),
            "Should contain omission marker: {:?}",
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

        let result = truncate_to_lines(text, &spans, Language::Python, 2).unwrap();
        assert!(
            result.contains("# ... (truncated)"),
            "Python should use # for omission marker: {:?}",
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

        let result = truncate_to_lines(text, &spans, Language::Markdown, 3).unwrap();
        assert!(
            result.contains("<!-- ... (truncated) -->"),
            "Markdown should use HTML comment for omission marker: {:?}",
            result
        );
    }

    #[test]
    fn test_empty_spans_falls_back_to_simple() {
        let text = "line 1\nline 2\nline 3\nline 4\nline 5\n";
        let spans: Vec<NodeSpan> = vec![];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3).unwrap();
        let line_count = result.lines().count();
        assert!(
            line_count <= 3,
            "Expected at most 3 lines, got {}",
            line_count
        );
    }

    #[test]
    fn test_simple_line_truncate() {
        let text = "line 1\nline 2\nline 3\nline 4\nline 5\n";

        let result = simple_line_truncate(text, Language::TypeScript, 3).unwrap();
        let line_count = result.lines().count();
        assert!(
            line_count <= 3,
            "Expected at most 3 lines, got {}",
            line_count
        );
        assert!(result.contains("line 1"));
        assert!(result.contains("line 2"));
        assert!(result.contains("// ... (3 lines truncated)"));
    }

    #[test]
    fn test_simple_line_truncate_no_truncation() {
        let text = "line 1\nline 2\n";

        let result = simple_line_truncate(text, Language::TypeScript, 5).unwrap();
        assert_eq!(result, text);
    }

    #[test]
    fn test_max_lines_1_returns_one_line() {
        let text = "type A = string\nfunction foo() {}\n";
        let spans = vec![
            NodeSpan::new(0..1, "type_alias_declaration"),
            NodeSpan::new(1..2, "function_declaration"),
        ];

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 1).unwrap();
        let line_count = result.lines().count();
        assert!(
            line_count <= 1,
            "Expected at most 1 line, got {}: {:?}",
            line_count,
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 5).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 5).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3).unwrap();
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
        // The truncation engine clamps effective_budget to 1, selects a span, then
        // result_lines.truncate(0) empties the output. However, the trailing-newline
        // preservation step appends '\n' when the original text ends with '\n',
        // producing "\n" -- a single empty trailing newline.
        // This test documents that edge behavior since 0 is not a valid input in practice.
        let text = "type A = string\nfunction foo() {}\n";
        let spans = vec![
            NodeSpan::new(0..1, "type_alias_declaration"),
            NodeSpan::new(1..2, "function_declaration"),
        ];

        // Should not panic
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 0).unwrap();
        // result_lines is empty after truncate(0), but trailing '\n' is preserved
        // from the original, so output is "\n"
        assert_eq!(
            result, "\n",
            "max_lines=0 with trailing newline should produce only the preserved newline"
        );
    }

    #[test]
    fn test_simple_line_truncate_max_lines_zero_does_not_panic() {
        // CONTRACT: max_lines=0 at simple_line_truncate level. The function uses
        // saturating_sub(1) so content_lines=0, producing only the marker line.
        // Then truncation to 0 would clip everything. This documents the edge behavior.
        let text = "line 1\nline 2\nline 3\n";

        let result = simple_line_truncate(text, Language::TypeScript, 0).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 4).unwrap();
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 4).unwrap();
        let line_count = result.lines().count();
        assert!(
            line_count <= 4,
            "Adjacent spans should not cause output to exceed budget of 4 lines, got {}: {:?}",
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

        let result = truncate_to_lines(text, &spans, Language::TypeScript, 5).unwrap();
        let result_lines: Vec<&str> = result.lines().collect();

        assert!(
            result_lines.len() <= 5,
            "Output should not exceed 5 lines, got {}: {:?}",
            result_lines.len(),
            result
        );
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
        // Function should be trimmed because markers + content > budget
        assert!(
            !result.contains("fn foo()"),
            "Function should be trimmed to make room for markers: {:?}",
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
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 3).unwrap();

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
        // Output respects budget
        assert!(result.lines().count() <= 3);
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
        let result = truncate_to_lines(text, &spans, Language::TypeScript, 2).unwrap();

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
}
