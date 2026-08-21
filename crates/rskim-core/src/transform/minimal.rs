//! Minimal mode transformation
//!
//! ARCHITECTURE: Strip non-doc comments at module/class level while keeping all code intact.
//! Preserves doc comments, comments inside function bodies, and shebangs.
//!
//! Token reduction target: 15-30%

use crate::transform::utils::is_function_scope_kind;
use crate::{Language, Result, SkimError, TransformConfig};
use tree_sitter::{Node, Tree};

/// Maximum AST recursion depth to prevent stack overflow attacks
pub(crate) const MAX_AST_DEPTH: usize = 500;

/// Maximum number of AST nodes to prevent memory exhaustion
pub(crate) const MAX_AST_NODES: usize = 100_000;

/// Bundled parameters for the recursive comment walker to avoid parameter explosion
pub(crate) struct CommentWalkContext<'a> {
    pub(crate) ranges: &'a mut Vec<(usize, usize)>,
    pub(crate) node_count: &'a mut usize,
    /// End byte of the last header comment, precomputed by `compute_header_end_byte`.
    /// 0 when there are no header comments or the language has no header-comment convention.
    pub(crate) header_end_byte: usize,
}

/// Transform source by stripping non-doc comments and normalizing blank lines
///
/// Two-pass algorithm:
/// 1. Walk AST collecting byte ranges of non-doc comment nodes to remove
///    (skip doc comments, skip comments inside function bodies, skip shebangs)
/// 2. Adjust ranges for full-line removal, then remove from source
/// 3. Trim trailing whitespace and normalize blank lines (3+ consecutive -> 2)
pub(crate) fn transform_minimal(
    source: &str,
    tree: &Tree,
    language: Language,
    _config: &TransformConfig,
) -> Result<String> {
    let root = tree.root_node();
    // Precompute the module-header boundary in a single O(N) forward pass.
    // This replaces the per-node O(N) backward walk that produced an overall O(N³).
    let header_end_byte = compute_header_end_byte(root, source, language);

    let mut ranges_to_remove: Vec<(usize, usize)> = Vec::new();
    let mut node_count: usize = 0;
    let mut ctx = CommentWalkContext {
        ranges: &mut ranges_to_remove,
        node_count: &mut node_count,
        header_end_byte,
    };
    collect_removable_comments(root, source, language, &mut ctx, 0, false)?;

    // Build the newline offset table once for the whole file: O(N).
    // Each adjust_range_for_line_removal call then resolves line boundaries in
    // O(log N) via binary search instead of O(start) via rfind, reducing the
    // total from O(N²) to O(N log N) across N ranges.
    let newlines = build_newline_table(source);

    // Adjust ranges for full-line removal, sort, and dedup
    let mut final_ranges: Vec<(usize, usize)> = ctx
        .ranges
        .iter()
        .map(|&(start, end)| adjust_range_for_line_removal(source, start, end, &newlines))
        .collect();
    final_ranges.sort_unstable_by_key(|&(start, _)| start);
    final_ranges.dedup();

    let after_removal = remove_ranges(source, &final_ranges)?;
    let normalized = trim_and_normalize(&after_removal);

    Ok(normalized)
}

