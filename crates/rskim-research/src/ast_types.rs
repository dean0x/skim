//! AST n-gram type definitions for node-kind frequency analysis.
//!
//! This module defines the core types used to represent AST bigrams and trigrams,
//! along with the vocabulary mapping and weight table structures.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Compact numeric ID for a tree-sitter node kind string.
pub type NodeKindId = u16;

/// Packed representation of a parent→child AST node-kind pair.
///
/// High 16 bits = parent `NodeKindId`, low 16 bits = child `NodeKindId`.
pub type AstBigram = u32;

/// Packed representation of a grandparent→parent→child AST node-kind triple.
///
/// Bits [47:32] = grandparent, bits [31:16] = parent, bits [15:0] = child.
pub type AstTrigram = u64;

// ─────────────────────────────────────────────────────────
// Encoding helpers
// ─────────────────────────────────────────────────────────

/// Encode a parent–child pair into a bigram key.
#[must_use]
pub fn encode_ast_bigram(parent: NodeKindId, child: NodeKindId) -> AstBigram {
    (u32::from(parent) << 16) | u32::from(child)
}

/// Decode a bigram key back into its (parent, child) component IDs.
#[must_use]
pub fn decode_ast_bigram(bigram: AstBigram) -> (NodeKindId, NodeKindId) {
    let parent = (bigram >> 16) as NodeKindId;
    let child = (bigram & 0xFFFF) as NodeKindId;
    (parent, child)
}

/// Encode a grandparent–parent–child triple into a trigram key.
///
/// Layout: bits `[47:32]` = grandparent, `[31:16]` = parent, `[15:0]` = child.
#[must_use]
pub fn encode_ast_trigram(
    grandparent: NodeKindId,
    parent: NodeKindId,
    child: NodeKindId,
) -> AstTrigram {
    (u64::from(grandparent) << 32) | (u64::from(parent) << 16) | u64::from(child)
}

/// Decode a trigram key back into its (grandparent, parent, child) component IDs.
#[must_use]
pub fn decode_ast_trigram(trigram: AstTrigram) -> (NodeKindId, NodeKindId, NodeKindId) {
    let grandparent = ((trigram >> 32) & 0xFFFF) as NodeKindId;
    let parent = ((trigram >> 16) & 0xFFFF) as NodeKindId;
    let child = (trigram & 0xFFFF) as NodeKindId;
    (grandparent, parent, child)
}

// ─────────────────────────────────────────────────────────
// Re-keying after stabilize
// ─────────────────────────────────────────────────────────

/// Re-encode a bigram key using an old-to-new ID remap table.
///
/// Decodes the bigram into its (parent, child) IDs, remaps each through
/// `remap[old_id]`, and re-encodes the result.
///
/// Returns `None` if either ID is out of bounds for the remap table.
#[must_use]
pub fn remap_bigram(bigram: AstBigram, remap: &[NodeKindId]) -> Option<AstBigram> {
    let (parent, child) = decode_ast_bigram(bigram);
    let new_parent = *remap.get(usize::from(parent))?;
    let new_child = *remap.get(usize::from(child))?;
    Some(encode_ast_bigram(new_parent, new_child))
}

/// Re-encode a trigram key using an old-to-new ID remap table.
///
/// Returns `None` if any ID is out of bounds for the remap table.
#[must_use]
pub fn remap_trigram(trigram: AstTrigram, remap: &[NodeKindId]) -> Option<AstTrigram> {
    let (gp, parent, child) = decode_ast_trigram(trigram);
    let new_gp = *remap.get(usize::from(gp))?;
    let new_parent = *remap.get(usize::from(parent))?;
    let new_child = *remap.get(usize::from(child))?;
    Some(encode_ast_trigram(new_gp, new_parent, new_child))
}

/// Re-key an entire bigram document-frequency map using the remap table.
///
/// Entries whose IDs fall outside the remap table are silently dropped.
#[must_use]
pub fn rekey_bigram_df_map(
    df_map: &HashMap<AstBigram, u32>,
    remap: &[NodeKindId],
) -> HashMap<AstBigram, u32> {
    let mut new_map = HashMap::with_capacity(df_map.len());
    for (&bigram, &count) in df_map {
        if let Some(new_key) = remap_bigram(bigram, remap) {
            *new_map.entry(new_key).or_default() += count;
        }
    }
    new_map
}

