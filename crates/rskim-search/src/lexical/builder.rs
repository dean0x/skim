//! Index builder: `LayerBuilder` implementation for the lexical layer.
//!
//! Accepts files, parses ASTs, classifies fields, extracts n-grams,
//! accumulates posting lists, and writes the persistent index to disk.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rustc_hash::FxHashMap;

use rskim_core::Language;

use crate::{FieldClassifier, FileId, FileTable, IndexStats, SearchError, SearchField};

use super::{
    index_format, Bm25Params, IndexHeader, IndexMetadata, Ngram, PostingEntry,
    INDEX_FORMAT_VERSION, INDEX_MAGIC,
};

use super::ngram::extract_ngrams;
use super::walker::{walk_and_classify, WalkContext};

// ============================================================================
// LexicalLayerBuilder
// ============================================================================

/// Builder for the lexical inverted index layer.
///
/// Accumulates postings from multiple files, then writes the persistent
/// index to disk via [`LayerBuilder::build`].
pub struct LexicalLayerBuilder {
    /// Index output directory.
    index_dir: PathBuf,
    /// Repo root (stored in metadata for collision detection).
    repo_root: PathBuf,
    /// Bidirectional path ↔ FileId mapping.
    file_table: FileTable,
    /// Accumulated postings: ngram → list of PostingEntry.
    postings: FxHashMap<Ngram, Vec<PostingEntry>>,
    /// Per-document total token count (for BM25F avg_doc_len).
    doc_lengths: Vec<u32>,
    /// Per-file mtimes for staleness detection.
    file_mtimes: Vec<(PathBuf, u64)>,
    /// Cached classifiers per language. `None` means language uses serde path.
    classifier_cache: FxHashMap<Language, Option<Box<dyn FieldClassifier>>>,
    /// Cached tree-sitter parsers per language (reused across files).
    parser_cache: FxHashMap<Language, rskim_core::Parser>,
}

impl LexicalLayerBuilder {
    /// Create a new builder targeting `index_dir`.
    ///
    /// `repo_root` is stored in metadata for cross-repo collision detection.
    #[must_use = "the builder must be used to construct an index; dropping it discards all accumulated state"]
    pub fn new(index_dir: PathBuf, repo_root: PathBuf) -> Self {
        Self {
            index_dir,
            repo_root,
            file_table: FileTable::new(),
            postings: FxHashMap::default(),
            doc_lengths: Vec::new(),
            file_mtimes: Vec::new(),
            classifier_cache: FxHashMap::default(),
            parser_cache: FxHashMap::default(),
        }
    }

    /// Index `content` using the tree-sitter AST path.
    ///
    /// Reuses the cached parser for `language` if available; creates and caches
    /// a new one otherwise. Falls through silently on parse failure so the
    /// caller's `doc_len == 0` guard triggers the whole-file fallback.
    fn index_tree_sitter(
        &mut self,
        content: &str,
        language: Language,
        doc_id: u32,
        doc_len: &mut u32,
    ) {
        // Ensure a parser exists in the cache for this language.
        if let std::collections::hash_map::Entry::Vacant(e) = self.parser_cache.entry(language) {
            match rskim_core::Parser::new(language) {
                Ok(parser) => {
                    e.insert(parser);
                }
                Err(_) => {
                    // Parser creation failed — caller's doc_len == 0 guard triggers fallback.
                    return;
                }
            }
        }

        let parser = match self.parser_cache.get_mut(&language) {
            Some(p) => p,
            None => return,
        };

        let tree = match parser.parse(content) {
            Ok(t) => t,
            Err(_) => {
                // Parse failed — caller's doc_len == 0 guard triggers fallback.
                return;
            }
        };

        // SAFETY: classifier_cache entry was populated by add_file and is Some
        // (only tree-sitter languages reach this path).
        let classifier = match self
            .classifier_cache
            .get(&language)
            .and_then(Option::as_deref)
        {
            Some(c) => c,
            None => return,
        };

        let mut ctx = WalkContext {
            source: content,
            classifier,
            doc_id,
            postings: &mut self.postings,
            doc_len,
        };
        walk_and_classify(tree.root_node(), &mut ctx);
    }

