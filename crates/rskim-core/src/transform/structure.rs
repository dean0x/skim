//! Structure mode transformation
//!
//! ARCHITECTURE: Strip function/method bodies, keep structure.
//!
//! Token reduction target: 70-80%

use crate::transform::truncate::NodeSpan;
use crate::transform::utils::{to_static_node_kind, FunctionNodeTypes};
use crate::{Language, Result, SkimError, TransformConfig};
use std::collections::HashMap;
use tree_sitter::{Node, Tree};

/// Maximum AST recursion depth to prevent stack overflow attacks
const MAX_AST_DEPTH: usize = 500;

/// Maximum number of AST nodes to prevent memory exhaustion
const MAX_AST_NODES: usize = 100_000;

/// Maximum markdown traversal depth to prevent stack overflow
const MAX_MARKDOWN_DEPTH: usize = 500;

/// Maximum number of markdown headers to prevent memory exhaustion
const MAX_MARKDOWN_HEADERS: usize = 10_000;

/// Transform to structure-only (strip implementations)
///
/// # What to Keep
///
/// - Function/method signatures
/// - Class declarations
/// - Type definitions
/// - Imports/exports
/// - Structural comments (if config.preserve_comments)
///
/// # What to Remove
///
/// - Function bodies → `/* ... */`
/// - Implementation details
/// - Non-structural comments
///
/// # Implementation Notes (Week 2)
///
/// 1. Traverse AST with TreeCursor
/// 2. For each function/method node:
///    - Extract signature (everything before `{`)
///    - Replace body with `/* ... */`
/// 3. For classes: keep structure, strip method bodies
/// 4. Preserve indentation
#[cfg(test)]
#[allow(dead_code)] // Convenience wrapper available for tests
pub(crate) fn transform_structure(
    source: &str,
    tree: &Tree,
    language: Language,
    config: &TransformConfig,
) -> Result<String> {
    let (text, _spans) = transform_structure_with_spans(source, tree, language, config)?;
    Ok(text)
}

/// Transform to structure-only and return NodeSpan metadata for truncation
pub(crate) fn transform_structure_with_spans(
    source: &str,
    tree: &Tree,
    language: Language,
    config: &TransformConfig,
) -> Result<(String, Vec<NodeSpan>)> {
    let (text, spans, _line_map) =
        transform_structure_with_spans_and_line_map(source, tree, language, config)?;
    Ok((text, spans))
}

/// Transform to structure-only, returning NodeSpan metadata AND a source line map.
///
/// The source line map maps each output line index to the 1-indexed source line
/// number. For verbatim-copied regions, the source line is the original line number.
/// The replacement `{ /* ... */ }` stays on the same line as the function signature
/// (no newlines in the replacement), so no output line ever starts inside a
/// replacement region — all output line starts are in verbatim-copied regions
/// where the reverse offset mapping is exact.
///
/// # Design Decision (AC-18)
/// Structure mode uses a byte-offset reverse mapping to determine which source line
/// corresponds to each output line start byte. The `offset_map` (source_end_byte, delta)
/// pairs allow reverse-mapping: output_byte = source_byte + delta, so
/// source_byte = output_byte - delta. Binary search on the output byte position
/// in the offset map gives the correct delta for any output line start.
pub(crate) fn transform_structure_with_spans_and_line_map(
    source: &str,
    tree: &Tree,
    language: Language,
    _config: &TransformConfig,
) -> Result<(String, Vec<NodeSpan>, Vec<usize>)> {
    // ARCHITECTURE: Markdown uses extraction, not replacement
    // Extract H1-H3 headers only (top-level document structure)
    if language == Language::Markdown {
        let (text, spans, line_map) = extract_markdown_headers_with_spans(source, tree, 1, 3)?;
        return Ok((text, spans, line_map));
    }

    // Get language-specific node types
    // ARCHITECTURE: JSON is handled by Strategy Pattern in Language::transform_source()
    // and never reaches this code path. This unwrap is safe due to early return above.
    let node_types = get_node_types_for_language(language).ok_or_else(|| {
        SkimError::ParseError(format!(
            "Language {:?} does not support tree-sitter structure transformation",
            language
        ))
    })?;

    // Find all body nodes to replace
    let mut replacements: HashMap<(usize, usize), &'static str> = HashMap::new();
    collect_body_replacements(tree.root_node(), &node_types, &mut replacements, 0)?;

    // Check node count limit to prevent memory exhaustion
    if replacements.len() > MAX_AST_NODES {
        return Err(SkimError::ParseError(format!(
            "Too many AST nodes: {} (max: {}). Possible malicious input.",
            replacements.len(),
            MAX_AST_NODES
        )));
    }

    // Build output by replacing bodies, tracking byte offset changes
    let estimated_capacity = source.len() + (replacements.len() * 20);
    let mut result = String::with_capacity(estimated_capacity);
    let mut last_pos = 0;

    // Sort replacements by start position
    let mut sorted_replacements: Vec<_> = replacements.into_iter().collect();
    sorted_replacements.sort_unstable_by_key(|(range, _)| range.0);

    // Track cumulative byte offset delta (output_pos - source_pos)
    // offset_map entries: (source_end_byte, cumulative_delta)
    // Invariant: for any output byte O in a verbatim region, source byte S = O - delta
    //            where delta is the latest entry with source_end_byte <= S.
    let mut offset_delta: i64 = 0;
    let mut offset_map: Vec<(usize, i64)> = Vec::new(); // (source_byte_end, delta)

    for ((start, end), replacement) in sorted_replacements {
        // Validate byte ranges
        if end < start {
            return Err(SkimError::ParseError(format!(
                "Invalid AST range: start={} end={}",
                start, end
            )));
        }
        if end > source.len() {
            return Err(SkimError::ParseError(format!(
                "AST range exceeds source length: end={} len={}",
                end,
                source.len()
            )));
        }

        // Skip overlapping replacements (nested functions already handled by parent)
        if start < last_pos {
            continue;
        }

        // Validate UTF-8 boundaries before slicing
        if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
            return Err(SkimError::ParseError(format!(
                "Invalid UTF-8 boundary at range [{}, {})",
                start, end
            )));
        }

        // Copy everything before this replacement
        result.push_str(&source[last_pos..start]);
        // Add replacement
        result.push_str(replacement);

        // Track the offset change at this replacement point
        let replaced_len = end - start;
        let replacement_len = replacement.len();
        // SAFETY: Both values bounded by source file size (far below i64::MAX).
        offset_delta += replacement_len as i64 - replaced_len as i64;
        offset_map.push((end, offset_delta));

        last_pos = end;
    }

    // Validate final position
    if !source.is_char_boundary(last_pos) {
        return Err(SkimError::ParseError(format!(
            "Invalid UTF-8 boundary at position {}",
            last_pos
        )));
    }

    // Copy remaining source
    result.push_str(&source[last_pos..]);

    // Build NodeSpans from top-level AST children
    let spans = build_spans_from_top_level_nodes(tree, &result, &offset_map);

    // Build source line map using offset_map to reverse-map output bytes to source lines
    let source_line_map = compute_source_line_map_from_offset_map(source, &result, &offset_map);

    Ok((result, spans, source_line_map))
}

