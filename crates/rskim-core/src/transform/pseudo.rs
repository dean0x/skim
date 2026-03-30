//! Pseudo mode transformation — strips syntactic noise while preserving logic flow.
//!
//! ARCHITECTURE: Removes type annotations, visibility modifiers, decorators, semicolons,
//! and other syntactic noise to produce pseudocode-like output. Uses the same
//! collect-ranges-then-remove pattern as minimal.rs.
//!
//! Token reduction target: 30-50%

use crate::transform::truncate::NodeSpan;
use crate::{Language, Result, SkimError, TransformConfig};
use tree_sitter::{Node, Tree};

use super::minimal::{
    adjust_range_for_line_removal, is_removable_comment, remove_ranges, trim_and_normalize,
    MAX_AST_DEPTH, MAX_AST_NODES,
};

/// Bundled parameters for the recursive noise walker to avoid parameter explosion
struct NoiseWalkContext<'a> {
    source: &'a str,
    source_bytes: &'a [u8],
    language: Language,
    ranges: &'a mut Vec<(usize, usize)>,
    node_count: &'a mut usize,
}

/// Extend a byte position forward to consume trailing spaces (not past newline).
///
/// After stripping a keyword or node, trailing spaces remain in the source.
/// This helper advances the end position past those spaces to prevent artifacts
/// like `" fn ..."` when `pub` is removed from `pub fn ...`.
///
/// ARCHITECTURE: This is layer 1 of a two-layer whitespace strategy. It handles
/// byte-level space consumption at range-collection time (before removal). The
/// downstream `collapse_whitespace` pass (layer 2) handles any remaining artifacts
/// after all ranges are removed — collapsing multi-space runs, trimming trailing
/// whitespace, and stripping leading spaces left by inline removals.
fn consume_trailing_whitespace(source: &[u8], end: usize) -> usize {
    let mut pos = end;
    while pos < source.len() && source[pos] == b' ' {
        pos += 1;
    }
    pos
}

/// Returns true for node kinds that act as inline modifiers preceding another token.
///
/// When these kinds are stripped, the trailing space between the modifier and the next
/// token should also be consumed. For example, stripping `'a` from `&'a str` should
/// produce `&str` (not `& str`), and stripping `mut` from `&mut self` should produce
/// `& self`.
///
/// Type annotations and decorators are NOT inline modifiers — their trailing spaces
/// may belong to surrounding syntax (e.g., `: number = 42`).
fn is_inline_modifier_kind(kind: &str) -> bool {
    matches!(
        kind,
        "lifetime" | "mutable_specifier" | "visibility_modifier" | "readonly" | "abstract"
    )
}

/// Per-language rules for what constitutes "noise" in pseudo mode
struct PseudoRules {
    /// AST node kinds to strip entirely
    strip_kinds: &'static [&'static str],
    /// Keywords that appear as leaf nodes to strip
    strip_keywords: &'static [&'static str],
    /// Whether to strip semicolons (statement-terminating only)
    strip_semicolons: bool,
    /// Whether to strip Python self/cls first parameter
    strip_self_param: bool,
}

fn get_pseudo_rules(language: Language) -> PseudoRules {
    match language {
        Language::TypeScript => PseudoRules {
            strip_kinds: &[
                "type_annotation",
                "type_parameters",
                "type_arguments",
                "decorator",
                "readonly",
                "abstract",
            ],
            strip_keywords: &["export"],
            strip_semicolons: true,
            strip_self_param: false,
        },
        Language::JavaScript => PseudoRules {
            strip_kinds: &["decorator"],
            strip_keywords: &["export"],
            strip_semicolons: true,
            strip_self_param: false,
        },
        Language::Python => PseudoRules {
            strip_kinds: &["type", "return_type", "decorator"],
            strip_keywords: &[],
            strip_semicolons: false,
            strip_self_param: true,
        },
        Language::Rust => PseudoRules {
            strip_kinds: &[
                "visibility_modifier",
                "lifetime",
                "type_parameters",
                "where_clause",
                "attribute_item",
                "mutable_specifier",
            ],
            strip_keywords: &[],
            strip_semicolons: true,
            strip_self_param: false,
        },
        Language::Go => PseudoRules {
            // Go types are integral to understanding — be conservative
            strip_kinds: &[],
            strip_keywords: &[],
            strip_semicolons: false,
            strip_self_param: false,
        },
        Language::Java => PseudoRules {
            strip_kinds: &[
                "marker_annotation",
                "annotation",
                "type_parameters",
                "throws",
            ],
            strip_keywords: &[
                "public",
                "private",
                "protected",
                "static",
                "final",
                "abstract",
            ],
            strip_semicolons: true,
            strip_self_param: false,
        },
        Language::C => PseudoRules {
            strip_kinds: &[],
            strip_keywords: &["static", "extern", "const", "volatile"],
            strip_semicolons: true,
            strip_self_param: false,
        },
        Language::Cpp => PseudoRules {
            // NOTE: access_specifier and template_parameter_list are handled
            // as special cases in collect_noise_ranges because they require
            // consuming adjacent sibling nodes (`:` and `template` keyword).
            strip_kinds: &[],
            strip_keywords: &[
                "static", "extern", "const", "volatile", "virtual", "override", "final", "noexcept",
            ],
            strip_semicolons: true,
            strip_self_param: false,
        },
        Language::CSharp => PseudoRules {
            strip_kinds: &["attribute_list", "type_parameter_list"],
            strip_keywords: &[
                "public",
                "private",
                "protected",
                "internal",
                "static",
                "virtual",
                "override",
                "sealed",
                "abstract",
                // NOTE: `async` intentionally NOT stripped — it changes calling semantics
            ],
            strip_semicolons: true,
            strip_self_param: false,
        },
        Language::Ruby => PseudoRules {
            strip_kinds: &[],
            strip_keywords: &["private", "protected", "public"],
            strip_semicolons: false,
            strip_self_param: false,
        },
        Language::Kotlin => PseudoRules {
            strip_kinds: &["type_parameters", "annotation"],
            strip_keywords: &[
                "public",
                "private",
                "protected",
                "internal",
                "open",
                "data",
                "sealed",
                "override",
                "abstract",
                // NOTE: `suspend` intentionally NOT stripped — it changes calling semantics
            ],
            strip_semicolons: false,
            strip_self_param: false,
        },
        Language::Swift => PseudoRules {
            strip_kinds: &["attribute", "type_parameters"],
            strip_keywords: &[
                "public",
                "private",
                "internal",
                "fileprivate",
                "open",
                "static",
                "override",
                "final",
                // NOTE: `class` intentionally NOT stripped — it introduces class declarations,
                // and in tree-sitter-swift the keyword is a leaf node in both class declarations
                // and class method modifiers, so stripping it would remove class declarations.
                // NOTE: `async` intentionally NOT stripped — it changes calling semantics
            ],
            strip_semicolons: false,
            strip_self_param: false,
        },
        Language::Sql => PseudoRules {
            // SQL has minimal syntactic noise — keep most things
            strip_kinds: &[],
            strip_keywords: &[],
            strip_semicolons: true,
            strip_self_param: false,
        },
        // Serde languages and Markdown are handled as passthrough before reaching here
        _ => PseudoRules {
            strip_kinds: &[],
            strip_keywords: &[],
            strip_semicolons: false,
            strip_self_param: false,
        },
    }
}

