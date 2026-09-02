//! Pseudo mode transformation — strips syntactic noise while preserving logic flow.
//!
//! ARCHITECTURE: Removes type annotations, decorators, semicolons, and other
//! syntactic noise to produce pseudocode-like output.  The following are
//! intentionally preserved as API surface (A4 contract):
//! - Visibility modifiers (`pub`/`export`/access modifiers)
//! - Function return type annotations (Python `-> T`, TypeScript `: T` at return
//!   position, Rust `-> T` via normal recursion since Rust has no strip_kinds for
//!   return types)
//!
//! For Python and TypeScript, parameter, variable, and property type annotations are
//! still stripped. Rust preserves parameter types (pseudo mode strips only lifetimes,
//! type parameters, where clauses, and attributes for Rust).
//! Uses the same collect-ranges-then-remove pattern as minimal.rs.
//!
//! Token reduction target: 30-50%
//!
//! # Traversal rule (PF-020)
//!
//! > The parent's own child loop already holds every relational fact tree-sitter
//! > would otherwise re-derive from the root — thread it down instead of asking
//! > for it back.
//!
//! A `TSNode` carries no parent pointer. `parent()`, `prev_sibling()` and
//! `next_sibling()` all re-derive their answer by descending from the tree root,
//! and the sibling variants then scan the parent's child list from the start —
//! so `prev_sibling()`/`next_sibling()` are O(index-in-parent) and a per-node
//! backward walk is O(N³) (that is the defect B1 fixed in `minimal.rs`).
//! `collect_noise_ranges` therefore threads a [`WalkPosition`] down from each
//! parent's child loop, where the parent kind, the previous sibling and the
//! `return_type` field membership are all already in hand at O(1).
//!
//! Note the asymmetry that makes this worth writing down: the fix is to stop
//! asking for the *parent*, not to stop using field lookups. `child_by_field_name`
//! called on a node you already hold descends *into* that node and is cheap; it is
//! `parent()` in front of it that pays the root descent.

use crate::transform::truncate::NodeSpan;
use crate::{Language, Result, SkimError, TransformConfig};
use tree_sitter::{Node, Tree};

use super::minimal::{
    CommentClassification, MAX_AST_DEPTH, MAX_AST_NODES, adjust_range_for_line_removal,
    build_newline_table, compute_go_doc_comment_starts, compute_header_end_byte,
    is_removable_comment, remove_ranges, trim_and_normalize,
};
use super::{compute_line_map_from_removed_ranges, normalize_line_map_blanks};
use crate::transform::utils::is_function_scope_kind;

/// Bundled parameters for the recursive noise walker to avoid parameter explosion
struct NoiseWalkContext<'a> {
    source: &'a str,
    source_bytes: &'a [u8],
    language: Language,
    ranges: &'a mut Vec<(usize, usize)>,
    node_count: &'a mut usize,
    /// Per-file comment-classification tables, precomputed before the walk.
    classification: CommentClassification<'a>,
}

/// A node's position within its parent's child list, captured by the parent's own
/// child loop and threaded down instead of re-derived from the tree root.
///
/// Every field is exactly what the corresponding parent-derived `Node` accessor
/// would return, so a site that used to call `parent()` / `prev_sibling()` /
/// `child_by_field_name` reads the same fact here at O(1) (PF-020). The kinds are
/// `&'static str` because tree-sitter interns node kinds in the language table.
///
/// `Default` is the root node's position: no parent, no previous sibling, no field.
#[derive(Clone, Copy, Default)]
struct WalkPosition {
    /// Equivalent to `node.parent().map(Node::kind)`.
    parent_kind: Option<&'static str>,
    /// Equivalent to `node.prev_sibling().map(|s| s.kind())`. Anonymous siblings
    /// are included, matching `Node::children`'s iteration order (and therefore
    /// `prev_sibling`, which is also anonymous-inclusive).
    prev_sibling_kind: Option<&'static str>,
    /// Equivalent to `node.prev_sibling().map(|s| s.start_byte())`.
    prev_sibling_start: Option<usize>,
    /// Equivalent to `is_return_type_candidate(node.kind(), language)` **and**
    /// `node.parent().child_by_field_name("return_type")` resolving back to
    /// `node` — the exact predicate that guards return-type preservation
    /// (ADR-007). The parent resolves the field on itself, so the `parent()`
    /// call disappears while `child_by_field_name`'s semantics are unchanged.
    ///
    /// Deliberately NOT derived from `TreeCursor::field_name()`: in
    /// tree-sitter-swift 0.7 the cursor reports field `name` for the very node
    /// `child_by_field_name("return_type")` resolves to, so the two are not
    /// interchangeable across grammars. `pseudo_walk_position_matches_node_apis_*`
    /// pins this.
    is_return_type_field: bool,
}

/// Yields each child of a node paired with the [`WalkPosition`] the parent can
/// supply at O(1).
///
/// This is the SINGLE source of truth for walk-position derivation: both
/// `collect_noise_ranges` and its differential test drive it, so the test compares
/// production output against the `Node` APIs rather than against a second copy of
/// the derivation that could drift from it independently.
///
/// Iteration order is identical to `Node::children` — all children, anonymous
/// included — and is bounded by `child_count`.
struct ChildPositions<'tree> {
    parent: Node<'tree>,
    parent_kind: &'static str,
    language: Language,
    cursor: tree_sitter::TreeCursor<'tree>,
    /// Remaining children to yield. Hard upper bound on the iteration.
    remaining: usize,
    /// `false` until `goto_first_child` has run.
    descended: bool,
    prev_sibling_kind: Option<&'static str>,
    prev_sibling_start: Option<usize>,
    /// Memoized `parent.child_by_field_name("return_type").map(Node::id)`. Resolved
    /// at most once per parent, and only once a child turns out to be a candidate
    /// kind, so grammars without return-type annotations never pay the lookup.
    ///
    /// `child_by_field_name` on the parent we are already holding descends INTO it;
    /// it is `parent()` in front of it that pays the root descent (PF-020).
    return_type_child_id: Option<Option<usize>>,
}

impl<'tree> ChildPositions<'tree> {
    fn new(parent: Node<'tree>, language: Language) -> Self {
        Self {
            parent,
            parent_kind: parent.kind(),
            language,
            cursor: parent.walk(),
            remaining: parent.child_count(),
            descended: false,
            prev_sibling_kind: None,
            prev_sibling_start: None,
            return_type_child_id: None,
        }
    }
}