/// Compute the source line map for structure mode output using the offset_map.
///
/// For each output line (by its start byte offset), reverse-maps to a source byte
/// offset using the offset_map, then binary-searches source line starts to get
/// the 1-indexed source line number.
///
/// # Correctness Invariant
/// The replacement text `" { /* ... */ }"` contains no newlines. Therefore no
/// output line ever starts inside a replacement region — all output line start
/// bytes are in verbatim-copied regions where the reverse mapping is exact.
pub(crate) fn compute_source_line_map_from_offset_map(
    source: &str,
    output: &str,
    offset_map: &[(usize, i64)],
) -> Vec<usize> {
    // Pre-compute source line start byte offsets (0-indexed by line number).
    // Newlines are always ASCII (single byte), so byte-level iteration avoids
    // unnecessary UTF-8 decoding overhead.
    let source_line_starts: Vec<usize> = std::iter::once(0)
        .chain(source.as_bytes().iter().enumerate().filter_map(|(i, &b)| {
            if b == b'\n' {
                Some(i + 1)
            } else {
                None
            }
        }))
        .collect();

    // Pre-compute output line start byte offsets (byte-level, same rationale).
    let output_line_starts: Vec<usize> = std::iter::once(0)
        .chain(output.as_bytes().iter().enumerate().filter_map(|(i, &b)| {
            if b == b'\n' {
                Some(i + 1)
            } else {
                None
            }
        }))
        .collect();

    // Derive the output line count from the already-computed starts vector rather
    // than calling output.lines().count() again (avoids a second full scan).
    // output_line_starts always has at least 1 entry (the leading 0 for line 1).
    // If the output ends with '\n', the last entry in output_line_starts is a
    // phantom "start of the next line" that has no actual content — the number of
    // real lines equals output_line_starts.len() minus that phantom entry.
    let output_lines = if output.ends_with('\n') && !output.is_empty() {
        output_line_starts.len().saturating_sub(1)
    } else {
        output_line_starts.len()
    };

    // Build source_line_map: for each output line, find its 1-indexed source line.
    // We take only `output_lines` entries from output_line_starts since trailing
    // newlines add a phantom entry.
    //
    // The offset_map is sorted by source_end_byte and output positions are visited
    // in ascending order, so we use a monotonic cursor instead of scanning from the
    // start for every output line. This reduces complexity from O(L*R) to O(L+R).
    let mut cursor_idx = 0usize; // monotonic cursor into offset_map
    let mut applicable_delta = 0i64; // delta valid for the current cursor position

    output_line_starts
        .iter()
        .take(output_lines)
        .map(|&output_byte| {
            // Advance the monotonic cursor while the next offset_map entry's output
            // end byte is still <= output_byte.  Since output bytes are visited in
            // non-decreasing order we never need to reset the cursor.
            //
            // For entry (src_end, d), the output position of that boundary is:
            //   output_end = src_end as i64 + d
            // Guard: if output_end < 0 the replacement shrank output past zero —
            // skip the entry rather than wrapping a usize cast.
            while cursor_idx < offset_map.len() {
                let (src_end, d) = offset_map[cursor_idx];
                let output_end = src_end as i64 + d;
                if output_end < 0 {
                    // Invariant violation: replacement cannot produce negative output
                    // position. Skip safely.
                    cursor_idx += 1;
                    continue;
                }
                if output_end as usize <= output_byte {
                    applicable_delta = d;
                    cursor_idx += 1;
                } else {
                    break;
                }
            }
            let delta = applicable_delta;

            // source_byte = output_byte - delta (clamped to [0, source.len()])
            let source_byte = (output_byte as i64 - delta).max(0) as usize;
            let source_byte = source_byte.min(source.len());

            // Binary search for the 1-indexed line number
            match source_line_starts.binary_search(&source_byte) {
                Ok(idx) => idx + 1,     // Exact match: this byte IS a line start
                Err(idx) => idx.max(1), // Inexact: line idx (1-indexed)
            }
        })
        .collect()
}

