//! [`AstIndexBuilder`] — constructs the two-file mmap'd AST structural n-gram index.
//!
//! # Atomicity contract
//!
//! `ast_index.skpost` is written first, then `ast_index.skidx` (commit point).
//! A reader that finds `.skidx` present can assume `.skpost` is coherent.
//! A partial write (e.g. process crash between the two) leaves no `.skidx`, so
//! the next `open` attempt fails cleanly with a "file not found" error rather
//! than producing a corrupt read.
//!
//! Rename durability depends on the filesystem.  Without a directory fsync after
//! `persist`, a power loss may leave the rename un-flushed on some filesystems
//! (e.g. ext4 without `data=journal`).  A directory fsync is not performed here;
//! this matches the posture of the cochange sibling.  Tracked as a follow-up.
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
use std::path::PathBuf;

use rayon::prelude::*;

use super::format::{
    AstBigramTableEntry, AstFileMetaEntry, AstPostingEntry, AstSkidxHeader, AstTrigramTableEntry,
    FILE_META_SIZE, FORMAT_VERSION, HEADER_SIZE, POSTING_ENTRY_SIZE, SKAX_MAGIC,
    encode_bigram_entry, encode_file_meta, encode_header, encode_posting, encode_trigram_entry,
    lang_to_id,
};
use super::reader::AstIndexReader;
use crate::{
    FileId, Result, SearchError,
    ast_index::{
        AstNgramSet, StructuralMetrics, extract_ast_ngrams_with_metrics, linearize_source,
    },
    io_util::atomic_write,
};

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
    /// Sum of `max_depth` values across all files (for avg_max_depth).
    total_max_depth: u64,
}

// ============================================================================
// Private helpers
// ============================================================================

/// Validate that a single n-gram entry has `count >= 1`.
///
/// Returns [`SearchError::InvalidQuery`] if `count == 0`.  Called once per
/// bigram and once per trigram entry in [`AstIndexBuilder::add_file_ngrams`].
#[inline]
fn check_count_nonzero(
    file_id: u32,
    key: impl std::fmt::LowerHex,
    count: u32,
    kind: &str,
) -> Result<()> {
    if count == 0 {
        return Err(SearchError::InvalidQuery(format!(
            "{kind} entry for FileId {file_id} has count == 0 (key {key:#x}); \
             every AstNgramSet entry must have count >= 1",
        )));
    }
    Ok(())
}

// ============================================================================
// Private serialization helper
// ============================================================================

/// Output of [`serialize_entry_table`]: the typed entry table and its flat
/// encoded byte representation (ready to pass to the CRC hasher and skidx buf).
struct EntryTableResult<E> {
    /// Decoded entry structs (used for debug assertions and count checks).
    table: Vec<E>,
    /// Flat encoded bytes (entry_size × n), CRC-hashable and skidx-appendable.
    encoded: Vec<u8>,
}

/// Shared serialization logic for bigram and trigram entry tables.
///
/// For each key in `sorted_keys`:
/// 1. Records the current `postings_buf` length as `posting_offset`.
/// 2. Serialises each posting entry in the list into `postings_buf`.
/// 3. Computes `posting_length = list.len() × POSTING_ENTRY_SIZE` (checked).
/// 4. Calls `make_entry(key, posting_offset, posting_length)` to build the typed entry.
/// 5. Encodes the entry via `encode_entry` and appends to the entry byte buffer.
///
/// The key type `K` is any key that can be formatted with `{:#x}` and used as a
/// `HashMap` key.  The entry type `E` is the on-disk struct (bigram or trigram).
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if any posting list overflows `usize`
/// or `u32`.
fn serialize_entry_table<K, E, const N: usize>(
    sorted_keys: &[K],
    postings_map: &HashMap<K, Vec<AstPostingEntry>>,
    postings_buf: &mut Vec<u8>,
    make_entry: impl Fn(K, u64, u32) -> E,
    encode_entry: impl Fn(&E) -> [u8; N],
    kind: &'static str,
) -> Result<EntryTableResult<E>>
where
    K: std::hash::Hash + Eq + Copy + std::fmt::LowerHex,
{
    let mut table: Vec<E> = Vec::with_capacity(sorted_keys.len());
    let mut encoded: Vec<u8> = Vec::with_capacity(sorted_keys.len() * N);

    for &key in sorted_keys {
        let list = &postings_map[&key];
        let offset = postings_buf.len() as u64;
        let byte_len = list.len().checked_mul(POSTING_ENTRY_SIZE).ok_or_else(|| {
            SearchError::IndexCorrupted(format!(
                "{kind} posting list for key {key:#x} overflows usize"
            ))
        })?;
        // Applies PF-004 analogue: explicit try_from, no silent as-cast narrowing.
        let length = u32::try_from(byte_len).map_err(|_| {
            SearchError::IndexCorrupted(format!(
                "{kind} posting list for key {key:#x} exceeds u32::MAX ({byte_len} bytes)"
            ))
        })?;
        for p in list {
            postings_buf.extend_from_slice(&encode_posting(p));
        }
        let entry = make_entry(key, offset, length);
        encoded.extend_from_slice(&encode_entry(&entry));
        table.push(entry);
    }

    Ok(EntryTableResult { table, encoded })
}

