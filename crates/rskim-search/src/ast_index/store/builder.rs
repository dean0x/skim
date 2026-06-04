//! [`AstIndexBuilder`] — constructs the two-file mmap'd AST structural n-gram index.
//!
//! # Atomicity contract
//!
//! `ast_index.skpost` is written first, then `ast_index.skidx` (commit point).
//! A reader that finds `.skidx` present can assume `.skpost` is coherent.
//! A partial write (e.g. power loss between the two) leaves no `.skidx`, so
//! the next `open` attempt fails cleanly with a "file not found" error rather
//! than producing a corrupt read.
//!
//! # Re-index concurrency safety
//!
//! The two-file rename pair is NOT atomic together (same posture as the lexical
//! index).  Callers MUST serialize re-index operations against concurrent reads.
//! There is no generation marker; the reserved header bytes stay zero.
//!
//! # FileId contract (PRECONDITION — enforced by this builder)
//!
//! FileIds MUST be dense and sequential starting from 0.  Every file — even
//! files that yield zero n-grams (non-tree-sitter languages, files >100 KiB,
//! empty content) — MUST receive exactly one `add_file_ngrams` call and
//! advance `file_count` by 1.  The shared file manifest is owned by the
//! CLI / Wave 4 layer, not this library.  This builder enforces the invariant
//! via `InvalidQuery` errors for duplicates and non-sequential IDs.
//!
//! # Parallel build
//!
//! [`AstIndexBuilder::build_from_files`] parallelises the pure
//! `linearize_source` + `extract_ast_ngrams` step across files using rayon,
//! then merges postings sequentially via `add_file_ngrams`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use tempfile::NamedTempFile;

use super::format::{
    AST_BIGRAM_ENTRY_SIZE, AST_FILE_META_SIZE, AST_FORMAT_VERSION, AST_HEADER_SIZE,
    AST_POSTING_ENTRY_SIZE, AST_SKIDX_MAGIC, AST_TRIGRAM_ENTRY_SIZE, AstBigramEntry,
    AstFileMetaEntry, AstPostingEntry, AstSkidxHeader, AstTrigramEntry, encode_bigram_entry,
    encode_file_meta, encode_header, encode_posting, encode_trigram_entry, lang_to_id,
};
use super::reader::AstIndexReader;
use crate::{
    FileId, Result, SearchError,
    ast_index::{AstNgramSet, extract_ast_ngrams, linearize_source},
};

// ============================================================================
// Public builder struct
// ============================================================================

/// Constructs the two-file mmap'd AST structural n-gram index.
///
/// Call [`AstIndexBuilder::add_file_ngrams`] for each file (passing a
/// pre-extracted [`AstNgramSet`]), or use the higher-level
/// [`AstIndexBuilder::add_file`] convenience method.  Finish by calling
/// [`AstIndexBuilder::build`] to flush the index and obtain a queryable
/// [`AstIndexReader`].
///
/// For bulk builds over many files prefer
/// [`AstIndexBuilder::build_from_files`], which parallelises extraction with
/// rayon and then merges sequentially.
#[derive(Debug)]
pub struct AstIndexBuilder {
    /// Directory where `ast_index.skidx` and `ast_index.skpost` will be written.
    output_dir: PathBuf,
    /// Accumulated postings: bigram key → list of (doc_id, count).
    bigram_postings: HashMap<u32, Vec<AstPostingEntry>>,
    /// Accumulated postings: trigram key → list of (doc_id, count).
    trigram_postings: HashMap<u64, Vec<AstPostingEntry>>,
    /// Per-file metadata in insertion order.
    file_meta: Vec<AstFileMetaEntry>,
    /// Guard against duplicate FileIds.
    seen_file_ids: HashSet<u32>,
    /// Number of files added.
    file_count: u32,
    /// Sum of all node counts (for avg_node_count).
    total_node_count: u64,
    /// Sum of distinct bigram counts per file (for avg_bigram_count).
    total_distinct_bigrams: u64,
    /// Sum of distinct trigram counts per file (for avg_trigram_count).
    total_distinct_trigrams: u64,
}