/// Re-key an entire trigram document-frequency map using the remap table.
///
/// Entries whose IDs fall outside the remap table are silently dropped.
#[must_use]
pub fn rekey_trigram_df_map(
    df_map: &HashMap<AstTrigram, u32>,
    remap: &[NodeKindId],
) -> HashMap<AstTrigram, u32> {
    let mut new_map = HashMap::with_capacity(df_map.len());
    for (&trigram, &count) in df_map {
        if let Some(new_key) = remap_trigram(trigram, remap) {
            *new_map.entry(new_key).or_default() += count;
        }
    }
    new_map
}

// ─────────────────────────────────────────────────────────
// Vocabulary
// ─────────────────────────────────────────────────────────

/// Bidirectional mapping between node-kind strings and compact `NodeKindId` integers.
///
/// IDs are assigned incrementally as new kinds are encountered. Call
/// [`stabilize`](Self::stabilize) after the corpus pass to sort alphabetically
/// and reassign IDs for deterministic, reproducible output.
#[derive(Debug, Clone, Default)]
pub struct NodeKindVocabulary {
    /// kind string → ID
    kind_to_id: HashMap<String, NodeKindId>,
    /// ID → kind string (same length as kind_to_id after stabilize)
    id_to_kind: Vec<String>,
}

impl NodeKindVocabulary {
    /// Create an empty vocabulary.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the ID for `kind`, inserting a new entry if not yet present.
    #[must_use]
    pub fn get_or_insert(&mut self, kind: &str) -> NodeKindId {
        if let Some(&id) = self.kind_to_id.get(kind) {
            return id;
        }
        // In practice tree-sitter grammars have O(100) node kinds per language,
        // so the u16 limit is never approached in normal usage, but we guard
        // in release builds too — silent truncation to 0 would corrupt all DF maps.
        assert!(
            self.id_to_kind.len() < usize::from(NodeKindId::MAX),
            "NodeKindVocabulary overflow: {} kinds exceeds u16::MAX",
            self.id_to_kind.len()
        );
        let id = self.id_to_kind.len() as NodeKindId;
        let owned = kind.to_string();
        self.kind_to_id.insert(owned.clone(), id);
        self.id_to_kind.push(owned);
        id
    }

    /// Look up the ID for an existing kind without inserting.
    #[must_use]
    pub fn get(&self, kind: &str) -> Option<NodeKindId> {
        self.kind_to_id.get(kind).copied()
    }

    /// Resolve an ID back to its kind string.
    ///
    /// Returns `None` for unknown IDs (e.g., IDs produced before `stabilize` was
    /// called on a different vocabulary instance).
    #[must_use]
    pub fn resolve(&self, id: NodeKindId) -> Option<&str> {
        self.id_to_kind.get(usize::from(id)).map(String::as_str)
    }

    /// Number of distinct node kinds in the vocabulary.
    #[must_use]
    pub fn len(&self) -> usize {
        self.id_to_kind.len()
    }