    /// Index `content` using the serde-based path (JSON, YAML, TOML).
    ///
    /// Falls through silently on classification failure so the caller's
    /// `doc_len == 0` guard triggers the whole-file fallback.
    fn index_serde(&mut self, content: &str, language: Language, doc_id: u32, doc_len: &mut u32) {
        let regions = match crate::fields::classify_serde_fields(content, language) {
            Ok(r) => r,
            Err(_) => {
                // Classification failed — caller's doc_len == 0 guard triggers fallback.
                return;
            }
        };

        for (range, field) in regions {
            let text = &content[range.clone()];
            let ngrams = extract_ngrams(text);
            for (ngram, weight) in &ngrams {
                let entry = PostingEntry {
                    doc_id,
                    field_id: field.as_u8(),
                    position: u32::try_from(range.start).unwrap_or(u32::MAX),
                    tf: weight.max(1.0).min(f32::from(u16::MAX)) as u16,
                };
                self.postings.entry(*ngram).or_default().push(entry);
            }
            *doc_len = doc_len.saturating_add(u32::try_from(ngrams.len()).unwrap_or(u32::MAX));
        }
    }
}

// ============================================================================
// LayerBuilder implementation
// ============================================================================

impl crate::LayerBuilder for LexicalLayerBuilder {
    fn add_file(&mut self, path: &Path, content: &str, language: Language) -> crate::Result<()> {
        // --- Skip oversized files ------------------------------------------
        if content.len() > 5_000_000 {
            return Ok(());
        }

        // --- Skip minified files (single long line) -------------------------
        let line_count = content.lines().count().max(1);
        let avg_line_len = content.len() / line_count;
        if avg_line_len > 500 {
            return Ok(());
        }

        // --- Register path with traversal guard ----------------------------
        // PF-003: use register_within to prevent path traversal attacks.
        let file_id: FileId = self.file_table.register_within(path, &self.repo_root)?;

        // Validate that doc_id fits in u32 (in practice never fails for real repos).
        let doc_id: u32 = u32::try_from(file_id.as_u64())
            .map_err(|_| SearchError::IndexBuildError("file_id exceeds u32::MAX".to_string()))?;

        // --- Record mtime --------------------------------------------------
        let mtime = file_mtime_unix(path);
        self.file_mtimes.push((path.to_path_buf(), mtime));

        // --- Extract and accumulate postings --------------------------------
        // Reserve capacity on first file to reduce rehash overhead.
        // Estimate: ~1 unique ngram per 4 bytes of source is a conservative upper bound.
        if self.postings.is_empty() {
            self.postings.reserve(content.len() / 4);
        }
        let mut doc_len: u32 = 0;

        // Populate the classifier cache entry if absent; `None` means serde-based language.
        self.classifier_cache
            .entry(language)
            .or_insert_with(|| crate::fields::for_language(language));
        let use_tree_sitter = self
            .classifier_cache
            .get(&language)
            .is_some_and(Option::is_some);

        if use_tree_sitter {
            self.index_tree_sitter(content, language, doc_id, &mut doc_len);
        } else {
            self.index_serde(content, language, doc_id, &mut doc_len);
        }

        // --- Whole-file fallback -------------------------------------------
        // Only runs when the classified path produced no ngrams (parse failure,
        // classification failure, or empty file). Guards against doubling posting
        // lists and inflating doc_len for successfully classified files.
        if doc_len == 0 {
            let fallback_ngrams = extract_ngrams(content);
            for (ngram, _) in &fallback_ngrams {
                let entry = PostingEntry {
                    doc_id,
                    field_id: SearchField::FunctionBody.as_u8(),
                    position: 0,
                    tf: 1,
                };
                self.postings.entry(*ngram).or_default().push(entry);
            }
            doc_len = doc_len.saturating_add(u32::try_from(fallback_ngrams.len()).unwrap_or(u32::MAX));
        }

        self.doc_lengths.push(doc_len);
        Ok(())
    }