impl AstIndexBuilder {
    /// Create a new builder that will write index files to `output_dir`.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] if `output_dir` does not exist.
    pub fn new(output_dir: PathBuf) -> Result<Self> {
        if !output_dir.exists() {
            return Err(SearchError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("output_dir does not exist: {}", output_dir.display()),
            )));
        }
        Ok(Self {
            output_dir,
            bigram_postings: HashMap::new(),
            trigram_postings: HashMap::new(),
            file_meta: Vec::new(),
            seen_file_ids: HashSet::new(),
            file_count: 0,
            total_node_count: 0,
            total_distinct_bigrams: 0,
            total_distinct_trigrams: 0,
        })
    }

    // -----------------------------------------------------------------------
    // Core merge primitive
    // -----------------------------------------------------------------------

    /// Record the n-grams for one file.
    ///
    /// This is the core merge primitive.  Every file — even files that yield
    /// zero n-grams — MUST produce exactly one `add_file_ngrams` call so that
    /// [`AstFileMetaEntry`] records are contiguous and the `file_count` matches
    /// the caller's file manifest.
    ///
    /// # Arguments
    ///
    /// - `id` — the [`FileId`] for this file (must be the next sequential value).
    /// - `lang` — language used for the `lang_id` byte in [`AstFileMetaEntry`].
    /// - `set` — the extracted [`AstNgramSet`] (may be empty).
    /// - `node_count` — emitted-node count from `linearize_source` (`lin.nodes.len()`).
    ///
    /// # Errors
    ///
    /// - [`SearchError::InvalidQuery`] if `id` duplicates a previously added
    ///   FileId (message contains "duplicate").
    /// - [`SearchError::InvalidQuery`] if `id.0 != file_count` (non-sequential;
    ///   message contains "sequential").
    /// - [`SearchError::IndexCorrupted`] if `file_count` would overflow `u32`.
    pub fn add_file_ngrams(
        &mut self,
        id: FileId,
        lang: rskim_core::Language,
        set: &AstNgramSet,
        node_count: u32,
    ) -> Result<()> {
        // ── FileId guards (mirrors lexical builder, ADR-001) ────────────────
        if self.seen_file_ids.contains(&id.0) {
            return Err(SearchError::InvalidQuery(format!(
                "duplicate FileId: {}",
                id.0
            )));
        }
        if id.0 != self.file_count {
            return Err(SearchError::InvalidQuery(format!(
                "FileId must equal sequential insertion index: expected {}, got {}",
                self.file_count, id.0
            )));
        }

        self.seen_file_ids.insert(id.0);

        // ── Merge bigram postings ────────────────────────────────────────────
        for entry in &set.bigrams {
            self.bigram_postings
                .entry(entry.ngram.key())
                .or_default()
                .push(AstPostingEntry {
                    doc_id: id.0,
                    count: entry.count,
                });
        }

        // ── Merge trigram postings ───────────────────────────────────────────
        for entry in &set.trigrams {
            self.trigram_postings
                .entry(entry.ngram.key())
                .or_default()
                .push(AstPostingEntry {
                    doc_id: id.0,
                    count: entry.count,
                });
        }

        // ── FileMetaEntry — ALWAYS one per file ─────────────────────────────
        self.file_meta.push(AstFileMetaEntry {
            lang_id: lang_to_id(lang),
            node_count,
        });

        // ── Accumulate totals ────────────────────────────────────────────────
        self.total_node_count += u64::from(node_count);
        self.total_distinct_bigrams += set.bigrams.len() as u64;
        self.total_distinct_trigrams += set.trigrams.len() as u64;

        self.file_count = self.file_count.checked_add(1).ok_or_else(|| {
            SearchError::IndexCorrupted("file_count overflow: too many files".into())
        })?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Convenience wrapper
    // -----------------------------------------------------------------------

    /// Linearize and extract n-grams for one file, then call
    /// [`add_file_ngrams`][Self::add_file_ngrams].
    ///
    /// `linearize_source` errors (grammar load failures) propagate.  Empty
    /// results (non-tree-sitter languages, files >100 KiB, empty content) are
    /// normal and still produce one [`AstFileMetaEntry`] with `node_count == 0`.
    ///
    /// # Errors
    ///
    /// Propagates [`SearchError`] from `linearize_source` or `add_file_ngrams`.
    pub fn add_file(
        &mut self,
        id: FileId,
        content: &str,
        lang: rskim_core::Language,
    ) -> Result<()> {
        let lin = linearize_source(content, lang)?;
        let node_count = lin.nodes.len() as u32;
        let set = extract_ast_ngrams(&lin.nodes, lang);
        self.add_file_ngrams(id, lang, &set, node_count)
    }

    // -----------------------------------------------------------------------
    // Parallel bulk build
    // -----------------------------------------------------------------------

    /// Build an [`AstIndexReader`] from a slice of `(FileId, content, Language)`
    /// tuples using rayon parallelism.
    ///
    /// The pure `linearize_source` + `extract_ast_ngrams` step is parallelised
    /// across files.  The merge into the builder is sequential (required for the
    /// sequential-FileId invariant).  This meets the <10 s target for 1,000 files.
    ///
    /// Files are processed in the order they appear in `files`, so FileIds MUST
    /// be 0-based and contiguous in the slice.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError`] from extraction, merging, or I/O.
    pub fn build_from_files(
        output_dir: PathBuf,
        files: &[(FileId, &str, rskim_core::Language)],
    ) -> Result<AstIndexReader> {
        // ── Parallel extraction ──────────────────────────────────────────────
        // Collect results into a Vec indexed by position in `files` so that
        // sequential merge preserves FileId order.
        let extracted: Vec<Result<(FileId, rskim_core::Language, AstNgramSet, u32)>> = files
            .par_iter()
            .map(|(id, content, lang)| {
                let lin = linearize_source(content, *lang)?;
                let node_count = lin.nodes.len() as u32;
                let set = extract_ast_ngrams(&lin.nodes, *lang);
                Ok((*id, *lang, set, node_count))
            })
            .collect();

        // ── Sequential merge ─────────────────────────────────────────────────
        let mut builder = AstIndexBuilder::new(output_dir)?;
        for result in extracted {
            let (id, lang, set, node_count) = result?;
            builder.add_file_ngrams(id, lang, &set, node_count)?;
        }

        builder.build()
    }

    // -----------------------------------------------------------------------
    // Serialization + atomic write
    // -----------------------------------------------------------------------

    /// Finalise the builder: serialise the index to disk and return a reader.
    ///
    /// Write order: `ast_index.skpost` first, then `ast_index.skidx` (commit
    /// point).  Both files are written atomically via `NamedTempFile`.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] if writing fails, or
    /// [`SearchError::IndexCorrupted`] on overflow or if the reader rejects the
    /// result.
    pub fn build(mut self) -> Result<AstIndexReader> {
        // ── Corpus averages ──────────────────────────────────────────────────
        let (avg_bigram_count, avg_trigram_count, avg_node_count) = if self.file_count == 0 {
            (0.0f32, 0.0f32, 0.0f32)
        } else {
            let n = f64::from(self.file_count);
            (
                (self.total_distinct_bigrams as f64 / n) as f32,
                (self.total_distinct_trigrams as f64 / n) as f32,
                (self.total_node_count as f64 / n) as f32,
            )
        };

        // ── Sort posting lists by doc_id ─────────────────────────────────────
        // One posting per FileId per key (builder invariant: no count-merge).
        for list in self.bigram_postings.values_mut() {
            list.sort_unstable_by_key(|p| p.doc_id);
        }
        for list in self.trigram_postings.values_mut() {
            list.sort_unstable_by_key(|p| p.doc_id);
        }

        // ── Sort keys ascending ──────────────────────────────────────────────
        let mut bigram_keys: Vec<u32> = self.bigram_postings.keys().copied().collect();
        bigram_keys.sort_unstable();
        let mut trigram_keys: Vec<u64> = self.trigram_postings.keys().copied().collect();
        trigram_keys.sort_unstable();

        // ── Serialise ────────────────────────────────────────────────────────
        let (postings_buf, skidx_buf) = self.serialize_index(
            &bigram_keys,
            &trigram_keys,
            avg_bigram_count,
            avg_trigram_count,
            avg_node_count,
        )?;

        let post_path = self.output_dir.join("ast_index.skpost");
        let idx_path = self.output_dir.join("ast_index.skidx");

        // Atomic writes: .skpost first, .skidx second (commit point).
        Self::atomic_write(&self.output_dir, &post_path, &postings_buf)?;
        Self::atomic_write(&self.output_dir, &idx_path, &skidx_buf)?;

        AstIndexReader::open(&self.output_dir)
    }

    /// Serialise postings, entry tables, file metadata, and header into the two
    /// on-disk byte buffers: `(postings_buf, skidx_buf)`.
    fn serialize_index(
        &self,
        bigram_keys: &[u32],
        trigram_keys: &[u64],
        avg_bigram_count: f32,
        avg_trigram_count: f32,
        avg_node_count: f32,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        // ── Postings buffer ──────────────────────────────────────────────────
        let total_posting_bytes_est: usize = (self
            .bigram_postings
            .values()
            .map(|v| v.len())
            .sum::<usize>()
            + self
                .trigram_postings
                .values()
                .map(|v| v.len())
                .sum::<usize>())
            * AST_POSTING_ENTRY_SIZE;
        let mut postings_buf = Vec::with_capacity(total_posting_bytes_est);

        // ── Bigram entries ───────────────────────────────────────────────────
        let mut bigram_entries: Vec<AstBigramEntry> = Vec::with_capacity(bigram_keys.len());
        for &key in bigram_keys {
            let list = &self.bigram_postings[&key];
            let offset = postings_buf.len() as u64;
            let byte_len = list
                .len()
                .checked_mul(AST_POSTING_ENTRY_SIZE)
                .ok_or_else(|| {
                    SearchError::IndexCorrupted(format!(
                        "bigram posting list for key {key:#010x} overflows usize"
                    ))
                })?;
            let length = u32::try_from(byte_len).map_err(|_| {
                SearchError::IndexCorrupted(format!(
                    "bigram posting list for key {key:#010x} exceeds u32::MAX ({byte_len} bytes)"
                ))
            })?;
            for p in list {
                postings_buf.extend_from_slice(&encode_posting(p));
            }
            bigram_entries.push(AstBigramEntry {
                key,
                posting_offset: offset,
                posting_length: length,
            });
        }

        // ── Trigram entries ──────────────────────────────────────────────────
        let mut trigram_entries: Vec<AstTrigramEntry> = Vec::with_capacity(trigram_keys.len());
        for &key in trigram_keys {
            let list = &self.trigram_postings[&key];
            let offset = postings_buf.len() as u64;
            let byte_len = list
                .len()
                .checked_mul(AST_POSTING_ENTRY_SIZE)
                .ok_or_else(|| {
                    SearchError::IndexCorrupted(format!(
                        "trigram posting list for key {key:#018x} overflows usize"
                    ))
                })?;
            let length = u32::try_from(byte_len).map_err(|_| {
                SearchError::IndexCorrupted(format!(
                    "trigram posting list for key {key:#018x} exceeds u32::MAX ({byte_len} bytes)"
                ))
            })?;
            for p in list {
                postings_buf.extend_from_slice(&encode_posting(p));
            }
            trigram_entries.push(AstTrigramEntry {
                key,
                posting_offset: offset,
                posting_length: length,
            });
        }

        // ── File meta buffer ─────────────────────────────────────────────────
        let mut meta_buf: Vec<u8> = Vec::with_capacity(self.file_meta.len() * AST_FILE_META_SIZE);
        for m in &self.file_meta {
            meta_buf.extend_from_slice(&encode_file_meta(m));
        }

        // ── Bigram entry buffer ──────────────────────────────────────────────
        let mut bigram_entries_buf: Vec<u8> =
            Vec::with_capacity(bigram_entries.len() * AST_BIGRAM_ENTRY_SIZE);
        for e in &bigram_entries {
            bigram_entries_buf.extend_from_slice(&encode_bigram_entry(e));
        }

        // ── Trigram entry buffer ─────────────────────────────────────────────
        let mut trigram_entries_buf: Vec<u8> =
            Vec::with_capacity(trigram_entries.len() * AST_TRIGRAM_ENTRY_SIZE);
        for e in &trigram_entries {
            trigram_entries_buf.extend_from_slice(&encode_trigram_entry(e));
        }

        // Debug assertion: bigram offsets are monotonic and contiguous.
        #[cfg(debug_assertions)]
        {
            let mut expected_offset = 0u64;
            for e in &bigram_entries {
                debug_assert_eq!(
                    e.posting_offset, expected_offset,
                    "bigram entry offsets must be contiguous"
                );
                expected_offset += u64::from(e.posting_length);
            }
            // trigram entries continue immediately after bigram postings
            for e in &trigram_entries {
                debug_assert_eq!(
                    e.posting_offset, expected_offset,
                    "trigram entry offsets must be contiguous (continue after bigrams)"
                );
                expected_offset += u64::from(e.posting_length);
            }
        }

        // ── CRC32 over the post-header payload ───────────────────────────────
        // Serialization order: bigram entries + trigram entries + file meta.
        // Must match the reader's slice: idx_mmap[AST_HEADER_SIZE..expected_idx].
        let mut crc_hasher = crc32fast::Hasher::new();
        crc_hasher.update(&bigram_entries_buf);
        crc_hasher.update(&trigram_entries_buf);
        crc_hasher.update(&meta_buf);
        let checksum = crc_hasher.finalize();

        // ── Overflow-checked counts ──────────────────────────────────────────
        let bigram_count = u32::try_from(bigram_entries.len()).map_err(|_| {
            SearchError::IndexCorrupted(format!(
                "bigram_count {} exceeds u32::MAX",
                bigram_entries.len()
            ))
        })?;
        let trigram_count = u32::try_from(trigram_entries.len()).map_err(|_| {
            SearchError::IndexCorrupted(format!(
                "trigram_count {} exceeds u32::MAX",
                trigram_entries.len()
            ))
        })?;

        // ── Header ───────────────────────────────────────────────────────────
        let header = AstSkidxHeader {
            magic: *AST_SKIDX_MAGIC,
            version: AST_FORMAT_VERSION,
            bigram_count,
            trigram_count,
            file_count: self.file_count,
            postings_file_size: postings_buf.len() as u64,
            avg_bigram_count,
            avg_trigram_count,
            avg_node_count,
            checksum,
        };

        // ── Assemble .skidx: header + bigram entries + trigram entries + file_meta
        let mut skidx_buf = Vec::with_capacity(
            AST_HEADER_SIZE + bigram_entries_buf.len() + trigram_entries_buf.len() + meta_buf.len(),
        );
        skidx_buf.extend_from_slice(&encode_header(&header));
        skidx_buf.extend_from_slice(&bigram_entries_buf);
        skidx_buf.extend_from_slice(&trigram_entries_buf);
        skidx_buf.extend_from_slice(&meta_buf);

        Ok((postings_buf, skidx_buf))
    }

    /// Atomically write `data` to `path` using a temp file in `dir`.
    ///
    /// Mirrors the cochange atomic_write: `NamedTempFile::new_in` + `write_all`
    /// + `sync_all` (crash safety) + `persist` (rename, atomic on POSIX).
    fn atomic_write(dir: &Path, path: &Path, data: &[u8]) -> Result<()> {
        let mut tmp = NamedTempFile::new_in(dir)?;
        use std::io::Write as _;
        tmp.write_all(data)?;
        tmp.as_file().sync_all()?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o644))?;
        }

        tmp.persist(path).map_err(|e| e.error)?;
        Ok(())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "builder_tests.rs"]
mod tests;