/// Recursively collect byte ranges of comment nodes that should be removed
///
/// Collects raw (unadjusted) byte ranges. Line-level adjustment is applied
/// by the caller after collection, matching the pattern used by pseudo.rs.
///
/// # Security
/// - Enforces MAX_AST_DEPTH to prevent stack overflow
/// - Enforces MAX_AST_NODES to prevent memory exhaustion
pub(crate) fn collect_removable_comments(
    node: Node,
    source: &str,
    language: Language,
    ctx: &mut CommentWalkContext<'_>,
    depth: usize,
    in_function_body: bool,
) -> Result<()> {
    // SECURITY: Prevent stack overflow from deeply nested AST
    if depth > MAX_AST_DEPTH {
        return Err(SkimError::ParseError(format!(
            "Maximum AST depth exceeded: {} (possible malicious input)",
            MAX_AST_DEPTH
        )));
    }

    // AST node count over the cap: typically a legitimate but very large generated
    // file, not an attack. Signal a complexity limit so the dispatcher degrades to
    // a lossless raw passthrough instead of failing the command. (#317)
    *ctx.node_count += 1;
    if *ctx.node_count > MAX_AST_NODES {
        return Err(SkimError::ComplexityLimit {
            what: "AST nodes",
            count: *ctx.node_count,
            max: MAX_AST_NODES,
        });
    }

    if is_removable_comment(
        node,
        source,
        language,
        ctx.header_end_byte,
        depth,
        in_function_body,
    ) {
        ctx.ranges.push((node.start_byte(), node.end_byte()));
    }

    // Descendants are inside a function body if any strict ancestor (including
    // this node) is a body/function-definition node. O(1) per node — avoids
    // the O(depth) ts_node_parent() call that made the old is_inside_function_body
    // ancestor walk O(N²) across N root-level nodes.
    let child_in_body = in_function_body || is_function_scope_kind(node.kind(), language);

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_removable_comments(child, source, language, ctx, depth + 1, child_in_body)?;
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
pub(crate) fn is_comment_node(kind: &str, language: Language) -> bool {
    match language {
        Language::TypeScript
        | Language::JavaScript
        | Language::Python
        | Language::Go
        | Language::C
        | Language::Cpp
        | Language::CSharp
        | Language::Ruby
        | Language::Sql
        | Language::Bash => kind == "comment",
        Language::Rust | Language::Java | Language::Kotlin => {
            kind == "line_comment" || kind == "block_comment"
        }
        Language::Swift => kind == "comment" || kind == "multiline_comment",
        // Markdown, JSON, YAML, TOML don't have comment nodes to strip
        Language::Markdown | Language::Json | Language::Yaml | Language::Toml => false,
    }
}

/// Check if a comment node should be removed (not a doc comment, shebang, or in-body comment)
///
/// Combines `is_comment_node` with doc-comment filtering, shebang detection, and
/// function-body detection. Returns true if the node is a comment that should be stripped.
///
/// `header_end_byte` is the precomputed module-header boundary from
/// `compute_header_end_byte`; pass 0 to disable header-comment preservation.
///
/// `depth` is the recursion depth from the root (root = 0, root children = 1, …).
/// `in_function_body` is a threaded flag — true when any strict ancestor of this
/// node is a function body or function definition node (maintained by the walker).
/// Both replace O(depth) `parent()` calls that were O(N²) across N root-level nodes.
///
/// Used by both minimal mode (via `collect_removable_comments`) and pseudo mode
/// (inlined into `collect_noise_ranges` for single-pass processing).
pub(crate) fn is_removable_comment(
    node: Node,
    source: &str,
    language: Language,
    header_end_byte: usize,
    depth: usize,
    in_function_body: bool,
) -> bool {
    if !is_comment_node(node.kind(), language) {
        return false;
    }
    let should_preserve = is_shebang(node, source)
        || in_function_body
        || is_doc_comment(node, source, language)
        || is_module_header_comment(node, language, header_end_byte, depth);
    !should_preserve
}

/// Check if a comment node is a doc comment that should be preserved
///
/// Language-specific doc comment detection. See match arms below for
/// per-language rules covering all supported tree-sitter languages.
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
        Language::C | Language::Cpp => {
            // Doxygen comments: /** or ///
            text.starts_with("/**") || text.starts_with("///")
        }
        Language::CSharp => {
            // C# XML doc comments: ///
            text.starts_with("///") || text.starts_with("/**")
        }
        Language::Ruby => {
            // Ruby doesn't have doc comments — all `#` comments are regular.
            // RDoc and YARD conventions use `#` but there's no syntactic distinction.
            false
        }
        Language::Kotlin => {
            // Kotlin doc comments: /** */ (KDoc)
            text.starts_with("/**")
        }
        Language::Swift => {
            // Swift doc comments: /// or /** */
            text.starts_with("///") || text.starts_with("/**")
        }
        Language::Sql => {
            // SQL `--` comments have no doc comment convention
            false
        }
        Language::Bash => {
            // Bash `#` comments have no doc comment convention
            false
        }
        // Markdown, JSON, YAML, TOML don't reach here
        Language::Markdown | Language::Json | Language::Yaml | Language::Toml => false,
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

/// Compute the end byte of the module-level header comment block in a single O(N) forward pass.
///
/// Returns the `end_byte()` of the last comment node that belongs to the
/// contiguous leading comment run at the top of the file, or `0` if there are
/// no header comments.
///
/// A header comment is a root-level named child that:
/// 1. Belongs to a language with a header-comment convention (Python, Ruby, SQL, Bash).
/// 2. Is part of a prefix run of comment nodes with no blank-line break
///    (more than one `\n` in the byte gap between consecutive named children).
///
/// **Complexity:** O(N) — each root-level named child is visited exactly once via a
/// `TreeCursor`, which is the only genuinely O(1)-per-step traversal in tree-sitter.
/// A `TSNode` carries no parent pointer, so `named_child(i)` (O(i) rescan from root)
/// and `next_named_sibling()` (also O(i) — calls `ts_node_parent` which re-walks down
/// from the root) are BOTH O(i) per step and would make the loop O(N²). Profiling at
/// N=8000 confirmed 8003 ms (alpha ≈ 2.0). A `TreeCursor` keeps an explicit ancestor
/// stack, so `goto_next_sibling` is true O(1). Empirically: N=8000 → 30 ms after fix.
///
/// **Equivalence to the old backward walk:** the forward pass produces an identical
/// header boundary because:
/// - The old walk returned `true` iff every preceding named sibling was a comment
///   with no blank-line gap in between.
/// - The forward pass stops at the first non-comment or blank-line gap and records
///   the end of the last accepted comment — exactly the same boundary.
pub(crate) fn compute_header_end_byte(root: Node, source: &str, language: Language) -> usize {
    match language {
        Language::Python | Language::Ruby | Language::Sql | Language::Bash => {}
        _ => return 0,
    }

    let mut header_end: usize = 0;
    let mut prev_end: usize = 0;

    // TreeCursor is the ONLY genuinely O(1)-per-step traversal API in tree-sitter.
    // A TSNode carries no parent pointer, so `ts_node_named_child(root, i)` (O(i)
    // rescan from position 0) and `ts_node_next_named_sibling()` (also O(i) — its
    // first action is ts_node_parent(), which re-walks down from the root) are BOTH
    // O(i) per step and make this loop O(N²). A TreeCursor keeps an explicit ancestor
    // stack, so goto_next_sibling is true O(1).
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        // A gap with more than one newline between the previous node end and this
        // node start means at least one blank line exists — the header block ends.
        // `prev_end > 0` is equivalent to `i > 0` in the old loop: prev_end stays
        // zero until the first comment is accepted, so the gap check is skipped for
        // the very first child (no predecessor to form a gap against).
        if prev_end > 0 {
            let gap_start = prev_end;
            let gap_end = child.start_byte();
            if gap_start < gap_end
                && let Some(gap) = source.get(gap_start..gap_end)
                && gap.bytes().filter(|&b| b == b'\n').count() > 1
            {
                break;
            }
        }

        // Extend the header only for comment nodes; any other node terminates it.
        if is_comment_node(child.kind(), language) {
            header_end = child.end_byte();
            prev_end = child.end_byte();
        } else {
            break;
        }
    }

    header_end
}

