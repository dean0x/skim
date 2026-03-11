//! Minimal mode transformation
//!
//! ARCHITECTURE: Strip non-doc comments at module/class level while keeping all code intact.
//! Preserves doc comments, comments inside function bodies, and shebangs.
//!
//! Token reduction target: 15-30%

use crate::transform::utils::is_inside_function_body;
use crate::{Language, Result, SkimError, TransformConfig};
use tree_sitter::{Node, Tree};

/// Maximum AST recursion depth to prevent stack overflow attacks
const MAX_AST_DEPTH: usize = 500;

/// Maximum number of AST nodes to prevent memory exhaustion
const MAX_AST_NODES: usize = 100_000;

/// Transform source by stripping non-doc comments and normalizing blank lines
///
/// Three-pass algorithm:
/// 1. Walk AST collecting byte ranges of non-doc comment nodes to remove
///    (skip doc comments, skip comments inside function bodies, skip shebangs)
/// 2. Remove collected ranges from source, trim trailing whitespace on affected lines
/// 3. Normalize blank lines (3+ consecutive -> 2)
pub(crate) fn transform_minimal(
    source: &str,
    tree: &Tree,
    language: Language,
    _config: &TransformConfig,
) -> Result<String> {
    let mut ranges_to_remove: Vec<(usize, usize)> = Vec::new();
    let mut node_count = 0;
    collect_removable_comments(
        tree.root_node(),
        source,
        language,
        &mut ranges_to_remove,
        &mut node_count,
        0,
    )?;

    ranges_to_remove.sort_unstable_by_key(|&(start, _)| start);
    ranges_to_remove.dedup();

    let after_removal = remove_ranges(source, &ranges_to_remove)?;
    let normalized = normalize_blank_lines(&after_removal);

    Ok(normalized)
}

