//! Structural metrics and synthetic n-gram marker IDs for AST Pattern Library.
//!
//! # Synthetic ID Allocation
//!
//! The shared vocabulary has 1740 entries (IDs 0..=1739). IDs >= 1740 are
//! "free" — `vocab_resolve(id)` returns `None` for them, which is the isolation
//! guarantee used by all synthetic markers defined here.
//!
//! Reserved synthetic parent IDs (used as the PARENT component of a bigram):
//! - `EMPTY_BODY`  = 65000 — enclosing construct has an empty body
//! - `DEEP_NODE`   = 65001 — subtree nesting depth crossed a threshold
//! - `LARGE_BODY`  = 65002 — function/method body statement count crossed a threshold
//! - `MANY_PARAMS` = 65003 — parameter list count crossed a threshold
//!
//! Reserved child ID block (bucket labels, base 64900):
//! - `bucket_label(0)` = 64900, `bucket_label(1)` = 64901, …
//! - Each bucket edge index maps to one child ID: `BUCKET_LABEL_BASE + edge_index`.
//!
//! # Correctness Rule — "Counted Children"
//!
//! The LinearNode stream includes anonymous punctuation (kind_id == 0, the
//! sentinel for vocabulary-unknown nodes) and comment kinds. The central
//! counting rule used throughout this module:
//!
//! > A "counted child" of a node at depth d is a subsequent stream node at
//! > depth d+1 that has `kind_id != 0` AND whose kind_id is NOT in
//! > `COMMENT_KIND_IDS`.
//!
//! This rule filters anonymous punctuation (sentinel) and comment nodes
//! consistently for body-statement counting, emptiness, and parameter counting.

use std::collections::HashSet;
use std::sync::LazyLock;

use super::{NodeKindId, vocab_lookup};

// ============================================================================
// Synthetic parent IDs
// ============================================================================

/// Synthetic marker: enclosing construct has an empty body.
///
/// Keyed on the enclosing construct (EMPTY_BODY → enclosing_kind_id).
/// A `kind_id` of `EMPTY_BODY` is >= 65000, so `vocab_resolve` returns `None`
/// for it — isolation is guaranteed.
///
/// Distinguishes: empty-catch (EMPTY_BODY → catch_clause)
///                from empty-function (EMPTY_BODY → function_declaration), etc.
pub const EMPTY_BODY: NodeKindId = 65000;

/// Synthetic marker: a node in the subtree is at a depth that crossed a bucket edge.
///
/// Used as the PARENT component; the CHILD is a `bucket_label(edge_index)`.
pub const DEEP_NODE: NodeKindId = 65001;

/// Synthetic marker: a function/method body contains statements that crossed a
/// bucket edge. Only emitted for bodies of function/method constructs.
///
/// Used as the PARENT component; the CHILD is a `bucket_label(edge_index)`.
pub const LARGE_BODY: NodeKindId = 65002;

/// Synthetic marker: a parameter list contains parameters that crossed a
/// bucket edge.
///
/// Used as the PARENT component; the CHILD is a `bucket_label(edge_index)`.
pub const MANY_PARAMS: NodeKindId = 65003;

// ============================================================================
// Bucket label IDs
// ============================================================================

/// Base of the reserved bucket-label child ID block.
///
/// `bucket_label(edge_index)` = `BUCKET_LABEL_BASE + edge_index`.
/// All must satisfy `vocab_resolve(id).is_none()`.
pub const BUCKET_LABEL_BASE: NodeKindId = 64900;

/// Maximum number of bucket edges across all dimensions (must not overflow
/// into any real vocabulary range, i.e. BUCKET_LABEL_BASE + MAX_BUCKET_EDGES < 65000).
const MAX_BUCKET_EDGES: u16 = 99;

/// Compute the child ID for a bucket label.
///
/// `edge_index` is a 0-based index into a bucket edge list. It must be < `MAX_BUCKET_EDGES`.
///
/// # Panics
///
/// Panics in debug if `edge_index >= MAX_BUCKET_EDGES`, preserving the ID range invariant.
#[inline]
#[must_use]
pub fn bucket_label(edge_index: usize) -> NodeKindId {
    debug_assert!(
        edge_index < MAX_BUCKET_EDGES as usize,
        "bucket_label edge_index {edge_index} exceeds MAX_BUCKET_EDGES {MAX_BUCKET_EDGES}"
    );
    BUCKET_LABEL_BASE + edge_index as NodeKindId
}

