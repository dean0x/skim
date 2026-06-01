//! CST linearization: converts tree-sitter parse trees into pre-order
//! depth-encoded node-type sequences for AST n-gram extraction.
//!
//! # Design
//!
//! `linearize_source` builds a `Vec<LinearNode>` in one pass. Each node
//! carries a compact vocabulary ID (mapped from tree-sitter's per-grammar
//! `kind_id`) and the traversal depth, which is sufficient to reconstruct
//! parent–child relationships downstream without retaining the full tree.
//!
//! Per-language lookup tables (`LANG_MAPS`) are built once at first use via
//! `LazyLock`. Each table is a `Vec<Option<u16>>` indexed by tree-sitter's
//! `node.kind_id()`, mapping it to a position in `NODE_KIND_VOCABULARY` via
//! binary search. This gives O(1) lookup per node during traversal at the
//! cost of one binary search per kind string at init time.
//!
//! # Invariant
//!
//! `result.node_count == result.nodes.len() + result.error_count`
//!
//! ERROR and MISSING nodes are excluded from `nodes` but counted in
//! `error_count`. `node_count` is the total nodes visited.

use std::collections::HashMap;
use std::sync::LazyLock;

use rskim_core::{AstWalkConfig, AstWalkIter, Language, Parser};

use crate::ast_weights::NODE_KIND_VOCABULARY;
use crate::types::SearchError;

// ============================================================================
// Constants
// ============================================================================

// Traversal bounds are centralized on `AstWalkConfig` as associated constants
// (`DEFAULT_MAX_DEPTH` = 500, `DEFAULT_MAX_NODES` = 100 000).  Reference them
// via `AstWalkConfig::DEFAULT_MAX_DEPTH` / `AstWalkConfig::DEFAULT_MAX_NODES`
// wherever a local override is needed, or use `AstWalkConfig::default()` to
// pick up both at once.

/// Maximum source file size accepted for linearization (100 KiB).
///
/// Files exceeding this limit return `Ok(LinearizeResult::default())` rather
/// than an error. Consistent with the limit in `rskim-research/src/ast_extract.rs`.
const MAX_FILE_SIZE: usize = 100 * 1024;

// ============================================================================
// Public types
// ============================================================================

/// A single node in the linearized pre-order traversal of a CST.
///
/// `kind_id` is an index into `NODE_KIND_VOCABULARY` — the canonical compact
/// vocabulary shared across all languages. Use `NODE_KIND_VOCABULARY[kind_id as usize]`
/// to resolve the string. A `kind_id` of `0` (sentinel) means the grammar
/// kind was not found in the vocabulary (unknown kind).
///
/// `depth` is the 0-indexed traversal depth from the root. Parent–child
/// relationships are recoverable: a node's parent is the nearest preceding
/// node with `depth == self.depth - 1`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinearNode {
    /// Vocabulary ID into `NODE_KIND_VOCABULARY`. `0` is the sentinel for
    /// unknown kinds.
    pub kind_id: u16,
    /// 0-indexed depth from the tree root (root node is depth 0).
    pub depth: u16,
}

/// Result of linearizing a single source file.
///
/// # Invariant
///
/// `node_count == nodes.len() + error_count`
///
/// This invariant holds at all times: every node visited either appears in
/// `nodes` (non-error) or is counted in `error_count` (ERROR/MISSING).
#[derive(Debug, Clone, Default)]
pub struct LinearizeResult {
    /// Linearized nodes in pre-order DFS order. Does not include ERROR or
    /// MISSING nodes, but their children may still be included if reachable.
    pub nodes: Vec<LinearNode>,
    /// Total nodes visited, including both emitted nodes and skipped
    /// ERROR/MISSING nodes. Equals `nodes.len() + error_count`.
    pub node_count: u32,
    /// Number of ERROR or MISSING nodes encountered during traversal.
    pub error_count: u32,
}

// ============================================================================
// Per-language vocabulary lookup tables
// ============================================================================

