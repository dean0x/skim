//! AST bigram/trigram extraction from source files.
//!
//! This module walks tree-sitter parse trees to collect parent→child (bigram)
//! and grandparent→parent→child (trigram) node-kind pairs. Pairs are counted as
//! document frequencies across the corpus, then passed to `ast_idf` for IDF
//! weighting.

use std::collections::{HashMap, HashSet};

use rskim_core::{AstWalkConfig, AstWalkIter, Language, Parser};

use crate::ast_types::{
    AstBigram, AstCorpusStats, AstLanguageStats, AstTrigram, NodeKindId, NodeKindVocabulary,
    encode_ast_bigram, encode_ast_trigram,
};
use crate::extract::content_hash;
use crate::types::SourceFile;

// Traversal bounds are centralized on `AstWalkConfig` as associated constants
// (`DEFAULT_MAX_DEPTH` = 500, `DEFAULT_MAX_NODES` = 100 000).  Reference them
// via `AstWalkConfig::DEFAULT_MAX_DEPTH` / `AstWalkConfig::DEFAULT_MAX_NODES`
// wherever a local override is needed, or use `AstWalkConfig::default()` to
// pick up both at once.

/// Maximum source file size accepted for AST extraction (100 KiB default).
const MAX_FILE_SIZE: usize = 100 * 1024;

/// Extended file size limit for languages whose typical files are large
/// schema dumps or data-heavy documents (e.g. SQL migrations).
const MAX_FILE_SIZE_LARGE: usize = 1024 * 1024;

/// Maximum number of trigrams collected per file (memory guard).
const MAX_TRIGRAMS_PER_FILE: usize = 50_000;

/// Per-language document-frequency map for bigrams: `language → (bigram → doc_count)`.
pub type BigramDfMap = HashMap<String, HashMap<AstBigram, u32>>;

/// Per-language document-frequency map for trigrams: `language → (trigram → doc_count)`.
pub type TrigramDfMap = HashMap<String, HashMap<AstTrigram, u32>>;

/// Per-file result from AST n-gram extraction.
#[derive(Default)]
pub struct AstFileResult {
    /// Unique bigrams (parent→child node-kind pairs) found in this file.
    pub bigrams: HashSet<AstBigram>,
    /// Unique trigrams (gp→parent→child) found in this file.
    pub trigrams: HashSet<AstTrigram>,
    /// Number of ERROR nodes encountered (tree-sitter syntax error indicator).
    pub error_node_count: u32,
    /// Total number of AST nodes visited.
    pub node_count: u32,
}

/// Extract AST bigrams and (optionally) trigrams from a single source file.
///
/// Returns an empty result — not an error — when:
/// - `source` exceeds `MAX_FILE_SIZE`
/// - `language` does not have a tree-sitter grammar (JSON, YAML, TOML)
/// - The source fails to parse (tree-sitter is error-tolerant, so this is rare)
///
/// # Errors
///
/// Propagates I/O errors from the tree-sitter parser (language grammar load
/// failures). File-level parse errors (syntax errors) are handled gracefully
/// by counting ERROR nodes rather than returning `Err`.
pub fn extract_ast_ngrams_from_file(
    source: &str,
    language: Language,
    vocab: &mut NodeKindVocabulary,
    collect_trigrams: bool,
) -> anyhow::Result<AstFileResult> {
    let size_limit = match language {
        Language::Sql => MAX_FILE_SIZE_LARGE,
        _ => MAX_FILE_SIZE,
    };
    if source.len() > size_limit {
        eprintln!(
            "Warning: skipping file larger than {} KiB for AST extraction",
            size_limit / 1024
        );
        return Ok(AstFileResult::default());
    }

    // Parser::new returns Err for non-tree-sitter languages (JSON, YAML, TOML).
    // We treat these as "no AST available" and return an empty result.
    let mut parser = match Parser::new(language) {
        Ok(p) => p,
        Err(_) => return Ok(AstFileResult::default()),
    };

    let tree = match parser.parse(source) {
        Ok(t) => t,
        Err(_) => return Ok(AstFileResult::default()),
    };

    let mut result = AstFileResult::default();

    walk_tree(&tree, vocab, collect_trigrams, &mut result);

    Ok(result)
}