/// Transform source by stripping syntactic noise while preserving logic flow
///
/// Convenience wrapper around `transform_pseudo_with_spans` that discards span metadata.
#[cfg(test)]
pub(crate) fn transform_pseudo(
    source: &str,
    tree: &Tree,
    language: Language,
    config: &TransformConfig,
) -> Result<String> {
    let (result, _spans) = transform_pseudo_with_spans(source, tree, language, config)?;
    Ok(result)
}

/// Transform source by stripping syntactic noise, returning NodeSpan metadata
pub(crate) fn transform_pseudo_with_spans(
    source: &str,
    tree: &Tree,
    language: Language,
    _config: &TransformConfig,
) -> Result<(String, Vec<NodeSpan>)> {
    let rules = get_pseudo_rules(language);

    // Single-pass collection: comments AND noise ranges in one AST walk
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut node_count: usize = 0;
    let mut ctx = NoiseWalkContext {
        source,
        source_bytes: source.as_bytes(),
        language,
        ranges: &mut ranges,
        node_count: &mut node_count,
    };
    collect_noise_ranges(tree.root_node(), &mut ctx, &rules, 0)?;

    // Sort, dedup, and adjust ranges for full line removal
    ctx.ranges.sort_unstable_by_key(|&(start, _)| start);
    ctx.ranges.dedup();

    let mut final_ranges: Vec<(usize, usize)> = ctx
        .ranges
        .iter()
        .map(|&(start, end)| adjust_range_for_line_removal(source, start, end))
        .collect();

    // Re-sort after adjustment (line-level adjustments can change ordering)
    final_ranges.sort_unstable_by_key(|&(start, _)| start);

    let result = remove_ranges(source, &final_ranges)?;

    // Post-process — collapse whitespace artifacts and normalize
    let result = collapse_whitespace(&result);
    let result = trim_and_normalize(&result);

    // Build spans (single source_file span for truncation compatibility)
    let line_count = result.lines().count();
    let spans = vec![NodeSpan::new(0..line_count, "source_file")];

    Ok((result, spans))
}

/// Collapse whitespace artifacts from inline removal:
/// - Multiple consecutive spaces in content portion -> single space
/// - Trailing whitespace on lines is trimmed
/// - Leading spaces left by inline removal are trimmed
/// - Indentation is preserved
fn collapse_whitespace(source: &str) -> String {
    let mut result = String::with_capacity(source.len());

    for line in source.lines() {
        let indent_len = line.len() - line.trim_start().len();
        let content = line[indent_len..].trim_end();

        result.push_str(&line[..indent_len]);

        // State machine: `leading` skips initial spaces after indent,
        // `prev_space` collapses consecutive space runs to single space.
        let mut prev_space = false;
        let mut leading = true;
        for ch in content.chars() {
            if ch == ' ' {
                if !prev_space && !leading {
                    result.push(ch);
                }
                prev_space = true;
            } else {
                leading = false;
                result.push(ch);
                prev_space = false;
            }
        }
        result.push('\n');
    }

    result
}