// ============================================================================
// Bucket edge tables
// ============================================================================

/// Body-statement count bucket edges (for `LARGE_BODY` synthetic marker).
///
/// A function/method body with `stmt_count` statements emits
/// `LARGE_BODY → bucket_label(i)` for every edge `i` where `BODY_STMT_EDGES[i] <= stmt_count`.
pub const BODY_STMT_EDGES: [u32; 3] = [10, 20, 40];

/// Parameter count bucket edges (for `MANY_PARAMS` synthetic marker).
///
/// A parameter list with `param_count` counted children emits
/// `MANY_PARAMS → bucket_label(i)` for every edge `i` where `PARAM_EDGES[i] <= param_count`.
pub const PARAM_EDGES: [u32; 3] = [5, 8, 12];

/// Nesting-depth bucket edges (for `DEEP_NODE` synthetic marker).
///
/// A node at depth `d` emits `DEEP_NODE → bucket_label(i)` for every edge `i`
/// where `DEPTH_EDGES[i] <= d`. Depth is zero-indexed from the root.
pub const DEPTH_EDGES: [u32; 3] = [4, 6, 8];

// ============================================================================
// Structural metrics
// ============================================================================

/// Per-file structural complexity metrics derived during n-gram extraction.
///
/// All fields are initialized to zero (the `Default` impl) and updated in a
/// single pass alongside n-gram extraction. Zero metrics are valid (e.g. for
/// data-format files that produce no CST nodes).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StructuralMetrics {
    /// Maximum CST traversal depth seen in this file (0 if empty).
    pub max_depth: u16,
    /// Maximum counted-child count in any function/method body block.
    /// Saturates at `u16::MAX` (PF-004: saturating cast, never wrapping).
    pub max_block_stmts: u16,
    /// Maximum counted-child count in any parameter list.
    /// Saturates at `u16::MAX` (PF-004: saturating cast, never wrapping).
    pub max_params: u16,
    /// Total count of branch-kind nodes in the file (if/while/for/match/etc.).
    /// Saturates at `u32::MAX`.
    pub branch_count: u32,
}

// ============================================================================
// Language-independent comment kind IDs (resolved once from the global vocab)
// ============================================================================

/// Global comment kind IDs resolved from `NODE_KIND_VOCABULARY`.
///
/// Any node whose `kind_id` is in this set is a comment and is excluded from
/// "counted child" counts.  Built once at first use via `LazyLock`.
pub static COMMENT_KIND_IDS: LazyLock<HashSet<NodeKindId>> = LazyLock::new(|| {
    // Known comment kind strings across all supported grammars.
    let comment_kinds = [
        "comment",
        "line_comment",
        "block_comment",
        "doc_comment",
        // Python (and some others) use these as well, but tree-sitter-python
        // typically emits them as "comment" — included for safety.
    ];
    comment_kinds
        .iter()
        .filter_map(|k| vocab_lookup(k))
        .collect()
});

