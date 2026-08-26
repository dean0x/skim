//! Literal-range collection for whitespace-preserving passes.
//!
//! Identifies the tree-sitter node types that carry raw string-literal text
//! for each supported language. Fragment-child nodes are targeted — NOT the
//! enclosing template/interpolated-string container — so that `${…}` / `#{…}`
//! interpolation bodies are still subject to whitespace normalisation.
//!
//! # Grammar verification
//!
//! Every entry in [`literal_fragment_kinds`] was checked against the grammar's
//! `node-types.json` (or `grammar.json`) in the exact crate version used by
//! this workspace.  The table and version are maintained together; adding a
//! new language requires a corresponding grammar check.
//!
//! | Language   | Kind(s)                                                           | Grammar crate                    |
//! |------------|-------------------------------------------------------------------|----------------------------------|
//! | TypeScript | `string_fragment`                                                 | tree-sitter-typescript 0.23.2    |
//! | JavaScript | `string_fragment`                                                 | tree-sitter-javascript 0.25.0    |
//! | Python     | `string_content`                                                  | tree-sitter-python 0.25.0        |
//! | Rust       | `string_content`                                                  | tree-sitter-rust 0.24.2          |
//! | Go         | `interpreted_string_literal_content`, `raw_string_literal_content`| tree-sitter-go 0.25.0            |
//! | Java       | `string_fragment`                                                 | tree-sitter-java 0.23.5          |
//! | C          | `string_content`                                                  | tree-sitter-c 0.24.2             |
//! | C++        | `string_content`, `raw_string_content`                            | tree-sitter-cpp 0.23.4           |
//! | C#         | `string_literal_content`, `string_content`, `raw_string_content`, | tree-sitter-c-sharp 0.23.5       |
//! |            | `verbatim_string_literal`                                         |                                  |
//! | Ruby       | `string_content`, `heredoc_content`                               | tree-sitter-ruby 0.23.1          |
//! | Kotlin     | `string_content`                                                  | tree-sitter-kotlin-ng 1.1.0      |
//! | Swift      | `line_str_text`, `raw_str_part`, `raw_str_end_part`               | tree-sitter-swift 0.7.3          |
//! | SQL        | `literal`                                                         | tree-sitter-sequel 0.3.11        |
//! | Bash       | `string_content`, `heredoc_content`                               | tree-sitter-bash 0.25.1          |

use crate::Language;
use crate::transform::minimal::{MAX_AST_DEPTH, MAX_AST_NODES};
use tree_sitter::Tree;