/// Build a sorted list of byte positions where `\n` appears in `source`.
///
/// Used by `adjust_range_for_line_removal` to resolve line boundaries in O(log N)
/// per call via binary search, instead of O(start) per call via `rfind`.
///
/// Build cost: O(N) — one pass through every byte.  Binary-search cost per
/// `adjust_range_for_line_removal` call: O(log N).  Total for M ranges:
/// O(N + M log N) instead of O(N · M) when M is proportional to N.
pub(crate) fn build_newline_table(source: &str) -> Vec<usize> {
    source
        .bytes()
        .enumerate()
        .filter_map(|(i, b)| if b == b'\n' { Some(i) } else { None })
        .collect()
}

/// Check if a comment is part of the module-level header comment block.
///
/// **O(1) lookup** — the module-header boundary is precomputed once per file by
/// `compute_header_end_byte` and passed in as `header_end_byte`. Classification
/// is a single integer comparison.
///
/// A module header is a contiguous run of comment nodes at the very top of the
/// file (direct children of the root node) with no blank-line break between them
/// and no preceding non-comment sibling.
///
/// Languages where this applies: Python, Ruby, SQL, and Bash — all use `#` or
/// `--` comments at module level for shebangs, copyright, SPDX,
/// `frozen_string_literal: true`, provenance markers, and FIXTURE/TESTS headers.
/// No doc-comment convention exists for these languages (`is_doc_comment` returns
/// `false`), so without this guard minimal/pseudo would strip them.
///
/// Pass `header_end_byte = 0` to disable (no comments classified as headers).
fn is_module_header_comment(
    node: Node,
    language: Language,
    header_end_byte: usize,
    depth: usize,
) -> bool {
    match language {
        Language::Python | Language::Ruby | Language::Sql | Language::Bash => {}
        _ => return false,
    }
    // Must be a direct child of the root node. Root is walked at depth 0, so its
    // direct children are depth 1. O(1) integer compare — no parent() call needed.
    // A TSNode has no parent pointer; every parent() call re-walks the tree from
    // the root, costing O(depth). Use the threaded depth instead.
    if depth != 1 {
        return false;
    }
    // O(1) integer comparison against the precomputed boundary.
    node.end_byte() <= header_end_byte
}

/// Adjust a range to remove the entire line if the range is the only
/// non-whitespace content on that line.
///
/// If the range occupies the full line (only whitespace before/after on same line),
/// remove the entire line including the newline. Otherwise, just remove the range
/// and any leading whitespace before it on the same line (for inline trailing content).
///
/// `newlines` is the precomputed newline-position table from `build_newline_table`.
/// Passing it eliminates the O(start) `rfind('\n')` scan that made the old
/// implementation O(N²) across N ranges — each call now resolves line boundaries
/// in O(log N) via binary search.
///
/// Used by both minimal mode (comment removal) and pseudo mode (noise removal).
pub(crate) fn adjust_range_for_line_removal(
    source: &str,
    start: usize,
    end: usize,
    newlines: &[usize],
) -> (usize, usize) {
    // Find the start of the line containing this range.
    // Binary-search for the last newline whose position is < start.
    // O(log N) vs O(start) for the old rfind scan.
    let line_start = {
        let i = newlines.partition_point(|&pos| pos < start);
        if i == 0 { 0 } else { newlines[i - 1] + 1 }
    };

    // Find the end of the line containing this range.
    // Binary-search for the first newline whose position is >= end.
    let line_end = {
        let i = newlines.partition_point(|&pos| pos < end);
        if i < newlines.len() {
            newlines[i] + 1
        } else {
            source.len()
        }
    };

    // Check if the range is the only non-whitespace content on the line
    let before_range = &source[line_start..start];
    let after_range = if end < line_end {
        let after_end = if line_end > 0 && source.as_bytes().get(line_end - 1) == Some(&b'\n') {
            line_end - 1
        } else {
            line_end
        };
        &source[end..after_end]
    } else {
        ""
    };

    let only_whitespace_before = before_range.chars().all(|c| c.is_whitespace());
    let only_whitespace_after = after_range.chars().all(|c| c.is_whitespace());

    if only_whitespace_before && only_whitespace_after {
        // Range is the only content on this line - remove the entire line
        (line_start, line_end)
    } else if only_whitespace_after {
        // Inline trailing range: remove leading whitespace before the range too
        let trimmed_start = source[line_start..start].trim_end().len() + line_start;
        (trimmed_start, end)
    } else {
        // Range is in the middle or start of a line with other content - just remove the range
        (start, end)
    }
}

/// Remove collected byte ranges from source
///
/// Builds a new string by copying everything except the removed ranges.
pub(crate) fn remove_ranges(source: &str, ranges: &[(usize, usize)]) -> Result<String> {
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

        // Skip overlapping ranges, extending the removal window if needed
        if start < last_pos {
            last_pos = last_pos.max(end);
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

    Ok(result)
}

/// Trim trailing whitespace from each line and normalize blank lines in a single pass
///
/// Combines two operations to avoid an extra allocation:
/// 1. Trims trailing whitespace from each line
/// 2. Normalizes blank lines: 3+ consecutive blank lines become 2
pub(crate) fn trim_and_normalize(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut consecutive_blanks: usize = 0;

    for line in source.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            consecutive_blanks += 1;
            if consecutive_blanks > 2 {
                continue;
            }
        } else {
            consecutive_blanks = 0;
        }

        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(trimmed);
    }

    if source.ends_with('\n') {
        result.push('\n');
    }

    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // acceptable in tests
