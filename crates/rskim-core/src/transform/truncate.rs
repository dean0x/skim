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
    let lines: Vec<&str> = text.lines().collect();

    // If output fits, return unchanged
    if lines.len() <= max_lines {
        return Ok(text.to_string());
    }

    // If no spans provided, fall back to simple line truncation
    if spans.is_empty() {
        return simple_line_truncate(text, language, max_lines);
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
    let mut scored: Vec<(u8, usize, &NodeSpan)> = valid_spans
        .iter()
        .enumerate()
        .map(|(idx, span)| (score_node_kind(span.node_kind), idx, *span))
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0).then_with(|| {
            a.2.transformed_range
                .start
                .cmp(&b.2.transformed_range.start)
        })
    });

    // Reserve budget for omission markers (estimate: at most one marker per gap)
    let marker_budget = 2;
    let effective_budget = if max_lines > marker_budget {
        max_lines - marker_budget
    } else {
        // Very tight budget - try to fit at least one span
        1
    };

    // Greedy selection: pick spans that fit within effective_budget
    let mut selected: Vec<&NodeSpan> = Vec::new();
    let mut lines_used: usize = 0;

    for &(_, _, span) in &scored {
        let span_lines = span.line_count();

        // Clamp span end to actual line count
        let clamped_end = span.transformed_range.end.min(lines.len());
        let clamped_lines = if clamped_end > span.transformed_range.start {
            clamped_end - span.transformed_range.start
        } else {
            continue;
        };

        if lines_used + clamped_lines <= effective_budget {
            selected.push(span);
            lines_used += clamped_lines;
        } else if selected.is_empty() && span_lines > 0 {
            // Fallback: if no span fits, take first max_lines of highest-priority span
            selected.push(span);
            break;
        }
    }

    if selected.is_empty() {
        return simple_line_truncate(text, language, max_lines);
    }

    // Re-sort selected spans by position (reading order)
    selected.sort_by_key(|s| s.transformed_range.start);

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
        let remaining_budget = if max_lines > result_lines.len() {
            max_lines
                - result_lines
                    .len()
                    // Reserve 1 for potential trailing marker
                    .saturating_sub(1)
        } else {
            0
        };

        let span_end = if end - start > remaining_budget {
            start + remaining_budget
        } else {
            end
        };

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
}