    /// Returns `true` if no kinds have been registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.id_to_kind.is_empty()
    }

    /// Sort all kinds alphabetically and reassign IDs for deterministic output.
    ///
    /// After calling `stabilize`, the same set of node kinds always maps to the
    /// same IDs regardless of insertion order, making generated weight tables
    /// reproducible across corpus passes.
    ///
    /// Returns a mapping from old IDs to new IDs so that callers can re-key
    /// any bigram/trigram maps that were built with pre-stabilize IDs.
    /// The returned vector is indexed by old ID; `remap[old_id] = new_id`.
    ///
    /// **Important:** Any bigram/trigram keys computed *before* calling `stabilize`
    /// must be re-encoded using the returned remap table.
    pub fn stabilize(&mut self) -> Vec<NodeKindId> {
        // Build remap: sort the old kinds alphabetically, re-populate the vocab,
        // then map each old ID to its new ID.
        //
        // We sort indices into old_kinds rather than cloning the whole vec, so each
        // String is moved exactly once (into self.id_to_kind) with no extra copies.
        let old_kinds: Vec<String> = self.id_to_kind.drain(..).collect();

        // Sort indices by the corresponding kind string — no extra String allocation.
        let mut sorted_indices: Vec<usize> = (0..old_kinds.len()).collect();
        sorted_indices.sort_unstable_by(|&a, &b| old_kinds[a].cmp(&old_kinds[b]));

        // Build remap[old_id] = new_id from the sorted index order.
        let mut remap = vec![0u16; old_kinds.len()];
        for (new_id, &old_id) in sorted_indices.iter().enumerate() {
            remap[old_id] = new_id as NodeKindId;
        }

        self.kind_to_id.clear();
        // Reconstruct id_to_kind in sorted order by moving Strings out of old_kinds.
        // sorted_indices contains each index exactly once, so the slot is always Some.
        let mut old_kinds_opt: Vec<Option<String>> = old_kinds.into_iter().map(Some).collect();
        self.id_to_kind = sorted_indices
            .iter()
            .map(|&old_id| {
                // INVARIANT: sorted_indices is a permutation of [0, N), so each slot
                // is taken exactly once.  The unwrap_or_else branch is unreachable.
                old_kinds_opt[old_id]
                    .take()
                    .unwrap_or_else(|| unreachable!("sorted_indices are unique"))
            })
            .collect();

        for (new_id, kind) in self.id_to_kind.iter().enumerate() {
            self.kind_to_id.insert(kind.clone(), new_id as NodeKindId);
        }

        remap
    }

    /// Returns all registered kind strings in the order of their assigned IDs.
    ///
    /// After [`stabilize`](Self::stabilize) this order is alphabetical, matching
    /// the stable ID assignment.  Call only after `stabilize` when sorted output
    /// is required.
    #[must_use]
    pub fn kinds(&self) -> &[String] {
        &self.id_to_kind
    }
}

// ─────────────────────────────────────────────────────────
// Weight structs
// ─────────────────────────────────────────────────────────

/// A single AST bigram with its IDF weight and human-readable kind strings.
///
/// **Sort contract:** When returned from `compute_ast_bigram_weights`, entries are
/// sorted by `idf` descending. Consumers that re-sort (e.g. by bigram key for
/// deterministic JSON output) must document their own sort order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstBigramWeight {
    pub parent_kind: String,
    pub child_kind: String,
    pub bigram: AstBigram,
    pub idf: f32,
}

/// A single AST trigram with its IDF weight and human-readable kind strings.
///
/// **Sort contract:** When returned from `compute_ast_trigram_weights`, entries are
/// sorted by `idf` descending. Consumers that re-sort (e.g. by trigram key for
/// deterministic JSON output) must document their own sort order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstTrigramWeight {
    pub grandparent_kind: String,
    pub parent_kind: String,
    pub child_kind: String,
    pub trigram: AstTrigram,
    pub idf: f32,
}

// ─────────────────────────────────────────────────────────
// Stats structs
// ─────────────────────────────────────────────────────────

/// Per-language statistics collected during AST extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstLanguageStats {
    pub language: String,
    pub file_count: u32,
    pub unique_bigrams: usize,
    pub unique_trigrams: usize,
    pub error_node_count: u32,
    pub total_node_count: u32,
}

/// Corpus-level statistics for the AST extraction pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstCorpusStats {
    pub total_files: u32,
    pub deduplicated_files: u32,
    pub language_stats: Vec<AstLanguageStats>,
}

// ─────────────────────────────────────────────────────────
// Weight table (final output written to JSON)
// ─────────────────────────────────────────────────────────

/// The complete AST weight table, written to JSON and read by codegen.
///
/// - `vocabulary`: All node kind strings in alphabetical order (index = ID).
/// - `bigram_weights`: Per-language bigram weight lists, keyed by language name.
/// - `trigram_weights`: Per-language trigram weight lists, keyed by language name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AstWeightTable {
    pub version: u8,
    pub generated_at: String,
    pub vocabulary: Vec<String>,
    pub corpus_stats: AstCorpusStats,
    /// Keys are language name strings (e.g. `"Rust"`, `"TypeScript"`).
    pub bigram_weights: HashMap<String, Vec<AstBigramWeight>>,
    /// Keys are language name strings.
    pub trigram_weights: HashMap<String, Vec<AstTrigramWeight>>,
}