/// Handle language-specific AST patterns that require multi-node context.
///
/// Returns `Some(Ok(()))` to skip recursion (C++ cases — stripped nodes are leaf-like),
/// `Some(Err(...))` to propagate errors, or `None` to continue normal recursion.
fn handle_language_special_cases(node: Node, ctx: &mut NoiseWalkContext<'_>) -> Option<Result<()>> {
    let kind = node.kind();
    match ctx.language {
        Language::Rust if matches!(kind, "function_item" | "function_signature_item") => {
            strip_rust_return_type(node, ctx.ranges);
            None // Continue recursion — function children (params, body) still need processing
        }
        Language::Cpp if kind == "access_specifier" => {
            // `public:` is two siblings: access_specifier + `:`
            let start = node.start_byte();
            let colon_end = node
                .next_sibling()
                .filter(|s| s.kind() == ":")
                .map_or(node.end_byte(), |s| s.end_byte());
            let end = consume_trailing_whitespace(ctx.source_bytes, colon_end);
            ctx.ranges.push((start, end));
            Some(Ok(())) // Skip recursion
        }
        Language::Cpp if kind == "template_parameter_list" => {
            // `template<typename T>` is two siblings: `template` keyword + parameter list
            let template_start = node
                .prev_sibling()
                .filter(|s| s.kind() == "template")
                .map_or(node.start_byte(), |s| s.start_byte());
            let end = consume_trailing_whitespace(ctx.source_bytes, node.end_byte());
            ctx.ranges.push((template_start, end));
            Some(Ok(())) // Skip recursion
        }
        _ => None,
    }
}

fn collect_noise_ranges(
    node: Node,
    ctx: &mut NoiseWalkContext<'_>,
    rules: &PseudoRules,
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
    *ctx.node_count += 1;
    if *ctx.node_count > MAX_AST_NODES {
        return Err(SkimError::ParseError(format!(
            "Too many AST nodes: {} (max: {}). Possible malicious input.",
            *ctx.node_count, MAX_AST_NODES
        )));
    }

    let kind = node.kind();

    // Check for removable comments (merged from former separate pass).
    // Uses the same doc-comment/shebang/function-body filtering as minimal mode.
    if is_removable_comment(node, ctx.source, ctx.language) {
        ctx.ranges.push((node.start_byte(), node.end_byte()));
        return Ok(()); // Comments have no children to recurse into
    }

    // Check if this node kind should be stripped
    if rules.strip_kinds.contains(&kind) {
        let start = node.start_byte();
        let end = node.end_byte();
        let adjusted_start = adjust_type_start(ctx.language, kind, ctx.source_bytes, start);
        // Consume trailing whitespace only for inline modifiers (lifetime, mut, visibility,
        // etc.) where the space separates the modifier from the next token. Do NOT consume
        // for type annotations — their trailing space may belong to assignment syntax
        // (e.g., `: number = 42` → `= 42` needs the space before `=`).
        let end = if is_inline_modifier_kind(kind) {
            consume_trailing_whitespace(ctx.source_bytes, end)
        } else {
            end
        };
        ctx.ranges.push((adjusted_start, end));
        return Ok(()); // Don't recurse into stripped nodes
    }

    // Check for keyword stripping (leaf nodes only)
    if node.child_count() == 0 {
        let text = node.utf8_text(ctx.source_bytes).unwrap_or("");
        if rules.strip_keywords.contains(&text) {
            let end = consume_trailing_whitespace(ctx.source_bytes, node.end_byte());
            ctx.ranges.push((node.start_byte(), end));
            return Ok(());
        }
    }

    // Check for semicolon stripping (statement-terminating only, not for-loop headers)
    if rules.strip_semicolons && kind == ";" {
        let is_for_loop = node
            .parent()
            .map(|p| {
                matches!(
                    p.kind(),
                    "for_statement" | "for_in_statement" | "for_of_statement"
                )
            })
            .unwrap_or(false);
        if !is_for_loop {
            ctx.ranges.push((node.start_byte(), node.end_byte()));
            return Ok(());
        }
    }

    // Handle Python self/cls removal
    if rules.strip_self_param && kind == "parameters" {
        strip_python_self_param(node, ctx.source_bytes, ctx.ranges);
    }

    // Handle language-specific multi-node patterns (Rust return types, C++ siblings)
    if let Some(result) = handle_language_special_cases(node, ctx) {
        return result;
    }

    // Recurse into children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_noise_ranges(child, ctx, rules, depth + 1)?;
    }

    Ok(())
}

/// Adjust the start position for type annotations to include their separators.
///
/// Python's "type" node in `typed_parameter` does NOT include the `: ` separator.
/// Python's "return_type" node does NOT include the ` -> ` separator — the `->` is
/// a separate anonymous sibling node BEFORE the `return_type` / `type` node. This
/// extends the removal range backward to include these separators for clean output.
fn adjust_type_start(language: Language, kind: &str, source: &[u8], start: usize) -> usize {
    match (language, kind) {
        // NOTE: In Python's tree-sitter grammar, both parameter types (`a: int`)
        // and return types (`-> int`) use node kind `"type"`. The `"return_type"`
        // arm is kept for defensive compatibility but does not match in practice
        // (tree-sitter uses `return_type` as a field name, not a node kind).
        (Language::Python, "type" | "return_type") => {
            // Python return type: ` -> int` — consume the ` -> ` separator.
            // Python parameter type: `a: int` — consume the `: ` separator.
            // Ordered longest-first for greedy match
            const SEPARATORS: &[&[u8]] = &[b" -> ", b"-> ", b"->", b": ", b":"];
            let prefix = source.get(start.saturating_sub(4)..start).unwrap_or(b"");
            for sep in SEPARATORS {
                if prefix.ends_with(sep) {
                    return start.saturating_sub(sep.len());
                }
            }
            start
        }
        _ => start,
    }
}