// ============================================================================
// Public builder struct
// ============================================================================

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
            total_max_depth: 0,
        })
    }

    // -----------------------------------------------------------------------
    // Core merge primitive
    // -----------------------------------------------------------------------

    /// Record the n-grams and structural metrics for one file.
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
    /// - `metrics` — per-file structural complexity metrics from extraction.
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
        metrics: StructuralMetrics,
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
        // Validate count >= 1 at the build boundary (aligns with reader C4 which
        // rejects count == 0 in decode_posting).  A zero-count entry builds fine
        // but makes every lookup fail with IndexCorrupted — a deferred trap that
        // is hard to diagnose.  Catching it here converts the error to an
        // immediate, located build-time failure.
        for entry in &set.bigrams {
            check_count_nonzero(id.0, entry.ngram.key(), entry.count, "bigram")?;
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
            check_count_nonzero(id.0, entry.ngram.key(), entry.count, "trigram")?;
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
            max_depth: metrics.max_depth,
            max_block_stmts: metrics.max_block_stmts,
            max_params: metrics.max_params,
            branch_count: metrics.branch_count,
        });

        // ── Accumulate totals ────────────────────────────────────────────────
        // Use saturating_add for parity with file_count's checked_add discipline
        // (PF-004 analogue: no silent overflow in arithmetic accumulation).
        self.total_node_count = self.total_node_count.saturating_add(u64::from(node_count));
        self.total_distinct_bigrams = self
            .total_distinct_bigrams
            .saturating_add(set.bigrams.len() as u64);
        self.total_distinct_trigrams = self
            .total_distinct_trigrams
            .saturating_add(set.trigrams.len() as u64);
        self.total_max_depth = self
            .total_max_depth
            .saturating_add(u64::from(metrics.max_depth));

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
        let node_count = u32::try_from(lin.nodes.len()).map_err(|_| {
            SearchError::IndexCorrupted(format!(
                "node_count {} exceeds u32::MAX for FileId {}",
                lin.nodes.len(),
                id.0
            ))
        })?;
        let (set, metrics) = extract_ast_ngrams_with_metrics(&lin.nodes, lang);
        self.add_file_ngrams(id, lang, &set, node_count, metrics)
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
    /// # Peak memory
    ///
    /// The rayon stage materialises the entire extracted corpus before sequential
    /// merge — peak memory is O(total distinct n-grams across all files held
    /// transiently).  A chunked-build strategy that bounds peak memory is tracked
    /// in issue #273.
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
        type ExtractedEntry = (
            FileId,
            rskim_core::Language,
            AstNgramSet,
            u32,
            StructuralMetrics,
        );
        let extracted: Vec<Result<ExtractedEntry>> = files
            .par_iter()
            .map(|(id, content, lang)| {
                let lin = linearize_source(content, *lang)?;
                let node_count = u32::try_from(lin.nodes.len()).map_err(|_| {
                    SearchError::IndexCorrupted(format!(
                        "node_count {} exceeds u32::MAX for FileId {}",
                        lin.nodes.len(),
                        id.0
                    ))
                })?;
                let (set, metrics) = extract_ast_ngrams_with_metrics(&lin.nodes, *lang);
                Ok((*id, *lang, set, node_count, metrics))
            })
            .collect();

        // ── Sequential merge ─────────────────────────────────────────────────
        let mut builder = AstIndexBuilder::new(output_dir)?;
        for result in extracted {
            let (id, lang, set, node_count, metrics) = result?;
            builder.add_file_ngrams(id, lang, &set, node_count, metrics)?;
        }

        builder.build()
    }

    // -----------------------------------------------------------------------
    // Serialization + atomic write
    // -----------------------------------------------------------------------

    /// Finalise the builder: serialise the index to disk and return a reader.
    ///
    /// Write order: `ast_index.skpost` first, then `ast_index.skidx` (commit
    /// point).  Both files are written atomically via
    /// [`crate::io_util::atomic_write`].
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::Io`] if writing fails, or
    /// [`SearchError::IndexCorrupted`] on overflow or if the reader rejects the
    /// result.
    pub fn build(self) -> Result<AstIndexReader> {
        // ── Corpus averages ──────────────────────────────────────────────────
        let (avg_bigram_count, avg_trigram_count, avg_node_count, avg_max_depth) =
            if self.file_count == 0 {
                (0.0f32, 0.0f32, 0.0f32, 0.0f32)
            } else {
                let n = f64::from(self.file_count);
                (
                    (self.total_distinct_bigrams as f64 / n) as f32,
                    (self.total_distinct_trigrams as f64 / n) as f32,
                    (self.total_node_count as f64 / n) as f32,
                    (self.total_max_depth as f64 / n) as f32,
                )
            };

        // ── Assert posting lists are already ascending by doc_id ─────────────
        // The sequential-FileId invariant (enforced in add_file_ngrams via the
        // `id.0 == file_count` guard) guarantees that doc_ids are pushed in
        // strictly increasing order per posting list.  No sort is needed.
        // This replaces the previous `sort_unstable_by_key` no-op that scaled
        // O(distinct_keys × files) for large corpora.
        //
        // NOTE: build_from_files parallelises extraction but merges sequentially
        // in FileId order (the rayon par_iter collects into a Vec indexed by
        // position, then iterates in order).  The invariant therefore holds for
        // both add_file_ngrams and build_from_files callers.
        #[cfg(debug_assertions)]
        {
            for list in self.bigram_postings.values() {
                debug_assert!(
                    list.windows(2).all(|w| w[0].doc_id < w[1].doc_id),
                    "bigram posting list must be ascending by doc_id \
                     (sequential FileId invariant violated)"
                );
            }
            for list in self.trigram_postings.values() {
                debug_assert!(
                    list.windows(2).all(|w| w[0].doc_id < w[1].doc_id),
                    "trigram posting list must be ascending by doc_id \
                     (sequential FileId invariant violated)"
                );
            }
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
            avg_max_depth,
        )?;

        let post_path = self.output_dir.join("ast_index.skpost");
        let idx_path = self.output_dir.join("ast_index.skidx");

        // Invalidate any prior validity marker BEFORE writing fresh files
        // (#376, AD-376-4 / AD-376-5).  Defensive unlink so a partial or
        // aborted rebuild cannot leave a stale marker validating wrong bytes.
        crate::validity::unlink_marker_best_effort(&self.output_dir.join("ast_index.skverify"));

        // Atomic writes: .skpost first, .skidx second (commit point).
        atomic_write(&self.output_dir, &post_path, &postings_buf)?;
        atomic_write(&self.output_dir, &idx_path, &skidx_buf)?;

        // Verify-back open re-validates and stamps ast_index.skverify (AC8).
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
        avg_max_depth: f32,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        // ── Postings buffer ──────────────────────────────────────────────────
        // Use saturating_mul for parity with the crate's overflow discipline
        // (PF-004 analogue: no silent overflow in size arithmetic).
        // The result is used only as a Vec::with_capacity hint — an underestimate
        // is harmless (it triggers at most one extra realloc).
        let total_posting_count: usize = self
            .bigram_postings
            .values()
            .map(|v| v.len())
            .fold(0usize, usize::saturating_add)
            .saturating_add(
                self.trigram_postings
                    .values()
                    .map(|v| v.len())
                    .fold(0usize, usize::saturating_add),
            );
        let total_posting_bytes_est = total_posting_count.saturating_mul(POSTING_ENTRY_SIZE);
        let mut postings_buf = Vec::with_capacity(total_posting_bytes_est);

        // ── Bigram entries ───────────────────────────────────────────────────
        // build_entry_table writes each posting into postings_buf and returns the
        // sorted entry table (offset/length pair per key).  Bigram keys are u32;
        // widened to u64 inside the helper for the shared offset representation.
        let bigram_entries = serialize_entry_table(
            bigram_keys,
            &self.bigram_postings,
            &mut postings_buf,
            |key, posting_offset, posting_length| AstBigramTableEntry {
                key,
                posting_offset,
                posting_length,
            },
            encode_bigram_entry,
            "bigram",
        )?;

        // ── Trigram entries ──────────────────────────────────────────────────
        let trigram_entries = serialize_entry_table(
            trigram_keys,
            &self.trigram_postings,
            &mut postings_buf,
            |key, posting_offset, posting_length| AstTrigramTableEntry {
                key,
                posting_offset,
                posting_length,
            },
            encode_trigram_entry,
            "trigram",
        )?;

        // ── File meta buffer ─────────────────────────────────────────────────
        let mut meta_buf: Vec<u8> = Vec::with_capacity(self.file_meta.len() * FILE_META_SIZE);
        for m in &self.file_meta {
            meta_buf.extend_from_slice(&encode_file_meta(m));
        }

        // Debug assertion: bigram offsets are monotonic and contiguous.
        #[cfg(debug_assertions)]
        {
            let mut expected_offset = 0u64;
            for e in &bigram_entries.table {
                debug_assert_eq!(
                    e.posting_offset, expected_offset,
                    "bigram entry offsets must be contiguous"
                );
                expected_offset += u64::from(e.posting_length);
            }
            // trigram entries continue immediately after bigram postings
            for e in &trigram_entries.table {
                debug_assert_eq!(
                    e.posting_offset, expected_offset,
                    "trigram entry offsets must be contiguous (continue after bigrams)"
                );
                expected_offset += u64::from(e.posting_length);
            }
        }

        // ── CRC32 over the post-header payload ───────────────────────────────
        // Serialization order: bigram entries + trigram entries + file meta.
        // Must match the reader's slice: idx_mmap[HEADER_SIZE..expected_idx].
        let mut crc_hasher = crc32fast::Hasher::new();
        crc_hasher.update(&bigram_entries.encoded);
        crc_hasher.update(&trigram_entries.encoded);
        crc_hasher.update(&meta_buf);
        let checksum = crc_hasher.finalize();

        // ── Overflow-checked counts ──────────────────────────────────────────
        let bigram_count = u32::try_from(bigram_entries.table.len()).map_err(|_| {
            SearchError::IndexCorrupted(format!(
                "bigram_count {} exceeds u32::MAX",
                bigram_entries.table.len()
            ))
        })?;
        let trigram_count = u32::try_from(trigram_entries.table.len()).map_err(|_| {
            SearchError::IndexCorrupted(format!(
                "trigram_count {} exceeds u32::MAX",
                trigram_entries.table.len()
            ))
        })?;

        // ── Header ───────────────────────────────────────────────────────────
        let header = AstSkidxHeader {
            magic: *SKAX_MAGIC,
            version: FORMAT_VERSION,
            bigram_count,
            trigram_count,
            file_count: self.file_count,
            postings_file_size: postings_buf.len() as u64,
            avg_bigram_count,
            avg_trigram_count,
            avg_node_count,
            avg_max_depth,
            checksum,
        };

        // ── Assemble .skidx: header + bigram entries + trigram entries + file_meta
        let mut skidx_buf = Vec::with_capacity(
            HEADER_SIZE
                + bigram_entries.encoded.len()
                + trigram_entries.encoded.len()
                + meta_buf.len(),
        );
        skidx_buf.extend_from_slice(&encode_header(&header));
        skidx_buf.extend_from_slice(&bigram_entries.encoded);
        skidx_buf.extend_from_slice(&trigram_entries.encoded);
        skidx_buf.extend_from_slice(&meta_buf);

        Ok((postings_buf, skidx_buf))
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "builder_tests.rs"]
mod tests;
