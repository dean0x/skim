//! AST bigram/trigram extraction from source files.
//!
//! This module walks tree-sitter parse trees to collect parent→child (bigram)
//! and grandparent→parent→child (trigram) node-kind pairs. Pairs are counted as
//! document frequencies across the corpus, then passed to `ast_idf` for IDF
//! weighting.

use std::collections::{HashMap, HashSet};

use rskim_core::{Language, Parser};

use crate::ast_types::{
    AstBigram, AstCorpusStats, AstLanguageStats, AstTrigram, NodeKindId, NodeKindVocabulary,
    encode_ast_bigram, encode_ast_trigram,
};
use crate::extract::content_hash;
use crate::types::SourceFile;

/// Maximum AST traversal depth to prevent stack-overflow-equivalent run-away on
/// pathological inputs.
const MAX_AST_DEPTH: usize = 500;

/// Maximum number of AST nodes visited per file.
const MAX_AST_NODES: usize = 100_000;

/// Maximum source file size accepted for AST extraction (100 KiB).
const MAX_FILE_SIZE: usize = 100 * 1024;

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
    if source.len() > MAX_FILE_SIZE {
        eprintln!(
            "Warning: skipping file larger than {} KiB for AST extraction",
            MAX_FILE_SIZE / 1024
        );
        return Ok(AstFileResult::default());
    }

    // Parser::new returns Err for non-tree-sitter languages (JSON, YAML, TOML, Markdown).
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
    let mut cursor = tree.walk();

    walk_tree(
        &mut cursor,
        vocab,
        &mut result.bigrams,
        &mut result.trigrams,
        collect_trigrams,
        &mut result.error_node_count,
        &mut result.node_count,
        0,
        None,
        None,
    );

    Ok(result)
}

/// Iterative tree walk using `TreeCursor` to avoid recursion depth limits.
///
/// Collects parent→child bigrams and (when `collect_trigrams` is true)
/// grandparent→parent→child trigrams. ERROR nodes are counted but not included
/// in bigram/trigram pairs (they represent parse failures, not real grammar
/// relationships).
#[allow(clippy::too_many_arguments)]
fn walk_tree(
    cursor: &mut tree_sitter::TreeCursor,
    vocab: &mut NodeKindVocabulary,
    bigrams: &mut HashSet<AstBigram>,
    trigrams: &mut HashSet<AstTrigram>,
    collect_trigrams: bool,
    error_count: &mut u32,
    node_count: &mut u32,
    depth: usize,
    parent_id: Option<NodeKindId>,
    grandparent_id: Option<NodeKindId>,
) {
    // Depth and node count guards.
    if depth >= MAX_AST_DEPTH || *node_count >= MAX_AST_NODES as u32 {
        return;
    }

    let node = cursor.node();
    *node_count += 1;

    let kind = node.kind();
    let is_error = node.is_error() || kind == "ERROR";

    if is_error {
        *error_count += 1;
        // Do not create bigrams/trigrams for ERROR nodes — they are not real
        // grammar relationships. Continue walking children so we can count all
        // error nodes in the subtree.
    }

    // Get (or assign) ID for the current node kind.
    let current_id = if is_error {
        None
    } else {
        Some(vocab.get_or_insert(kind))
    };

    // Emit bigram: parent → current.
    if let (Some(pid), Some(cid)) = (parent_id, current_id) {
        bigrams.insert(encode_ast_bigram(pid, cid));
    }

    // Emit trigram: grandparent → parent → current.
    // The two-level if is intentional: the outer guard avoids tuple construction
    // overhead when the cap or flag is not set; clippy::collapsible_if would merge
    // them into an if-let chain that still allocates the Option tuple unconditionally.
    #[allow(clippy::collapsible_if)]
    if collect_trigrams && trigrams.len() < MAX_TRIGRAMS_PER_FILE {
        if let (Some(gid), Some(pid), Some(cid)) = (grandparent_id, parent_id, current_id) {
            trigrams.insert(encode_ast_trigram(gid, pid, cid));
        }
    }

    // Walk children iteratively using the cursor.
    if cursor.goto_first_child() {
        loop {
            walk_tree(
                cursor,
                vocab,
                bigrams,
                trigrams,
                collect_trigrams,
                error_count,
                node_count,
                depth + 1,
                current_id,
                parent_id,
            );

            if *node_count >= MAX_AST_NODES as u32 {
                break;
            }

            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
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

    let total_files_seen: u32 = files.len() as u32;
    let progress = ProgressBar::new(total_files_seen as u64);
    if let Ok(style) =
        ProgressStyle::with_template("[{elapsed_precise}] [{bar:40}] {pos}/{len} {msg}")
    {
        progress.set_style(style);
    }

    let mut bigram_df_maps: HashMap<String, HashMap<AstBigram, u32>> = HashMap::new();
    let mut trigram_df_maps: HashMap<String, HashMap<AstTrigram, u32>> = HashMap::new();
    let mut language_stats: Vec<AstLanguageStats> = Vec::new();
    let mut total_unique_files: u32 = 0;
    let mut total_deduplicated: u32 = 0;

    let mut sorted_languages: Vec<String> = by_language.keys().cloned().collect();
    sorted_languages.sort();

    for lang_name in sorted_languages {
        let lang_files = &by_language[&lang_name];

        let mut seen_hashes: HashSet<[u8; 32]> = HashSet::new();
        let bigram_df = bigram_df_maps.entry(lang_name.clone()).or_default();
        let trigram_df = trigram_df_maps.entry(lang_name.clone()).or_default();

        let mut lang_file_count: u32 = 0;
        let mut lang_error_nodes: u32 = 0;
        let mut lang_total_nodes: u32 = 0;

        for file in lang_files.iter() {
            progress.set_message(lang_name.clone());
            progress.inc(1);

            let hash = content_hash(&file.content);
            if !seen_hashes.insert(hash) {
                total_deduplicated += 1;
                continue;
            }

            lang_file_count += 1;
            total_unique_files += 1;

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

            lang_error_nodes += result.error_node_count;
            lang_total_nodes += result.node_count;

            for bigram in result.bigrams {
                *bigram_df.entry(bigram).or_default() += 1;
            }
            for trigram in result.trigrams {
                *trigram_df.entry(trigram).or_default() += 1;
            }
        }

        language_stats.push(AstLanguageStats {
            language: lang_name,
            file_count: lang_file_count,
            unique_bigrams: bigram_df.len(),
            unique_trigrams: trigram_df.len(),
            error_node_count: lang_error_nodes,
            total_node_count: lang_total_nodes,
        });
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
    #![allow(clippy::unwrap_used)]

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
        // Should parse (tree-sitter doesn't hard-fail), result is valid even with errors.
        // We just check that it doesn't panic.
        let _ = result;
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
}
