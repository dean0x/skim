//! Structure mode transformation
//!
//! ARCHITECTURE: Strip function/method bodies, keep structure.
//!
//! Token reduction target: 70-80%

use crate::transform::compute_line_starts;
use crate::transform::minimal::{MAX_AST_DEPTH, MAX_AST_NODES};
use crate::transform::truncate::NodeSpan;
use crate::transform::utils::{FunctionNodeTypes, to_static_node_kind};
use crate::{Language, Result, SkimError, TransformConfig};
use std::collections::HashMap;
use tree_sitter::{Node, Tree};

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
/// - Function bodies → `{...}`
/// - Implementation details
/// - Non-structural comments
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
/// The replacement `{...}` stays on the same line as the function signature
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
/// The replacement text `" {...}"` contains no newlines. Therefore no
/// output line ever starts inside a replacement region — all output line start
/// bytes are in verbatim-copied regions where the reverse mapping is exact.
pub(crate) fn compute_source_line_map_from_offset_map(
    source: &str,
    output: &str,
    offset_map: &[(usize, i64)],
) -> Vec<usize> {
    // Empty output produces no output lines — return early before computing starts.
    if output.is_empty() {
        return Vec::new();
    }

    // Pre-compute source and output line start byte offsets.
    // Newlines are always ASCII (single byte); see `compute_line_starts` for details.
    let source_line_starts: Vec<usize> = compute_line_starts(source.as_bytes());
    let output_line_starts: Vec<usize> = compute_line_starts(output.as_bytes());

    // Derive the output line count from the already-computed starts vector rather
    // than calling output.lines().count() again (avoids a second full scan).
    // output_line_starts always has at least 1 entry (the leading 0 for line 1).
    // If the output ends with '\n', the last entry in output_line_starts is a
    // phantom "start of the next line" that has no actual content — the number of
    // real lines equals output_line_starts.len() minus that phantom entry.
    let output_lines = if output.ends_with('\n') {
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
            // source_byte = output_byte - delta (clamped to [0, source.len()])
            let source_byte =
                ((output_byte as i64 - applicable_delta).max(0) as usize).min(source.len());

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
    if matches_function_node(kind, node_types)
        && let Some(body) = find_body_node(node)
    {
        let start = body.start_byte();
        let end = body.end_byte();
        replacements.insert((start, end), " {...}");
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
    let line_starts = crate::transform::compute_line_starts(output.as_bytes());

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
/// - Enforces MAX_AST_DEPTH to prevent stack overflow
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
        if depth > MAX_AST_DEPTH {
            return Err(SkimError::ParseError(format!(
                "Maximum markdown depth exceeded: {} (possible malicious input)",
                MAX_AST_DEPTH
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

    // Sort headers into document order (ascending source start line).
    // The LIFO visit_stack produces children in reverse sibling order, so without
    // this sort siblings appear reversed in output.  Stable sort ensures that
    // setext headings (which share a start line with their underline) remain in
    // insertion order when source lines are equal (in practice, distinct headers
    // always have distinct start lines).
    // A1 invariant: output is now in document order; texts/spans/source_line_map
    // are all derived from the sorted slice so they remain consistent.
    headers.sort_by_key(|h| h.2);

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
#[allow(clippy::unwrap_used, clippy::expect_used)] // Unwrapping/expect is acceptable in tests
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
    ///   1: "function foo() {...}"
    ///   2: "// end"
    ///
    /// offset_map: [(src_end, delta)] where src_end is the byte after '}' (the
    /// newline before "// end") and delta = len(" {...}") - replaced_len.
    #[test]
    fn test_single_replacement_shrinks_output() {
        // Source bytes (verified by enumeration):
        //   "function foo() {\n  return 42;\n}\n// end\n"
        //    0123456789...  15 16           29 30 31    38
        //    '{' is at 15, '\n' at 16, '}' at 30, '\n' at 31
        // body node bytes (tree-sitter would give): start=15 ('{'), end=31 ('\n' after '}')
        // i.e. source[15..31] = "{\n  return 42;\n}"  (16 bytes)
        // The replacement " {...}" is 6 bytes; delta = 6 - 16 = -10.
        let source = "function foo() {\n  return 42;\n}\n// end\n";
        //                             ^15            ^30^31     ^38
        let body_start: usize = 15; // start of '{'
        let body_end: usize = 31; // first byte after '}': the '\n' before "// end"

        assert_eq!(
            &source[body_start..body_end],
            "{\n  return 42;\n}",
            "sanity-check body slice"
        );

        let repl = " {...}"; // 6 bytes
        let delta: i64 = repl.len() as i64 - (body_end - body_start) as i64; // 6 - 16 = -10

        // output = "function foo() " + repl + "\n// end\n"
        let output = format!("{}{}{}", &source[..body_start], repl, &source[body_end..]);
        // output = "function foo() {...}\n// end\n"
        //           bytes 0-14 (15) + bytes 15-20 (6) = '\n' at 21; "// end" starts at 22
        assert_eq!(
            output.lines().count(),
            2,
            "output should have 2 lines, got: {:?}",
            output
        );

        let offset_map = vec![(body_end, delta)];
        let map = compute_source_line_map_from_offset_map(source, &output, &offset_map);

        assert_eq!(map.len(), 2, "expected 2 output lines, got {}", map.len());

        // Output line 1 (byte 0) maps to source line 1.
        assert_eq!(map[0], 1, "output line 1 should map to source line 1");

        // Output line 2 ("// end") starts at output byte 22.
        // source_byte = output_byte - delta = 22 - (-10) = 32.
        // Source line_starts = [0, 17, 30, 32, 39]; binary_search(32) = Ok(3) → line 4.
        assert_eq!(
            map[1], 4,
            "output line 2 ('// end') should map to source line 4"
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
        let repl = " {...}"; // 6 bytes

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

        let repl = " {...}"; // 6 bytes

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

        let delta: i64 = repl.len() as i64 - body_len as i64; // 6 - 15 = -9

        let output = format!("{}{}{}", &source[..body_start], repl, &source[body_end..]);

        // Output:
        //   1: "function complex("
        //   2: "  a: number,"
        //   3: "  b: string"
        //   4: ") {...}"
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

// ============================================================================
// Unit tests for extract_markdown_headers_with_spans line map
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Unwrapping/expect is acceptable in tests
mod markdown_line_map_tests {
    use super::extract_markdown_headers_with_spans;
    use crate::{Language, Parser};

    /// Parse markdown with tree-sitter and return the source line map from
    /// `extract_markdown_headers_with_spans`.
    fn parse_and_extract_line_map(source: &str) -> Vec<usize> {
        let mut parser = Parser::new(Language::Markdown).unwrap();
        let tree = parser.parse(source).unwrap();
        let (_text, _spans, line_map) =
            extract_markdown_headers_with_spans(source, &tree, 1, 6).unwrap();
        line_map
    }

    /// Headers at non-contiguous source lines: the returned line map must
    /// contain actual 1-indexed source line numbers in document order.
    ///
    /// Source (9 lines + trailing newline):
    ///   1: "# Title"
    ///   2: ""
    ///   3: "Some text."
    ///   4: ""
    ///   5: "## Section"
    ///   6: ""
    ///   7: "More text."
    ///   8: ""
    ///   9: "### Sub"
    ///
    /// After the document-order sort, the line map is [1, 5, 9].
    ///
    /// KEY: each map[i] must equal the source row of that header node (tree-sitter
    /// `node.start_position().row + 1`) AND they must ascend (document order).
    /// A broken sequential-output-position implementation would yield [1, 2, 3]
    /// (no entry would equal 5 or 9).
    #[test]
    fn test_markdown_line_map_non_contiguous_headers() {
        let source = "# Title\n\nSome text.\n\n## Section\n\nMore text.\n\n### Sub\n";
        let line_map = parse_and_extract_line_map(source);

        assert_eq!(
            line_map.len(),
            3,
            "expected 3 output lines (one per header), got {:?}",
            line_map
        );

        // All three values must be real source lines (1, 5, 9), not positions (1, 2, 3).
        // After the document-order sort, the line map is already ascending [1, 5, 9].
        assert_eq!(
            line_map,
            vec![1, 5, 9],
            "line map must be [1, 5, 9] in document order, got {:?}",
            line_map
        );

        // Verify no map entry equals a sequential output position (1, 2, 3).
        // If the implementation incorrectly uses output-position indexing, all three
        // entries would be 1, 2, or 3. Source line 9 cannot equal any of those.
        assert!(
            line_map.contains(&9),
            "### Sub is at source line 9 — must appear in line map, got {:?}",
            line_map
        );
        assert!(
            line_map.contains(&5),
            "## Section is at source line 5 — must appear in line map, got {:?}",
            line_map
        );
        assert!(
            line_map.contains(&1),
            "# Title is at source line 1 — must appear in line map, got {:?}",
            line_map
        );
    }

    /// Source with headers on consecutive lines (no interleaved prose).
    /// After the document-order sort, the map must be [1, 2, 3] — ascending
    /// source lines, matching visual top-to-bottom reading order.
    ///
    /// The key invariant: map[0] == 1 (# H1 is first), map[2] == 3 (### H3 is last).
    #[test]
    fn test_markdown_line_map_consecutive_headers() {
        let source = "# H1\n## H2\n### H3\n";
        let line_map = parse_and_extract_line_map(source);

        assert_eq!(line_map.len(), 3, "expected 3 headers, got {:?}", line_map);

        // Document order: # H1 (source line 1) must come first.
        assert_eq!(
            line_map[0], 1,
            "# H1 is at source line 1 and must be first after document-order sort, \
             got line_map[0] = {}",
            line_map[0]
        );
        assert_eq!(
            line_map[1], 2,
            "## H2 is at source line 2, got {}",
            line_map[1]
        );
        assert_eq!(
            line_map[2], 3,
            "### H3 is at source line 3, got {}",
            line_map[2]
        );
    }

    /// Headers in document (top-to-bottom) order: the extracted text must list
    /// headings top-to-bottom so that `--mode=structure` output reads naturally.
    ///
    /// This is the primary regression test for the LIFO stack bug: before the
    /// document-order sort, siblings were emitted in reverse order so the last
    /// sibling appeared first in the output.
    #[test]
    fn test_markdown_headers_document_order() {
        // Five sibling headings in ascending source order.
        let source = "# Alpha\n## Beta\n## Gamma\n### Delta\n## Epsilon\n";
        let mut parser = crate::Parser::new(Language::Markdown).unwrap();
        let tree = parser.parse(source).unwrap();
        let (text, _spans, line_map) =
            extract_markdown_headers_with_spans(source, &tree, 1, 6).unwrap();

        // All headings must appear in the text
        assert!(text.contains("Alpha"), "Alpha missing from output: {text}");
        assert!(text.contains("Beta"), "Beta missing from output: {text}");
        assert!(text.contains("Gamma"), "Gamma missing from output: {text}");
        assert!(text.contains("Delta"), "Delta missing from output: {text}");
        assert!(
            text.contains("Epsilon"),
            "Epsilon missing from output: {text}"
        );

        // Headings must appear in document order (Alpha before Beta, Beta before Gamma, etc.)
        let pos_alpha = text.find("Alpha").unwrap();
        let pos_beta = text.find("Beta").unwrap();
        let pos_gamma = text.find("Gamma").unwrap();
        let pos_delta = text.find("Delta").unwrap();
        let pos_epsilon = text.find("Epsilon").unwrap();

        assert!(
            pos_alpha < pos_beta,
            "Alpha (pos {pos_alpha}) must precede Beta (pos {pos_beta}) in: {text}"
        );
        assert!(
            pos_beta < pos_gamma,
            "Beta (pos {pos_beta}) must precede Gamma (pos {pos_gamma}) in: {text}"
        );
        assert!(
            pos_gamma < pos_delta,
            "Gamma (pos {pos_gamma}) must precede Delta (pos {pos_delta}) in: {text}"
        );
        assert!(
            pos_delta < pos_epsilon,
            "Delta (pos {pos_delta}) must precede Epsilon (pos {pos_epsilon}) in: {text}"
        );

        // Line map must strictly ascend (document order invariant)
        assert_eq!(
            line_map.len(),
            5,
            "expected 5 entries in line_map: {:?}",
            line_map
        );
        for i in 1..line_map.len() {
            assert!(
                line_map[i] > line_map[i - 1],
                "line_map must be strictly ascending but line_map[{}]={} >= line_map[{}]={}, \
                 full map: {:?}",
                i,
                line_map[i],
                i - 1,
                line_map[i - 1],
                line_map
            );
        }
    }

    /// A markdown document with a single header deep in the file must map to
    /// the correct source line, not to output position 1.
    ///
    /// # Deep Header is at source line 5. A broken sequential-position
    /// implementation would map it to output line 1 (always 1 for a single header).
    /// Since source line 5 != output position 1, this test catches that regression.
    #[test]
    fn test_markdown_line_map_single_deep_header() {
        // Header at source line 5 (after 4 lines of prose)
        let source = "Intro line.\n\nAnother para.\n\n# Deep Header\n\nTrailing text.\n";
        let line_map = parse_and_extract_line_map(source);

        assert_eq!(line_map.len(), 1, "expected 1 header, got {:?}", line_map);
        assert_eq!(
            line_map[0], 5,
            "# Deep Header is on source line 5, got {}. \
             A sequential-position implementation would give 1, not 5.",
            line_map[0]
        );
    }
}