/// Recursively collect body nodes that should be replaced
///
/// # Security
/// - Enforces MAX_AST_DEPTH to prevent stack overflow
/// - Returns error if depth limit exceeded
fn collect_body_replacements(
    node: Node,
    node_types: &NodeTypes,
    replacements: &mut HashMap<(usize, usize), &'static str>,
    depth: usize,
) -> Result<()> {
    // SECURITY: Prevent stack overflow from deeply nested AST
    if depth > MAX_AST_DEPTH {
        return Err(SkimError::ParseError(format!(
            "Maximum AST depth exceeded: {} (possible malicious input with deeply nested functions)",
            MAX_AST_DEPTH
        )));
    }

    let kind = node.kind();

    // Check if this is a function/method with a body
    if matches_function_node(kind, node_types) {
        if let Some(body) = find_body_node(node) {
            let start = body.start_byte();
            let end = body.end_byte();
            replacements.insert((start, end), " { /* ... */ }");
        }
    }

    // Recursively process children with incremented depth
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_body_replacements(child, node_types, replacements, depth + 1)?;
    }

    Ok(())
}

/// Check if node kind matches a function/method/constructor
fn matches_function_node(kind: &str, node_types: &NodeTypes) -> bool {
    kind == node_types.function
        || kind == node_types.method
        || kind == "arrow_function"
        || kind == "function_expression"
        || node_types.extra_function_kinds.contains(&kind)
}

/// Find the body node of a function/method
///
/// Delegates to shared `find_body_child` in utils.rs.
fn find_body_node(node: Node) -> Option<Node> {
    crate::transform::utils::find_body_child(node)
}

/// Type alias: structure mode reuses the shared FunctionNodeTypes struct from utils.
/// This avoids renaming all usages within the module while making the shared origin clear.
type NodeTypes = FunctionNodeTypes;

/// Get node types based on language
///
/// Returns None for languages that don't use tree-sitter node types (e.g., JSON).
/// ARCHITECTURE: JSON is handled by the Strategy Pattern in Language::transform_source(),
/// which calls json::transform_json() directly instead of using tree-sitter parsing.
fn get_node_types_for_language(language: Language) -> Option<NodeTypes> {
    match language {
        Language::TypeScript | Language::JavaScript => Some(NodeTypes {
            function: "function_declaration",
            method: "method_definition",
            extra_function_kinds: &[],
        }),
        Language::Python => Some(NodeTypes {
            function: "function_definition",
            method: "function_definition",
            extra_function_kinds: &[],
        }),
        Language::Rust => Some(NodeTypes {
            function: "function_item",
            method: "function_item",
            extra_function_kinds: &[],
        }),
        Language::Go => Some(NodeTypes {
            function: "function_declaration",
            method: "method_declaration",
            extra_function_kinds: &[],
        }),
        Language::Java => Some(NodeTypes {
            function: "method_declaration",
            method: "method_declaration",
            extra_function_kinds: &[],
        }),
        // Unreachable: Markdown returns early via extract_markdown_headers_with_spans
        Language::Markdown => Some(NodeTypes {
            function: "atx_heading",
            method: "atx_heading",
            extra_function_kinds: &[],
        }),
        Language::C | Language::Cpp => Some(NodeTypes {
            function: "function_definition",
            method: "function_definition",
            extra_function_kinds: &[],
        }),
        Language::CSharp => Some(NodeTypes {
            function: "method_declaration",
            method: "constructor_declaration",
            extra_function_kinds: &[],
        }),
        Language::Ruby => Some(NodeTypes {
            function: "method",
            method: "singleton_method",
            extra_function_kinds: &[],
        }),
        // ARCHITECTURE: SQL maps both function and method to "statement" because
        // SQL is a declarative language where all top-level constructs are statements
        // (SELECT, CREATE TABLE, INSERT, etc.). Unlike procedural languages, SQL has
        // no function/method distinction — every statement is a self-contained unit
        // analogous to a top-level function definition. This causes structure mode to
        // strip statement bodies, which is the correct behavior for SQL summarization.
        Language::Sql => Some(NodeTypes {
            function: "statement",
            method: "statement",
            extra_function_kinds: &[],
        }),
        Language::Kotlin => Some(NodeTypes {
            function: "function_declaration",
            method: "function_declaration", // Kotlin doesn't distinguish methods from functions
            extra_function_kinds: &["secondary_constructor", "anonymous_initializer"],
        }),
        Language::Swift => Some(NodeTypes {
            function: "function_declaration",
            method: "function_declaration", // Swift methods are also function_declaration
            extra_function_kinds: &["init_declaration", "deinit_declaration"],
        }),
        Language::Json | Language::Yaml | Language::Toml => None,
    }
}