/// Punctuation and keyword token kind IDs resolved from `NODE_KIND_VOCABULARY`.
///
/// Tree-sitter includes named tokens for punctuation (e.g. `"{"`, `"}"`, `","`,
/// `";"`) and structural keywords (e.g. `"fn"`, `"catch"`, `"try"`, `"->"`) in
/// its CST. These tokens appear as children in the LinearNode stream but do NOT
/// represent "statements" or "parameters" for structural-complexity counting.
///
/// This set excludes them from "counted child" counts so that:
/// - An empty `statement_block` `{}` has 0 counted children (EMPTY_BODY fires).
/// - A `parameters` list `(a: i32, b: i32)` counts only `parameter` nodes, not
///   the surrounding `(`, `)`, and `,` tokens.
///
/// # Design
///
/// The set is built from known single-character and keyword token strings that
/// appear in the vocabularies of our supported grammars. This is more precise
/// than a length-based heuristic and more stable than tree-sitter `is_named()`.
/// Only strings that are in the actual vocabulary contribute entries.
pub static PUNCTUATION_KIND_IDS: LazyLock<HashSet<NodeKindId>> = LazyLock::new(|| {
    // Punctuation: single-character tokens that appear as named nodes in CSTs.
    let punct_kinds: &[&str] = &[
        // Brackets / delimiters
        "{",
        "}",
        "(",
        ")",
        "[",
        "]",
        "<",
        ">",
        // Separators / terminators
        ",",
        ";",
        ":",
        "::",
        ".",
        "...",
        "..",
        "->",
        "=>",
        "@",
        // Operators used as tokens at statement-block level
        "|",
        "&",
        "*",
        "+",
        "-",
        "/",
        "%",
        "=",
        "==",
        "!=",
        "+=",
        "-=",
        "*=",
        "/=",
        "%=",
        "&=",
        "|=",
        "^=",
        "<=",
        ">=",
        "&&",
        "||",
        "!",
        "~",
        "^",
        "<<",
        ">>",
        "?",
        "??",
        "?.",
        "?:",
        // Universal structural keywords (not statement-level constructs)
        "fn",
        "function",
        "def",
        "class",
        "struct",
        "impl",
        "trait",
        "let",
        "var",
        "const",
        "return",
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "case",
        "default",
        "break",
        "continue",
        "try",
        "catch",
        "finally",
        "throw",
        "throws",
        "public",
        "private",
        "protected",
        "static",
        "final",
        "abstract",
        "async",
        "await",
        "yield",
        "import",
        "export",
        "from",
        "as",
        "type",
        "interface",
        "enum",
        "namespace",
        "module",
        "new",
        "delete",
        "typeof",
        "instanceof",
        "in",
        "of",
        "true",
        "false",
        "null",
        "undefined",
        "nil",
        "None",
        "True",
        "False",
        "self",
        "Self",
        "super",
        "this",
        "mut",
        "ref",
        "pub",
        "use",
        "mod",
        "crate",
        "extern",
        "match",
        "where",
        "move",
        "dyn",
        "box",
        "go",
        "defer",
        "chan",
        "select",
        "range",
        "make",
        "append",
        "rescue",
        "ensure",
        "begin",
        "end",
        "synchronized",
        "volatile",
        "native",
        "transient",
        "override",
        "open",
        "closed",
        "sealed",
        "unsafe",
        "pack",
        "unpack",
        // Annotation / preprocessor tokens
        "#",
    ];
    punct_kinds.iter().filter_map(|k| vocab_lookup(k)).collect()
});

// ============================================================================
// Function and body kind ID sets (for LARGE_BODY filtering)
// ============================================================================

/// Function/method definition kind IDs.
///
/// `LARGE_BODY` is emitted ONLY for bodies of nodes whose kind_id appears in
/// this set. Built once at first use.
pub static FUNCTION_KIND_IDS: LazyLock<HashSet<NodeKindId>> = LazyLock::new(|| {
    // These are the function/method definition kinds from all supported grammars.
    // Derived from rskim-core/src/transform/utils.rs get_function_node_kinds +
    // the broader function-construct list in node_kind_info.
    let fn_kinds = [
        // Generic / cross-language
        "function_declaration", // TypeScript/JavaScript/Go/C; Swift reuses same string
        "function_item",
        "method_declaration", // Java, C#, Kotlin reuse same string
        "function_definition",
        "method_definition",
        "arrow_function",
        "function_expression",
        // C/C++
        "declaration", // covers many C/C++ function decls in tree-sitter
        "template_declaration",
        // C# / Java
        "constructor_declaration",
        // Ruby
        "method",
        "singleton_method",
        // Swift
        "init_declaration",
        "deinit_declaration",
        // Kotlin
        "secondary_constructor",
        "anonymous_initializer",
    ];
    fn_kinds.iter().filter_map(|k| vocab_lookup(k)).collect()
});