impl<'tree> Iterator for ChildPositions<'tree> {
    type Item = (Node<'tree>, WalkPosition);

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let advanced = if self.descended {
            self.cursor.goto_next_sibling()
        } else {
            self.descended = true;
            self.cursor.goto_first_child()
        };
        if !advanced {
            self.remaining = 0;
            return None;
        }
        self.remaining -= 1;

        let child = self.cursor.node();
        let parent = self.parent;
        let is_return_type_field = is_return_type_candidate(child.kind(), self.language)
            && *self
                .return_type_child_id
                .get_or_insert_with(|| parent.child_by_field_name("return_type").map(|n| n.id()))
                == Some(child.id());

        let pos = WalkPosition {
            parent_kind: Some(self.parent_kind),
            prev_sibling_kind: self.prev_sibling_kind,
            prev_sibling_start: self.prev_sibling_start,
            is_return_type_field,
        };

        self.prev_sibling_kind = Some(child.kind());
        self.prev_sibling_start = Some(child.start_byte());
        Some((child, pos))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(self.remaining))
    }
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
        "lifetime" | "mutable_specifier" | "readonly" | "abstract"
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
            // `export` is structural API/re-export information — preserved (A4 contract).
            strip_keywords: &[],
            strip_semicolons: true,
            strip_self_param: false,
        },
        Language::JavaScript => PseudoRules {
            strip_kinds: &["decorator"],
            // `export` is structural API/re-export information — preserved (A4 contract).
            strip_keywords: &[],
            strip_semicolons: true,
            strip_self_param: false,
        },
        Language::Python => PseudoRules {
            // "type" covers parameter/variable annotations (`a: int`).
            // "return_type" is intentionally absent: it is a field name in the
            // grammar, not a node kind — it never appeared in real trees.
            // Return-type annotations are preserved (A4); the guard in
            // collect_noise_ranges catches them before strip_kinds fires.
            strip_kinds: &["type", "decorator"],
            strip_keywords: &[],
            strip_semicolons: false,
            strip_self_param: true,
        },
        Language::Rust => PseudoRules {
            strip_kinds: &[
                // "visibility_modifier" intentionally NOT listed — pub/pub(crate)/pub(super)
                // convey API surface and re-export intent; preserving them matches the
                // decision to keep visibility in pseudo output (A4 contract).
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
            // Access modifiers (public/private/protected) preserved — API surface (A4).
            // Non-visibility modifiers (static/final/abstract) still stripped as noise.
            strip_keywords: &["static", "final", "abstract"],
            strip_semicolons: true,
            strip_self_param: false,
        },
        Language::C => PseudoRules {
            strip_kinds: &[],
            // C has no OOP access modifiers; `static`/`extern` are linkage specifiers —
            // intentionally treated as noise (not preserved as API surface).
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
            // Access modifiers (public/private/protected/internal) preserved (A4).
            // Non-visibility modifiers still stripped as noise.
            strip_keywords: &[
                "static", "virtual", "override", "sealed",
                "abstract",
                // NOTE: `async` intentionally NOT stripped — it changes calling semantics
            ],
            strip_semicolons: true,
            strip_self_param: false,
        },
        Language::Ruby => PseudoRules {
            strip_kinds: &[],
            // Access modifiers (private/protected/public) preserved (A4).
            strip_keywords: &[],
            strip_semicolons: false,
            strip_self_param: false,
        },
        Language::Kotlin => PseudoRules {
            strip_kinds: &["type_parameters", "annotation"],
            // Access modifiers (public/private/protected/internal) preserved (A4).
            // `open` is an inheritance modifier (not visibility) — still stripped as noise.
            // `data`/`sealed`/`override`/`abstract` are non-visibility — still stripped.
            strip_keywords: &[
                "open", "data", "sealed", "override",
                "abstract",
                // NOTE: `suspend` intentionally NOT stripped — it changes calling semantics
            ],
            strip_semicolons: false,
            strip_self_param: false,
        },
        Language::Swift => PseudoRules {
            strip_kinds: &["attribute", "type_parameters"],
            // All Swift visibility modifiers (public/private/internal/fileprivate/open)
            // preserved (A4).  Note: Swift `open` is a visibility level (unlike Kotlin).
            // Non-visibility modifiers (static/override/final) still stripped.
            strip_keywords: &[
                "static", "override",
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
        Language::Bash => PseudoRules {
            // Shell scripts have no type annotations or decorators to strip;
            // semicolons are meaningful in bash (e.g. `if ...; then`)
            strip_kinds: &[],
            strip_keywords: &[],
            strip_semicolons: false,
            strip_self_param: false,
        },
        // Serde languages and Markdown are handled as passthrough before reaching here
        Language::Markdown | Language::Json | Language::Yaml | Language::Toml => PseudoRules {
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
    config: &TransformConfig,
) -> Result<(String, Vec<NodeSpan>)> {
    let (text, spans, _line_map) =
        transform_pseudo_with_spans_and_line_map(source, tree, language, config)?;
    Ok((text, spans))
}

/// Transform source by stripping syntactic noise, returning NodeSpan metadata AND a source line map.
///
/// ARCHITECTURE: The line map is computed from the byte-level removal ranges rather
/// than by text matching. Text matching fails for lines that are *partially* modified
/// (e.g., `def f(a: int) -> int:` → `def f(a):`) because the output line does not
/// appear verbatim in the source.
///
/// The byte-range approach works for any mutation: after all ranges are removed,
/// each output byte can be traced back to its source byte, and therefore to its
/// source line number. The post-processing steps (`collapse_whitespace`,
/// `trim_and_normalize`) operate within lines without changing line-to-source
/// correspondence — with two exceptions where `trim_and_normalize` drops lines:
///
/// 1. **Leading blank lines** — blank lines before the first non-blank content
///    are silently dropped (`result.push_str("")` is a no-op on an empty accumulator).
/// 2. **3+ consecutive blank lines** — runs longer than 2 are capped; the third
///    and subsequent blank lines in a run are skipped.
///
/// `normalize_line_map_blanks` mirrors both rules on the line map so the map stays
/// in sync with the final output text.
pub(crate) fn transform_pseudo_with_spans_and_line_map(
    source: &str,
    tree: &Tree,
    language: Language,
    _config: &TransformConfig,
) -> Result<(String, Vec<NodeSpan>, Vec<usize>)> {
    let rules = get_pseudo_rules(language);

    // Precompute the module-header boundary in a single O(N) forward pass.
    // This prevents the O(N³) per-node backward walk inside is_module_header_comment.
    let header_end_byte = compute_header_end_byte(tree.root_node(), source, language);
    // Precompute Go doc-comment starts in a single O(N) TreeCursor pass. This
    // prevents the Θ(M³/3) per-node forward sibling walk inside is_go_doc_comment.
    let go_doc_comment_starts = compute_go_doc_comment_starts(tree.root_node(), source, language);

    // Single-pass collection: comments AND noise ranges in one AST walk
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut node_count: usize = 0;
    let mut ctx = NoiseWalkContext {
        source,
        source_bytes: source.as_bytes(),
        language,
        ranges: &mut ranges,
        node_count: &mut node_count,
        classification: CommentClassification {
            header_end_byte,
            go_doc_comment_starts: &go_doc_comment_starts,
        },
    };
    // The root has no parent, no previous sibling and no field name — exactly
    // `WalkPosition::default()`. Every deeper position is supplied by the caller's
    // child loop.
    collect_noise_ranges(
        tree.root_node(),
        &mut ctx,
        &rules,
        0,
        false,
        WalkPosition::default(),
    )?;

    // Sort, dedup, and adjust ranges for full line removal
    ctx.ranges.sort_unstable_by_key(|&(start, _)| start);
    ctx.ranges.dedup();

    // Precompute the newline offset table so adjust_range_for_line_removal
    // resolves line boundaries in O(log N) (binary search) instead of O(start)
    // (rfind scan), reducing the total from O(N²) to O(N log N) across N ranges.
    let newlines = build_newline_table(source);

    let mut final_ranges: Vec<(usize, usize)> = ctx
        .ranges
        .iter()
        .map(|&(start, end)| adjust_range_for_line_removal(source, start, end, &newlines))
        .collect();

    // Re-sort after adjustment (line-level adjustments can change ordering)
    final_ranges.sort_unstable_by_key(|&(start, _)| start);

    // Compute the source line map from the byte-level removal ranges.
    // Must be done before remove_ranges, using the ranges themselves, so that
    // modified lines (e.g. `def f(a: int):` → `def f(a):`) still map to their
    // correct source line rather than getting source_line=0 from text matching.
    let line_map_after_removal = compute_line_map_from_removed_ranges(source, &final_ranges);

    let result = remove_ranges(source, &final_ranges)?;

    // Post-process — collapse whitespace artifacts and normalize.
    // collapse_whitespace is line-count-preserving (works within lines only).
    let result = collapse_whitespace(&result);
    // trim_and_normalize may drop lines when there are 3+ consecutive blanks.
    // Capture the text before that step so normalize_line_map_blanks can replay
    // the same logic to keep the line map in sync.
    let pre_normalized = result;
    let result = trim_and_normalize(&pre_normalized);

    // Mirror trim_and_normalize's blank-line dropping on the line map so the
    // two stay in sync (3+ consecutive blank lines → drop beyond 2).
    let line_map = normalize_line_map_blanks(&pre_normalized, line_map_after_removal);

    // Build spans (single source_file span for truncation compatibility)
    let line_count = result.lines().count();
    let spans = vec![NodeSpan::new(0..line_count, "source_file")];

    Ok((result, spans, line_map))
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
fn handle_language_special_cases(
    node: Node,
    ctx: &mut NoiseWalkContext<'_>,
    pos: WalkPosition,
) -> Option<Result<()>> {
    let kind = node.kind();
    match ctx.language {
        Language::Cpp if kind == "access_specifier" => {
            // Access specifiers (`public:`, `private:`, `protected:`) are visibility
            // markers that convey API surface.  They are preserved in pseudo mode (A4).
            // Return None so the normal recursion path processes children instead of
            // accumulating a removal range.
            None
        }
        Language::Cpp if kind == "template_parameter_list" => {
            // `template<typename T>` is two siblings: `template` keyword + parameter list.
            // The previous sibling is threaded from the parent's child loop rather than
            // read back via `prev_sibling()`, which is O(index-in-parent) (PF-020).
            let template_start = if pos.prev_sibling_kind == Some("template") {
                pos.prev_sibling_start.unwrap_or_else(|| node.start_byte())
            } else {
                node.start_byte()
            };
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
    in_function_body: bool,
    pos: WalkPosition,
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

    let kind = node.kind();

    // Check for removable comments (merged from former separate pass).
    // Uses the same doc-comment/shebang/function-body filtering as minimal mode.
    // Pass the threaded depth and in_function_body to avoid O(depth) parent() calls.
    if is_removable_comment(
        node,
        ctx.source,
        ctx.language,
        ctx.classification,
        depth,
        in_function_body,
    ) {
        ctx.ranges.push((node.start_byte(), node.end_byte()));
        return Ok(()); // Comments have no children to recurse into
    }

    // Check if this node kind should be stripped
    if rules.strip_kinds.contains(&kind) {
        // Return type annotations are API surface — preserved wholesale (A4 contract,
        // ADR-007). Stopping recursion here means nested type args (Promise<User>,
        // tuple[int, str]) survive intact.  Param/variable/property annotations under
        // other field names still fall through to be stripped.
        //
        // `is_return_type_field` is `is_return_type_candidate(kind, language)` AND the
        // `child_by_field_name("return_type")` identity test, both resolved by the
        // parent's child loop — see `WalkPosition` (PF-020).
        if pos.is_return_type_field {
            return Ok(());
        }

        let start = node.start_byte();
        let end = node.end_byte();
        let adjusted_start = adjust_type_start(ctx.language, kind, ctx.source_bytes, start);
        // Consume trailing whitespace only for inline modifiers (lifetime, mut, readonly,
        // abstract, etc.) where the space separates the modifier from the next token. Do NOT
        // consume for type annotations — their trailing space may belong to assignment syntax
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

    // Check for semicolon stripping (statement-terminating only, not for-loop headers).
    // The parent kind is threaded from the parent's child loop rather than read back
    // via `parent()`, which re-descends from the tree root (PF-020).
    if rules.strip_semicolons && kind == ";" {
        let is_for_loop = pos.parent_kind.is_some_and(|parent_kind| {
            matches!(
                parent_kind,
                "for_statement" | "for_in_statement" | "for_of_statement"
            )
        });
        if !is_for_loop {
            ctx.ranges.push((node.start_byte(), node.end_byte()));
            return Ok(());
        }
    }

    // Handle Python self/cls removal
    if rules.strip_self_param && kind == "parameters" {
        strip_python_self_param(node, ctx.source_bytes, ctx.ranges);
    }

    // Handle language-specific multi-node patterns (C++ siblings)
    if let Some(result) = handle_language_special_cases(node, ctx, pos) {
        return result;
    }

    // Recurse into children through `ChildPositions`, which walks with a TreeCursor
    // — the only O(1)-per-step traversal API tree-sitter offers (PF-020) — and hands
    // each child the relational facts its parent already holds, instead of letting the
    // child ask for them back via a root-descending `parent()` / `prev_sibling()`.
    // `child_in_body` is threaded on the same principle, so `is_removable_comment`
    // never calls `parent()` either.
    let child_in_body = in_function_body || is_function_scope_kind(kind, ctx.language);
    let language = ctx.language;
    for (child, child_pos) in ChildPositions::new(node, language) {
        collect_noise_ranges(child, ctx, rules, depth + 1, child_in_body, child_pos)?;
    }

    Ok(())
}

/// Adjust the start position for type annotations to include their separators.
///
/// Python's "type" node in `typed_parameter` does NOT include the `: ` separator.
/// This extends the removal range backward to include the `: ` separator for clean
/// output.  Return-type annotations are preserved (A4) and never reach this function.
fn adjust_type_start(language: Language, kind: &str, source: &[u8], start: usize) -> usize {
    match (language, kind) {
        // Python parameter / variable type: `a: int` — consume the `: ` separator.
        // Return-type annotations (`-> int`) are preserved (A4) and never reach here.
        (Language::Python, "type") => {
            const SEPARATORS: &[&[u8]] = &[b": ", b":"];
            // Derive the look-back window from the longest separator so this stays
            // in sync automatically if SEPARATORS ever gains a longer entry.
            let max_sep_len = SEPARATORS.iter().map(|s| s.len()).max().unwrap_or(0);
            let prefix = source
                .get(start.saturating_sub(max_sep_len)..start)
                .unwrap_or(b"");
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

/// Returns `true` when `node` is a type-annotation node whose parent treats it
/// as the function's return type via the `return_type` field.
///
/// This guards the wholesale preservation of return-type annotations in pseudo mode
/// (A4 contract).  Only Python `"type"` and TypeScript `"type_annotation"` nodes are
/// candidates; all other languages either do not use these kinds or already preserve
/// return types through normal recursion.
///
/// Stopping recursion at this point means nested type arguments
/// (`Promise<User>`, `tuple[int, str]`) survive intact inside the return annotation.
///
/// Only Python `"type"` and TypeScript `"type_annotation"` nodes are candidates; all
/// other languages either do not use these kinds or already preserve return types
/// through normal recursion. A candidate still has to BE the parent's `return_type`
/// field child — that half of the test is resolved by the parent's child loop and
/// arrives as [`WalkPosition::is_return_type_field`].
fn is_return_type_candidate(kind: &str, language: Language) -> bool {
    matches!(
        (language, kind),
        (Language::Python, "type") | (Language::TypeScript, "type_annotation")
    )
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
                child
                    .children(&mut inner_cursor)
                    .next()
                    .and_then(|first_child| first_child.utf8_text(source_bytes).ok())
                    .is_some_and(|t| matches!(t, "self" | "cls"))
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
    if let Some(next) = children.get(index + 1)
        && next.kind() == ","
    {
        let comma_end = next.end_byte();
        if comma_end < source_bytes.len() && source_bytes[comma_end] == b' ' {
            return comma_end + 1;
        }
        return comma_end;
    }
    end
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Unwrapping/expect is acceptable in tests
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
        // Param type annotations are stripped; RETURN type annotation is preserved
        // as API surface (A4 contract — pseudo mode preserves return types).
        let source = "function add(a: number, b: number): number {\n    return a + b;\n}\n";
        let result = transform(source, Language::TypeScript);
        // Parameter type annotations should be stripped
        assert!(
            result.contains("function add(a, b)"),
            "function name and params preserved without param types, got: {result}"
        );
        // Return type annotation is preserved as API surface
        assert!(
            result.contains("): number"),
            "return type annotation must be preserved as API surface, got: {result}"
        );
        assert!(result.contains("return a + b"), "logic preserved");
    }

    #[test]
    fn test_typescript_pseudo_preserves_export() {
        // `export` is API-surface information — preserved in pseudo mode (A4 contract).
        let source =
            "export function greet(name: string): string {\n    return `Hello, ${name}!`;\n}\n";
        let result = transform(source, Language::TypeScript);
        assert!(
            result.contains("export"),
            "export keyword must be preserved as API surface, got: {result}"
        );
        assert!(
            result.contains("function greet(name)"),
            "function signature preserved without types, got: {result}"
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

    #[test]
    fn test_typescript_pseudo_strips_class_property_annotation() {
        // Class property type annotations (e.g. `count: number = 0`) are stripped in
        // pseudo mode — the `: number` annotation is removed; the property name and
        // value survive.  `is_return_field_child` does NOT fire here because the
        // parent field name is `type`, not `return_type`, so the annotation is
        // correctly stripped rather than accidentally preserved.
        let source = "class Counter {\n    count: number = 0\n}\n";
        let result = transform(source, Language::TypeScript);
        assert!(
            !result.contains("count: number"),
            "class property type annotation should be stripped, got: {result}"
        );
        assert!(
            result.contains("count") && result.contains("= 0"),
            "property name and value must be preserved, got: {result}"
        );
    }

    // ========================================================================
    // JavaScript pseudo tests
    // ========================================================================

    #[test]
    fn test_javascript_pseudo_preserves_export_strips_semicolons() {
        // `export` is API-surface information — preserved (A4); semicolons still stripped.
        let source = "export function add(x, y) {\n    return x + y;\n}\n";
        let result = transform(source, Language::JavaScript);
        assert!(
            result.contains("export"),
            "export must be preserved as API surface, got: {result}"
        );
        assert!(
            result.contains("function add(x, y)"),
            "function preserved, got: {result}"
        );
        // Semicolons are still stripped
        assert!(
            result.contains("return x + y"),
            "logic preserved, got: {result}"
        );
    }

    // ========================================================================
    // Python pseudo tests
    // ========================================================================

    #[test]
    fn test_python_pseudo_strips_type_hints() {
        // Param type annotations are stripped; RETURN type annotation is preserved
        // as API surface (A4 contract — pseudo mode preserves return types).
        let source =
            "def calculate_sum(a: int, b: int) -> int:\n    result = a + b\n    return result\n";
        let result = transform(source, Language::Python);
        // Parameter type annotations should be stripped
        assert!(
            !result.contains(": int"),
            "param type annotations should be stripped, got: {result}"
        );
        // Return type annotation is preserved as API surface
        assert!(
            result.contains("-> int"),
            "return type must be preserved as API surface, got: {result}"
        );
        assert!(
            result.contains("def calculate_sum(a, b)"),
            "function signature preserved without param types, got: {result}"
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

    #[test]
    fn test_python_pseudo_strips_variable_annotation() {
        // Standalone variable annotations (e.g. `x: int = 5`) are stripped in
        // pseudo mode — the `: int` annotation is removed; name and value survive.
        // `is_return_field_child` does NOT fire here (parent is an assignment, not a
        // function_definition with a `return_type` field), so the annotation is
        // correctly stripped rather than accidentally preserved.
        let source = "x: int = 5\n";
        let result = transform(source, Language::Python);
        assert!(
            !result.contains(": int"),
            "variable type annotation should be stripped, got: {result}"
        );
        assert!(
            result.contains("x") && result.contains("= 5"),
            "variable name and value must be preserved, got: {result}"
        );
    }

    // ========================================================================
    // Rust pseudo tests
    // ========================================================================

    #[test]
    fn test_rust_pseudo_preserves_visibility() {
        // `pub` and `pub(crate)` convey API surface — preserved in pseudo mode (A4).
        let source = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let result = transform(source, Language::Rust);
        assert!(
            result.contains("pub "),
            "visibility modifier must be preserved as API surface, got: {result}"
        );
        assert!(
            result.contains("fn add"),
            "function preserved, got: {result}"
        );
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
    fn test_rust_pseudo_preserves_return_type() {
        // Return type is preserved as API surface (A4 contract).
        let source = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";
        let result = transform(source, Language::Rust);
        assert!(
            result.contains("-> i32"),
            "return type must be preserved as API surface, got: {result}"
        );
        assert!(result.contains("fn add"), "function preserved");
    }

    // ========================================================================
    // Java pseudo tests
    // ========================================================================

    #[test]
    fn test_java_pseudo_preserves_visibility() {
        // Access modifiers (public/private/protected) convey API surface — preserved (A4).
        // Non-visibility modifiers (static/final/abstract) are still stripped.
        let source = "public class Simple {\n    private int value;\n    public int add(int a, int b) {\n        return a + b;\n    }\n}\n";
        let result = transform(source, Language::Java);
        assert!(
            result.contains("public "),
            "public modifier must be preserved as API surface, got: {result}"
        );
        assert!(
            result.contains("private "),
            "private modifier must be preserved as API surface, got: {result}"
        );
        assert!(
            result.contains("class Simple"),
            "class preserved, got: {result}"
        );
        assert!(
            result.contains("int add(int a, int b)"),
            "method preserved, got: {result}"
        );
    }

    #[test]
    fn test_java_pseudo_still_strips_static_final() {
        // Non-visibility modifiers must still be stripped (A4 contract: only visibility preserved).
        let source = "public class Simple {\n    public static final int MAX = 100;\n    public int add(int a, int b) {\n        return a + b;\n    }\n}\n";
        let result = transform(source, Language::Java);
        assert!(
            !result.contains("static "),
            "static must still be stripped, got: {result}"
        );
        assert!(
            !result.contains("final "),
            "final must still be stripped, got: {result}"
        );
        // visibility preserved
        assert!(
            result.contains("public "),
            "public must still be present, got: {result}"
        );
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
    fn test_cpp_pseudo_preserves_access_specifiers() {
        // C++ access specifiers (public:/private:/protected:) convey API surface
        // and are preserved in pseudo mode (A4 contract).
        let source = "class Foo {\npublic:\n    int bar();\nprivate:\n    int baz_;\n};\n";
        let result = transform(source, Language::Cpp);
        assert!(
            result.contains("public:"),
            "public: access specifier must be preserved, got: {result}"
        );
        assert!(
            result.contains("private:"),
            "private: access specifier must be preserved, got: {result}"
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

        // The transform itself still enforces the cap; the *dispatcher* is what
        // degrades a ComplexityLimit to passthrough (see types.rs). This direct
        // call therefore surfaces the typed cap error.
        let result = transform_pseudo(&source, &tree, Language::Python, &config);
        let err = result.expect_err("Expected error when exceeding MAX_AST_NODES");
        assert!(
            err.is_complexity_limit(),
            "Expected a ComplexityLimit error, got: {err}"
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
        let (result, has_errors) = Language::Markdown
            .transform_source(source, &config)
            .unwrap();
        assert_eq!(
            result, source,
            "Markdown should pass through unchanged in pseudo mode"
        );
        assert!(!has_errors, "passthrough should not report parse errors");
    }

    // ========================================================================
    // Regression tests: output quality bug fixes
    // ========================================================================

    #[test]
    fn test_python_pseudo_no_arrow_residue() {
        // Return type is now preserved as API surface (A4 contract); the full
        // `-> int` annotation must survive intact without residue or duplication.
        let source = "def calculate_sum(a: int, b: int) -> int:\n    return a + b\n";
        let result = transform(source, Language::Python);
        // Return type preserved: must contain `-> int:`
        assert!(
            result.contains("-> int:"),
            "return type must be preserved intact, got: {result}"
        );
        // Param types stripped: no `: int` in param list
        assert!(
            result.contains("def calculate_sum(a, b)"),
            "function signature clean (param types stripped), got: {result}"
        );
        // No double-arrow residue
        assert!(
            !result.contains("->  int") && !result.contains("-> -> "),
            "no arrow residue or duplication, got: {result}"
        );
    }

    #[test]
    fn test_cpp_pseudo_access_specifiers_preserved_no_orphaned_colon() {
        // C++ access specifiers (public:/private:) are now preserved as-is (A4).
        // The old orphaned-colon guard is no longer needed because we no longer strip
        // access specifiers; the full `public:` token remains intact.
        let source = "class Foo {\npublic:\n    int bar();\nprivate:\n    int baz_;\n};\n";
        let result = transform(source, Language::Cpp);
        assert!(
            result.contains("public:"),
            "public: must be preserved intact (no orphaned colon), got: {result}"
        );
        assert!(
            !result.lines().any(|l| l.trim() == ":"),
            "no orphaned colon lines must exist, got: {result}"
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
    fn test_rust_pseudo_trait_preserves_return_type() {
        // Return type is preserved as API surface (A4 contract); the trailing
        // `;` on trait method signatures is still stripped.
        let source = "pub trait Compute {\n    fn compute(&self, value: i32) -> i32;\n    fn reset(&mut self);\n}\n";
        let result = transform(source, Language::Rust);
        assert!(
            result.contains("-> i32"),
            "trait method return type must be preserved as API surface, got: {result}"
        );
        assert!(
            result.contains("fn compute"),
            "trait method name preserved, got: {result}"
        );
    }

    #[test]
    fn test_rust_pseudo_lifetime_no_space() {
        // BUG 6: Stripping lifetime from `&'a str` left `& str` (extra space).
        // Return type is preserved (A4), with lifetime stripped inside it: `-> &str`.
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
        // Return type preserved with lifetime stripped: -> &str (not -> &'a str)
        assert!(
            result.contains("-> &str"),
            "return type must be preserved with lifetime stripped, got: {result}"
        );
    }

    #[test]
    fn test_typescript_pseudo_no_leading_space() {
        // BUG 5 (historical): Stripping `export` used to leave a leading space.
        // Now `export` is preserved (A4), so output starts with "export function …"
        // — no leading space in either case.  The no-leading-space invariant holds.
        let source = "export function add(a: number, b: number): number {\n    return a + b;\n}\n";
        let result = transform(source, Language::TypeScript);
        assert!(
            !result.starts_with(' '),
            "output should not start with a leading space, got: {result}"
        );
        assert!(
            result.contains("function add(a, b)"),
            "function signature clean (type annotations stripped), got: {result}"
        );
    }

    #[test]
    fn test_java_pseudo_no_leading_spaces() {
        // BUG 5 (historical): Stripping `static final` must not leave leading spaces.
        // `public`/`private` are now preserved (A4); `static`/`final` are still stripped.
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
        // Return types preserved (A4); param types stripped.
        let source = "def foo(x: int) -> str:\n    return str(x)\n\ndef bar(y: str) -> int:\n    return int(y)\n";
        let result = transform(source, Language::Python);
        // Return types must be preserved
        assert!(
            result.contains("-> str"),
            "first function return type must be preserved, got: {result}"
        );
        assert!(
            result.contains("-> int"),
            "second function return type must be preserved, got: {result}"
        );
        // Param types stripped (the `x:` and `y:` annotations gone)
        assert!(
            result.contains("def foo(x)"),
            "first function param type stripped, got: {result}"
        );
        assert!(
            result.contains("def bar(y)"),
            "second function param type stripped, got: {result}"
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
        // Rust function body children are still reachable via normal recursion.
        // mutable_specifier inside params should still be stripped.
        // visibility_modifier (pub) is PRESERVED as API surface (A4 contract).
        // Return type is PRESERVED as API surface (A4 contract).
        let source =
            "pub fn update(&mut self, value: i32) -> bool {\n    self.val = value;\n    true\n}\n";
        let result = transform(source, Language::Rust);
        assert!(
            result.contains("pub "),
            "pub must be preserved as API surface (A4), got: {result}"
        );
        assert!(
            !result.contains("mut "),
            "mut should be stripped via child recursion, got: {result}"
        );
        assert!(
            result.contains("-> bool"),
            "return type must be preserved as API surface (A4), got: {result}"
        );
        assert!(
            result.contains("self.val = value"),
            "function body should be preserved (recursion continued), got: {result}"
        );
    }

    #[test]
    fn test_cpp_access_specifier_preserved_with_members() {
        // C++ access specifiers (public:, protected:) are now preserved as API surface (A4).
        // Children (member declarations) are also preserved via normal recursion.
        let source = "class Widget {\npublic:\n    void draw();\nprotected:\n    int x_;\n};\n";
        let result = transform(source, Language::Cpp);
        assert!(
            result.contains("public:"),
            "public: access specifier must be preserved, got: {result}"
        );
        assert!(
            result.contains("protected:"),
            "protected: access specifier must be preserved, got: {result}"
        );
        assert!(
            !result.lines().any(|l| l.trim() == ":"),
            "no orphaned colons (specifiers appear intact with colon), got: {result}"
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

    // ========================================================================
    // Return-type preservation tests (A4 contract, Fix 2)
    // ========================================================================

    /// Rust: generic return types are preserved verbatim (including type args).
    /// Note: Rust pseudo preserves parameter types too (no strip_kinds for them).
    #[test]
    fn test_rust_pseudo_preserves_generic_return() {
        let source =
            "pub fn read_lines(path: &str) -> Result<Vec<String>, io::Error> {\n    todo!()\n}\n";
        let result = transform(source, Language::Rust);
        assert!(
            result.contains("-> Result<Vec<String>, io::Error>"),
            "generic return type must be preserved intact, got: {result}"
        );
        assert!(
            result.contains("fn read_lines"),
            "function name preserved, got: {result}"
        );
    }

    /// Rust: impl-trait return type is preserved.
    #[test]
    fn test_rust_pseudo_preserves_impl_trait_return() {
        let source =
            "pub fn make_iter() -> impl Iterator<Item = u32> {\n    [1, 2, 3].iter().copied()\n}\n";
        let result = transform(source, Language::Rust);
        assert!(
            result.contains("-> impl Iterator<Item = u32>"),
            "impl-trait return type must be preserved, got: {result}"
        );
    }

    /// Python: async function with `-> None` return type is preserved.
    #[test]
    fn test_python_pseudo_preserves_async_return_none() {
        let source = "async def shutdown(self) -> None:\n    await self.close()\n";
        let result = transform(source, Language::Python);
        assert!(
            result.contains("-> None"),
            "async function return type must be preserved, got: {result}"
        );
        // async keyword preserved (calling semantics)
        assert!(
            result.contains("async def"),
            "async keyword preserved, got: {result}"
        );
    }

    /// Python: nested return type (tuple) is preserved wholesale.
    #[test]
    fn test_python_pseudo_preserves_tuple_return() {
        let source = "def split(s: str) -> tuple[int, str]:\n    return 0, s\n";
        let result = transform(source, Language::Python);
        assert!(
            result.contains("-> tuple[int, str]"),
            "nested tuple return type must be preserved wholesale, got: {result}"
        );
        // Param type stripped
        assert!(
            result.contains("def split(s)"),
            "param type annotation stripped, got: {result}"
        );
    }

    /// Python: default-param function with return type.
    #[test]
    fn test_python_pseudo_preserves_return_with_default_param() {
        let source = "def add(x, y = 5) -> int:\n    return x + y\n";
        let result = transform(source, Language::Python);
        assert!(
            result.contains("-> int"),
            "return type must be preserved even when params have defaults, got: {result}"
        );
    }

    /// TypeScript: arrow function with return type preserved.
    #[test]
    fn test_typescript_pseudo_preserves_arrow_return() {
        let source =
            "const getUser = async (id: number): Promise<User> => {\n    return fetch(id);\n};\n";
        let result = transform(source, Language::TypeScript);
        assert!(
            result.contains("): Promise<User>"),
            "arrow function return type must be preserved, got: {result}"
        );
        // Param type stripped
        assert!(
            !result.contains("id: number"),
            "param type annotation must be stripped, got: {result}"
        );
    }

    /// TypeScript: interface method return type preserved; param type stripped.
    #[test]
    fn test_typescript_pseudo_interface_method_return_preserved() {
        let source = "interface Repo {\n    find(id: number): User;\n}\n";
        let result = transform(source, Language::TypeScript);
        assert!(
            result.contains("): User"),
            "interface method return type must be preserved, got: {result}"
        );
        // Param type stripped
        assert!(
            !result.contains("id: number"),
            "param type must be stripped in interface method, got: {result}"
        );
    }

    /// TypeScript: optional param with return type — param type stripped, return preserved.
    #[test]
    fn test_typescript_pseudo_optional_param_return_preserved() {
        let source = "function opt(a?: string): string {\n    return a ?? '';\n}\n";
        let result = transform(source, Language::TypeScript);
        assert!(
            result.contains("): string"),
            "return type must be preserved for optional-param function, got: {result}"
        );
    }

    // ========================================================================
    // WalkPosition differential equivalence  (B2 / #494 / PF-020)
    // ========================================================================
    //
    // `collect_noise_ranges` used to ask tree-sitter three relational questions
    // its own child loop already knew the answers to:
    //
    //   1. `node.parent()`        — once per `;` (TS/JS/Rust/Java/C/C++/C#/SQL)
    //   2. `is_return_field_child`— once per Python `type` / TS `type_annotation`
    //      (`node.parent()` + `child_by_field_name("return_type")`)
    //   3. `node.prev_sibling()`  — once per C++ `template_parameter_list`
    //
    // All three now read `WalkPosition`. The tests below pin the two forms
    // together node-for-node so the threading can never silently diverge from
    // the `Node` APIs it replaced — the sweep becomes a permanent invariant
    // rather than a one-shot check.

    /// Verbatim copy of the `parent()`-based predicate that
    /// `is_return_type_annotation` used before the field name was threaded down.
    ///
    /// Reference implementation for the differential tests ONLY — production code
    /// must never call it. Deleting it would turn those tests into self-comparisons.
    fn reference_is_return_field_child(node: Node) -> bool {
        let Some(parent) = node.parent() else {
            return false;
        };
        let Some(return_type_node) = parent.child_by_field_name("return_type") else {
            return false;
        };
        return_type_node.id() == node.id()
    }

    /// Walk `node`'s subtree exactly as `collect_noise_ranges` does, asserting at
    /// every node that the threaded `WalkPosition` equals what the parent-derived
    /// `Node` APIs return. Returns the number of nodes compared.
    ///
    /// The child loop here MUST mirror `collect_noise_ranges`'s child loop; if that
    /// loop changes shape, change this one with it.
    fn assert_position_matches_node_apis(
        node: Node,
        pos: WalkPosition,
        label: &str,
        language: Language,
        depth: usize,
        compared: &mut usize,
    ) {
        assert!(
            depth <= MAX_AST_DEPTH,
            "[{label}] differential walk exceeded MAX_AST_DEPTH"
        );
        *compared += 1;

        let at = format!("[{label}] node {:?} @{}", node.kind(), node.start_byte());

        assert_eq!(
            pos.parent_kind,
            node.parent().map(|p| p.kind()),
            "{at}: threaded parent_kind must equal node.parent().kind()"
        );

        let prev = node.prev_sibling();
        assert_eq!(
            pos.prev_sibling_kind,
            prev.map(|s| s.kind()),
            "{at}: threaded prev_sibling_kind must equal node.prev_sibling().kind()"
        );
        assert_eq!(
            pos.prev_sibling_start,
            prev.map(|s| s.start_byte()),
            "{at}: threaded prev_sibling_start must equal node.prev_sibling().start_byte()"
        );

        // The exact predicate site 2 replaced, asserted on EVERY node of every
        // language — not just the reachable candidates — so a future widening of
        // `is_return_type_candidate` cannot quietly change meaning.
        assert_eq!(
            pos.is_return_type_field,
            is_return_type_candidate(node.kind(), language)
                && reference_is_return_field_child(node),
            "{at}: threaded is_return_type_field must equal \
             is_return_type_candidate(kind, language) && \
             parent.child_by_field_name(\"return_type\") == node"
        );

        // Drive the PRODUCTION iterator — not a copy of it — so this test fails if
        // `ChildPositions` ever stops agreeing with the `Node` APIs it replaced.
        let mut yielded = 0usize;
        for (child, child_pos) in ChildPositions::new(node, language) {
            yielded += 1;
            assert_position_matches_node_apis(
                child,
                child_pos,
                label,
                language,
                depth + 1,
                compared,
            );
        }
        assert_eq!(
            yielded,
            node.child_count(),
            "{at}: ChildPositions must yield exactly child_count() children, \
             matching Node::children"
        );
    }

    fn compare_positions(label: &str, source: &str, language: Language) -> usize {
        let mut parser = Parser::new(language).unwrap();
        let tree = parser.parse(source).unwrap();
        let mut compared = 0usize;
        assert_position_matches_node_apis(
            tree.root_node(),
            WalkPosition::default(),
            label,
            language,
            0,
            &mut compared,
        );
        compared
    }

    /// The Swift finding that ruled out a `TreeCursor::field_name()` design, pinned
    /// so it cannot silently change under a grammar bump.
    ///
    /// In tree-sitter-swift 0.7 a `function_declaration` reports field `name` from
    /// the cursor for BOTH the `simple_identifier` and the return-type node, while
    /// `child_by_field_name("return_type")` resolves to the return-type node. The two
    /// APIs disagree, so a threaded field NAME is not a faithful substitute for
    /// `child_by_field_name` — which is why `WalkPosition` carries the resolved
    /// identity test instead. This test is the evidence, not a behaviour requirement.
    #[test]
    fn cursor_field_name_is_not_interchangeable_with_child_by_field_name() {
        let source =
            "func first<T: Equatable>(in a: [T], matching v: T) -> Int? {\n    return nil\n}\n";
        let mut parser = Parser::new(Language::Swift).unwrap();
        let tree = parser.parse(source).unwrap();

        let mut checked = false;
        // Bounded: the tree is finite and this fixture is a single declaration.
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            if node.kind() == "function_declaration" {
                let return_type = node
                    .child_by_field_name("return_type")
                    .expect("Swift function_declaration must expose a return_type field");
                assert!(
                    reference_is_return_field_child(return_type),
                    "child_by_field_name must round-trip through parent()"
                );

                let mut cursor = node.walk();
                let mut cursor_field: Option<Option<&str>> = None;
                if cursor.goto_first_child() {
                    for _ in 0..node.child_count() {
                        if cursor.node().id() == return_type.id() {
                            cursor_field = Some(cursor.field_name());
                            break;
                        }
                        if !cursor.goto_next_sibling() {
                            break;
                        }
                    }
                }
                assert_eq!(
                    cursor_field,
                    Some(Some("name")),
                    "tree-sitter-swift 0.7 reports `name` from the cursor for the \
                     return-type child. If this changes, re-evaluate whether \
                     WalkPosition can carry a field name instead of a resolved \
                     identity test — but do NOT assume the two APIs agree."
                );
                checked = true;
            }
            let mut c = node.walk();
            for ch in node.children(&mut c) {
                stack.push(ch);
            }
        }
        assert!(
            checked,
            "expected a Swift function_declaration in the snippet"
        );
    }

    /// Degenerate shapes: empty input, ERROR nodes, for-loop headers (the one
    /// place a `;` must NOT be stripped), repeated/absent return types, C++
    /// templates with and without a preceding `template` keyword, CRLF and
    /// non-ASCII bytes.
    const POSITION_DIFFERENTIAL_SNIPPETS: &[(&str, Language, &str)] = &[
        ("ts-empty", Language::TypeScript, ""),
        ("ts-lone-semicolon", Language::TypeScript, ";"),
        ("ts-only-semicolons", Language::TypeScript, ";;;;\n"),
        (
            "ts-for-header",
            Language::TypeScript,
            "for (let i = 0; i < 10; i++) { doIt(); }\n",
        ),
        (
            "ts-for-in-of",
            Language::TypeScript,
            "for (const k in o) { f(k); }\nfor (const v of a) { g(v); }\n",
        ),
        (
            "ts-return-types",
            Language::TypeScript,
            "function a(x: number): Promise<User> { return get(x); }\nconst b = (y: string): void => {};\n",
        ),
        (
            "ts-nested-return-type",
            Language::TypeScript,
            "function f(): Map<string, Array<number>> { return new Map(); }\n",
        ),
        (
            "ts-malformed",
            Language::TypeScript,
            "function broken(a: { ] ) : number {\n  return ;\n",
        ),
        (
            "ts-crlf",
            Language::TypeScript,
            "const a: number = 1;\r\nfunction f(): void {}\r\n",
        ),
        (
            "ts-cjk",
            Language::TypeScript,
            "const 名前: string = \"値\";\nfunction 関数(): string { return 名前; }\n",
        ),
        (
            "js-semicolons",
            Language::JavaScript,
            "const a = 1;\nfor (let i = 0; i < 3; i++) { a++; }\n",
        ),
        (
            "py-return-types",
            Language::Python,
            "def f(a: int, b: str) -> tuple[int, str]:\n    return a, b\n\ndef g(c):\n    return c\n",
        ),
        (
            "py-annotated-var",
            Language::Python,
            "x: int = 1\n\nclass C:\n    y: str = 'a'\n\n    def m(self) -> None:\n        pass\n",
        ),
        (
            "py-malformed",
            Language::Python,
            "def broken(a: int -> int:\n    return (\n",
        ),
        (
            "cpp-template",
            Language::Cpp,
            "template<typename T> T add(T a, T b) { return a + b; }\n",
        ),
        (
            "cpp-template-class-member",
            Language::Cpp,
            "class C {\npublic:\n  template<typename T> void m(T v) { use(v); }\n};\n",
        ),
        (
            "cpp-nested-template",
            Language::Cpp,
            "template<typename T>\nstruct S {\n  template<typename U> U conv(T t) { return (U)t; }\n};\n",
        ),
        (
            "cpp-malformed-template",
            Language::Cpp,
            "template<typename T T broken( { return; }\n",
        ),
        (
            "rust-semicolons",
            Language::Rust,
            "pub fn f(a: i32) -> i32 {\n    let b = a;\n    b\n}\n",
        ),
        (
            "java-semicolons",
            Language::Java,
            "class A {\n  public int f(int a) {\n    int b = a;\n    for (int i = 0; i < 3; i++) { b++; }\n    return b;\n  }\n}\n",
        ),
        (
            "c-semicolons",
            Language::C,
            "int f(int a) {\n  int b = a;\n  for (int i = 0; i < 3; i++) { b++; }\n  return b;\n}\n",
        ),
        (
            "csharp-semicolons",
            Language::CSharp,
            "class A {\n  public int F(int a) {\n    var b = a;\n    for (int i = 0; i < 3; i++) { b++; }\n    return b;\n  }\n}\n",
        ),
        (
            "sql-semicolons",
            Language::Sql,
            "SELECT 1;\nSELECT id FROM t WHERE x = 2;\n",
        ),
    ];

    #[test]
    fn pseudo_walk_position_matches_node_apis_on_snippets() {
        let mut total = 0usize;
        for (label, language, source) in POSITION_DIFFERENTIAL_SNIPPETS {
            total += compare_positions(label, source, *language);
        }
        // Tripwire against a snippet being silently dropped from the table.
        assert!(
            POSITION_DIFFERENTIAL_SNIPPETS.len() >= 23,
            "the degenerate-snippet table must keep its coverage; got {} entries",
            POSITION_DIFFERENTIAL_SNIPPETS.len()
        );
        // 763 nodes across the 23 snippets as of this commit. A floor, not an
        // exact count, so editing a snippet does not fail here.
        assert!(
            total >= 700,
            "expected the degenerate snippets to contribute a meaningful number of \
             nodes to the differential comparison; got {total}"
        );
    }

    #[test]
    fn pseudo_walk_position_matches_node_apis_on_fixtures() {
        const FIXTURES: &[(&str, Language, &str)] = &[
            (
                "typescript/simple.ts",
                Language::TypeScript,
                include_str!("../../../../tests/fixtures/typescript/simple.ts"),
            ),
            (
                "typescript/types.ts",
                Language::TypeScript,
                include_str!("../../../../tests/fixtures/typescript/types.ts"),
            ),
            (
                "typescript/mixed_priority.ts",
                Language::TypeScript,
                include_str!("../../../../tests/fixtures/typescript/mixed_priority.ts"),
            ),
            (
                "javascript/comments.js",
                Language::JavaScript,
                include_str!("../../../../tests/fixtures/javascript/comments.js"),
            ),
            (
                "python/simple.py",
                Language::Python,
                include_str!("../../../../tests/fixtures/python/simple.py"),
            ),
            (
                "python/mixed_priority.py",
                Language::Python,
                include_str!("../../../../tests/fixtures/python/mixed_priority.py"),
            ),
            (
                "rust/mixed_priority.rs",
                Language::Rust,
                include_str!("../../../../tests/fixtures/rust/mixed_priority.rs"),
            ),
            (
                "java/Simple.java",
                Language::Java,
                include_str!("../../../../tests/fixtures/java/Simple.java"),
            ),
            (
                "c/types.c",
                Language::C,
                include_str!("../../../../tests/fixtures/c/types.c"),
            ),
            (
                "cpp/types.cpp",
                Language::Cpp,
                include_str!("../../../../tests/fixtures/cpp/types.cpp"),
            ),
            (
                "cpp/mixed_priority.cpp",
                Language::Cpp,
                include_str!("../../../../tests/fixtures/cpp/mixed_priority.cpp"),
            ),
            (
                "csharp/generics.cs",
                Language::CSharp,
                include_str!("../../../../tests/fixtures/csharp/generics.cs"),
            ),
            (
                "go/simple.go",
                Language::Go,
                include_str!("../../../../tests/fixtures/go/simple.go"),
            ),
            (
                "sql/joins.sql",
                Language::Sql,
                include_str!("../../../../tests/fixtures/sql/joins.sql"),
            ),
            (
                "kotlin/Simple.kt",
                Language::Kotlin,
                include_str!("../../../../tests/fixtures/kotlin/Simple.kt"),
            ),
            (
                "swift/Generics.swift",
                Language::Swift,
                include_str!("../../../../tests/fixtures/swift/Generics.swift"),
            ),
            (
                "ruby/class.rb",
                Language::Ruby,
                include_str!("../../../../tests/fixtures/ruby/class.rb"),
            ),
            (
                "bash/functions.sh",
                Language::Bash,
                include_str!("../../../../tests/fixtures/bash/functions.sh"),
            ),
        ];
        let mut total = 0usize;
        for (label, language, source) in FIXTURES {
            total += compare_positions(label, source, *language);
        }
        // 4740 nodes across the 18 fixtures as of this commit. A floor, not an
        // exact count, so editing a fixture does not fail here.
        assert!(
            total >= 4000,
            "expected the fixtures to contribute >= 4000 nodes to the differential \
             comparison; got {total}"
        );
    }

    // ========================================================================
    // pseudo-walker artifact pins  (B2 / #494)
    // ========================================================================
    //
    // pseudo is the PRODUCTION path: the cat/head/tail rewrite selects
    // --mode=pseudo for regular code files (ADR-008), so this walker is the
    // hottest agent-facing transform in the product. Every other walker pin in
    // this workspace is Python-minimal-only or Go-only, so a TypeScript / Python /
    // C++ pseudo regression had no coverage at all.
    //
    // What these tests pin is the exact set of byte ranges the walker collects and
    // the number of AST nodes it visits — both computable from the fixture's text,
    // neither dependent on a clock.
    //
    // What they deliberately do NOT pin is complexity. A walker that re-derived
    // `parent()` / `prev_sibling()` per node instead of threading `WalkPosition`
    // would produce EXACTLY these ranges and EXACTLY this visit count: the extra
    // cost is inside tree-sitter's C code, where a `TSNode` has no parent pointer.
    // That construct is forbidden at the source level by
    // `contract_transform_walkers_use_no_root_descending_node_apis` in
    // `transform/mod.rs`, which is the only gate that can see it.

    /// Total AST node count, derived independently of the walker under test.
    ///
    /// Explicit stack + `Node::children`, so the count never depends on the
    /// traversal `collect_noise_ranges` performs — that independence is what lets
    /// it catch a walker that descends into the same node twice, which leaves the
    /// output byte-identical.
    fn count_all_nodes(root: Node) -> usize {
        let mut n = 0usize;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            n += 1;
            let mut c = node.walk();
            for ch in node.children(&mut c) {
                stack.push(ch);
            }
        }
        n
    }

    /// Drive the production noise walker directly, composing `NoiseWalkContext` the
    /// way `transform_pseudo_with_spans_and_line_map` does.
    ///
    /// Returns the raw (unadjusted, unsorted) removal ranges and the number of
    /// nodes visited.
    fn collect_pseudo_ranges(
        source: &str,
        tree: &Tree,
        language: Language,
    ) -> (Vec<(usize, usize)>, usize) {
        let rules = get_pseudo_rules(language);
        let root = tree.root_node();
        let header_end_byte = compute_header_end_byte(root, source, language);
        let go_doc_comment_starts = compute_go_doc_comment_starts(root, source, language);
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut node_count: usize = 0;
        let mut ctx = NoiseWalkContext {
            source,
            source_bytes: source.as_bytes(),
            language,
            ranges: &mut ranges,
            node_count: &mut node_count,
            classification: CommentClassification {
                header_end_byte,
                go_doc_comment_starts: &go_doc_comment_starts,
            },
        };
        collect_noise_ranges(root, &mut ctx, &rules, 0, false, WalkPosition::default())
            .expect("noise walk must succeed on these fixtures");
        (ranges, node_count)
    }

    /// N top-level statements, each terminated by a `;` — the site-1 population.
    fn ts_semicolon_source(n: usize) -> String {
        let mut s = String::with_capacity(n * 20 + 32);
        for i in 0..n {
            s.push_str(&format!("const v{i} = {i};\n"));
        }
        s
    }

    /// N annotated defs. Two parameter annotations each (both stripped) plus one
    /// return annotation each (preserved — ADR-007), all `type` nodes reaching the
    /// site-2 guard.
    fn python_return_type_source(n: usize) -> String {
        let mut s = String::with_capacity(n * 48 + 32);
        for i in 0..n {
            s.push_str(&format!(
                "def f{i}(a: int, b: int) -> int:\n    return a + b\n\n"
            ));
        }
        s
    }

    /// N template functions — the site-3 previous-sibling population.
    fn cpp_template_source(n: usize) -> String {
        let mut s = String::with_capacity(n * 64 + 32);
        for i in 0..n {
            s.push_str(&format!(
                "template<typename T> T f{i}(T a, T b) {{ return a + b; }}\n"
            ));
        }
        s
    }

    /// The parameter annotation stripped from every `python_return_type_source`
    /// def. `adjust_type_start` extends the `type` node backward over the `: `
    /// separator, so the collected range covers this whole string.
    const PY_PARAM_ANNOTATION: &str = ": int";

    /// The C++ template prefix consumed as one range: the `template` keyword, its
    /// parameter list, and the single trailing space that
    /// `consume_trailing_whitespace` eats before the return type.
    const CPP_TEMPLATE_PREFIX: &str = "template<typename T> ";

    /// The TypeScript semicolon ranges are byte-exact and the walk is single-visit.
    ///
    /// WHAT THIS PINS: `collect_noise_ranges` emits exactly one 1-byte range per
    /// source `;` and nothing else (`const vI = I;` carries no type annotation,
    /// decorator or stripped keyword), and never descends into a node twice.
    ///
    /// WHAT THIS DOES NOT PIN: complexity. A walker that called `node.parent()` per
    /// `;` instead of reading `pos.parent_kind` would produce these exact ranges and
    /// this exact visit count — the defect is output-preserving. See the section
    /// note above and the contract test in `transform/mod.rs`.
    #[test]
    fn test_typescript_pseudo_semicolon_ranges_are_exact_and_single_visit() {
        // Behaviour pin: these `;` are statement terminators, not for-loop
        // separators, so all of them must be stripped.
        let out = transform(&ts_semicolon_source(4), Language::TypeScript);
        assert!(
            !out.contains(';'),
            "top-level statement semicolons must be stripped, got: {out}"
        );

        for n in [256usize, 512] {
            let source = ts_semicolon_source(n);
            let mut parser = Parser::new(Language::TypeScript).unwrap();
            let tree = parser.parse(&source).unwrap();
            let root = tree.root_node();
            let total_nodes = count_all_nodes(root);

            let (mut ranges, node_count) =
                collect_pseudo_ranges(&source, &tree, Language::TypeScript);
            ranges.sort_unstable();

            let expected: Vec<(usize, usize)> =
                source.match_indices(';').map(|(i, _)| (i, i + 1)).collect();
            assert_eq!(
                expected.len(),
                n,
                "n={n}: the fixture must contain exactly one `;` per statement"
            );
            assert_eq!(
                ranges, expected,
                "n={n}: exactly one 1-byte removal range per source `;`, and nothing else"
            );

            // `<=`, not `==`: collect_noise_ranges returns early on stripped kinds,
            // so it legitimately visits fewer nodes than the tree holds. Visiting
            // MORE is the regression this catches.
            assert!(
                node_count <= total_nodes,
                "n={n}: collect_noise_ranges must not visit a node twice \
                 (visited {node_count}, tree has {total_nodes})"
            );
        }
    }

    /// The Python parameter-annotation ranges are byte-exact and the walk is
    /// single-visit.
    ///
    /// WHAT THIS PINS: exactly the two `: int` parameter annotations per def are
    /// collected — the `-> int` return annotation is preserved wholesale (ADR-007)
    /// — at the exact byte offsets, including the `: ` separator that
    /// `adjust_type_start` folds in. And the walk never descends twice.
    ///
    /// WHAT THIS DOES NOT PIN: complexity — the defect was output-preserving.
    #[test]
    fn test_python_pseudo_return_type_ranges_are_exact_and_single_visit() {
        // ADR-007: pseudo PRESERVES function return types. Pin it here so the test
        // cannot pass on a walker that stopped classifying return types.
        let out = transform(&python_return_type_source(2), Language::Python);
        assert!(
            out.contains("-> int"),
            "pseudo must preserve Python return types (ADR-007), got: {out}"
        );
        assert!(
            !out.contains("a: int"),
            "pseudo must still strip Python parameter annotations, got: {out}"
        );

        for n in [256usize, 512] {
            let source = python_return_type_source(n);
            let mut parser = Parser::new(Language::Python).unwrap();
            let tree = parser.parse(&source).unwrap();
            let root = tree.root_node();
            let total_nodes = count_all_nodes(root);

            let (mut ranges, node_count) = collect_pseudo_ranges(&source, &tree, Language::Python);
            ranges.sort_unstable();

            let expected: Vec<(usize, usize)> = source
                .match_indices(PY_PARAM_ANNOTATION)
                .map(|(i, _)| (i, i + PY_PARAM_ANNOTATION.len()))
                .collect();
            assert_eq!(
                expected.len(),
                2 * n,
                "n={n}: two parameter annotations per def; `-> int` must not match"
            );
            assert_eq!(
                ranges, expected,
                "n={n}: exactly the two parameter annotations per def are collected, \
                 each covering its `: ` separator; the return annotation is preserved"
            );

            assert!(
                node_count <= total_nodes,
                "n={n}: collect_noise_ranges must not visit a node twice \
                 (visited {node_count}, tree has {total_nodes})"
            );
        }
    }

    /// The C++ template ranges are byte-exact and the walk is single-visit.
    ///
    /// WHAT THIS PINS: the `template_parameter_list` special case reaches BACK over
    /// its previous sibling, so each collected range starts at the `template`
    /// keyword — not at `<` — and ends after the single trailing space. `template`
    /// is NOT in the C++ `strip_keywords` list, so a walker that loses the
    /// previous-sibling reach leaves the keyword orphaned in the output and fails
    /// both this and the `!out.contains("template")` pin.
    ///
    /// WHAT THIS DOES NOT PIN: complexity. Reading the previous sibling back via
    /// `node.prev_sibling()` instead of `pos.prev_sibling_kind` yields the identical
    /// ranges — see the section note above and the contract test in
    /// `transform/mod.rs`.
    #[test]
    fn test_cpp_pseudo_template_ranges_are_exact_and_single_visit() {
        // Behaviour pin: the `template` keyword must be consumed along with its
        // parameter list, leaving no orphan.
        let out = transform(&cpp_template_source(2), Language::Cpp);
        assert!(
            !out.contains("template"),
            "the `template` keyword must be consumed with its parameter list, got: {out}"
        );

        for n in [128usize, 256] {
            let source = cpp_template_source(n);
            let mut parser = Parser::new(Language::Cpp).unwrap();
            let tree = parser.parse(&source).unwrap();
            let root = tree.root_node();
            let total_nodes = count_all_nodes(root);

            let (mut ranges, node_count) = collect_pseudo_ranges(&source, &tree, Language::Cpp);
            ranges.sort_unstable();

            // The fixture also yields one `;` range per function body; isolate the
            // template ranges by the text they start on.
            let template_ranges: Vec<(usize, usize)> = ranges
                .iter()
                .copied()
                .filter(|&(start, _)| source[start..].starts_with("template"))
                .collect();
            let expected: Vec<(usize, usize)> = source
                .match_indices(CPP_TEMPLATE_PREFIX)
                .map(|(i, _)| (i, i + CPP_TEMPLATE_PREFIX.len()))
                .collect();
            assert_eq!(expected.len(), n, "n={n}: one template prefix per function");
            assert_eq!(
                template_ranges, expected,
                "n={n}: each template range must start at the `template` keyword and end \
                 after its single trailing space"
            );

            assert!(
                node_count <= total_nodes,
                "n={n}: collect_noise_ranges must not visit a node twice \
                 (visited {node_count}, tree has {total_nodes})"
            );
        }
    }
}