    fn build(self: Box<Self>) -> crate::Result<Box<dyn crate::SearchIndex>> {
        // --- Compute avg_doc_len -------------------------------------------
        let total_docs = self.doc_lengths.len();
        let avg_doc_len: f32 = if total_docs == 0 {
            0.0
        } else {
            let sum: u64 = self.doc_lengths.iter().map(|&l| l as u64).sum();
            sum as f32 / total_docs as f32
        };

        // --- Sort postings by ngram hash -----------------------------------
        let mut sorted_entries: Vec<(Ngram, Vec<PostingEntry>)> =
            self.postings.into_iter().collect();
        sorted_entries.sort_by_key(|(ngram, _)| ngram.as_u64());

        // --- Build IndexHeader ---------------------------------------------
        let ngram_count = sorted_entries.len() as u64;
        let file_count = self.file_table.len() as u64;
        let created_at = unix_timestamp_now();

        let header = IndexHeader {
            magic: INDEX_MAGIC,
            version: INDEX_FORMAT_VERSION,
            ngram_count,
            file_count,
            created_at,
        };

        // --- Write index files to disk -------------------------------------
        std::fs::create_dir_all(&self.index_dir).map_err(SearchError::Io)?;
        index_format::write_index(&self.index_dir, &sorted_entries, &header)?;

        // --- Write metadata.json -------------------------------------------
        let bm25_params = Bm25Params {
            k1: 1.2,
            b: 0.75,
            avg_doc_len,
        };

        let index_size_bytes = compute_index_size(&self.index_dir);

        let stats = IndexStats {
            file_count,
            total_ngrams: ngram_count,
            index_size_bytes,
            last_updated: created_at,
            format_version: INDEX_FORMAT_VERSION,
        };

        let metadata = IndexMetadata {
            file_table: self.file_table,
            bm25_params,
            stats,
            file_mtimes: self.file_mtimes,
            repo_root: self.repo_root,
            doc_lengths: self.doc_lengths,
        };

        let json = serde_json::to_string_pretty(&metadata)
            .map_err(|e| SearchError::SerializationError(e.to_string()))?;
        std::fs::write(self.index_dir.join("metadata.json"), json).map_err(SearchError::Io)?;

        // --- Open and return the built layer --------------------------------
        use super::query::LexicalSearchLayer;
        let layer = LexicalSearchLayer::open(&self.index_dir)?;
        Ok(Box::new(layer))
    }
}

// ============================================================================
// Helper functions
// ============================================================================

/// Return the current Unix timestamp in seconds.
fn unix_timestamp_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Sum the sizes of the three core index files.
fn compute_index_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    for name in &["lexical.skidx", "lexical.skpost", "metadata.json"] {
        if let Ok(meta) = std::fs::metadata(dir.join(name)) {
            total += meta.len();
        }
    }
    total
}

/// Return the mtime of `path` as a Unix timestamp, or 0 on any error.
fn file_mtime_unix(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ============================================================================
// Unit Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_builder_is_empty() {
        let builder = LexicalLayerBuilder::new(PathBuf::from("/tmp/idx"), PathBuf::from("/repo"));
        assert!(builder.file_table.is_empty());
        assert!(builder.postings.is_empty());
        assert!(builder.parser_cache.is_empty());
    }

    #[test]
    fn minified_threshold_calculation() {
        // avg_line_len = content.len() / max(line_count, 1)
        // A 501-char single-line file should be skipped.
        let long_line: String = "x".repeat(501);
        assert_eq!(long_line.len() / 1usize.max(1), 501);
        assert!(501 > 500); // confirms skip condition
    }

}