/// Returns the tree-sitter node-type names whose byte spans contain verbatim
/// string-literal text that must be preserved during whitespace normalisation.
///
/// Fragment-child nodes only — the enclosing `template_string` / interpolated
/// string container is excluded so that `${ a + b }` still has its whitespace
/// normalised (per design: interpolation bodies are NOT protected).
///
/// Verified against each language's grammar for the workspace's pinned version.
pub(crate) fn literal_fragment_kinds(language: Language) -> &'static [&'static str] {
    match language {
        // `string_fragment` is the text between the delimiters in `"…"`, `'…'`,
        // and `` `…` `` literals.  Template strings split into
        // `string_fragment` + `template_substitution` nodes; protecting only
        // `string_fragment` leaves `${ … }` unprotected.
        Language::TypeScript | Language::JavaScript => &["string_fragment"],

        // `string_content` is the inner text of both `"…"` and raw strings
        // (`r"…"`, `r#"…"#`); verified in tree-sitter-python-0.25.0 grammar.json.
        Language::Python => &["string_content"],

        // `string_content` appears as a child of both `string_literal` and
        // `raw_string_literal`; verified in tree-sitter-rust-0.24.2 node-types.json
        // lines 3319-3330.
        Language::Rust => &["string_content"],

        // Go distinguishes interpreted (`"…"`, escape-processed) from raw
        // (backtick, no escapes) via separate content node types.
        Language::Go => &[
            "interpreted_string_literal_content",
            "raw_string_literal_content",
        ],

        // `string_fragment` is the text field of `string_literal`; verified in
        // tree-sitter-java-0.23.5 node-types.json.
        Language::Java => &["string_fragment"],

        // `string_content` is the inner text of `"…"` string literals.
        Language::C => &["string_content"],

        // C++ adds raw string literals (`R"delimiter(…)delimiter"`) whose
        // inner text has its own `raw_string_content` node type.
        Language::Cpp => &["string_content", "raw_string_content"],

        // C# has four string forms:
        //   - Regular `"…"` → `string_literal_content`
        //   - Interpolated `$"…"` → `string_content`
        //   - Raw `"""…"""` → `raw_string_content`
        //   - Verbatim `@"…"` → `verbatim_string_literal` (leaf; whole node is protected)
        Language::CSharp => &[
            "string_literal_content",
            "string_content",
            "raw_string_content",
            "verbatim_string_literal",
        ],

        // `string_content` is the inner text of `"…"` / `'…'`; `heredoc_content`
        // covers heredoc bodies.
        Language::Ruby => &["string_content", "heredoc_content"],

        // `string_content` is the inner text of both plain and template strings;
        // verified in tree-sitter-kotlin-ng-1.1.0 node-types.json.
        Language::Kotlin => &["string_content"],

        // `line_str_text` is the text field of `line_string_literal` (regular `"…"`);
        // `raw_str_part` and `raw_str_end_part` are the text fields of
        // `raw_string_literal`; verified in tree-sitter-swift-0.7.3 node-types.json.
        Language::Swift => &["line_str_text", "raw_str_part", "raw_str_end_part"],

        // `literal` covers all quoted string values in SQL; verified in
        // tree-sitter-sequel-0.3.11 node-types.json.
        Language::Sql => &["literal"],

        // `string_content` is the inner text of `"…"` / `'…'`; `heredoc_content`
        // covers heredoc bodies.
        Language::Bash => &["string_content", "heredoc_content"],

        // Non-code / serde-based formats have no tree-sitter literal nodes.
        Language::Markdown | Language::Json | Language::Yaml | Language::Toml => &[],
    }
}

/// Walk the tree and collect byte ranges of literal-content nodes.
///
/// Returns sorted, non-overlapping ranges in source byte coordinates.
/// Bounded by `MAX_AST_NODES` to prevent memory exhaustion on adversarial
/// or deeply recursive input.
///
/// The ranges cover only the TEXT content (e.g. the characters between the
/// quote delimiters), not the delimiters themselves, for most languages.
/// The exception is `verbatim_string_literal` in C# which is a leaf node
/// covering the complete `@"…"` literal — protecting it is equivalent to
/// protecting its content because the delimiters are fixed syntax with no
/// collapsable whitespace.
pub(crate) fn collect_literal_ranges(tree: &Tree, language: Language) -> Vec<(usize, usize)> {
    let kinds = literal_fragment_kinds(language);
    if kinds.is_empty() {
        return Vec::new();
    }
    let mut ranges = Vec::new();
    let mut node_count: usize = 0;
    collect_inner(tree.root_node(), kinds, &mut ranges, &mut node_count, 0);
    // Tree traversal visits nodes in document order; sort for safety.
    ranges.sort_unstable_by_key(|&(s, _)| s);
    ranges
}

fn collect_inner(
    node: tree_sitter::Node<'_>,
    kinds: &[&str],
    ranges: &mut Vec<(usize, usize)>,
    node_count: &mut usize,
    depth: usize,
) {
    *node_count += 1;
    if *node_count > MAX_AST_NODES || depth > MAX_AST_DEPTH {
        return;
    }

    if kinds.contains(&node.kind()) {
        let r = node.byte_range();
        if r.start < r.end {
            ranges.push((r.start, r.end));
        }
        // Do NOT recurse: children of content nodes are escape sequences
        // (e.g. `\n`, `\"`) — the whole content-node span is the protected unit.
        return;
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_inner(child, kinds, ranges, node_count, depth + 1);
    }
}

