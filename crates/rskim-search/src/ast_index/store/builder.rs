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
use std::path::{Path, PathBuf};

use rayon::prelude::*;
use tempfile::NamedTempFile;

use super::format::{
    AstBigramTableEntry, AstFileMetaEntry, AstPostingEntry, AstSkidxHeader, AstTrigramTableEntry,
    FILE_META_SIZE, FORMAT_VERSION, HEADER_SIZE, POSTING_ENTRY_SIZE, SKAX_MAGIC,
    encode_bigram_entry, encode_file_meta, encode_header, encode_posting, encode_trigram_entry,
    lang_to_id,
};
use super::reader::AstIndexReader;
use crate::{
    FileId, Result, SearchError,
    ast_index::{AstNgramSet, extract_ast_ngrams, linearize_source},
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
        let node_count = u32::try_from(lin.nodes.len()).map_err(|_| {
            SearchError::IndexCorrupted(format!(
                "node_count {} exceeds u32::MAX for FileId {}",
                lin.nodes.len(),
                id.0
            ))
        })?;
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
        let extracted: Vec<Result<(FileId, rskim_core::Language, AstNgramSet, u32)>> = files
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
            * POSTING_ENTRY_SIZE;
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