/// Body/block kind IDs (direct-child bodies of function constructs).
///
/// When a subtree-close happens for a node in `FUNCTION_KIND_IDS`, we look at
/// the body-block kind to know how many statements it contained. These are the
/// body-container kinds whose direct children we count as "body statements".
/// Derived from rskim-core/src/transform/utils.rs get_body_node_kinds.
pub static BODY_KIND_IDS: LazyLock<HashSet<NodeKindId>> = LazyLock::new(|| {
    let body_kinds = [
        "statement_block",    // TypeScript / JavaScript
        "block",              // Python, Rust, Go, Java, C#, CSharp
        "compound_statement", // C / C++
        "constructor_body",   // Java
        "body_statement",     // Ruby
        "function_body",      // Swift, Kotlin
    ];
    body_kinds.iter().filter_map(|k| vocab_lookup(k)).collect()
});

/// Parameter list kind IDs.
///
/// When a subtree-close happens for a node in this set, we count its counted
/// children as parameters and emit `MANY_PARAMS` synthetic markers.
pub static PARAM_LIST_KIND_IDS: LazyLock<HashSet<NodeKindId>> = LazyLock::new(|| {
    let param_kinds = [
        "formal_parameters",     // TypeScript / JavaScript
        "parameters",            // Python, Rust, Go, Swift
        "formal_parameter_list", // Java, C, C++ (some grammars)
        "parameter_list",        // C#, Kotlin, Swift (some grammars)
        "method_parameters",     // some grammars
    ];
    param_kinds.iter().filter_map(|k| vocab_lookup(k)).collect()
});

/// Branch-construct kind IDs (for `branch_count` in `StructuralMetrics`).
///
/// Any node whose `kind_id` appears here increments `branch_count`.
/// Curated across supported grammars; GOLD-verified against real parse output.
pub static BRANCH_KIND_IDS: LazyLock<HashSet<NodeKindId>> = LazyLock::new(|| {
    let branch_kinds = [
        // Conditionals
        "if_statement",
        "if_expression",          // Rust
        "conditional_expression", // C/C++ ternary
        "ternary_expression",     // TypeScript / JavaScript
        // Loops
        "while_statement",
        "while_expression", // Rust
        "for_statement",
        "for_in_statement",
        "for_expression",  // Rust
        "loop_expression", // Rust `loop`
        "do_statement",    // Java, C/C++ do-while
        // Pattern matching / switch
        "match_expression", // Rust
        "switch_statement",
        "switch_expression", // Java 14+
        "case_statement",    // some grammars treat case as branch
        // Misc
        "try_statement",
        "except_clause", // Python
        "rescue_clause", // Ruby
        "catch_clause",
    ];
    branch_kinds
        .iter()
        .filter_map(|k| vocab_lookup(k))
        .collect()
});

// ============================================================================
// Counting helper
// ============================================================================

/// Test whether a node is a "counted child" per the central counting rule.
///
/// A node is counted if:
/// - `kind_id != 0` (not the anonymous-punctuation sentinel)
/// - Its kind_id is NOT in `COMMENT_KIND_IDS`
/// - Its kind_id is NOT in `PUNCTUATION_KIND_IDS`
///
/// The third condition is necessary because tree-sitter emits named nodes for
/// punctuation tokens (e.g. `"{"`, `"}"`, `","`) and structural keywords
/// (e.g. `"fn"`, `"catch"`) that appear as children in the LinearNode stream
/// but do not represent semantic statements or parameters. Without this
/// exclusion, an empty `statement_block {}` would have 2 counted children
/// (`{` and `}`) and would never be recognized as "empty".
///
/// # Note on lazy initialization
///
/// `COMMENT_KIND_IDS` and `PUNCTUATION_KIND_IDS` are initialized at first call
/// (via `LazyLock`). The initialization itself is O(#kinds × log(vocab_len)),
/// which is tiny.
#[inline]
#[must_use]
pub fn is_counted_child(kind_id: NodeKindId) -> bool {
    kind_id != 0 && !COMMENT_KIND_IDS.contains(&kind_id) && !PUNCTUATION_KIND_IDS.contains(&kind_id)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "structural_tests.rs"]
mod tests;