/// Per-language map from tree-sitter `kind_id` → `NODE_KIND_VOCABULARY` index.
///
/// Built once at first access via `LazyLock`. Each entry is `Vec<Option<u16>>`:
/// - Index: tree-sitter's native `node.kind_id()` (grammar-local, per-language)
/// - Value: `Some(vocab_idx)` if the kind string appears in `NODE_KIND_VOCABULARY`,
///   `None` if the kind is unknown to the shared vocabulary
///
/// Using a `Vec` (not `HashMap`) for O(1) array indexing during traversal.
/// Using `HashMap<Language, ...>` since `Language: Eq + Hash`.
static LANG_MAPS: LazyLock<HashMap<Language, Vec<Option<u16>>>> = LazyLock::new(|| {
    let ts_languages = [
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
        Language::Sql,
        Language::Kotlin,
        Language::Swift,
        Language::Markdown,
    ];

    let mut map = HashMap::with_capacity(ts_languages.len());

    for lang in ts_languages {
        // Parser::new returns Err for non-tree-sitter languages. We only call
        // it here for tree-sitter languages, so this should always succeed.
        // If grammar loading fails, skip the language (no entry in the map).
        let mut parser = match Parser::new(lang) {
            Ok(p) => p,
            Err(_) => continue,
        };

        // Parse an empty source to obtain a tree, from which we extract the
        // tree-sitter Language object and its node kind table.
        let tree = match parser.parse("") {
            Ok(t) => t,
            Err(_) => continue,
        };

        let ts_lang = tree.language();
        let kind_count = ts_lang.node_kind_count();

        // Build a lookup Vec indexed by tree-sitter kind_id.
        // For each kind_id, binary-search NODE_KIND_VOCABULARY for the kind
        // string to get its vocabulary index.
        let mut lang_map: Vec<Option<u16>> = vec![None; kind_count];
        for (kind_id, entry) in lang_map.iter_mut().enumerate() {
            // node_kind_for_id takes u16; skip any kind_id that would overflow.
            // Current grammars have 200–500 kinds, so this path is unreachable
            // in practice, but the pattern is safe for future grammar growth.
            let kind_id_u16 = match u16::try_from(kind_id) {
                Ok(id) => id,
                Err(_) => continue,
            };
            if let Some(kind_str) = ts_lang.node_kind_for_id(kind_id_u16) {
                // Binary search NODE_KIND_VOCABULARY (which is sorted).
                // NODE_KIND_VOCABULARY.len() == 1740, well within u16::MAX.
                if let Ok(vocab_idx) = NODE_KIND_VOCABULARY.binary_search(&kind_str) {
                    *entry = Some(vocab_idx as u16);
                }
                // Unknown kinds remain None; they emit sentinel ID 0 at traversal time.
            }
        }

        map.insert(lang, lang_map);
    }

    map
});

// ============================================================================
// Public API
// ============================================================================

/// Linearize a source file into a pre-order depth-encoded node sequence.
///
/// Returns `Ok(LinearizeResult::default())` (empty result) for:
/// - Files exceeding `MAX_FILE_SIZE` (100 KiB)
/// - Non-tree-sitter languages (JSON, YAML, TOML)
/// - Parse failures (tree-sitter is error-tolerant, so this is rare)
///
/// Returns `Err(SearchError::Ast)` only when the grammar itself fails to
/// load — a configuration-level failure, not a file-level parse failure.
///
/// # Errors
///
/// Returns `Err(SearchError::Ast)` if the tree-sitter grammar for
/// `language` fails to load (grammar crate not compiled in, ABI mismatch,
/// etc.). This is distinct from a parse error, which produces an empty result.
pub fn linearize_source(
    source: &str,
    language: Language,
) -> crate::types::Result<LinearizeResult> {
    // Guard 1: oversized files return empty result (not an error).
    if source.len() > MAX_FILE_SIZE {
        return Ok(LinearizeResult::default());
    }

    // Guard 2: non-tree-sitter languages have no CST → return empty result.
    let lang_map = match LANG_MAPS.get(&language) {
        Some(m) => m,
        None => return Ok(LinearizeResult::default()),
    };

    // Parse. Parser::new failures for non-TS languages are already handled
    // above via LANG_MAPS lookup. A failure here means grammar load error.
    let mut parser = Parser::new(language)
        .map_err(|e| SearchError::Ast(format!("grammar load failure for {language:?}: {e}")))?;

    // Parse errors produce empty results (tree-sitter is error-tolerant, so
    // parse() only returns Err on internal grammar failures, which are rare).
    let tree = match parser.parse(source) {
        Ok(t) => t,
        Err(_) => return Ok(LinearizeResult::default()),
    };

    Ok(linearize_tree(&tree, lang_map))
}

// ============================================================================
// Tree traversal
// ============================================================================

/// Iterative pre-order DFS traversal of a tree-sitter CST via `AstWalkIter`.
///
/// Delegates all cursor management, bounds guarding, and depth tracking to the
/// shared `AstWalkIter` in `rskim-core`. Caller-specific logic (vocabulary
/// lookup, `LinearNode` construction) stays here.
///
/// # Invariant maintained
///
/// `result.node_count == result.nodes.len() + result.error_count`
fn linearize_tree(tree: &tree_sitter::Tree, lang_map: &[Option<u16>]) -> LinearizeResult {
    let capacity = tree
        .root_node()
        .descendant_count()
        .min(AstWalkConfig::DEFAULT_MAX_NODES as usize);
    let mut nodes = Vec::with_capacity(capacity);

    let mut iter = AstWalkIter::new(tree.walk(), AstWalkConfig::default());

    for item in iter.by_ref() {
        if item.is_error {
            // ERROR/MISSING nodes are not emitted but their children are still
            // visited. error_count is tracked by the iterator.
            continue;
        }
        let ts_kind = item.node.kind_id() as usize;
        let vocab_id = lang_map.get(ts_kind).copied().flatten().unwrap_or(0);
        // Saturate depth to u16::MAX — traversal depth never reaches 500, but
        // saturating_cast is the correct pattern for converting u32 → u16.
        #[allow(clippy::cast_possible_truncation)]
        let depth = item.depth.min(u32::from(u16::MAX)) as u16;
        nodes.push(LinearNode { kind_id: vocab_id, depth });
    }

    LinearizeResult {
        nodes,
        node_count: iter.node_count(),
        error_count: iter.error_count(),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "linearize_tests.rs"]
mod tests;