mod tests {
    use super::*;

    // ========================================================================
    // compute_header_end_byte and is_module_header_comment unit tests
    // ========================================================================
    //
    // These tests exercise the forward-pass precomputation and the O(1) predicate.
    // All cases use Python (simplest grammar for comment positioning); the
    // dispatch rules are identical for Python, Ruby, SQL, and Bash.

    // Helper: parse Python source into a tree-sitter Tree.
    fn parse_python(source: &str) -> Tree {
        let mut parser = crate::Parser::new(Language::Python).unwrap();
        parser.parse(source).unwrap()
    }

    // Helper: find the Nth root-level comment node (0-indexed) in a parsed tree.
    fn nth_root_comment<'a>(tree: &'a Tree, _source: &str, n: usize) -> Node<'a> {
        let root = tree.root_node();
        let mut found = 0usize;
        for i in 0..root.named_child_count() {
            let child = root.named_child(i).expect("named child must exist");
            if child.kind() == "comment" {
                if found == n {
                    return child;
                }
                found += 1;
            }
        }
        panic!("could not find comment #{n} in root named children");
    }

    // ── compute_header_end_byte correctness ─────────────────────────────────

    #[test]
    fn test_compute_header_end_byte_no_leading_comments() {
        // Code node first → header_end = 0 (no header).
        let source = "x = 1\n# trailing comment\n";
        let tree = parse_python(source);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        assert_eq!(heb, 0, "code-first file must have header_end_byte = 0");
    }

    #[test]
    fn test_compute_header_end_byte_single_comment() {
        // Single leading comment → header_end = end_byte of that comment.
        let source = "# Copyright 2024 Acme Corp.\nx = 1\n";
        let tree = parse_python(source);
        let comment = nth_root_comment(&tree, source, 0);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        assert_eq!(
            heb,
            comment.end_byte(),
            "single leading comment must produce header_end_byte = its end_byte"
        );
    }

    #[test]
    fn test_compute_header_end_byte_contiguous_block() {
        // Three contiguous comments → header_end = end_byte of the third.
        let source =
            "#!/usr/bin/env python3\n# SPDX-License-Identifier: MIT\n# Copyright 2024\nx = 1\n";
        let tree = parse_python(source);
        let third = nth_root_comment(&tree, source, 2);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        assert_eq!(
            heb,
            third.end_byte(),
            "three contiguous comments → header_end_byte = end of last"
        );
    }

    #[test]
    fn test_compute_header_end_byte_blank_line_stops_block() {
        // Blank line between first and second comment → only first is the header.
        let source = "# Copyright 2024\n\n# helper used by the CLI\nx = 1\n";
        let tree = parse_python(source);
        let first = nth_root_comment(&tree, source, 0);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        assert_eq!(
            heb,
            first.end_byte(),
            "blank-line break must stop header at the first comment"
        );
    }

    #[test]
    fn test_compute_header_end_byte_all_comments_file() {
        // File consisting entirely of comments → all are in the header.
        let source = "# Line 1\n# Line 2\n# Line 3\n";
        let tree = parse_python(source);
        let last = nth_root_comment(&tree, source, 2);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        assert_eq!(
            heb,
            last.end_byte(),
            "100%% comment file: all comments are the header, end_byte = last"
        );
    }

    #[test]
    fn test_compute_header_end_byte_cjk_in_comment() {
        // CJK multibyte content in a leading comment — end_byte is a byte position,
        // not a char index. The comparison must not panic on non-char boundaries.
        let source = "# 版权所有 2024 公司名\nx = 1\n";
        let tree = parse_python(source);
        let comment = nth_root_comment(&tree, source, 0);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        assert_eq!(
            heb,
            comment.end_byte(),
            "CJK comment must produce header_end_byte = its end_byte without panicking"
        );
        // Confirm end_byte is a valid UTF-8 char boundary (source.get must succeed).
        assert!(
            source.is_char_boundary(heb),
            "end_byte {heb} must be a valid UTF-8 char boundary in {source:?}"
        );
    }

    #[test]
    fn test_compute_header_end_byte_non_header_language_returns_zero() {
        // TypeScript is not in the header-language set → always returns 0.
        let ts_source = "// comment\nconst x = 1;\n";
        let mut parser = crate::Parser::new(Language::TypeScript).unwrap();
        let tree = parser.parse(ts_source).unwrap();
        let heb = compute_header_end_byte(tree.root_node(), ts_source, Language::TypeScript);
        assert_eq!(heb, 0, "non-header language must return 0");
    }

    // ── is_module_header_comment O(1) predicate (via compute_header_end_byte) ─

    #[test]
    fn test_is_module_header_comment_at_byte_0() {
        // A single comment at byte 0 with no preceding siblings → header.
        let source = "# Copyright 2024 Acme Corp.\nx = 1\n";
        let tree = parse_python(source);
        let comment = nth_root_comment(&tree, source, 0);
        assert_eq!(
            comment.start_byte(),
            0,
            "test fixture: comment must be at byte 0"
        );
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        // Root children are at depth 1 in the walker.
        assert!(
            is_module_header_comment(comment, Language::Python, heb, 1),
            "comment at byte 0 with no preceding siblings must be a module header"
        );
    }

    #[test]
    fn test_is_module_header_comment_after_shebang() {
        // A comment immediately following the shebang (no blank line) → header.
        let source = "#!/usr/bin/env python3\n# SPDX-License-Identifier: MIT\nx = 1\n";
        let tree = parse_python(source);
        let spdx = nth_root_comment(&tree, source, 1); // index 0 is shebang, 1 is SPDX
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        // Root children are at depth 1 in the walker.
        assert!(
            is_module_header_comment(spdx, Language::Python, heb, 1),
            "comment contiguous with shebang must be identified as a module header"
        );
    }

    #[test]
    fn test_is_module_header_comment_after_blank_line_break_is_stripped() {
        // A comment separated from the preceding comment by a blank line → NOT a header.
        let source = "# Copyright 2024\n\n# helper used by the CLI\nx = 1\n";
        let tree = parse_python(source);
        let non_header = nth_root_comment(&tree, source, 1);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        // Root children are at depth 1; the byte comparison gates this (not depth).
        assert!(
            !is_module_header_comment(non_header, Language::Python, heb, 1),
            "comment after a blank-line break must NOT be a module header (should be stripped)"
        );
    }

    #[test]
    fn test_is_module_header_comment_after_code_is_stripped() {
        // A comment that has a code node as its preceding named sibling → NOT a header.
        let source = "x = 1\n# standalone comment after code\n";
        let tree = parse_python(source);
        let comment = nth_root_comment(&tree, source, 0);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        // Root children are at depth 1; heb=0 means no header (byte comparison fails).
        assert!(
            !is_module_header_comment(comment, Language::Python, heb, 1),
            "comment following a code statement must NOT be a module header"
        );
    }

    #[test]
    fn test_is_module_header_comment_inline_in_body_is_unchanged() {
        // A comment inside a function body is at depth > 1 → returns false immediately
        // (depth != 1 guard fires before the byte comparison).
        let source = "def f():\n    # body comment — never a header\n    pass\n";
        let tree = parse_python(source);
        let root = tree.root_node();
        // Walk into the function body to find the comment node.
        let func = root.named_child(0).expect("function_definition expected");
        let body_comment = {
            let mut found = None;
            'outer: for i in 0..func.named_child_count() {
                let child = func.named_child(i).unwrap();
                for j in 0..child.named_child_count() {
                    let grandchild = child.named_child(j).unwrap();
                    if grandchild.kind() == "comment" {
                        found = Some(grandchild);
                        break 'outer;
                    }
                }
                if child.kind() == "comment" {
                    found = Some(child);
                    break;
                }
            }
            found.expect("expected a comment node inside the function body")
        };
        // depth=3 (root→function_definition→block/function_body→comment).
        // Any value != 1 makes the depth guard fire. Use a large heb to confirm
        // the depth guard is the real gate, not the byte comparison.
        let heb = usize::MAX;
        assert!(
            !is_module_header_comment(body_comment, Language::Python, heb, 3),
            "inline body comment must NOT be a module header (depth != 1 guard fires)"
        );
    }

    // ── Performance regression guard ─────────────────────────────────────────

    #[test]
    fn test_large_header_block_linear_time() {
        // CUBIC SMOKE TEST for the O(N³) defect in the old backward-walk
        // implementation of is_module_header_comment.
        //
        // WHAT THIS TEST PROVES: that N=500 comments complete within 200 ms.
        // The old O(N³) code took ~3 s at N=500 in a DEBUG build — well beyond
        // 200 ms. The fixed O(N) code completes in < 10 ms.
        //
        // WHAT THIS TEST DOES NOT PROVE: linear vs quadratic scaling. An O(N²)
        // regression at N=500 would complete in ~31 ms and pass this test. For
        // the doubling-ratio guard that discriminates O(N) from O(N²), see
        // test_quadratic_scaling_guard.
        //
        // Empirical measurement (fix/init-pin-wrappers-header-comments, DEBUG build):
        //   O(N³) unfixed: N=200 → 213ms, N=400 → 1736ms, N=1000 → 23920ms
        //   O(N)  fixed:   N=500 → < 10ms
        //
        // Budget tightened from 2000ms to 200ms: the O(N³) code reliably exceeds
        // 200ms (extrapolated ~3 s at N=500), while the fixed code runs in < 10ms,
        // leaving ~20× CI headroom. 2000ms gave a 222× margin and asserted almost
        // nothing about scaling.
        let n = 500usize;
        let mut source = String::with_capacity(n * 25);
        for i in 0..n {
            source.push_str(&format!("# Header comment {i}\n"));
        }
        source.push_str("x = 1\n");

        let start = std::time::Instant::now();
        let mut parser = crate::Parser::new(Language::Python).unwrap();
        let tree = parser.parse(&source).unwrap();
        let config = TransformConfig::default();
        let result = transform_minimal(&source, &tree, Language::Python, &config);
        let elapsed = start.elapsed();

        assert!(result.is_ok(), "transform must succeed: {:?}", result.err());
        let output = result.unwrap();

        // All 500 header comments must be preserved (they form a contiguous leading block).
        assert!(
            output.contains("# Header comment 0"),
            "first header comment must be preserved; got:\n{output}"
        );
        assert!(
            output.contains(&format!("# Header comment {}", n - 1)),
            "last header comment must be preserved; got:\n{output}"
        );

        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "{n} leading comments must process in < 200ms (got {elapsed:?}); \
             old O(N³) code took ~3 s at N={n} in DEBUG — this indicates a cubic regression. \
             For quadratic regressions, see test_quadratic_scaling_guard."
        );
    }

    // ── Additional correctness: full-transform round-trips ───────────────────

    #[test]
    fn test_all_comments_file_preserved_minimal() {
        // A file that is 100% comments with no code: all are in the header block
        // and must all be preserved by minimal mode.
        let source = "# Line one\n# Line two\n# Line three\n";
        let tree = parse_python(source);
        let config = TransformConfig::default();
        let result = transform_minimal(source, &tree, Language::Python, &config).unwrap();
        assert!(
            result.contains("# Line one"),
            "first comment must be preserved in all-comment file; got:\n{result}"
        );
        assert!(
            result.contains("# Line three"),
            "last comment must be preserved in all-comment file; got:\n{result}"
        );
    }

    #[test]
    fn test_comment_after_blank_line_gap_is_stripped_minimal() {
        // Header ends at blank line; the comment after the gap must be stripped.
        let source = "# Header\n\n# Not a header\nx = 1\n";
        let tree = parse_python(source);
        let config = TransformConfig::default();
        let result = transform_minimal(source, &tree, Language::Python, &config).unwrap();
        assert!(
            result.contains("# Header"),
            "header comment must be preserved; got:\n{result}"
        );
        assert!(
            !result.contains("# Not a header"),
            "post-gap comment must be stripped; got:\n{result}"
        );
    }

    #[test]
    fn test_non_root_comment_not_treated_as_header_minimal() {
        // A comment inside a function body must be preserved by the body-comment
        // rule (in_function_body flag), not by the header rule; and the depth != 1
        // guard must prevent it from being classified as a header.
        let source = "def f():\n    # body comment\n    pass\n";
        let tree = parse_python(source);
        let config = TransformConfig::default();
        let result = transform_minimal(source, &tree, Language::Python, &config).unwrap();
        // Body comments are preserved by the in_function_body threaded flag, not stripped.
        assert!(
            result.contains("# body comment"),
            "in-body comment must be preserved by body-comment rule; got:\n{result}"
        );
    }

    #[test]
    fn test_cjk_header_comment_not_stripped_minimal() {
        // CJK multibyte content in a module header comment — must be preserved
        // and source.get must not panic on the end_byte boundary.
        let source = "# 版权所有 2024\n# SPDX-License-Identifier: MIT\nx = 1\n";
        let tree = parse_python(source);
        let config = TransformConfig::default();
        let result = transform_minimal(source, &tree, Language::Python, &config).unwrap();
        assert!(
            result.contains("# 版权所有 2024"),
            "CJK header comment must be preserved; got:\n{result}"
        );
        assert!(
            result.contains("# SPDX-License-Identifier: MIT"),
            "second header comment (after CJK) must be preserved; got:\n{result}"
        );
    }

    #[test]
    fn test_trim_and_normalize_preserves_two_blanks() {
        let input = "a\n\n\nb\n";
        let result = trim_and_normalize(input);
        assert_eq!(result, "a\n\n\nb\n");
    }

    #[test]
    fn test_trim_and_normalize_reduces_four_blanks_to_two() {
        let input = "a\n\n\n\n\nb\n";
        let result = trim_and_normalize(input);
        assert_eq!(result, "a\n\n\nb\n");
    }

    #[test]
    fn test_trim_and_normalize_no_change_needed() {
        let input = "a\n\nb\n";
        let result = trim_and_normalize(input);
        assert_eq!(result, "a\n\nb\n");
    }

    #[test]
    fn test_trim_and_normalize_trims_trailing_whitespace() {
        let input = "hello   \nworld  \n";
        let result = trim_and_normalize(input);
        assert_eq!(result, "hello\nworld\n");
    }

    #[test]
    fn test_trim_and_normalize_combined() {
        // Verify both trimming and normalization happen in one pass
        let input = "hello   \n\n\n\n\nworld  \n";
        let result = trim_and_normalize(input);
        assert_eq!(result, "hello\n\n\nworld\n");
    }

    #[test]
    fn test_adjust_range_full_line_comment() {
        let source = "code\n// comment\nmore code\n";
        let newlines = build_newline_table(source);
        // "// comment" starts at byte 5, ends at byte 15
        let (start, end) = adjust_range_for_line_removal(source, 5, 15, &newlines);
        // Should remove the entire line including newline
        assert_eq!(start, 5);
        assert_eq!(end, 16); // includes the newline
    }

    // ========================================================================
    // Issue 5: adjust_range_for_line_removal trailing/inline comment branches
    // ========================================================================

    #[test]
    fn test_adjust_range_trailing_comment() {
        let source = "let x = 1; // trailing\nmore code\n";
        let newlines = build_newline_table(source);
        // "// trailing" starts at byte 11, ends at byte 22
        let (start, end) = adjust_range_for_line_removal(source, 11, 22, &newlines);
        // Should remove " // trailing" (the trailing whitespace + comment) but keep "let x = 1;"
        // The function trims whitespace before the comment on the same line
        assert!(start <= 11, "start should be at or before comment start");
        assert_eq!(end, 22);
        // Verify the remaining text makes sense
        let remaining = format!("{}{}", &source[..start], &source[end..]);
        assert!(
            remaining.starts_with("let x = 1;"),
            "should preserve code before trailing comment, got: {:?}",
            remaining
        );
    }

    #[test]
    fn test_adjust_range_inline_comment_with_code_after() {
        // Comment at start of line with code after it -- the "middle" branch
        let source = "/* comment */ let x = 1;\n";
        let newlines = build_newline_table(source);
        // "/* comment */" starts at byte 0, ends at byte 13
        let (start, end) = adjust_range_for_line_removal(source, 0, 13, &newlines);
        // There is non-whitespace after the comment, so just remove the comment itself
        assert_eq!(start, 0);
        assert_eq!(end, 13);
    }

    // ========================================================================
    // Issue 4: remove_ranges error-path tests
    // ========================================================================

    #[test]
    fn test_remove_ranges_end_before_start() {
        let source = "hello world";
        let ranges = vec![(5, 3)]; // end < start
        let result = remove_ranges(source, &ranges);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Invalid range"),
            "Expected 'Invalid range' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_remove_ranges_end_exceeds_source_length() {
        let source = "hello";
        let ranges = vec![(0, 100)]; // end > source.len()
        let result = remove_ranges(source, &ranges);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Range exceeds source length"),
            "Expected 'Range exceeds source length' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_remove_ranges_non_char_boundary() {
        // Multi-byte UTF-8 character: the euro sign takes 3 bytes
        let source = "a\u{20AC}b"; // "a" + euro sign (3 bytes) + "b" = 5 bytes total
        // Byte 2 is in the middle of the euro sign (bytes 1..4)
        let ranges = vec![(2, 4)];
        let result = remove_ranges(source, &ranges);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Invalid UTF-8 boundary"),
            "Expected 'Invalid UTF-8 boundary' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_remove_ranges_empty_ranges() {
        let source = "hello world";
        let ranges = vec![];
        let result = remove_ranges(source, &ranges).unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_remove_ranges_valid_removal() {
        let source = "hello beautiful world";
        let ranges = vec![(5, 15)]; // remove " beautiful"
        let result = remove_ranges(source, &ranges).unwrap();
        assert_eq!(result, "hello world");
    }

    // ========================================================================
    // Issue 3: Security limit error-path tests
    // ========================================================================

    #[test]
    fn test_max_ast_nodes_limit() {
        // Generate Python source with many expressions to exceed MAX_AST_NODES (100,000).
        // Each line `x = 0 + 1 + 2 + ... + 19` generates ~25 AST nodes (identifiers,
        // operators, integers, expression_statement wrappers), so 4500 lines is enough.
        let mut source = String::new();
        for i in 0..4500 {
            source.push_str("x = ");
            for j in 0..20 {
                if j > 0 {
                    source.push_str(" + ");
                }
                source.push_str(&(i * 20 + j).to_string());
            }
            source.push('\n');
        }

        let mut parser = crate::Parser::new(Language::Python).unwrap();
        let tree = parser.parse(&source).unwrap();
        let config = TransformConfig::default();

        // The transform itself still enforces the cap; the *dispatcher* is what
        // degrades a ComplexityLimit to passthrough (see types.rs). This direct
        // call therefore surfaces the typed cap error.
        let result = transform_minimal(&source, &tree, Language::Python, &config);
        let err = result.expect_err("Expected error when exceeding MAX_AST_NODES");
        assert!(
            err.is_complexity_limit(),
            "Expected a ComplexityLimit error, got: {err}"
        );
    }

    // NOTE: MAX_AST_DEPTH (500) is not tested because tree-sitter grammars impose
    // their own nesting limits that are well below 500 levels. Even deeply nested
    // expressions like `(((((...))))` do not produce 500 levels of AST depth in
    // practice. The depth guard exists as a defense-in-depth measure against
    // hypothetical malicious grammars or future grammar changes.

    // ========================================================================
    // Scaling regression guard — catches regression to O(N²)
    // ========================================================================
    //
    // The single-point timing tests above catch catastrophic regressions but cannot
    // reliably detect a reintroduced O(N²) path if the constant is small.
    // This doubling-ratio test makes the complexity class directly observable:
    //
    //   O(N):  doubling N doubles time → ratio ≈ 2.0
    //   O(N²): doubling N quadruples time → ratio ≈ 4.0
    //
    // Design choice: wall-clock ratio over a deterministic step counter.
    // A per-iteration step counter at the outer loop level would count the same
    // N+1 iterations for BOTH O(N) and O(N²) implementations — the cost difference
    // is internal to tree-sitter's C code (ts_node_parent re-walks from the root,
    // ts_node_named_child re-scans the child list from position 0). These are
    // invisible from Rust without modifying tree-sitter. The ratio test at
    // large-enough N (where signal exceeds scheduling noise) is the practical
    // discriminator.
    //
    // NOTE: transform_minimal is called directly here, not via the CLI binary, so
    // the skim file-level disk cache (which would mask the defect by serving a cached
    // result instead of running the transform) is not involved. The measurements
    // reflect the transform cost directly.
    //
    // Empirical basis (debug build, fix/init-pin-wrappers-header-comments):
    //   O(N²) unfixed: N=1000 → 125.5ms, N=2000 → 508.0ms, ratio = 4.05
    //   O(N)  fixed:   N=2000 → ~12.7ms,  N=8000 → ~30.4ms
    //                  N=4000 → ~18.6ms (linear interpolation from the two points above)
    //   N=4000 vs N=8000 ratio on fixed code:         ~30.4 / ~18.6 ≈ 1.63×
    //   N=4000 vs N=8000 ratio on O(N²) unfixed code: ~8032ms / ~2008ms ≈ 4.0×
    //   Threshold 2.5× sits midway: ≥ 0.9 margin from linear upper (1.6×),
    //                               ≥ 1.3 margin below quadratic lower (3.8×).
    //
    // N sizes chosen so that t1 (N=4000) reliably exceeds 2 ms even on fast debug
    // hardware (~18 ms measured), keeping the noise floor assertion below the expected
    // measurement by ~9×.

    #[test]
    fn test_quadratic_scaling_guard() {
        // WHAT THIS TEST PROVES: that the doubling ratio (N=4000 → N=8000) stays
        // below 2.5×. An O(N) implementation produces ~1.3–1.6×; O(N²) produces
        // ~4.0×. The 2.5 threshold sits midway between them.
        //
        // WHAT THIS TEST DOES NOT PROVE: absolute throughput or strict O(N) vs
        // O(N log N). It discriminates linear from quadratic, no finer.
        //
        // Build N=4000 and N=8000 contiguous-leading-comment Python files.
        // (The same "gap-then-body-function" fixture as the other timing tests.)
        let make_source = |n: usize| {
            let mut s = String::with_capacity(n * 25 + 16);
            for i in 0..n {
                s.push_str(&format!("# Header comment {i}\n"));
            }
            s.push_str("def f(x): return x\n");
            s
        };
        let source_4k = make_source(4000);
        let source_8k = make_source(8000);

        let mut parser = crate::Parser::new(Language::Python).unwrap();
        let config = TransformConfig::default();

        // Warm up (parse once before measuring; avoids one-time tree-sitter
        // initialisation costs skewing the N=4000 sample).
        {
            let tree = parser.parse(&source_4k).unwrap();
            let _ = transform_minimal(&source_4k, &tree, Language::Python, &config);
        }

        // Measure N=4000
        let t1 = {
            let tree = parser.parse(&source_4k).unwrap();
            let start = std::time::Instant::now();
            let r = transform_minimal(&source_4k, &tree, Language::Python, &config);
            let elapsed = start.elapsed();
            assert!(r.is_ok(), "N=4000 transform must succeed: {:?}", r.err());
            elapsed
        };

        // Measure N=8000
        let t2 = {
            let tree = parser.parse(&source_8k).unwrap();
            let start = std::time::Instant::now();
            let r = transform_minimal(&source_8k, &tree, Language::Python, &config);
            let elapsed = start.elapsed();
            assert!(r.is_ok(), "N=8000 transform must succeed: {:?}", r.err());
            elapsed
        };

        let t1_ms = t1.as_secs_f64() * 1000.0;
        let t2_ms = t2.as_secs_f64() * 1000.0;

        // N=4000 must produce a measurable result above the OS noise floor.
        // In debug builds this is ~18 ms; 2 ms is the floor — if it completes
        // faster than that, either the transform is being cached/skipped or N
        // needs to be raised further.
        //
        // We FAIL rather than skip: a silently-passing ratio guard is worse than
        // no guard at all. This assertion is the tripwire against that failure mode.
        assert!(
            t1_ms >= 2.0,
            "N=4000 transform completed in {t1_ms:.3}ms — too fast to measure reliably \
             (expected ≥ 2ms; ~18ms measured on debug builds). Either the transform is \
             being cached/skipped or N should be raised further. \
             DO NOT convert this to a skip — a silently-passing guard provides no protection."
        );

        // The doubling ratio must stay below 2.5 (O(N²) produces ~4.0×, O(N) ~1.3–1.6×).
        // Threshold 2.5 is midway: ≥ 0.9 margin from linear upper bound, ≥ 1.3 below quadratic.
        let ratio = t2_ms / t1_ms;
        assert!(
            ratio < 2.5,
            "Doubling N from 4000 to 8000 must produce a ratio below 2.5 (got {ratio:.2}×). \
             O(N) → ~1.3–1.6×; O(N²) → ~4.0× (empirically measured). \
             This indicates a regression to super-linear scaling. Check that \
             compute_header_end_byte uses a TreeCursor (not next_named_sibling), \
             is_module_header_comment uses depth (not parent() calls), and \
             collect_removable_comments threads in_function_body (not is_inside_function_body). \
             N=4000 took {t1_ms:.1}ms, N=8000 took {t2_ms:.1}ms."
        );
    }

    // ── build_newline_table correctness ─────────────────────────────────────

    #[test]
    fn test_build_newline_table_empty() {
        let table = build_newline_table("");
        assert!(table.is_empty(), "empty source → empty table");
    }

    #[test]
    fn test_build_newline_table_no_newlines() {
        let table = build_newline_table("hello world");
        assert!(table.is_empty(), "no newlines → empty table");
    }

    #[test]
    fn test_build_newline_table_single_newline() {
        let table = build_newline_table("hello\nworld");
        assert_eq!(table, vec![5], "single newline at byte 5");
    }

    #[test]
    fn test_build_newline_table_multiple_newlines() {
        let table = build_newline_table("a\nb\nc\n");
        assert_eq!(table, vec![1, 3, 5]);
    }

    #[test]
    fn test_build_newline_table_multibyte() {
        // CJK char (3 bytes each): "你" is U+4F60, 3 bytes; newline at byte 3
        let source = "你\nhello\n";
        let table = build_newline_table(source);
        // "你" is bytes 0..3, '\n' at byte 3, "hello" at bytes 4..9, '\n' at byte 9
        assert_eq!(
            table,
            vec![3, 9],
            "CJK multibyte source, newlines at bytes 3 and 9"
        );
    }

    #[test]
    fn test_adjust_range_uses_newline_table_line_start() {
        // Verify that adjust_range_for_line_removal correctly identifies line_start
        // using the newline table (regression test for O(N²) rfind replacement).
        let source = "first line\nsecond line\nthird line\n";
        let newlines = build_newline_table(source);
        // "second line" starts at byte 11, ends at byte 22
        // line_start should be 11 (byte after '\n' at byte 10)
        let (start, end) = adjust_range_for_line_removal(source, 11, 22, &newlines);
        // "second line\n" is the full line, should remove all of it
        assert_eq!(
            start, 11,
            "line start must be byte 11 (start of 'second line')"
        );
        assert_eq!(
            end, 23,
            "line end must be byte 23 (includes the trailing newline)"
        );
    }
}