/// Recursively collect byte ranges of comment nodes that should be removed
///
/// # Security
/// - Enforces MAX_AST_DEPTH to prevent stack overflow
/// - Enforces MAX_AST_NODES to prevent memory exhaustion
fn collect_removable_comments(
    node: Node,
    source: &str,
    language: Language,
    ranges: &mut Vec<(usize, usize)>,
    node_count: &mut usize,
    depth: usize,
) -> Result<()> {
    // SECURITY: Prevent stack overflow from deeply nested AST
    if depth > MAX_AST_DEPTH {
        return Err(SkimError::ParseError(format!(
            "Maximum AST depth exceeded: {} (possible malicious input)",
            MAX_AST_DEPTH
        )));
    }

    // SECURITY: Prevent memory exhaustion from excessive nodes
    *node_count += 1;
    if *node_count > MAX_AST_NODES {
        return Err(SkimError::ParseError(format!(
            "Too many AST nodes: {} (max: {}). Possible malicious input.",
            *node_count, MAX_AST_NODES
        )));
    }

    let kind = node.kind();

    if is_comment_node(kind, language) {
        let should_preserve = is_shebang(node, source)
            || is_inside_function_body(node, language)
            || is_doc_comment(node, source, language);

        if !should_preserve {
            let start = node.start_byte();
            let end = node.end_byte();
            let (adjusted_start, adjusted_end) = adjust_range_for_line_removal(source, start, end);
            ranges.push((adjusted_start, adjusted_end));
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_removable_comments(child, source, language, ranges, node_count, depth + 1)?;
    }

    Ok(())
}

/// Check if a comment node is a shebang line (e.g., `#!/usr/bin/env python3`)
///
/// Shebangs must start at byte 0 and begin with `#!`.
fn is_shebang(node: Node, source: &str) -> bool {
    if node.start_byte() != 0 {
        return false;
    }
    node.utf8_text(source.as_bytes())
        .map(|text| text.starts_with("#!"))
        .unwrap_or(false)
}

/// Check if a node kind represents a comment in the given language
fn is_comment_node(kind: &str, language: Language) -> bool {
    match language {
        Language::TypeScript | Language::JavaScript | Language::Python | Language::Go => {
            kind == "comment"
        }
        Language::Rust | Language::Java => kind == "line_comment" || kind == "block_comment",
        // Markdown, JSON, YAML don't have comment nodes to strip
        Language::Markdown | Language::Json | Language::Yaml => false,
    }
}

/// Check if a comment node is a doc comment that should be preserved
///
/// Doc comment detection per language:
/// - TypeScript/JS: Starts with `/**`
/// - Python: Comment nodes are `#` -- docstrings are `expression_statement > string`, not comments
/// - Rust: `///`, `//!`, `/**`, `/*!`
/// - Go: Adjacent to a declaration (next non-comment sibling is a declaration)
/// - Java: Starts with `/**`
fn is_doc_comment(node: Node, source: &str, language: Language) -> bool {
    let text = match node.utf8_text(source.as_bytes()) {
        Ok(t) => t,
        Err(_) => return false,
    };

    match language {
        Language::TypeScript | Language::JavaScript => {
            // JSDoc comments start with /**
            text.starts_with("/**")
        }
        Language::Python => {
            // Python docstrings are expression_statement > string nodes, NOT comment nodes.
            // All Python `comment` nodes (starting with #) at module level are regular comments.
            false
        }
        Language::Rust => {
            // Rust doc comments: ///, //!, /**, /*!
            text.starts_with("///")
                || text.starts_with("//!")
                || text.starts_with("/**")
                || text.starts_with("/*!")
        }
        Language::Go => {
            // Go doc comments are comments that are adjacent to a declaration.
            // Walk forward through siblings to find next non-comment named sibling.
            is_go_doc_comment(node, source)
        }
        Language::Java => {
            // Javadoc comments start with /**
            text.starts_with("/**")
        }
        // Markdown, JSON, YAML don't reach here
        Language::Markdown | Language::Json | Language::Yaml => false,
    }
}

/// Check if a Go comment is a doc comment (adjacent to a declaration)
///
/// Go doc comments are comments that immediately precede a declaration
/// (function, type, var, const) with no blank lines between them.
/// Walks forward through siblings to find the end of the contiguous comment
/// block and checks whether a declaration immediately follows.
fn is_go_doc_comment(node: Node, source: &str) -> bool {
    let mut current_end = node.end_byte();
    let mut sibling = node.next_named_sibling();

    while let Some(sib) = sibling {
        let sib_start = sib.start_byte();

        if current_end <= sib_start && sib_start <= source.len() {
            let between = &source[current_end..sib_start];
            let newline_count = between.chars().filter(|&c| c == '\n').count();
            if newline_count > 1 {
                return false;
            }
        }

        if is_comment_node(sib.kind(), Language::Go) {
            current_end = sib.end_byte();
            sibling = sib.next_named_sibling();
            continue;
        }

        return is_go_declaration(sib.kind());
    }

    false
}

/// Check if a Go node kind is a declaration type
fn is_go_declaration(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "method_declaration"
            | "type_declaration"
            | "var_declaration"
            | "const_declaration"
            | "type_spec"
    )
}

/// Adjust a comment range to remove the entire line if the comment is the only
/// non-whitespace content on that line.
///
/// If the comment occupies the full line (only whitespace before/after on same line),
/// remove the entire line including the newline. Otherwise, just remove the comment
/// and any leading whitespace before it on the same line (for inline trailing comments).
fn adjust_range_for_line_removal(source: &str, start: usize, end: usize) -> (usize, usize) {
    // Find the start of the line containing this comment
    let line_start = source[..start].rfind('\n').map(|pos| pos + 1).unwrap_or(0);

    // Find the end of the line containing this comment
    let line_end = source[end..]
        .find('\n')
        .map(|pos| end + pos + 1)
        .unwrap_or(source.len());

    // Check if the comment is the only non-whitespace content on the line
    let before_comment = &source[line_start..start];
    let after_comment = if end < line_end {
        let after_end = if line_end > 0 && source.as_bytes().get(line_end - 1) == Some(&b'\n') {
            line_end - 1
        } else {
            line_end
        };
        &source[end..after_end]
    } else {
        ""
    };

    let only_whitespace_before = before_comment.chars().all(|c| c.is_whitespace());
    let only_whitespace_after = after_comment.chars().all(|c| c.is_whitespace());

    if only_whitespace_before && only_whitespace_after {
        // Comment is the only content on this line - remove the entire line
        (line_start, line_end)
    } else if only_whitespace_after {
        // Inline trailing comment: remove leading whitespace before the comment too
        let trimmed_start = source[line_start..start].trim_end().len() + line_start;
        (trimmed_start, end)
    } else {
        // Comment is in the middle or start of a line with other content - just remove the comment
        (start, end)
    }
}