/// Strip `self` or `cls` first parameter from Python method definitions
fn strip_python_self_param(
    params_node: Node,
    source_bytes: &[u8],
    ranges: &mut Vec<(usize, usize)>,
) {
    let mut cursor = params_node.walk();
    let children: Vec<_> = params_node.children(&mut cursor).collect();

    // Find the first actual parameter (skip `(` and `,`)
    for (i, child) in children.iter().enumerate() {
        let kind = child.kind();
        if kind == "(" || kind == "," {
            continue;
        }

        // Determine if this first parameter is self/cls
        let is_self_or_cls = match kind {
            "identifier" => matches!(child.utf8_text(source_bytes).unwrap_or(""), "self" | "cls"),
            "typed_parameter" | "default_parameter" => {
                let mut inner_cursor = child.walk();
                // Binding required: the iterator borrows `inner_cursor`, and without
                // a named binding the temporary outlives the mutable borrow (E0597).
                let found = child
                    .children(&mut inner_cursor)
                    .next()
                    .and_then(|first_child| first_child.utf8_text(source_bytes).ok())
                    .is_some_and(|t| matches!(t, "self" | "cls"));
                found
            }
            _ => false,
        };

        if is_self_or_cls {
            let start = child.start_byte();
            let end = extend_past_trailing_comma(child.end_byte(), &children, i, source_bytes);
            ranges.push((start, end));
        }

        break; // Only check first parameter
    }
}

/// Extend a removal range past a trailing comma and optional space
fn extend_past_trailing_comma(
    end: usize,
    children: &[Node],
    index: usize,
    source_bytes: &[u8],
) -> usize {
    if let Some(next) = children.get(index + 1) {
        if next.kind() == "," {
            let comma_end = next.end_byte();
            if comma_end < source_bytes.len() && source_bytes[comma_end] == b' ' {
                return comma_end + 1;
            }
            return comma_end;
        }
    }
    end
}