// ─────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    // get_or_insert is called in test setup for its side effect only (populating
    // the vocabulary); the returned ID is intentionally discarded in those cases.
    #![allow(unused_must_use)]

    use super::*;

    // ── encode/decode roundtrips ──────────────────────────

    #[test]
    fn bigram_encode_decode_roundtrip() {
        for parent in [0u16, 1, 100, 300, u16::MAX] {
            for child in [0u16, 1, 100, 300, u16::MAX] {
                let encoded = encode_ast_bigram(parent, child);
                let (p2, c2) = decode_ast_bigram(encoded);
                assert_eq!(
                    (p2, c2),
                    (parent, child),
                    "bigram roundtrip failed for ({parent},{child})"
                );
            }
        }
    }

    #[test]
    fn trigram_encode_decode_roundtrip() {
        let cases = [
            (0u16, 0u16, 0u16),
            (1, 2, 3),
            (100, 200, 300),
            (u16::MAX, u16::MAX, u16::MAX),
            (0, u16::MAX, 0),
        ];
        for (gp, p, c) in cases {
            let encoded = encode_ast_trigram(gp, p, c);
            let (gp2, p2, c2) = decode_ast_trigram(encoded);
            assert_eq!(
                (gp2, p2, c2),
                (gp, p, c),
                "trigram roundtrip failed for ({gp},{p},{c})"
            );
        }
    }

    // ── vocabulary ─────────────────────────────────────────

    #[test]
    fn vocabulary_get_or_insert_idempotent() {
        let mut vocab = NodeKindVocabulary::new();
        let id1 = vocab.get_or_insert("function_item");
        let id2 = vocab.get_or_insert("function_item");
        assert_eq!(id1, id2, "same kind must return same ID");
    }

    #[test]
    fn vocabulary_different_kinds_get_different_ids() {
        let mut vocab = NodeKindVocabulary::new();
        let id_a = vocab.get_or_insert("function_item");
        let id_b = vocab.get_or_insert("identifier");
        assert_ne!(id_a, id_b);
        assert_eq!(vocab.len(), 2);
    }

    #[test]
    fn vocabulary_alphabetical_stability() {
        let mut vocab = NodeKindVocabulary::new();
        // Insert in reverse alphabetical order
        vocab.get_or_insert("z_kind");
        vocab.get_or_insert("m_kind");
        vocab.get_or_insert("a_kind");

        vocab.stabilize();

        // After stabilize: "a_kind" → 0, "m_kind" → 1, "z_kind" → 2
        assert_eq!(vocab.get("a_kind"), Some(0));
        assert_eq!(vocab.get("m_kind"), Some(1));
        assert_eq!(vocab.get("z_kind"), Some(2));

        // kinds() returns them sorted
        let kinds = vocab.kinds();
        assert_eq!(kinds, ["a_kind", "m_kind", "z_kind"]);
    }

    #[test]
    fn vocabulary_resolve_unknown_id_returns_none() {
        let vocab = NodeKindVocabulary::new();
        // No entries → any ID is unknown
        assert_eq!(vocab.resolve(0), None);
        assert_eq!(vocab.resolve(999), None);
    }

    #[test]
    fn vocabulary_resolve_after_stabilize() {
        let mut vocab = NodeKindVocabulary::new();
        vocab.get_or_insert("b_kind");
        vocab.get_or_insert("a_kind");
        vocab.stabilize();

        // After stabilize: "a_kind"=0, "b_kind"=1
        assert_eq!(vocab.resolve(0), Some("a_kind"));
        assert_eq!(vocab.resolve(1), Some("b_kind"));
        assert_eq!(vocab.resolve(2), None);
    }

    // ── serde roundtrip ───────────────────────────────────

    #[test]
    fn ast_weight_table_serde_roundtrip() {
        let table = AstWeightTable {
            version: 1,
            generated_at: "unix:0".to_string(),
            vocabulary: vec!["function_item".to_string(), "identifier".to_string()],
            corpus_stats: AstCorpusStats {
                total_files: 10,
                deduplicated_files: 2,
                language_stats: vec![AstLanguageStats {
                    language: "Rust".to_string(),
                    file_count: 10,
                    unique_bigrams: 5,
                    unique_trigrams: 3,
                    error_node_count: 0,
                    total_node_count: 100,
                }],
            },
            bigram_weights: {
                let mut m = HashMap::new();
                m.insert(
                    "Rust".to_string(),
                    vec![AstBigramWeight {
                        parent_kind: "function_item".to_string(),
                        child_kind: "identifier".to_string(),
                        bigram: encode_ast_bigram(0, 1),
                        idf: 3.5,
                    }],
                );
                m
            },
            trigram_weights: HashMap::new(),
        };

        let json = serde_json::to_string(&table).unwrap();
        let restored: AstWeightTable = serde_json::from_str(&json).unwrap();

        assert_eq!(restored.version, table.version);
        assert_eq!(restored.vocabulary, table.vocabulary);
        assert_eq!(
            restored.corpus_stats.total_files,
            table.corpus_stats.total_files
        );
        assert_eq!(
            restored.bigram_weights["Rust"][0].idf,
            table.bigram_weights["Rust"][0].idf
        );
    }

    // ── stabilize remap ──────────────────────────────────

    #[test]
    fn stabilize_returns_correct_remap() {
        let mut vocab = NodeKindVocabulary::new();
        // Insert in reverse alphabetical order: z=0, m=1, a=2
        vocab.get_or_insert("z_kind");
        vocab.get_or_insert("m_kind");
        vocab.get_or_insert("a_kind");

        let remap = vocab.stabilize();

        // After stabilize: a_kind=0, m_kind=1, z_kind=2
        // Old IDs: z_kind was 0, m_kind was 1, a_kind was 2
        // remap[0] = new id of z_kind = 2
        // remap[1] = new id of m_kind = 1
        // remap[2] = new id of a_kind = 0
        assert_eq!(remap, vec![2, 1, 0]);
    }

    #[test]
    fn remap_bigram_correctness() {
        let mut vocab = NodeKindVocabulary::new();
        vocab.get_or_insert("z_kind"); // old ID 0
        vocab.get_or_insert("a_kind"); // old ID 1

        // Bigram encoded with old IDs: parent=0 (z_kind), child=1 (a_kind)
        let old_bigram = encode_ast_bigram(0, 1);

        let remap = vocab.stabilize();
        // After stabilize: a_kind=0, z_kind=1
        // remap[0] = 1 (z_kind old:0 -> new:1)
        // remap[1] = 0 (a_kind old:1 -> new:0)

        let new_bigram = remap_bigram(old_bigram, &remap).unwrap();
        let (new_parent, new_child) = decode_ast_bigram(new_bigram);

        // parent was z_kind (new ID 1), child was a_kind (new ID 0)
        assert_eq!(new_parent, 1);
        assert_eq!(new_child, 0);
        assert_eq!(vocab.resolve(new_parent), Some("z_kind"));
        assert_eq!(vocab.resolve(new_child), Some("a_kind"));
    }

    #[test]
    fn rekey_bigram_df_map_preserves_counts() {
        let mut vocab = NodeKindVocabulary::new();
        vocab.get_or_insert("z_kind"); // old ID 0
        vocab.get_or_insert("a_kind"); // old ID 1

        let old_bigram = encode_ast_bigram(0, 1);
        let mut df_map = HashMap::new();
        df_map.insert(old_bigram, 42u32);

        let remap = vocab.stabilize();
        let rekeyed = rekey_bigram_df_map(&df_map, &remap);

        // The count should be preserved under the new key.
        assert_eq!(rekeyed.len(), 1);
        let new_bigram = remap_bigram(old_bigram, &remap).unwrap();
        assert_eq!(rekeyed[&new_bigram], 42);
    }

    #[test]
    fn rekey_bigram_df_map_merges_collisions() {
        // Two distinct old bigrams that remap to the same new bigram key after
        // stabilize must have their counts summed (the += accumulation path).
        //
        // Manual remap table: remap = [0, 0] forces a collision —
        //   both old ID 0 and old ID 1 map to new ID 0.
        //
        // bigram_a = (0, 1) → remaps to (0, 0)
        // bigram_b = (1, 0) → remaps to (0, 0)
        //
        // Both have count 10 → merged count must be 20.
        let remap: Vec<NodeKindId> = vec![0, 0];

        let bigram_a = encode_ast_bigram(0, 1);
        let bigram_b = encode_ast_bigram(1, 0);
        let mut df_map = HashMap::new();
        df_map.insert(bigram_a, 10_u32);
        df_map.insert(bigram_b, 10_u32);

        let rekeyed = rekey_bigram_df_map(&df_map, &remap);

        // Collision: both entries map to the same key; counts must be summed.
        assert_eq!(
            rekeyed.len(),
            1,
            "colliding bigrams must merge into one entry"
        );
        let merged_key = encode_ast_bigram(0, 0);
        assert_eq!(
            rekeyed[&merged_key], 20,
            "collision counts must be summed: 10 + 10 = 20"
        );
    }

    #[test]
    fn remap_trigram_correctness() {
        let mut vocab = NodeKindVocabulary::new();
        vocab.get_or_insert("z_kind"); // old ID 0
        vocab.get_or_insert("m_kind"); // old ID 1
        vocab.get_or_insert("a_kind"); // old ID 2

        // Trigram encoded with pre-stabilize IDs: gp=0 (z), parent=1 (m), child=2 (a)
        let old_trigram = encode_ast_trigram(0, 1, 2);

        let remap = vocab.stabilize();
        // After stabilize: a_kind=0, m_kind=1, z_kind=2
        // remap[0]=2 (z), remap[1]=1 (m), remap[2]=0 (a)

        let new_trigram = remap_trigram(old_trigram, &remap).unwrap();
        let (new_gp, new_parent, new_child) = decode_ast_trigram(new_trigram);

        // gp was z_kind (new ID 2), parent was m_kind (new ID 1), child was a_kind (new ID 0)
        assert_eq!(new_gp, 2);
        assert_eq!(new_parent, 1);
        assert_eq!(new_child, 0);
        assert_eq!(vocab.resolve(new_gp), Some("z_kind"));
        assert_eq!(vocab.resolve(new_parent), Some("m_kind"));
        assert_eq!(vocab.resolve(new_child), Some("a_kind"));
    }

    #[test]
    fn rekey_trigram_df_map_preserves_counts() {
        let mut vocab = NodeKindVocabulary::new();
        vocab.get_or_insert("z_kind"); // old ID 0
        vocab.get_or_insert("m_kind"); // old ID 1
        vocab.get_or_insert("a_kind"); // old ID 2

        let old_trigram = encode_ast_trigram(0, 1, 2);
        let mut df_map = HashMap::new();
        df_map.insert(old_trigram, 99u32);

        let remap = vocab.stabilize();
        let rekeyed = rekey_trigram_df_map(&df_map, &remap);

        // Count preserved under the new key.
        assert_eq!(rekeyed.len(), 1);
        let new_trigram = remap_trigram(old_trigram, &remap).unwrap();
        assert_eq!(rekeyed[&new_trigram], 99);
    }

    #[test]
    fn remap_bigram_returns_none_for_out_of_bounds_id() {
        // Remap table covers only IDs 0..=1; a bigram referencing ID 2 is out-of-bounds.
        let remap: Vec<NodeKindId> = vec![1, 0]; // only IDs 0 and 1 are valid
        let oob_bigram = encode_ast_bigram(0, 2); // child ID 2 is outside remap
        assert_eq!(remap_bigram(oob_bigram, &remap), None);
    }

    #[test]
    fn remap_trigram_returns_none_for_out_of_bounds_id() {
        // Remap table covers only IDs 0..=1; a trigram referencing ID 3 is out-of-bounds.
        let remap: Vec<NodeKindId> = vec![1, 0]; // only IDs 0 and 1 are valid
        let oob_trigram = encode_ast_trigram(0, 1, 3); // child ID 3 is outside remap
        assert_eq!(remap_trigram(oob_trigram, &remap), None);
    }
}