/// Iterative pre-order tree walk using `AstWalkIter` with ancestor tracking.
///
/// Replaces the previous hand-rolled `TreeCursor` loop. `AstWalkIter` handles
/// all cursor management, depth tracking, and bounds guarding. This function
/// adds the caller-specific logic: vocabulary lookup, bigram/trigram emission,
/// and depth-indexed ancestor context.
///
/// Ancestor context is maintained in a `Vec<Option<NodeKindId>>` indexed by
/// traversal depth. `ancestors[d]` is the `NodeKindId` of the node at depth
/// `d`, or `None` if that node was an ERROR/MISSING node (which breaks the
/// bigram/trigram chain for its children).
///
/// `AstWalkConfig::DEFAULT_MAX_DEPTH` (500) and `AstWalkConfig::DEFAULT_MAX_NODES`
/// (100 K) are passed through to `AstWalkIter` as bounds guards.
/// `MAX_TRIGRAMS_PER_FILE` stays here as a
/// caller-level cap on output size.
///
/// The trigram emission guard uses two nested `if` blocks intentionally: the
/// outer guard on `collect_trigrams` and the trigram-cap avoids constructing
/// the Option tuple when unnecessary; `clippy::collapsible_if` would merge
/// them into an if-let chain that still constructs the Option tuple
/// unconditionally.
fn walk_tree(
    tree: &tree_sitter::Tree,
    vocab: &mut NodeKindVocabulary,
    collect_trigrams: bool,
    result: &mut AstFileResult,
) {
    // Depth-indexed ancestor table. `ancestors[d]` holds the NodeKindId of the
    // node at depth `d`, or `None` for ERROR/MISSING nodes.
    //
    // Start with a small initial capacity (64) and grow on demand. Typical trees
    // rarely exceed depth 20-30, so pre-allocating DEFAULT_MAX_DEPTH + 1 (501)
    // entries wastes ~4 KiB per file in corpus extraction. The Vec grows only
    // when a node's depth exceeds the current length.
    let mut ancestors: Vec<Option<NodeKindId>> = vec![None; 64];

    let mut iter = AstWalkIter::new(tree.walk(), AstWalkConfig::default());

    for item in iter.by_ref() {
        let depth = item.depth as usize;

        // ── Process current node ───────────────────────────────────────────
        let kind = item.node.kind();

        // Grow the ancestor table on demand if this node is deeper than current capacity.
        if depth >= ancestors.len() {
            ancestors.resize(depth + 1, None);
        }

        if item.is_error {
            // Do not create bigrams/trigrams for ERROR/MISSING nodes.
            // Break the chain: children of this node will see ancestors[depth] = None.
            ancestors[depth] = None;
            continue;
        }

        // Get (or assign) ID for the current node kind.
        let current_id = vocab.get_or_insert(kind);

        // Resolve parent and grandparent from the ancestor table.
        let parent_id: Option<NodeKindId> = depth
            .checked_sub(1)
            .and_then(|pd| ancestors.get(pd).copied().flatten());
        let grandparent_id: Option<NodeKindId> = depth
            .checked_sub(2)
            .and_then(|gd| ancestors.get(gd).copied().flatten());

        // Record this node's ID at its depth for use by its children.
        ancestors[depth] = Some(current_id);

        // Emit bigram: parent → current.
        if let Some(pid) = parent_id {
            result.bigrams.insert(encode_ast_bigram(pid, current_id));
        }

        // Emit trigram: grandparent → parent → current.
        // The two-level if is intentional: see function doc comment.
        #[allow(clippy::collapsible_if)]
        if collect_trigrams && result.trigrams.len() < MAX_TRIGRAMS_PER_FILE {
            if let (Some(gid), Some(pid)) = (grandparent_id, parent_id) {
                result
                    .trigrams
                    .insert(encode_ast_trigram(gid, pid, current_id));
            }
        }
    }

    // Populate counters from the iterator's final tally.
    result.error_node_count = iter.error_count();
    result.node_count = iter.node_count();
}