/// Remove collected byte ranges from source
///
/// Builds a new string by copying everything except the removed ranges.
/// Also trims trailing whitespace on lines where content was removed.
fn remove_ranges(source: &str, ranges: &[(usize, usize)]) -> Result<String> {
    if ranges.is_empty() {
        return Ok(source.to_string());
    }

    let mut result = String::with_capacity(source.len());
    let mut last_pos = 0;

    for &(start, end) in ranges {
        if end < start {
            return Err(SkimError::ParseError(format!(
                "Invalid range: start={} end={}",
                start, end
            )));
        }
        if end > source.len() {
            return Err(SkimError::ParseError(format!(
                "Range exceeds source length: end={} len={}",
                end,
                source.len()
            )));
        }

        if start < last_pos {
            if end > last_pos {
                last_pos = end;
            }
            continue;
        }

        if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
            return Err(SkimError::ParseError(format!(
                "Invalid UTF-8 boundary at range [{}, {})",
                start, end
            )));
        }

        result.push_str(&source[last_pos..start]);
        last_pos = end;
    }

    if !source.is_char_boundary(last_pos) {
        return Err(SkimError::ParseError(format!(
            "Invalid UTF-8 boundary at position {}",
            last_pos
        )));
    }

    result.push_str(&source[last_pos..]);

    Ok(trim_trailing_whitespace_on_lines(&result))
}

/// Trim trailing whitespace from each line
fn trim_trailing_whitespace_on_lines(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut first = true;

    for line in source.lines() {
        if !first {
            result.push('\n');
        }
        result.push_str(line.trim_end());
        first = false;
    }

    if source.ends_with('\n') {
        result.push('\n');
    }

    result
}

/// Normalize blank lines: 3+ consecutive blank lines become 2
///
/// A "blank line" is a line containing only whitespace.
fn normalize_blank_lines(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut consecutive_blanks: usize = 0;
    let mut first = true;

    for line in source.lines() {
        if line.trim().is_empty() {
            consecutive_blanks += 1;
            if consecutive_blanks > 2 {
                continue;
            }
        } else {
            consecutive_blanks = 0;
        }

        if !first {
            result.push('\n');
        }
        result.push_str(line);
        first = false;
    }

    if source.ends_with('\n') {
        result.push('\n');
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_blank_lines_preserves_two() {
        let input = "a\n\n\nb\n";
        let result = normalize_blank_lines(input);
        assert_eq!(result, "a\n\n\nb\n");
    }

    #[test]
    fn test_normalize_blank_lines_reduces_four_to_two() {
        let input = "a\n\n\n\n\nb\n";
        let result = normalize_blank_lines(input);
        assert_eq!(result, "a\n\n\nb\n");
    }

    #[test]
    fn test_normalize_blank_lines_no_change_needed() {
        let input = "a\n\nb\n";
        let result = normalize_blank_lines(input);
        assert_eq!(result, "a\n\nb\n");
    }

    #[test]
    fn test_adjust_range_full_line_comment() {
        let source = "code\n// comment\nmore code\n";
        // "// comment" starts at byte 5, ends at byte 15
        let (start, end) = adjust_range_for_line_removal(source, 5, 15);
        // Should remove the entire line including newline
        assert_eq!(start, 5);
        assert_eq!(end, 16); // includes the newline
    }

    #[test]
    fn test_trim_trailing_whitespace() {
        let input = "hello   \nworld  \n";
        let result = trim_trailing_whitespace_on_lines(input);
        assert_eq!(result, "hello\nworld\n");
    }
}