/// Map sorted source byte ranges to their positions in the post-`remove_ranges` output.
///
/// `source_ranges` must be sorted by start position.
/// `removed_ranges` must be sorted and non-overlapping (as produced by the
/// line-adjustment pass inside `transform_pseudo` / `transform_minimal`).
///
/// Ranges that fall entirely inside a removed span are dropped (the literal
/// was part of a removed region — unusual, but possible for block comments
/// that contained raw string delimiters).  Partially overlapping ranges are
/// emitted with clipped coordinates; in practice this should not occur because
/// tree-sitter tokens are always atomic.
pub(crate) fn map_ranges_to_output(
    source_ranges: &[(usize, usize)],
    removed_ranges: &[(usize, usize)],
) -> Vec<(usize, usize)> {
    let mut result = Vec::with_capacity(source_ranges.len());
    let mut ri = 0usize;
    let mut cumulative_removed: usize = 0;

    for &(ls, le) in source_ranges {
        // Advance past removed ranges that end at or before ls.
        while ri < removed_ranges.len() && removed_ranges[ri].1 <= ls {
            cumulative_removed += removed_ranges[ri].1 - removed_ranges[ri].0;
            ri += 1;
        }

        // Drop ranges that lie entirely inside the current removed span.
        if ri < removed_ranges.len() {
            let (ra, rb) = removed_ranges[ri];
            if ra <= ls && le <= rb {
                continue;
            }
        }

        let out_ls = ls.saturating_sub(cumulative_removed);
        let out_le = le.saturating_sub(cumulative_removed);
        if out_ls < out_le {
            result.push((out_ls, out_le));
        }
    }

    result
}