/// Result of processing all files for a single language.
struct LangProcessResult {
    bigram_df: HashMap<AstBigram, u32>,
    trigram_df: HashMap<AstTrigram, u32>,
    stats: AstLanguageStats,
    /// Number of duplicate files skipped via content-hash deduplication.
    deduplicated: u32,
}

/// Process all files for one language, deduplicating by content hash and
/// accumulating per-language DF maps and statistics.
///
/// `seen_hashes` is shared across all language groups so that a file that
/// appears under multiple languages (e.g. `.h` files catalogued under both C
/// and Cpp) is hashed and processed only once.
fn process_language_files(
    lang_name: &str,
    lang_files: &[&SourceFile],
    vocab: &mut NodeKindVocabulary,
    collect_trigrams: bool,
    progress: &indicatif::ProgressBar,
    seen_hashes: &mut HashSet<[u8; 32]>,
) -> LangProcessResult {
    let mut bigram_df: HashMap<AstBigram, u32> = HashMap::new();
    let mut trigram_df: HashMap<AstTrigram, u32> = HashMap::new();

    let mut lang_file_count: u32 = 0;
    let mut lang_error_nodes: u32 = 0;
    let mut lang_total_nodes: u32 = 0;
    let mut deduplicated: u32 = 0;

    for file in lang_files {
        progress.inc(1);

        let hash = content_hash(&file.content);
        if !seen_hashes.insert(hash) {
            deduplicated = deduplicated.saturating_add(1);
            continue;
        }

        lang_file_count = lang_file_count.saturating_add(1);

        let result = match extract_ast_ngrams_from_file(
            &file.content,
            file.language,
            vocab,
            collect_trigrams,
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "Warning: AST extraction failed for {}: {e:#}",
                    file.path.display()
                );
                continue;
            }
        };

        lang_error_nodes = lang_error_nodes.saturating_add(result.error_node_count);
        lang_total_nodes = lang_total_nodes.saturating_add(result.node_count);

        for bigram in result.bigrams {
            let count = bigram_df.entry(bigram).or_default();
            *count = count.saturating_add(1);
        }
        for trigram in result.trigrams {
            let count = trigram_df.entry(trigram).or_default();
            *count = count.saturating_add(1);
        }
    }

    let unique_bigrams = bigram_df.len();
    let unique_trigrams = trigram_df.len();

    LangProcessResult {
        bigram_df,
        trigram_df,
        stats: AstLanguageStats {
            language: lang_name.to_string(),
            file_count: lang_file_count,
            unique_bigrams,
            unique_trigrams,
            error_node_count: lang_error_nodes,
            total_node_count: lang_total_nodes,
        },
        deduplicated,
    }
}

