//! Field classification for Markdown files.
//!
//! Uses line-by-line scanning (headings, code fences, links)
//! rather than tree-sitter or serde parsing.
//!
//! # Classification rules
//!
//! - `^#{1,3} ` → TypeDefinition (H1-H3 headings)
//! - Lines between ` ``` ` fences → FunctionBody (code blocks)
//! - Lines matching `[text](url)` pattern → ImportExport (links)
//! - All other non-empty lines → Comment (prose)

use std::ops::Range;

use crate::SearchField;

/// Classify regions in Markdown content into `SearchField` spans.
///
/// Returns `(byte_range, SearchField)` pairs for each semantically meaningful line.
pub fn classify_markdown_fields(
    source: &str,
) -> crate::Result<Vec<(Range<usize>, SearchField)>> {
    let mut results = Vec::new();
    let mut byte_offset: usize = 0;
    let mut in_code_block = false;

    for line in source.lines() {
        let line_len = line.len();
        let trimmed = line.trim();

        // Toggle code fence state.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code_block = !in_code_block;
            // The fence line itself is part of the code block region.
            results.push((byte_offset..byte_offset + line_len, SearchField::FunctionBody));
            byte_offset += line_len + 1;
            continue;
        }

        if in_code_block {
            // Inside a code block — classify as FunctionBody.
            if !trimmed.is_empty() {
                results.push((byte_offset..byte_offset + line_len, SearchField::FunctionBody));
            }
            byte_offset += line_len + 1;
            continue;
        }

        // Skip blank lines outside code blocks.
        if trimmed.is_empty() {
            byte_offset += line_len + 1;
            continue;
        }

        // Headings: H1-H3 (`# `, `## `, `### `).
        if is_heading(trimmed) {
            results.push((byte_offset..byte_offset + line_len, SearchField::TypeDefinition));
            byte_offset += line_len + 1;
            continue;
        }

        // Links: any line containing at least one `[text](url)` pattern.
        if contains_markdown_link(trimmed) {
            results.push((byte_offset..byte_offset + line_len, SearchField::ImportExport));
            byte_offset += line_len + 1;
            continue;
        }

        // Default: prose → Comment.
        results.push((byte_offset..byte_offset + line_len, SearchField::Comment));
        byte_offset += line_len + 1;
    }

    Ok(results)
}

// ============================================================================
// Private helpers
// ============================================================================

/// Returns true if the line is an H1, H2, or H3 heading.
///
/// Matches `# `, `## `, or `### ` at the start of a trimmed line.
/// H4-H6 (`####` etc.) are NOT classified as TypeDefinition to keep the
/// signal strong (only primary structural headings matter for search).
fn is_heading(trimmed: &str) -> bool {
    matches!(
        trimmed.split_once(' '),
        Some(("#", _)) | Some(("##", _)) | Some(("###", _))
    )
}

/// Returns true if the line contains a Markdown link pattern `[text](url)`.
///
/// Uses manual scanning instead of regex (no `regex` dep in rskim-search).
fn contains_markdown_link(line: &str) -> bool {
    // Simple FSM: look for `[...](...)`
    let bytes = line.as_bytes();
    let len = bytes.len();
    let mut i = 0;
    while i < len {
        if bytes[i] == b'[' {
            // Find matching `]`
            if let Some(close_bracket) = bytes[i + 1..].iter().position(|&b| b == b']') {
                let after_bracket = i + 1 + close_bracket + 1; // position after `]`
                if after_bracket < len && bytes[after_bracket] == b'(' {
                    // Find matching `)`
                    if bytes[after_bracket + 1..].contains(&b')') {
                        return true;
                    }
                }
            }
        }
        i += 1;
    }
    false
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_source_is_empty() {
        let result = classify_markdown_fields("").expect("should succeed");
        assert!(result.is_empty());
    }

    #[test]
    fn blank_lines_only_is_empty() {
        let result = classify_markdown_fields("\n\n\n").expect("should succeed");
        assert!(result.is_empty());
    }

    #[test]
    fn h1_is_type_definition() {
        let source = "# Hello World\n";
        let result = classify_markdown_fields(source).expect("should succeed");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].1, SearchField::TypeDefinition);
    }

    #[test]
    fn h2_is_type_definition() {
        let source = "## Section\n";
        let result = classify_markdown_fields(source).expect("should succeed");
        assert_eq!(result[0].1, SearchField::TypeDefinition);
    }

    #[test]
    fn h3_is_type_definition() {
        let source = "### Subsection\n";
        let result = classify_markdown_fields(source).expect("should succeed");
        assert_eq!(result[0].1, SearchField::TypeDefinition);
    }

    #[test]
    fn h4_is_not_type_definition() {
        let source = "#### Deep\n";
        let result = classify_markdown_fields(source).expect("should succeed");
        // H4+ is treated as prose (Comment), not TypeDefinition.
        assert_ne!(result[0].1, SearchField::TypeDefinition);
    }

    #[test]
    fn prose_is_comment() {
        let source = "This is ordinary prose.\n";
        let result = classify_markdown_fields(source).expect("should succeed");
        assert_eq!(result[0].1, SearchField::Comment);
    }

    #[test]
    fn code_block_is_function_body() {
        let source = "```rust\nfn main() {}\n```\n";
        let result = classify_markdown_fields(source).expect("should succeed");
        for (_, field) in &result {
            assert_eq!(*field, SearchField::FunctionBody, "all code block lines should be FunctionBody");
        }
    }

    #[test]
    fn link_line_is_import_export() {
        let source = "See [docs](https://example.com) for more.\n";
        let result = classify_markdown_fields(source).expect("should succeed");
        assert_eq!(result[0].1, SearchField::ImportExport);
    }

    #[test]
    fn mixed_content_classifies_correctly() {
        let source = "# Title\nSome prose.\n[link](http://x.com)\n```\ncode\n```\n";
        let result = classify_markdown_fields(source).expect("should succeed");

        let fields: Vec<SearchField> = result.iter().map(|(_, f)| *f).collect();

        // Title → TypeDefinition
        assert!(fields.contains(&SearchField::TypeDefinition));
        // Prose → Comment
        assert!(fields.contains(&SearchField::Comment));
        // Link → ImportExport
        assert!(fields.contains(&SearchField::ImportExport));
        // Code block → FunctionBody
        assert!(fields.contains(&SearchField::FunctionBody));
    }

    #[test]
    fn byte_ranges_are_within_source_bounds() {
        let source = "# Title\nProse line.\n";
        let result = classify_markdown_fields(source).expect("should succeed");
        for (range, _) in &result {
            assert!(range.end <= source.len(), "range {:?} out of bounds for source len {}", range, source.len());
        }
    }
}