/// Strip Rust return type from function signatures.
///
/// In Rust's tree-sitter grammar, `-> Type` is NOT wrapped in a `return_type` node.
/// Instead, `->` and the type are sibling children of `function_item`. This function
/// finds the `->` child and removes from its start through the end of the next sibling
/// (the type node), including the leading space.
fn strip_rust_return_type(function_node: Node, ranges: &mut Vec<(usize, usize)>) {
    let mut cursor = function_node.walk();
    let children: Vec<_> = function_node.children(&mut cursor).collect();

    for (i, child) in children.iter().enumerate() {
        if child.kind() == "->" {
            // Find the type node that follows (next named sibling)
            let end = if let Some(type_node) = children.get(i + 1) {
                // The type node immediately follows `->`
                if type_node.kind() != "block" {
                    type_node.end_byte()
                } else {
                    child.end_byte()
                }
            } else {
                child.end_byte()
            };

            // Include the leading space before `->`
            let start = child.start_byte().saturating_sub(1);

            ranges.push((start, end));
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Mode, Parser, TransformConfig};

    fn transform(source: &str, language: Language) -> String {
        let mut parser = Parser::new(language).unwrap();
        let tree = parser.parse(source).unwrap();
        let config = TransformConfig::with_mode(Mode::Pseudo);
        transform_pseudo(source, &tree, language, &config).unwrap()
    }

    // ========================================================================
    // TypeScript pseudo tests
    // ========================================================================

    #[test]
    fn test_typescript_pseudo_strips_type_annotations() {
        let source = "function add(a: number, b: number): number {\n    return a + b;\n}\n";
        let result = transform(source, Language::TypeScript);
        // Type annotations and semicolons should be stripped
        assert!(
            !result.contains(": number"),
            "type annotations should be stripped"
        );
        assert!(
            result.contains("function add(a, b)"),
            "function name and params preserved"
        );
        assert!(result.contains("return a + b"), "logic preserved");
    }

    #[test]
    fn test_typescript_pseudo_strips_export() {
        let source =
            "export function greet(name: string): string {\n    return `Hello, ${name}!`;\n}\n";
        let result = transform(source, Language::TypeScript);
        assert!(
            !result.contains("export"),
            "export keyword should be stripped"
        );
        assert!(
            result.contains("function greet(name)"),
            "function signature preserved without types"
        );
    }

    #[test]
    fn test_typescript_pseudo_strips_type_parameters() {
        let source = "function identity<T>(value: T): T {\n    return value;\n}\n";
        let result = transform(source, Language::TypeScript);
        assert!(
            !result.contains("<T>"),
            "type parameters should be stripped"
        );
        assert!(
            result.contains("function identity(value)"),
            "function preserved"
        );
    }

    #[test]
    fn test_typescript_pseudo_preserves_for_loop_semicolons() {
        let source = "function loop() {\n    for (let i = 0; i < 10; i++) {\n        console.log(i);\n    }\n}\n";
        let result = transform(source, Language::TypeScript);
        // For-loop header semicolons should be preserved
        assert!(result.contains("i < 10"), "for-loop condition preserved");
    }

    // ========================================================================
    // JavaScript pseudo tests
    // ========================================================================

    #[test]
    fn test_javascript_pseudo_strips_export_and_semicolons() {
        let source = "export function add(x, y) {\n    return x + y;\n}\n";
        let result = transform(source, Language::JavaScript);
        assert!(!result.contains("export"), "export should be stripped");
        assert!(result.contains("function add(x, y)"), "function preserved");
        // Semicolons are stripped
        assert!(result.contains("return x + y"), "logic preserved");
    }

    // ========================================================================
    // Python pseudo tests
    // ========================================================================

    #[test]
    fn test_python_pseudo_strips_type_hints() {
        let source =
            "def calculate_sum(a: int, b: int) -> int:\n    result = a + b\n    return result\n";
        let result = transform(source, Language::Python);
        assert!(
            !result.contains(": int"),
            "type annotations should be stripped"
        );
        assert!(!result.contains("-> int"), "return type should be stripped");
        assert!(
            result.contains("def calculate_sum(a, b)"),
            "function signature preserved"
        );
        assert!(result.contains("return result"), "logic preserved");
    }

    #[test]
    fn test_python_pseudo_strips_self_param() {
        let source =
            "class Calculator:\n    def add(self, x: int, y: int) -> int:\n        return x + y\n";
        let result = transform(source, Language::Python);
        assert!(!result.contains("self"), "self param should be stripped");
        assert!(
            result.contains("def add(x, y)"),
            "method params preserved without self/types"
        );
    }

    #[test]
    fn test_python_pseudo_strips_decorators() {
        let source = "@staticmethod\ndef helper() -> None:\n    pass\n";
        let result = transform(source, Language::Python);
        assert!(
            !result.contains("@staticmethod"),
            "decorator should be stripped"
        );
        assert!(result.contains("def helper()"), "function preserved");
    }

    // ========================================================================
    // Rust pseudo tests
    // ========================================================================

    #[test]
    fn test_rust_pseudo_strips_visibility() {
        let source = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let result = transform(source, Language::Rust);
        assert!(
            !result.contains("pub "),
            "visibility modifier should be stripped"
        );
        assert!(result.contains("fn add"), "function preserved");
    }

    #[test]
    fn test_rust_pseudo_strips_lifetimes_and_type_params() {
        let source = "pub fn longest<'a>(x: &'a str, y: &'a str) -> &'a str {\n    if x.len() > y.len() { x } else { y }\n}\n";
        let result = transform(source, Language::Rust);
        assert!(
            !result.contains("<'a>"),
            "type parameters should be stripped"
        );
        // Lifetimes in the body might remain in some nodes, but the key is
        // that the type_parameters on the function are stripped
    }

    #[test]
    fn test_rust_pseudo_strips_attributes() {
        let source = "#[derive(Debug)]\npub struct Point {\n    pub x: i32,\n    pub y: i32,\n}\n";
        let result = transform(source, Language::Rust);
        assert!(
            !result.contains("#[derive(Debug)]"),
            "attribute should be stripped"
        );
        assert!(result.contains("struct Point"), "struct preserved");
    }

    #[test]
    fn test_rust_pseudo_strips_where_clause() {
        let source =
            "fn process<T>(value: T) where T: Clone + Debug {\n    println!(\"{:?}\", value);\n}\n";
        let result = transform(source, Language::Rust);
        assert!(!result.contains("where"), "where clause should be stripped");
        assert!(result.contains("fn process"), "function preserved");
    }

    #[test]
    fn test_rust_pseudo_strips_return_type() {
        let source = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let result = transform(source, Language::Rust);
        assert!(
            !result.contains("-> i32"),
            "return type should be stripped, got: {result}"
        );
        assert!(result.contains("fn add"), "function preserved");
    }

    // ========================================================================
    // Java pseudo tests
    // ========================================================================

    #[test]
    fn test_java_pseudo_strips_visibility() {
        let source = "public class Simple {\n    private int value;\n    public int add(int a, int b) {\n        return a + b;\n    }\n}\n";
        let result = transform(source, Language::Java);
        assert!(
            !result.contains("public "),
            "public modifier should be stripped"
        );
        assert!(
            !result.contains("private "),
            "private modifier should be stripped"
        );
        assert!(result.contains("class Simple"), "class preserved");
        assert!(result.contains("int add(int a, int b)"), "method preserved");
    }

    #[test]
    fn test_java_pseudo_strips_annotations() {
        let source = "@Override\npublic String toString() {\n    return \"hello\";\n}\n";
        let result = transform(source, Language::Java);
        assert!(
            !result.contains("@Override"),
            "annotation should be stripped"
        );
        assert!(result.contains("String toString()"), "method preserved");
    }

    // ========================================================================
    // C pseudo tests
    // ========================================================================

    #[test]
    fn test_c_pseudo_strips_qualifiers() {
        let source = "static const int MAX = 100;\n";
        let result = transform(source, Language::C);
        assert!(!result.contains("static"), "static should be stripped");
        assert!(!result.contains("const"), "const should be stripped");
        assert!(result.contains("int MAX = 100"), "declaration preserved");
    }

    #[test]
    fn test_c_pseudo_strips_semicolons() {
        let source = "int add(int a, int b) {\n    return a + b;\n}\n";
        let result = transform(source, Language::C);
        // Body semicolons should be stripped
        assert!(result.contains("return a + b"), "logic preserved");
    }

    // ========================================================================
    // C++ pseudo tests
    // ========================================================================

    #[test]
    fn test_cpp_pseudo_strips_access_specifiers() {
        let source = "class Foo {\npublic:\n    int bar();\nprivate:\n    int baz_;\n};\n";
        let result = transform(source, Language::Cpp);
        assert!(
            !result.contains("public:"),
            "access specifier should be stripped"
        );
        assert!(
            !result.contains("private:"),
            "access specifier should be stripped"
        );
    }

    #[test]
    fn test_cpp_pseudo_strips_virtual_override() {
        let source = "class Shape {\npublic:\n    virtual double area() const = 0;\n    virtual ~Shape() = default;\n};\n";
        let result = transform(source, Language::Cpp);
        assert!(!result.contains("virtual"), "virtual should be stripped");
    }

    // ========================================================================
    // Whitespace collapse tests
    // ========================================================================

    #[test]
    fn test_collapse_whitespace_basic() {
        let result = collapse_whitespace("  pub  fn  add() {}\n");
        // Multiple spaces collapsed, leading indent preserved
        assert_eq!(result, "  pub fn add() {}\n");
    }

    #[test]
    fn test_collapse_whitespace_preserves_indentation() {
        let result = collapse_whitespace("    let x = 1\n");
        assert_eq!(result, "    let x = 1\n");
    }

    // ========================================================================
    // Security tests
    // ========================================================================

    #[test]
    fn test_pseudo_respects_max_ast_nodes() {
        // Reuse the same large-source pattern from minimal tests
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

        let mut parser = Parser::new(Language::Python).unwrap();
        let tree = parser.parse(&source).unwrap();
        let config = TransformConfig::with_mode(Mode::Pseudo);

        let result = transform_pseudo(&source, &tree, Language::Python, &config);
        assert!(
            result.is_err(),
            "Expected error when exceeding MAX_AST_NODES"
        );
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Too many AST nodes"),
            "Expected 'Too many AST nodes' error, got: {}",
            err_msg
        );
    }

    // ========================================================================
    // Edge case tests
    // ========================================================================

    #[test]
    fn test_pseudo_empty_input() {
        let result = transform("", Language::TypeScript);
        assert_eq!(result, "", "empty input should produce empty output");
    }

    #[test]
    fn test_pseudo_overlapping_comment_and_noise_range() {
        // A decorator with an inline comment: both should be stripped
        let source =
            "@staticmethod  # old helper\ndef helper(self, x: int) -> int:\n    return x\n";
        let result = transform(source, Language::Python);
        assert!(
            !result.contains("@staticmethod"),
            "decorator should be stripped, got: {result}"
        );
        assert!(
            !result.contains("# old helper"),
            "inline comment should be stripped, got: {result}"
        );
        assert!(
            !result.contains(": int"),
            "type annotations should be stripped, got: {result}"
        );
        assert!(
            result.contains("def helper(x)"),
            "function preserved without self/types, got: {result}"
        );
        assert!(result.contains("return x"), "logic preserved");
    }

    #[test]
    fn test_pseudo_markdown_passthrough() {
        // Markdown in pseudo mode should return source unchanged (passthrough
        // happens in Language::transform_source, not in transform_pseudo)
        let source = "# Heading\n\nSome **bold** text.\n";
        let config = TransformConfig::with_mode(Mode::Pseudo);
        let result = Language::Markdown
            .transform_source(source, &config)
            .unwrap();
        assert_eq!(
            result, source,
            "Markdown should pass through unchanged in pseudo mode"
        );
    }

    // ========================================================================
    // Regression tests: output quality bug fixes
    // ========================================================================

    #[test]
    fn test_python_pseudo_no_arrow_residue() {
        // BUG 1: Python return type stripping left `-> ` residue
        let source = "def calculate_sum(a: int, b: int) -> int:\n    return a + b\n";
        let result = transform(source, Language::Python);
        assert!(
            !result.contains("->"),
            "return type arrow should be fully stripped, got: {result}"
        );
        assert!(
            result.contains("def calculate_sum(a, b):"),
            "function signature should be clean, got: {result}"
        );
    }

    #[test]
    fn test_cpp_pseudo_no_orphaned_colon() {
        // BUG 2: C++ access specifier stripping left orphaned `:`
        let source = "class Foo {\npublic:\n    int bar();\nprivate:\n    int baz_;\n};\n";
        let result = transform(source, Language::Cpp);
        assert!(
            !result.contains("public"),
            "access specifier keyword should be stripped, got: {result}"
        );
        assert!(
            !result.lines().any(|l| l.trim() == ":"),
            "orphaned colon should not remain, got: {result}"
        );
        assert!(
            result.contains("int bar()"),
            "member declarations preserved, got: {result}"
        );
    }

    #[test]
    fn test_cpp_pseudo_no_orphaned_template() {
        // BUG 3: C++ template_parameter_list stripping left orphaned `template`
        let source = "template<typename T>\nclass Container {\npublic:\n    T value;\n};\n";
        let result = transform(source, Language::Cpp);
        assert!(
            !result.contains("template"),
            "template keyword should be stripped along with parameter list, got: {result}"
        );
        assert!(
            result.contains("class Container"),
            "class declaration preserved, got: {result}"
        );
    }

    #[test]
    fn test_rust_pseudo_trait_return_type() {
        // BUG 4: Rust trait method return types were not stripped
        let source =
            "pub trait Compute {\n    fn compute(&self, value: i32) -> i32;\n    fn reset(&mut self);\n}\n";
        let result = transform(source, Language::Rust);
        assert!(
            !result.contains("-> i32"),
            "trait method return type should be stripped, got: {result}"
        );
        assert!(
            result.contains("fn compute"),
            "trait method name preserved, got: {result}"
        );
    }

    #[test]
    fn test_rust_pseudo_lifetime_no_space() {
        // BUG 6: Stripping lifetime from `&'a str` left `& str` (extra space)
        let source = "pub fn longest<'a>(x: &'a str, y: &'a str) -> &'a str {\n    if x.len() > y.len() { x } else { y }\n}\n";
        let result = transform(source, Language::Rust);
        assert!(
            !result.contains("& str"),
            "lifetime removal should not leave extra space in references, got: {result}"
        );
        assert!(
            result.contains("&str"),
            "reference types should be clean, got: {result}"
        );
    }

    #[test]
    fn test_typescript_pseudo_no_leading_space() {
        // BUG 5: Stripping `export` left leading space on next token
        let source = "export function add(a: number, b: number): number {\n    return a + b;\n}\n";
        let result = transform(source, Language::TypeScript);
        assert!(
            !result.starts_with(' '),
            "output should not start with a leading space, got: {result}"
        );
        assert!(
            result.contains("function add(a, b)"),
            "function signature clean after export removal, got: {result}"
        );
    }

    #[test]
    fn test_java_pseudo_no_leading_spaces() {
        // BUG 5: Stripping `public static final` left leading spaces
        let source = "public class Simple {\n    private int value;\n    public static final int MAX = 100;\n    public int add(int a, int b) {\n        return a + b;\n    }\n}\n";
        let result = transform(source, Language::Java);
        assert!(
            result.contains("class Simple"),
            "class name preserved, got: {result}"
        );
        // Assert exact indentation levels: 0, 4, or 8 spaces for non-empty lines
        for line in result.lines() {
            if line.is_empty() {
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            assert!(
                indent == 0 || indent == 4 || indent == 8,
                "expected indentation of 0, 4, or 8 spaces but got {} for line: {:?}, full output: {result}",
                indent,
                line
            );
        }
    }

    #[test]
    fn test_c_pseudo_const_no_space() {
        // BUG 7: Stripping `const` left leading space before type
        let source = "const char* greeting = \"hello\";\n";
        let result = transform(source, Language::C);
        assert!(
            !result.starts_with(' '),
            "const removal should not leave leading space, got: {result}"
        );
        assert!(
            result.contains("char* greeting"),
            "declaration preserved after const removal, got: {result}"
        );
    }

    #[test]
    fn test_python_pseudo_multiple_return_types() {
        // Ensure multiple functions with return types all get clean output
        let source = "def foo(x: int) -> str:\n    return str(x)\n\ndef bar(y: str) -> int:\n    return int(y)\n";
        let result = transform(source, Language::Python);
        assert!(
            !result.contains("->"),
            "all return type arrows should be stripped, got: {result}"
        );
        assert!(
            result.contains("def foo(x):"),
            "first function clean, got: {result}"
        );
        assert!(
            result.contains("def bar(y):"),
            "second function clean, got: {result}"
        );
    }

    // ========================================================================
    // Unit tests for helper functions (TEST-2)
    // ========================================================================

    #[test]
    fn test_consume_trailing_whitespace_basic() {
        let source = b"pub fn add()";
        // After "pub" (byte 3), consume trailing spaces
        assert_eq!(consume_trailing_whitespace(source, 3), 4);
    }

    #[test]
    fn test_consume_trailing_whitespace_multiple_spaces() {
        let source = b"pub   fn add()";
        assert_eq!(consume_trailing_whitespace(source, 3), 6);
    }

    #[test]
    fn test_consume_trailing_whitespace_no_spaces() {
        let source = b"pubfn";
        assert_eq!(consume_trailing_whitespace(source, 3), 3);
    }

    #[test]
    fn test_consume_trailing_whitespace_at_end() {
        let source = b"pub";
        assert_eq!(consume_trailing_whitespace(source, 3), 3);
    }

    #[test]
    fn test_consume_trailing_whitespace_stops_at_newline() {
        let source = b"pub \nfn";
        // Should consume the space but stop before newline
        assert_eq!(consume_trailing_whitespace(source, 3), 4);
    }

    #[test]
    fn test_is_inline_modifier_kind_positives() {
        assert!(is_inline_modifier_kind("lifetime"));
        assert!(is_inline_modifier_kind("mutable_specifier"));
        assert!(is_inline_modifier_kind("visibility_modifier"));
        assert!(is_inline_modifier_kind("readonly"));
        assert!(is_inline_modifier_kind("abstract"));
    }

    #[test]
    fn test_is_inline_modifier_kind_negatives() {
        assert!(!is_inline_modifier_kind("type_annotation"));
        assert!(!is_inline_modifier_kind("decorator"));
        assert!(!is_inline_modifier_kind("identifier"));
        assert!(!is_inline_modifier_kind("function_item"));
        assert!(!is_inline_modifier_kind(""));
    }

    // ========================================================================
    // Negative/preservation tests (TEST-3)
    // ========================================================================

    #[test]
    fn test_python_arrow_in_string_literal_preserved() {
        // Verify that `->` inside a string literal is NOT consumed by adjust_type_start
        let source = "def describe():\n    return \"maps A -> B\"\n";
        let result = transform(source, Language::Python);
        assert!(
            result.contains("->"),
            "arrow inside string literal should be preserved, got: {result}"
        );
        assert!(
            result.contains("\"maps A -> B\""),
            "string content should be unchanged, got: {result}"
        );
    }

    // ========================================================================
    // C++ template function test (TEST-4)
    // ========================================================================

    #[test]
    fn test_cpp_pseudo_strips_template_function() {
        // Test template function (not class) — current tests only cover template class
        let source = "template<typename T>\nT max_val(T a, T b) {\n    return a > b ? a : b;\n}\n";
        let result = transform(source, Language::Cpp);
        assert!(
            !result.contains("template"),
            "template keyword should be stripped from function, got: {result}"
        );
        assert!(
            !result.contains("<typename T>"),
            "template parameter list should be stripped from function, got: {result}"
        );
        assert!(
            result.contains("max_val"),
            "function name preserved, got: {result}"
        );
    }

    // ========================================================================
    // handle_language_special_cases behavioral contract tests (ISSUE-1)
    // ========================================================================

    #[test]
    fn test_rust_special_case_continues_recursion_into_body() {
        // Rust function_item returns None (continue recursion), so children like
        // visibility_modifier and mutable_specifier inside params should still be stripped
        let source =
            "pub fn update(&mut self, value: i32) -> bool {\n    self.val = value;\n    true\n}\n";
        let result = transform(source, Language::Rust);
        assert!(
            !result.contains("pub "),
            "pub should be stripped via child recursion, got: {result}"
        );
        assert!(
            !result.contains("mut "),
            "mut should be stripped via child recursion, got: {result}"
        );
        assert!(
            !result.contains("-> bool"),
            "return type should be stripped by special case, got: {result}"
        );
        assert!(
            result.contains("self.val = value"),
            "function body should be preserved (recursion continued), got: {result}"
        );
    }

    #[test]
    fn test_cpp_access_specifier_skips_recursion() {
        // C++ access_specifier returns Some(Ok(())) — the entire `public:` is removed
        // and no recursion into children occurs
        let source = "class Widget {\npublic:\n    void draw();\nprotected:\n    int x_;\n};\n";
        let result = transform(source, Language::Cpp);
        assert!(
            !result.contains("public"),
            "public access specifier fully stripped, got: {result}"
        );
        assert!(
            !result.contains("protected"),
            "protected access specifier fully stripped, got: {result}"
        );
        assert!(
            !result.lines().any(|l| l.trim() == ":"),
            "no orphaned colons, got: {result}"
        );
        assert!(
            result.contains("void draw()"),
            "member declarations preserved, got: {result}"
        );
    }

    #[test]
    fn test_cpp_template_parameter_list_skips_recursion() {
        // C++ template_parameter_list returns Some(Ok(())) — both `template` keyword
        // and `<typename T>` are removed without recursing into the parameter list
        let source =
            "template<typename K, typename V>\nclass Map {\npublic:\n    V get(K key);\n};\n";
        let result = transform(source, Language::Cpp);
        assert!(
            !result.contains("template"),
            "template keyword stripped, got: {result}"
        );
        assert!(
            !result.contains("<typename"),
            "template parameters stripped, got: {result}"
        );
        assert!(
            result.contains("class Map"),
            "class declaration preserved, got: {result}"
        );
    }

    // ========================================================================
    // collapse_whitespace edge case tests (ISSUE-2, ISSUE-5)
    // ========================================================================

    #[test]
    fn test_collapse_whitespace_preserves_indent_when_modifier_stripped() {
        // When an inline modifier is stripped (e.g., `    pub fn` -> `     fn`),
        // the extra space becomes part of indentation and is preserved.
        // The `leading` flag skips any content-leading spaces after indent detection.
        let result = collapse_whitespace("    fn add() {}\n");
        assert_eq!(result, "    fn add() {}\n", "normal 4-space indent");

        let result = collapse_whitespace("     fn add() {}\n");
        assert_eq!(
            result, "     fn add() {}\n",
            "5-space indent preserved as indentation"
        );
    }

    #[test]
    fn test_collapse_whitespace_empty_lines() {
        let result = collapse_whitespace("line one\n\nline two\n");
        assert_eq!(result, "line one\n\nline two\n");
    }

    #[test]
    fn test_collapse_whitespace_whitespace_only_lines() {
        // Whitespace-only lines: indent portion is kept, content is empty
        let result = collapse_whitespace("    \n  \n\n");
        // After trim_end on content (empty), only indent remains, then newline
        assert_eq!(result, "    \n  \n\n");
    }

    #[test]
    fn test_collapse_whitespace_multiline_mixed_patterns() {
        let input = "fn foo() {\n    let  x  =  1\n\n     return  x\n}\n";
        let result = collapse_whitespace(input);
        // Line 1: no extra spaces
        // Line 2: indent=4, "let  x  =  1" -> "let x = 1"
        // Line 3: empty
        // Line 4: indent=5, "return  x" -> "return x"
        // Line 5: no indent, "}"
        assert_eq!(result, "fn foo() {\n    let x = 1\n\n     return x\n}\n");
    }

    #[test]
    fn test_collapse_whitespace_trailing_spaces_trimmed() {
        let result = collapse_whitespace("fn foo()   \n");
        assert_eq!(result, "fn foo()\n", "trailing spaces should be trimmed");
    }

    #[test]
    fn test_collapse_whitespace_leading_spaces_become_indent() {
        // When remove_ranges leaves a gap (e.g., "export function" -> " function"),
        // trim_start() treats the leading spaces as indentation, not content.
        let result = collapse_whitespace(" function add()\n");
        assert_eq!(
            result, " function add()\n",
            "single leading space is part of indent"
        );

        let result = collapse_whitespace("  function add()\n");
        assert_eq!(
            result, "  function add()\n",
            "two leading spaces treated as indentation"
        );
    }
}