/// Returns `true` if byte offset `pos` falls within any of the sorted,
/// non-overlapping `protected` ranges.
///
/// O(log N) via binary search.
#[inline]
pub(crate) fn in_protected(pos: usize, protected: &[(usize, usize)]) -> bool {
    // Find the last range whose start ≤ pos.
    let idx = protected.partition_point(|&(s, _)| s <= pos);
    idx > 0 && protected[idx - 1].1 > pos
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{Language, Parser};

    fn parse(source: &str, lang: Language) -> tree_sitter::Tree {
        let mut parser = Parser::new(lang).unwrap();
        parser.parse(source).unwrap()
    }

    // -----------------------------------------------------------------------
    // literal_fragment_kinds coverage
    // -----------------------------------------------------------------------

    #[test]
    fn test_no_kinds_for_data_formats() {
        for lang in [
            Language::Json,
            Language::Yaml,
            Language::Toml,
            Language::Markdown,
        ] {
            assert!(
                literal_fragment_kinds(lang).is_empty(),
                "{lang:?} should return no literal kinds"
            );
        }
    }

    #[test]
    fn test_all_code_langs_have_kinds() {
        let code_langs = [
            Language::TypeScript,
            Language::JavaScript,
            Language::Python,
            Language::Rust,
            Language::Go,
            Language::Java,
            Language::C,
            Language::Cpp,
            Language::CSharp,
            Language::Ruby,
            Language::Kotlin,
            Language::Swift,
            Language::Sql,
            Language::Bash,
        ];
        for lang in code_langs {
            assert!(
                !literal_fragment_kinds(lang).is_empty(),
                "{lang:?} should return at least one literal kind"
            );
        }
    }

    // -----------------------------------------------------------------------
    // collect_literal_ranges: TypeScript
    // -----------------------------------------------------------------------

    #[test]
    fn test_collect_ts_string_fragment() {
        let source = "const x = \"hello  world\";\n";
        let tree = parse(source, Language::TypeScript);
        let ranges = collect_literal_ranges(&tree, Language::TypeScript);
        assert!(!ranges.is_empty(), "should find string_fragment");
        // The fragment should contain "hello  world" (double space preserved)
        let (s, e) = ranges[0];
        assert_eq!(&source[s..e], "hello  world");
    }

    #[test]
    fn test_collect_ts_template_literal_not_whole_node() {
        // The whole `template_string` must NOT be protected — only the
        // string_fragment children.  Interpolation `${ a + b }` must not be.
        let source = "const x = `a  b${ a + b }c  d`;\n";
        let tree = parse(source, Language::TypeScript);
        let ranges = collect_literal_ranges(&tree, Language::TypeScript);
        // Each string_fragment: "a  b" and "c  d"
        let combined: String = ranges
            .iter()
            .map(|&(s, e)| &source[s..e])
            .collect::<Vec<_>>()
            .join("|");
        assert!(
            combined.contains("a  b"),
            "first fragment must be collected"
        );
        assert!(
            combined.contains("c  d"),
            "second fragment must be collected"
        );
        // "a + b" from the interpolation must NOT appear as protected content
        let covered: String = ranges
            .iter()
            .flat_map(|&(s, e)| source[s..e].chars())
            .collect();
        assert!(
            !covered.contains('+'),
            "interpolation body must not be in protected ranges"
        );
    }

    // -----------------------------------------------------------------------
    // collect_literal_ranges: Python multiline
    // -----------------------------------------------------------------------

    #[test]
    fn test_collect_python_multiline_string() {
        let source = "x = \"\"\"\n  hello  \n\"\"\"\n";
        let tree = parse(source, Language::Python);
        let ranges = collect_literal_ranges(&tree, Language::Python);
        assert!(
            !ranges.is_empty(),
            "should find string_content in multiline"
        );
        // The string_content should include the middle line's trailing spaces
        let content: String = ranges
            .iter()
            .map(|&(s, e)| &source[s..e])
            .collect::<Vec<_>>()
            .join("");
        assert!(
            content.contains("  hello  "),
            "multiline content with trailing spaces must be collected: {content:?}"
        );
    }

    // -----------------------------------------------------------------------
    // map_ranges_to_output
    // -----------------------------------------------------------------------

    #[test]
    fn test_map_no_removal() {
        let source_ranges = vec![(5, 10), (20, 25)];
        let removed: Vec<(usize, usize)> = vec![];
        let out = map_ranges_to_output(&source_ranges, &removed);
        assert_eq!(out, source_ranges);
    }

    #[test]
    fn test_map_removal_before_range() {
        // Remove [0..3) — shifts subsequent ranges left by 3
        let source_ranges = vec![(5, 10)];
        let removed = vec![(0, 3)];
        let out = map_ranges_to_output(&source_ranges, &removed);
        assert_eq!(out, vec![(2, 7)]);
    }

    #[test]
    fn test_map_range_fully_removed() {
        // Remove [5..10) — the literal itself was removed
        let source_ranges = vec![(5, 10)];
        let removed = vec![(5, 10)];
        let out = map_ranges_to_output(&source_ranges, &removed);
        assert!(out.is_empty(), "fully removed literal should be dropped");
    }

    #[test]
    fn test_map_removal_between_ranges() {
        // Remove [10..15) between two literals
        let source_ranges = vec![(5, 9), (20, 25)];
        let removed = vec![(10, 15)];
        let out = map_ranges_to_output(&source_ranges, &removed);
        assert_eq!(out[0], (5, 9)); // before removal — unchanged
        assert_eq!(out[1], (15, 20)); // shifted left by 5
    }

    // -----------------------------------------------------------------------
    // in_protected
    // -----------------------------------------------------------------------

    #[test]
    fn test_in_protected_basic() {
        let protected = vec![(5, 10), (20, 30)];
        assert!(!in_protected(4, &protected));
        assert!(in_protected(5, &protected));
        assert!(in_protected(9, &protected));
        assert!(!in_protected(10, &protected));
        assert!(in_protected(20, &protected));
        assert!(!in_protected(30, &protected));
    }

    #[test]
    fn test_in_protected_empty() {
        assert!(!in_protected(0, &[]));
        assert!(!in_protected(100, &[]));
    }
}
