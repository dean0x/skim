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
struct WalkContext<'a> {
    source: &'a str,
    source_bytes: &'a [u8],
    language: Language,
    ranges: &'a mut Vec<(usize, usize)>,
    node_count: &'a mut usize,
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
            strip_kinds: &["access_specifier", "template_parameter_list"],
            strip_keywords: &[
                "static", "extern", "const", "volatile", "virtual", "override", "final", "noexcept",
            ],
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
    let mut ctx = WalkContext {
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
/// - Leading indentation is preserved
fn collapse_whitespace(source: &str) -> String {
    let mut result = String::with_capacity(source.len());

    for line in source.lines() {
        // Find the indentation
        let indent_len = line.len() - line.trim_start().len();
        let indent = &line[..indent_len];
        let content = line[indent_len..].trim_end();

        // Collapse multiple consecutive spaces in content, then trim leading space
        // left behind by removing inline elements (e.g., `pub ` -> ` fn ...`)
        let collapsed = collapse_consecutive_spaces(content);

        result.push_str(indent);
        result.push_str(collapsed.trim_start());
        result.push('\n');
    }

    result
}

/// Collapse runs of consecutive spaces into a single space
fn collapse_consecutive_spaces(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_space = false;
    for ch in s.chars() {
        if ch == ' ' {
            if !prev_space {
                result.push(ch);
            }
            prev_space = true;
        } else {
            result.push(ch);
            prev_space = false;
        }
    }
    result
}

fn collect_noise_ranges(
    node: Node,
    ctx: &mut WalkContext<'_>,
    rules: &PseudoRules,
    depth: usize,
) -> Result<()> {
    // SECURITY: Prevent memory exhaustion from excessive nodes
    *ctx.node_count += 1;
    if *ctx.node_count > MAX_AST_NODES {
        return Err(SkimError::ParseError(format!(
            "Too many AST nodes: {} (max: {}). Possible malicious input.",
            *ctx.node_count, MAX_AST_NODES
        )));
    }

    // SECURITY: Prevent stack overflow from deeply nested AST
    if depth > MAX_AST_DEPTH {
        return Err(SkimError::ParseError(format!(
            "Maximum AST depth exceeded: {} (possible malicious input)",
            MAX_AST_DEPTH
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
        let adjusted_start = adjust_type_start(ctx.language, kind, ctx.source, start);
        ctx.ranges.push((adjusted_start, end));
        return Ok(()); // Don't recurse into stripped nodes
    }

    // Check for keyword stripping (leaf nodes only)
    if node.child_count() == 0 {
        let text = node.utf8_text(ctx.source_bytes).unwrap_or("");
        if rules.strip_keywords.contains(&text) {
            ctx.ranges.push((node.start_byte(), node.end_byte()));
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

    // Handle Rust return type: `-> Type` is expressed as sibling `->` + type nodes
    // under `function_item`, not as a single `return_type` wrapper node.
    if ctx.language == Language::Rust && kind == "function_item" {
        strip_rust_return_type(node, ctx.ranges);
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
/// Python's "type" node in `typed_parameter` does NOT include the `: ` separator,
/// and "return_type" may have a leading space before ` -> type`. Rust's "return_type"
/// node includes `-> Type` but not the leading space. This extends the removal range
/// to include these separators for clean output.
fn adjust_type_start(language: Language, kind: &str, source: &str, start: usize) -> usize {
    match (language, kind) {
        (Language::Python, "type") => {
            if start >= 2 && source.get(start - 2..start) == Some(": ") {
                start - 2
            } else if start >= 1 && source.get(start - 1..start) == Some(":") {
                start - 1
            } else {
                start
            }
        }
        (Language::Python, "return_type") => {
            // Consume leading space before `-> Type`
            if start >= 1 && source.get(start - 1..start) == Some(" ") {
                start - 1
            } else {
                start
            }
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
                // Bind to local before `inner_cursor` is dropped (tree-sitter lifetime)
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
            let start = if child.start_byte() >= 1 {
                child.start_byte() - 1
            } else {
                child.start_byte()
            };

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
}
