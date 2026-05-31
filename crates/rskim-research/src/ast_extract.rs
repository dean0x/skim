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
const MAX_AST_NODES: u32 = 100_000;

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
    let mut cursor = tree.walk();
    let mut ctx = WalkContext {
        vocab,
        bigrams: &mut result.bigrams,
        trigrams: &mut result.trigrams,
        collect_trigrams,
        error_count: &mut result.error_node_count,
        node_count: &mut result.node_count,
    };

    walk_tree(&mut cursor, &mut ctx);

    Ok(result)
}

/// Mutable traversal state threaded through the iterative `walk_tree` calls.
///
/// Bundles the vocabulary, output sets, counters, and configuration so that
/// `walk_tree` takes only two parameters instead of ten.
struct WalkContext<'a> {
    /// Shared vocabulary mapping node-kind strings to compact IDs.
    vocab: &'a mut NodeKindVocabulary,
    /// Unique parent→child bigrams collected so far for this file.
    bigrams: &'a mut HashSet<AstBigram>,
    /// Unique grandparent→parent→child trigrams collected so far for this file.
    trigrams: &'a mut HashSet<AstTrigram>,
    /// Whether to collect trigrams at all.
    collect_trigrams: bool,
    /// Running count of ERROR nodes (parse failures).
    error_count: &'a mut u32,
    /// Running count of all AST nodes visited.
    node_count: &'a mut u32,
}

/// Iterative pre-order tree walk using `TreeCursor` with bounded depth and
/// node-count guards.
///
/// Replaces the previous recursive implementation to eliminate call-stack
/// growth on pathological inputs. Uses a manual stack of
/// `(depth, parent_id, grandparent_id)` entries — one per level — to track
/// the same ancestor context that the recursive version carried in activation
/// frames.
///
/// `MAX_AST_DEPTH` (500) caps how deep we descend; `MAX_AST_NODES` (100 K)
/// caps total nodes visited per file.
///
/// ERROR nodes are counted but not included in bigram/trigram pairs (they
/// represent parse failures, not real grammar relationships). Children of
/// ERROR nodes are still visited so we count all error nodes in the subtree.
///
/// The trigram emission guard uses two nested `if` blocks intentionally: the
/// outer guard on `collect_trigrams` and the trigram-cap avoids constructing
/// the Option tuple when unnecessary; `clippy::collapsible_if` would merge
/// them into an if-let chain that still allocates the tuple unconditionally.
fn walk_tree(cursor: &mut tree_sitter::TreeCursor, ctx: &mut WalkContext<'_>) {
    // Stack of (depth, parent_id, grandparent_id) for the current cursor
    // position.  Each entry is pushed when we descend into a child level and
    // popped when we return to the parent.
    let mut level_stack: Vec<(usize, Option<NodeKindId>, Option<NodeKindId>)> = Vec::new();

    // Start at depth 0 with no parent or grandparent.
    let mut depth: usize = 0;
    let mut parent_id: Option<NodeKindId> = None;
    let mut grandparent_id: Option<NodeKindId> = None;

    loop {
        // ── Guard: depth and node-count caps ──────────────────────────────
        if depth >= MAX_AST_DEPTH || *ctx.node_count >= MAX_AST_NODES {
            // Skip this node and its subtree.  Move to the next sibling or
            // ascend until we find one.
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if level_stack.is_empty() {
                    return;
                }
                cursor.goto_parent();
                if let Some((d, p, g)) = level_stack.pop() {
                    depth = d;
                    parent_id = p;
                    grandparent_id = g;
                }
            }
            continue;
        }

        // ── Process current node ───────────────────────────────────────────
        let node = cursor.node();
        *ctx.node_count += 1;

        let kind = node.kind();
        let is_error = node.is_error() || kind == "ERROR";

        if is_error {
            *ctx.error_count += 1;
            // Do not create bigrams/trigrams for ERROR nodes — they are not
            // real grammar relationships.  Continue walking children so we
            // count all error nodes in the subtree.
        }

        // Get (or assign) ID for the current node kind.
        let current_id = if is_error {
            None
        } else {
            Some(ctx.vocab.get_or_insert(kind))
        };

        // Emit bigram: parent → current.
        if let (Some(pid), Some(cid)) = (parent_id, current_id) {
            ctx.bigrams.insert(encode_ast_bigram(pid, cid));
        }

        // Emit trigram: grandparent → parent → current.
        // The two-level if is intentional: the outer guard avoids tuple
        // construction overhead when the cap or flag is not set;
        // clippy::collapsible_if would merge them into an if-let chain that
        // still allocates the Option tuple unconditionally.
        #[allow(clippy::collapsible_if)]
        if ctx.collect_trigrams && ctx.trigrams.len() < MAX_TRIGRAMS_PER_FILE {
            if let (Some(gid), Some(pid), Some(cid)) = (grandparent_id, parent_id, current_id) {
                ctx.trigrams.insert(encode_ast_trigram(gid, pid, cid));
            }
        }

        // ── Advance cursor ─────────────────────────────────────────────────
        if cursor.goto_first_child() {
            // Descend: push current level context and move one level deeper.
            level_stack.push((depth, parent_id, grandparent_id));
            grandparent_id = parent_id;
            parent_id = current_id;
            depth += 1;
        } else {
            // No children — try next sibling at the same level, or ascend.
            loop {
                if cursor.goto_next_sibling() {
                    // Stay at current depth/parent/grandparent context.
                    break;
                }
                if level_stack.is_empty() {
                    return;
                }
                cursor.goto_parent();
                if let Some((d, p, g)) = level_stack.pop() {
                    depth = d;
                    parent_id = p;
                    grandparent_id = g;
                }
            }
        }
    }
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