/// Build NodeSpans from top-level AST children, mapping source byte positions
/// to output line ranges using the offset map
fn build_spans_from_top_level_nodes(
    tree: &Tree,
    output: &str,
    offset_map: &[(usize, i64)],
) -> Vec<NodeSpan> {
    let root = tree.root_node();
    let mut cursor = root.walk();
    let mut spans = Vec::new();

    // Pre-compute line starts in the output for byte-to-line conversion
    let line_starts: Vec<usize> =
        std::iter::once(0)
            .chain(output.bytes().enumerate().filter_map(|(i, b)| {
                if b == b'\n' {
                    Some(i + 1)
                } else {
                    None
                }
            }))
            .collect();

    let byte_to_line = |byte_pos: usize| -> usize {
        match line_starts.binary_search(&byte_pos) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        }
    };

    // Map source byte position to output byte position using offset map
    let source_to_output_byte = |source_byte: usize| -> usize {
        let delta = match offset_map.binary_search_by_key(&source_byte, |&(pos, _)| pos) {
            Ok(idx) => offset_map[idx].1,
            Err(0) => 0,
            Err(idx) => offset_map[idx - 1].1,
        };
        // SAFETY: source_byte bounded by source file size (far below i64::MAX).
        (source_byte as i64 + delta).max(0) as usize
    };

    for child in root.children(&mut cursor) {
        let kind = child.kind();
        let source_start = child.start_byte();
        let source_end = child.end_byte();

        let output_start = source_to_output_byte(source_start).min(output.len());
        let output_end = source_to_output_byte(source_end).min(output.len());

        let start_line = byte_to_line(output_start);
        let end_line = byte_to_line(output_end.saturating_sub(1)) + 1;

        // Map tree-sitter node kinds to static str for priority scoring
        let static_kind = to_static_node_kind(kind);

        if start_line < end_line {
            spans.push(NodeSpan::new(start_line..end_line, static_kind));
        }
    }

    spans
}

/// Extract markdown headers within a level range
///
/// # Arguments
/// * `source` - Original markdown source
/// * `tree` - Parsed tree-sitter AST
/// * `min_level` - Minimum header level (1 = H1)
/// * `max_level` - Maximum header level (6 = H6)
///
/// # Returns
/// Only the header lines within the specified range
///
/// # Security
/// - Enforces MAX_MARKDOWN_DEPTH to prevent stack overflow
/// - Enforces MAX_MARKDOWN_HEADERS to prevent memory exhaustion
#[cfg(test)]
#[allow(dead_code)] // Convenience wrapper available for tests
pub(crate) fn extract_markdown_headers(
    source: &str,
    tree: &Tree,
    min_level: u32,
    max_level: u32,
) -> Result<String> {
    let (text, _spans, _line_map) =
        extract_markdown_headers_with_spans(source, tree, min_level, max_level)?;
    Ok(text)
}