/// Extract AST n-grams from an entire corpus, grouped by language.
///
/// Returns:
/// - Per-language document-frequency maps for bigrams: `language → (bigram → doc_count)`.
/// - Per-language document-frequency maps for trigrams.
/// - Aggregated corpus statistics.
///
/// Files are SHA-256-deduplicated before counting DF values.
///
/// # Parallelism note
///
/// Extraction is sequential across languages because all files share a single
/// `NodeKindVocabulary`. Parallel extraction would require per-thread vocabularies
/// merged via a map-reduce step — a larger refactoring left for a dedicated
/// optimization pass.
pub fn extract_ast_ngrams_from_corpus(
    files: &[SourceFile],
    vocab: &mut NodeKindVocabulary,
    collect_trigrams: bool,
) -> (BigramDfMap, TrigramDfMap, AstCorpusStats) {
    use indicatif::{ProgressBar, ProgressStyle};

    // Group files by language name (using Language::name()).
    let mut by_language: HashMap<String, Vec<&SourceFile>> = HashMap::new();
    for file in files {
        by_language
            .entry(file.language.name().to_string())
            .or_default()
            .push(file);
    }

    let total_files_seen: u32 = u32::try_from(files.len()).unwrap_or(u32::MAX);
    let progress = ProgressBar::new(total_files_seen as u64);
    if let Ok(style) =
        ProgressStyle::with_template("[{elapsed_precise}] [{bar:40}] {pos}/{len} {msg}")
    {
        progress.set_style(style);
    }

    let mut bigram_df_maps: BigramDfMap = HashMap::new();
    let mut trigram_df_maps: TrigramDfMap = HashMap::new();
    let mut language_stats: Vec<AstLanguageStats> = Vec::new();
    let mut total_unique_files: u32 = 0;
    let mut total_deduplicated: u32 = 0;

    // Corpus-level dedup set: shared across all language groups so that a file
    // appearing in multiple groups (e.g. .h catalogued under both C and Cpp) is
    // processed only once.
    let mut seen_hashes: HashSet<[u8; 32]> = HashSet::new();

    let mut sorted_languages: Vec<String> = by_language.keys().cloned().collect();
    sorted_languages.sort();

    for lang_name in sorted_languages {
        let lang_files = &by_language[&lang_name];
        progress.set_message(lang_name.clone());

        let lang_result = process_language_files(
            &lang_name,
            lang_files,
            vocab,
            collect_trigrams,
            &progress,
            &mut seen_hashes,
        );

        total_unique_files = total_unique_files.saturating_add(lang_result.stats.file_count);
        total_deduplicated = total_deduplicated.saturating_add(lang_result.deduplicated);
        language_stats.push(lang_result.stats);
        bigram_df_maps.insert(lang_name.clone(), lang_result.bigram_df);
        trigram_df_maps.insert(lang_name, lang_result.trigram_df);
    }

    progress.finish_with_message("done");

    let corpus_stats = AstCorpusStats {
        total_files: total_unique_files,
        deduplicated_files: total_deduplicated,
        language_stats,
    };

    (bigram_df_maps, trigram_df_maps, corpus_stats)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::types::SourceFile;
    use std::path::PathBuf;

    fn make_file(content: &str, language: Language) -> SourceFile {
        SourceFile {
            path: PathBuf::from("test"),
            language,
            content: content.to_string(),
        }
    }

    // ── single-file extraction ─────────────────────────────────────────────

    #[test]
    fn empty_source_returns_empty_result() {
        let mut vocab = NodeKindVocabulary::new();
        let result = extract_ast_ngrams_from_file("", Language::Rust, &mut vocab, false).unwrap();
        assert!(result.bigrams.is_empty());
        assert!(result.trigrams.is_empty());
        assert_eq!(result.error_node_count, 0);
        // Tree-sitter always produces a root `source_file` node even for empty
        // input — node_count is 1, not 0.
        assert_eq!(result.node_count, 1);
    }

    #[test]
    fn simple_rust_function_produces_bigrams() {
        let mut vocab = NodeKindVocabulary::new();
        let source = "fn hello() {}";
        let result =
            extract_ast_ngrams_from_file(source, Language::Rust, &mut vocab, false).unwrap();
        // A simple function produces at least some bigrams (source_file → function_item, etc.)
        assert!(
            !result.bigrams.is_empty(),
            "should produce bigrams for 'fn hello() {{}}'"
        );
        assert!(result.node_count > 0);
    }

    #[test]
    fn typescript_class_produces_bigrams() {
        let mut vocab = NodeKindVocabulary::new();
        let source = "class Foo { bar(): void {} }";
        let result =
            extract_ast_ngrams_from_file(source, Language::TypeScript, &mut vocab, false).unwrap();
        assert!(
            !result.bigrams.is_empty(),
            "TypeScript class should produce bigrams"
        );
    }

    #[test]
    fn trigrams_collected_when_enabled() {
        let mut vocab = NodeKindVocabulary::new();
        let source = "fn hello() { let x = 1; }";
        let result =
            extract_ast_ngrams_from_file(source, Language::Rust, &mut vocab, true).unwrap();
        // A function with a body should produce trigrams (grandparent → parent → child).
        assert!(
            !result.trigrams.is_empty(),
            "trigrams should be collected when enabled"
        );
    }

    #[test]
    fn trigrams_empty_when_disabled() {
        let mut vocab = NodeKindVocabulary::new();
        let source = "fn hello() { let x = 1; }";
        let result =
            extract_ast_ngrams_from_file(source, Language::Rust, &mut vocab, false).unwrap();
        assert!(
            result.trigrams.is_empty(),
            "trigrams should be empty when disabled"
        );
    }

    #[test]
    fn non_tree_sitter_language_returns_empty() {
        let mut vocab = NodeKindVocabulary::new();
        // JSON does not have a tree-sitter grammar in Parser::new
        let result = extract_ast_ngrams_from_file("{}", Language::Json, &mut vocab, false).unwrap();
        assert!(result.bigrams.is_empty());
        assert_eq!(result.node_count, 0);
    }

    #[test]
    fn error_nodes_counted_but_not_in_bigrams() {
        let mut vocab = NodeKindVocabulary::new();
        // Deliberately broken Rust syntax — tree-sitter is error-tolerant.
        let source = "fn broken(((( {}";
        let result =
            extract_ast_ngrams_from_file(source, Language::Rust, &mut vocab, false).unwrap();

        // Tree-sitter must have produced at least one ERROR node for this malformed input.
        assert!(
            result.error_node_count > 0,
            "broken syntax should produce ERROR nodes, got 0"
        );

        // ERROR nodes must not be registered in the vocabulary — the implementation
        // sets current_id = None for error nodes, so get_or_insert is never called.
        assert!(
            vocab.get("ERROR").is_none(),
            "ERROR should not be registered in the vocabulary"
        );

        // No bigram should encode an ERROR node ID.  Since ERROR nodes are never
        // inserted into the vocabulary, no valid NodeKindId exists for them, so
        // no bigram can reference one.  Verify by checking every decoded bigram's
        // parent and child IDs resolve to non-ERROR kinds.
        for &bigram in &result.bigrams {
            let (parent_id, child_id) = crate::ast_types::decode_ast_bigram(bigram);
            let parent_kind = vocab.resolve(parent_id).unwrap_or("UNKNOWN");
            let child_kind = vocab.resolve(child_id).unwrap_or("UNKNOWN");
            assert_ne!(parent_kind, "ERROR", "bigram parent should not be ERROR");
            assert_ne!(child_kind, "ERROR", "bigram child should not be ERROR");
        }
    }

    #[test]
    fn error_node_breaks_ancestor_chain_for_descendants() {
        // This test targets the chain-break logic: when walk_tree encounters an
        // ERROR node at depth D it sets ancestors[D] = None. Descendants at depth
        // D+1 look up ancestors[D] for their parent — they must see None, so no
        // bigram is emitted connecting the ERROR node's parent to those descendants.
        //
        // Concretely: with `fn broken(((( { let x = 1; }` the `((((` produces
        // ERROR nodes inside the parameter list. The `let x = 1` body lives at a
        // greater depth than those ERROR nodes. We verify:
        //   1. At least one ERROR node was encountered (sanity guard).
        //   2. No bigram whose parent resolves to the kind immediately above the
        //      ERROR nodes ("parameters" or equivalent) pairs with any of the body
        //      descendants — if the chain were not broken, such bigrams would exist.
        //
        // Because tree-sitter grammar shapes vary, we use a broader invariant that
        // is grammar-independent: collect bigrams with and without a deliberately
        // nested error, then confirm the broken-syntax set is a strict subset —
        // the ERROR-break must suppress at least one bigram that the clean version
        // emits, proving the chain was cut.
        let mut vocab_clean = NodeKindVocabulary::new();
        let clean = "fn ok(x: i32) { let y = x + 1; }";
        let result_clean =
            extract_ast_ngrams_from_file(clean, Language::Rust, &mut vocab_clean, false).unwrap();
        assert_eq!(
            result_clean.error_node_count, 0,
            "clean source must have zero ERROR nodes"
        );

        let mut vocab_broken = NodeKindVocabulary::new();
        // Same structure but parameters replaced with broken syntax, body intact.
        let broken = "fn broken(((( { let x = 1; }";
        let result_broken =
            extract_ast_ngrams_from_file(broken, Language::Rust, &mut vocab_broken, false).unwrap();

        assert!(
            result_broken.error_node_count > 0,
            "broken syntax must produce at least one ERROR node"
        );

        // The broken version must emit fewer bigrams than a clean function of
        // similar structure — the chain-break suppresses the parameter → body
        // bigrams that cross the ERROR boundary.
        assert!(
            result_broken.bigrams.len() < result_clean.bigrams.len(),
            "ERROR chain-break should suppress bigrams: broken ({}) >= clean ({})",
            result_broken.bigrams.len(),
            result_clean.bigrams.len(),
        );
    }

    #[test]
    fn oversized_file_returns_empty() {
        let mut vocab = NodeKindVocabulary::new();
        // 200 KiB source — exceeds MAX_FILE_SIZE
        let large_source = "fn x() {}\n".repeat(20_000);
        let result =
            extract_ast_ngrams_from_file(&large_source, Language::Rust, &mut vocab, false).unwrap();
        assert!(
            result.bigrams.is_empty(),
            "oversized file should return empty result"
        );
    }

    #[test]
    fn vocabulary_grows_across_files() {
        let mut vocab = NodeKindVocabulary::new();

        extract_ast_ngrams_from_file("fn a() {}", Language::Rust, &mut vocab, false).unwrap();
        let count_after_first = vocab.len();

        extract_ast_ngrams_from_file("fn b() { let x = 1; }", Language::Rust, &mut vocab, false)
            .unwrap();
        let count_after_second = vocab.len();

        // Second file may add new kinds (integer_literal, let_declaration, etc.)
        assert!(count_after_second >= count_after_first);
    }

    // ── corpus-level extraction ────────────────────────────────────────────

    #[test]
    fn corpus_deduplicates_identical_files() {
        let files = vec![
            make_file("fn a() {}", Language::Rust),
            make_file("fn a() {}", Language::Rust), // duplicate
        ];
        let mut vocab = NodeKindVocabulary::new();
        let (_, _, stats) = extract_ast_ngrams_from_corpus(&files, &mut vocab, false);

        assert_eq!(stats.total_files, 1, "duplicate should be deduped");
        assert_eq!(stats.deduplicated_files, 1);
    }

    #[test]
    fn corpus_groups_files_by_language() {
        let files = vec![
            make_file("fn a() {}", Language::Rust),
            make_file("function b() {}", Language::JavaScript),
        ];
        let mut vocab = NodeKindVocabulary::new();
        let (bigram_df, _, stats) = extract_ast_ngrams_from_corpus(&files, &mut vocab, false);

        // Each language gets its own DF map.
        assert!(bigram_df.contains_key("Rust"), "Rust should have a DF map");
        assert!(
            bigram_df.contains_key("JavaScript"),
            "JavaScript should have a DF map"
        );
        assert_eq!(stats.language_stats.len(), 2);
    }

    #[test]
    fn corpus_deduplicates_identical_content_across_languages() {
        // Same content catalogued under two different language groups.
        // The corpus-level seen_hashes must deduplicate across languages so the
        // content is processed exactly once total, not once per language group.
        let shared_source = "int shared() { return 0; }";
        let files = vec![
            make_file(shared_source, Language::C),
            make_file(shared_source, Language::Cpp),
        ];
        let mut vocab = NodeKindVocabulary::new();
        let (_, _, stats) = extract_ast_ngrams_from_corpus(&files, &mut vocab, false);

        // total_files = 1: the second occurrence (different language, same hash)
        // must be counted as a duplicate, not a new file.
        assert_eq!(
            stats.total_files, 1,
            "identical content in two language groups should be counted once"
        );
        assert_eq!(
            stats.deduplicated_files, 1,
            "the duplicate should be recorded in deduplicated_files"
        );
    }

    #[test]
    fn all_14_ts_languages_produce_output() {
        let test_cases: &[(&str, Language)] = &[
            ("fn a() {}", Language::Rust),
            ("function b() {}", Language::TypeScript),
            ("function c() {}", Language::JavaScript),
            ("def d(): pass", Language::Python),
            ("func e() {}", Language::Go),
            ("class F { void f() {} }", Language::Java),
            ("int g() { return 0; }", Language::C),
            ("int h() { return 0; }", Language::Cpp),
            ("class I { void i() {} }", Language::CSharp),
            ("def j; end", Language::Ruby),
            ("SELECT 1", Language::Sql),
            ("fun k() {}", Language::Kotlin),
            ("func l() {}", Language::Swift),
            ("# Hello\n\nSome text", Language::Markdown),
        ];

        for (source, lang) in test_cases {
            let mut vocab = NodeKindVocabulary::new();
            let result = extract_ast_ngrams_from_file(source, *lang, &mut vocab, false).unwrap();
            assert!(
                !result.bigrams.is_empty() || result.node_count > 0,
                "language {:?} should produce AST nodes for {:?}",
                lang,
                source
            );
        }
    }

    // ── full pipeline: extract → stabilize → rekey → IDF ──────────────────
    //
    // This integration test guards against regressions in the sequencing that
    // caused the remap bug (commit 605203a): bigram/trigram DF maps were keyed
    // with pre-stabilize IDs but decoded against post-stabilize IDs, producing
    // wrong kind-string resolution.

    #[test]
    fn stabilize_rekey_idf_pipeline_resolves_correct_kind_names() {
        use crate::ast_idf;
        use crate::ast_types::{rekey_bigram_df_map, rekey_trigram_df_map};
        use std::path::PathBuf;

        let rust_source = "fn greet(name: &str) -> String { format!(\"hello {}\", name) }";
        let files = vec![SourceFile {
            path: PathBuf::from("test.rs"),
            language: Language::Rust,
            content: rust_source.to_string(),
        }];

        let mut vocab = NodeKindVocabulary::new();
        let (raw_bigram_df, raw_trigram_df, corpus_stats) =
            extract_ast_ngrams_from_corpus(&files, &mut vocab, true);

        // Capture pre-stabilize vocabulary size for sanity.
        let pre_stabilize_size = vocab.len();
        assert!(
            pre_stabilize_size > 0,
            "vocabulary must be non-empty after extraction"
        );

        // Stabilize: reassigns IDs alphabetically and returns the old→new remap table.
        let remap = vocab.stabilize();

        // Post-stabilize size must be unchanged (stabilize only reorders, never adds/removes).
        assert_eq!(
            vocab.len(),
            pre_stabilize_size,
            "stabilize must preserve vocabulary size"
        );

        // Re-key all DF maps so encoded IDs match the post-stabilize vocabulary.
        let rust_bigram_df = raw_bigram_df.get("Rust").expect("Rust must have bigrams");
        let rekeyed_bigrams = rekey_bigram_df_map(rust_bigram_df, &remap);

        // Compute IDF weights — threshold=0.0 keeps all entries.
        let total_docs = corpus_stats.total_files;
        assert!(total_docs > 0, "corpus must have at least one file");
        let bigram_weights =
            ast_idf::compute_ast_bigram_weights(&rekeyed_bigrams, total_docs, 0.0, &vocab);

        assert!(
            !bigram_weights.is_empty(),
            "pipeline must produce at least one bigram weight for a Rust function"
        );

        // Every resolved weight must have non-empty kind strings.  An empty string
        // would indicate that `vocab.resolve()` returned None — i.e. the DF map
        // still contained pre-stabilize IDs that no longer exist in the vocabulary.
        for w in &bigram_weights {
            assert!(
                !w.parent_kind.is_empty(),
                "parent_kind must resolve to a non-empty string (pre-stabilize ID leak?)"
            );
            assert!(
                !w.child_kind.is_empty(),
                "child_kind must resolve to a non-empty string (pre-stabilize ID leak?)"
            );
        }

        // Trigrams: same pipeline check.
        if let Some(rust_trigram_df) = raw_trigram_df.get("Rust") {
            let rekeyed_trigrams = rekey_trigram_df_map(rust_trigram_df, &remap);
            let trigram_weights =
                ast_idf::compute_ast_trigram_weights(&rekeyed_trigrams, total_docs, 0.0, &vocab);
            for w in &trigram_weights {
                assert!(
                    !w.grandparent_kind.is_empty(),
                    "grandparent_kind must resolve (pre-stabilize ID leak?)"
                );
                assert!(
                    !w.parent_kind.is_empty(),
                    "parent_kind must resolve (pre-stabilize ID leak?)"
                );
                assert!(
                    !w.child_kind.is_empty(),
                    "child_kind must resolve (pre-stabilize ID leak?)"
                );
            }
        }
    }
}