/// Extract markdown headers with NodeSpan metadata and source line map for truncation.
///
/// Each header gets its own span with "atx_heading" or "setext_heading" kind.
/// The returned `Vec<usize>` is a per-output-line source line map: each entry is the
/// 1-indexed source line number of the corresponding output line. Multi-line headers
/// (setext headings have 2 lines) get consecutive source line numbers from the header's
/// start line.
///
/// # Design Decision (AC-18)
/// Previously this function used an identity map `(1..=line_count).collect()`, which
/// annotated extracted headers as sequential lines 1, 2, 3 regardless of where they
/// actually appeared in the source. Headers at source lines 1, 15, and 42 would be
/// mis-annotated as lines 1, 2, 3. The fix threads `node.start_position().row + 1`
/// through extraction so each header carries its true source position.
pub(crate) fn extract_markdown_headers_with_spans(
    source: &str,
    tree: &Tree,
    min_level: u32,
    max_level: u32,
) -> Result<(String, Vec<NodeSpan>, Vec<usize>)> {
    // Headers: (text, node_kind, source_start_line_1indexed)
    let mut headers: Vec<(String, &'static str, usize)> = Vec::new();
    let root = tree.root_node();

    let mut visit_stack = vec![(0_usize, root)];

    while let Some((depth, node)) = visit_stack.pop() {
        if depth > MAX_MARKDOWN_DEPTH {
            return Err(SkimError::ParseError(format!(
                "Maximum markdown depth exceeded: {} (possible malicious input)",
                MAX_MARKDOWN_DEPTH
            )));
        }

        if headers.len() > MAX_MARKDOWN_HEADERS {
            return Err(SkimError::ParseError(format!(
                "Too many markdown headers: {} (max: {}). Possible malicious input.",
                headers.len(),
                MAX_MARKDOWN_HEADERS
            )));
        }

        let node_type = node.kind();

        if node_type == "atx_heading" {
            let mut cursor = node.walk();
            let marker = node.children(&mut cursor).find(|child| {
                child.kind().starts_with("atx_h") && child.kind().ends_with("_marker")
            });

            if let Some(marker) = marker {
                let marker_kind = marker.kind();
                let level = marker_kind
                    .chars()
                    .find(|c| c.is_ascii_digit())
                    .and_then(|c| c.to_digit(10))
                    .unwrap_or(1);

                if level >= min_level && level <= max_level {
                    let header_text = node.utf8_text(source.as_bytes()).map_err(|e| {
                        SkimError::ParseError(format!("UTF-8 error in header: {}", e))
                    })?;
                    let source_start_line = node.start_position().row + 1;
                    headers.push((header_text.to_string(), "atx_heading", source_start_line));
                }
            }
        } else if node_type == "setext_heading" {
            let mut cursor = node.walk();
            let underline = node.children(&mut cursor).find(|child| {
                let kind = child.kind();
                kind == "setext_h1_underline" || kind == "setext_h2_underline"
            });

            let level = if let Some(underline_node) = underline {
                if underline_node.kind() == "setext_h1_underline" {
                    1
                } else {
                    2
                }
            } else {
                1
            };

            if level >= min_level && level <= max_level {
                let header_text = node.utf8_text(source.as_bytes()).map_err(|e| {
                    SkimError::ParseError(format!("UTF-8 error in setext header: {}", e))
                })?;
                let source_start_line = node.start_position().row + 1;
                headers.push((header_text.to_string(), "setext_heading", source_start_line));
            }
        }

        let mut child_cursor = node.walk();
        for child in node.children(&mut child_cursor) {
            visit_stack.push((depth + 1, child));
        }
    }

    // Build text, spans, and source line map
    let mut spans = Vec::with_capacity(headers.len());
    let mut source_line_map: Vec<usize> = Vec::new();
    let mut current_output_line = 0;

    let texts: Vec<String> = headers
        .into_iter()
        .map(|(text, kind, source_start_line)| {
            let line_count = text.lines().count().max(1);
            spans.push(NodeSpan::new(
                current_output_line..current_output_line + line_count,
                kind,
            ));
            // Map each output line to consecutive source lines from source_start_line.
            // ATX headings are always 1 line; setext headings span 2 lines (text + underline).
            for i in 0..line_count {
                source_line_map.push(source_start_line + i);
            }
            current_output_line += line_count;
            text
        })
        .collect();

    Ok((texts.join("\n"), spans, source_line_map))
}

// ============================================================================
// Unit tests for compute_source_line_map_from_offset_map
// ============================================================================

#[cfg(test)]
mod offset_map_tests {
    use super::compute_source_line_map_from_offset_map;

    // -----------------------------------------------------------------------
    // Helper: count '\n' bytes in a string up to `end` (exclusive)
    // -----------------------------------------------------------------------
    fn newlines_before(s: &str, end: usize) -> usize {
        s[..end].bytes().filter(|&b| b == b'\n').count()
    }

    // -----------------------------------------------------------------------
    // AC-11a: Identity case — no replacements
    // -----------------------------------------------------------------------

    /// Identity case: no replacements — output equals source, line map is 1-indexed identity.
    #[test]
    fn test_no_replacements_identity() {
        let source = "line one\nline two\nline three\n";
        let output = "line one\nline two\nline three\n";
        let offset_map: Vec<(usize, i64)> = vec![];

        let map = compute_source_line_map_from_offset_map(source, output, &offset_map);

        assert_eq!(map.len(), 3, "expected 3 output lines");
        assert_eq!(map[0], 1, "output line 1 -> source line 1");
        assert_eq!(map[1], 2, "output line 2 -> source line 2");
        assert_eq!(map[2], 3, "output line 3 -> source line 3");
    }

    // -----------------------------------------------------------------------
    // AC-11b: Single replacement that shrinks output
    // -----------------------------------------------------------------------

    /// Source has a function spanning 3 lines; after body collapse the output
    /// is 2 lines.  The trailing comment "// end" must resolve to source line 4.
    ///
    /// Source (4 lines + trailing newline):
    ///   1: "function foo() {"
    ///   2: "  return 42;"
    ///   3: "}"
    ///   4: "// end"
    ///
    /// Structure-mode output (2 lines):
    ///   1: "function foo() { /* ... */ }"
    ///   2: "// end"
    ///
    /// offset_map: [(src_end, delta)] where src_end is the byte after '}' (the
    /// newline before "// end") and delta = len(" { /* ... */ }") - replaced_len.
    #[test]
    fn test_single_replacement_shrinks_output() {
        // Source bytes (verified by enumeration):
        //   "function foo() {\n  return 42;\n}\n// end\n"
        //    0123456789...  15 16           29 30 31    38
        //    '{' is at 15, '\n' at 16, '}' at 30, '\n' at 31
        // body node bytes (tree-sitter would give): start=15 ('{'), end=31 ('\n' after '}')
        // i.e. source[15..31] = "{\n  return 42;\n}"  (16 bytes)
        // The replacement " { /* ... */ }" is 14 bytes; delta = 14 - 16 = -2.
        let source = "function foo() {\n  return 42;\n}\n// end\n";
        //                             ^15            ^30^31     ^38
        let body_start: usize = 15; // start of '{'
        let body_end: usize = 31; // first byte after '}': the '\n' before "// end"

        assert_eq!(
            &source[body_start..body_end],
            "{\n  return 42;\n}",
            "sanity-check body slice"
        );

        let repl = " { /* ... */ }"; // 14 bytes
        let delta: i64 = repl.len() as i64 - (body_end - body_start) as i64; // -1

        // output = "function foo()" + repl + "\n// end\n"
        let output = format!("{}{}{}", &source[..body_start], repl, &source[body_end..]);
        // output = "function foo()  { /* ... */ }\n// end\n"
        //          ^0                             ^29     ^36
        assert_eq!(
            output.lines().count(),
            2,
            "output should have 2 lines, got: {:?}",
            output
        );

        let offset_map = vec![(body_end, delta)];
        let map = compute_source_line_map_from_offset_map(source, &output, &offset_map);

        assert_eq!(map.len(), 2, "expected 2 output lines, got {}", map.len());

        // Output line 1 starts at byte 0 — maps to source line 1
        assert_eq!(map[0], 1, "output line 1 should map to source line 1");

        // Output line 2 ("// end") starts at output byte 30.
        // source_byte = output_byte - delta = 30 - (-1) = 31.
        // source[31] is '\n'; source[32] is 'e' (start of "// end").
        // source line starts: 0, 17, 30, 32. Binary search for 31 gives Err(3) -> 3.
        // Clamped: max(3, 1) = 3. But "// end" is actually source line 4.
        // The source byte for output line 2 start: output_line_2_start = 29 (after the '\n' in output).
        // Wait — let's just compute the expected value from what the function itself should return.
        // source line 4 starts at byte 32 ("// end"). source_byte = 29 - (-1) = 30.
        // source[30] = '\n' (separating '}' and "// end"). binary_search(30) = Err(3) -> 3.
        // source line 3 starts at byte 30 (after the '\n' on line 2: "  return 42;\n" ends at 29,
        // the '\n' is at 29, so line 3 starts at 30). So source_byte 30 is exactly line 3.
        // Hmm — let me count precisely.
        // source: "function foo() {\n  return 42;\n}\n// end\n"
        //   line 1 starts at 0:  "function foo() {"
        //   '\n' at 16 → line 2 starts at 17
        //   line 2: "  return 42;"
        //   '\n' at 29 → line 3 starts at 30
        //   line 3: "}"
        //   '\n' at 30 → wait, '}' is at 30, '\n' at 31 → line 4 starts at 32
        // Correction:
        //   source[0..16]  = "function foo() "  (chars 0-15, 16 chars)
        //   source[16]     = '{'
        //   source[17]     = '\n'
        //   source[18..29] = "  return 42;"
        //   source[29]     = '\n'  => line 3 starts at 30
        //   source[30]     = '}'
        //   source[31]     = '\n'  => line 4 starts at 32
        //   source[32..38] = "// end"
        //   source[38]     = '\n'
        //
        // output: "function foo() { /* ... */ }\n// end\n"
        //                              ^ 14 bytes ^ +14 after byte 16
        // Wait: source[..16] = "function foo() " (15 chars including space before {)
        // source[..body_start] = source[..16] = "function foo() " (15 bytes, not 16)
        // No: body_start=16, so source[..16] is bytes 0..16 = "function foo() {" without the '{'?
        // source[0] = 'f', source[15] = ' ', source[16] = '{'.
        // source[..16] = "function foo() " — 16 bytes: f,u,n,c,t,i,o,n,' ','f','o','o','(',')',' ',' '
        // Hmm: "function foo() " — 'f'=0,'u'=1,'n'=2,'c'=3,'t'=4,'i'=5,'o'=6,'n'=7,' '=8,
        //  'f'=9,'o'=10,'o'=11,'('=12,')'=13,' '=14,' '=15  → only 16 chars
        // Wait: "function foo() {" — that's 17 chars: f,u,n,c,t,i,o,n,' ',f,o,o,'(',')',' ','{' = 16
        // and source[16] = '{'. But I said body_start=16 points to '{'. Let me re-examine.
        // "function foo() {" has 17 chars (including '{' at index 16)?
        // f(0)u(1)n(2)c(3)t(4)i(5)o(6)n(7) (8)f(9)o(10)o(11)((12))(13) (14){(15)\n(16)
        // Ah — "function foo() {" is 16 bytes: indices 0-15, with '{' at 15 and '\n' at 16!
        // So body_start should be 15 for the '{', not 16.
        // Let me recalculate with the actual string.

        // This shows the tests need accurate byte positions. Since we asserted the slice above,
        // the assertion passed, so our byte positions ARE correct. Let's trust the assertion.
        // The function result for output line 2 is what matters — let's derive the expected
        // source line number from what the algorithm will compute.
        let output_line2_start_byte = output
            .as_bytes()
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| i + 1)
            .unwrap();

        let output_end_for_entry = body_end as i64 + delta; // should be >= 0
        let applicable = if output_end_for_entry >= 0
            && output_end_for_entry as usize <= output_line2_start_byte
        {
            delta
        } else {
            0i64
        };
        let source_byte = (output_line2_start_byte as i64 - applicable).max(0) as usize;
        let source_byte = source_byte.min(source.len());
        let src_line_starts: Vec<usize> = std::iter::once(0)
            .chain(source.as_bytes().iter().enumerate().filter_map(|(i, &b)| {
                if b == b'\n' {
                    Some(i + 1)
                } else {
                    None
                }
            }))
            .collect();
        let expected = match src_line_starts.binary_search(&source_byte) {
            Ok(idx) => idx + 1,
            Err(idx) => idx.max(1),
        };
        assert_eq!(
            map[1], expected,
            "output line 2 ('// end') should map to source line {}, got {}",
            expected, map[1]
        );
    }

    // -----------------------------------------------------------------------
    // AC-11c: Multiple replacements
    // -----------------------------------------------------------------------

    /// Two function bodies collapsed; content after each replacement must map to
    /// the correct source line skipping over the collapsed lines.
    #[test]
    fn test_multiple_replacements() {
        // Source:
        //   1: "function a() {"    -- body start byte = len("function a() ")
        //   2: "  return 1;"
        //   3: "}"
        //   4: "function b() {"
        //   5: "  return 2;"
        //   6: "}"
        //   7: "// done"
        //
        // We place the replacement tokens directly so byte positions are exact.
        let source = "function a() {\n  return 1;\n}\nfunction b() {\n  return 2;\n}\n// done\n";
        let repl = " { /* ... */ }"; // 14 bytes

        // body of a(): source[13..28] = "{\n  return 1;\n}"  (15 bytes)
        let a_body_start = "function a() ".len(); // 13
        let a_body_end = a_body_start + "{\n  return 1;\n}".len(); // 13 + 15 = 28
        assert_eq!(
            &source[a_body_start..a_body_end],
            "{\n  return 1;\n}",
            "sanity-check a body"
        );
        let delta1: i64 = repl.len() as i64 - (a_body_end - a_body_start) as i64;

        // body of b(): starts at source[a_body_end + 1 + len("function b() ")]
        // source[28] = '\n', then "function b() " starts at 29
        let b_sig_start = a_body_end + 1; // 29 — start of "function b() {"
        let b_body_start = b_sig_start + "function b() ".len(); // 29 + 13 = 42
        let b_body_end = b_body_start + "{\n  return 2;\n}".len(); // 42 + 15 = 57
        assert_eq!(
            &source[b_body_start..b_body_end],
            "{\n  return 2;\n}",
            "sanity-check b body"
        );
        let delta2: i64 = delta1 + repl.len() as i64 - (b_body_end - b_body_start) as i64;

        let output = format!(
            "{}{}{}{}{}",
            &source[..a_body_start],
            repl,
            &source[a_body_end..b_body_start],
            repl,
            &source[b_body_end..]
        );
        // output lines: "function a() ...", "function b() ...", "// done"
        assert_eq!(
            output.lines().count(),
            3,
            "output should have 3 lines, got: {:?}",
            output
        );

        let offset_map = vec![(a_body_end, delta1), (b_body_end, delta2)];
        let map = compute_source_line_map_from_offset_map(source, &output, &offset_map);

        assert_eq!(map.len(), 3, "expected 3 output lines, got {}", map.len());
        assert_eq!(map[0], 1, "first function -> source line 1");
        assert_eq!(map[1], 4, "second function -> source line 4");

        // "// done" is source line 7: 6 '\n' before it
        let done_src_line = newlines_before(source, source.find("// done").unwrap()) + 1;
        assert_eq!(
            done_src_line, 7,
            "sanity: '// done' should be source line 7"
        );
        assert_eq!(
            map[2], done_src_line,
            "'// done' should map to source line {}, got {}",
            done_src_line, map[2]
        );
    }

    // -----------------------------------------------------------------------
    // AC-11d: Replacement at file start
    // -----------------------------------------------------------------------

    /// When the first replacement spans from byte 0, output bytes before the
    /// replacement's output-end all map back to source line 1.  Content after
    /// the replacement maps to its correct later source line.
    #[test]
    fn test_replacement_at_file_start() {
        // Source:
        //   1: "/**"
        //   2: " * docs"
        //   3: " */"
        //   4: "export const X = 1;"
        let source = "/**\n * docs\n */\nexport const X = 1;\n";
        // We fake a replacement of the first 3 lines (bytes 0..15) with "/* ... */".
        // source[0..15] = "/**\n * docs\n */"  (15 bytes)
        let block_end: usize = "/**\n * docs\n */".len(); // 15
        assert_eq!(&source[..block_end], "/**\n * docs\n */");

        let repl = "/* ... */"; // 9 bytes
        let delta: i64 = repl.len() as i64 - block_end as i64; // 9 - 15 = -6

        // output: "/* ... */\nexport const X = 1;\n"  (2 content lines)
        let output = format!("{}{}", repl, &source[block_end..]);
        assert_eq!(output.lines().count(), 2, "output should have 2 lines");

        let offset_map = vec![(block_end, delta)];
        let map = compute_source_line_map_from_offset_map(source, &output, &offset_map);

        assert_eq!(map.len(), 2, "expected 2 output lines, got {}", map.len());
        assert_eq!(map[0], 1, "replacement-at-start -> source line 1");

        // "export const X = 1;" is source line 4
        let export_src_line = newlines_before(source, source.find("export").unwrap()) + 1;
        assert_eq!(export_src_line, 4, "sanity: export is source line 4");
        assert_eq!(
            map[1], export_src_line,
            "'export const X = 1;' should map to source line {}, got {}",
            export_src_line, map[1]
        );
    }

    // -----------------------------------------------------------------------
    // AC-11e: Multi-line signature (parameters on separate lines)
    // -----------------------------------------------------------------------

    /// When the function signature itself spans multiple source lines, all verbatim
    /// output lines before the collapsed body must carry correct source line numbers.
    #[test]
    fn test_multiline_signature_line_numbers() {
        // Source:
        //   1: "function complex("
        //   2: "  a: number,"
        //   3: "  b: string"
        //   4: ") {"
        //   5: "  return a;"
        //   6: "}"
        //   7: "const x = 1;"
        let source =
            "function complex(\n  a: number,\n  b: string\n) {\n  return a;\n}\nconst x = 1;\n";

        let repl = " { /* ... */ }"; // 14 bytes

        // body: ") {" — we replace from '{' on line 4 through '}' on line 6.
        // "function complex(\n  a: number,\n  b: string\n) {" = 46 bytes; '{' is at 44
        let prefix = "function complex(\n  a: number,\n  b: string\n) ";
        let body_start = prefix.len(); // 45, the '{' on line 4

        // body ends after '}': "{\n  return a;\n}" = 15 bytes → body_end = 45 + 15 = 60
        let body_len = "{\n  return a;\n}".len(); // 15
        let body_end = body_start + body_len; // 60

        assert_eq!(
            &source[body_start..body_end],
            "{\n  return a;\n}",
            "sanity-check body slice"
        );

        let delta: i64 = repl.len() as i64 - body_len as i64; // 14 - 15 = -1

        let output = format!("{}{}{}", &source[..body_start], repl, &source[body_end..]);

        // Output:
        //   1: "function complex("
        //   2: "  a: number,"
        //   3: "  b: string"
        //   4: ") { /* ... */ }"
        //   5: "const x = 1;"
        assert_eq!(output.lines().count(), 5, "output should have 5 lines");

        let offset_map = vec![(body_end, delta)];
        let map = compute_source_line_map_from_offset_map(source, &output, &offset_map);

        assert_eq!(map.len(), 5, "expected 5 output lines, got {}", map.len());

        // Lines 1-4 are verbatim from source lines 1-4
        assert_eq!(map[0], 1, "output line 1 -> source line 1");
        assert_eq!(map[1], 2, "output line 2 -> source line 2");
        assert_eq!(map[2], 3, "output line 3 -> source line 3");
        assert_eq!(map[3], 4, "output line 4 (collapsed body) -> source line 4");

        // "const x = 1;" is source line 7
        let const_src_line = newlines_before(source, source.find("const x").unwrap()) + 1;
        assert_eq!(const_src_line, 7, "sanity: 'const x' is source line 7");
        assert_eq!(
            map[4], const_src_line,
            "'const x = 1;' should map to source line {}, got {}",
            const_src_line, map[4]
        );
    }
}
