//! [`NgramIndexReader`] — mmap'd BM25 query layer for the two-file n-gram index.
//!
//! # Memory layout
//!
//! The `.skidx` file is memory-mapped in its entirety.  The layout is:
//!
//! ```text
//! [SkidxHeader: 62 bytes]
//! [SkidxEntry × ngram_count: 16 bytes each]   ← v3: ngram_key widened to u32
//! [FileMetaEntry × file_count: 37 bytes each]
//! ```
//!
//! The `.skpost` file is also memory-mapped.  Entry offsets/lengths in the
//! `.skidx` lookup table directly index into it.
//!
//! # Send + Sync
//!
//! `NgramIndexReader` is `Send + Sync` because:
//! - Both `Mmap` fields are `Send + Sync` on all platforms supported by
//!   `memmap2`.
//! - All fields are read-only after construction.
//! - The `SearchLayer` trait bound requires `Send + Sync`.

use std::collections::HashMap;
use std::path::Path;

use memmap2::Mmap;

use super::format::{
    FILE_META_SIZE, FileMetaEntry, POSTING_ENTRY_SIZE, SKIDX_ENTRY_SIZE, SKIDX_HEADER_SIZE,
    SkidxHeader, compute_checksum, decode_file_meta, decode_header, decode_posting, idf_for_key,
    lookup_ngram,
};
use crate::{
    FileId, IndexStats, Result, SearchError, SearchField, SearchLayer, SearchQuery, SearchResult,
    lexical::{BM25FConfig, FIELD_COUNT, bm25f_score, dominant_field},
    ngram::extract_query_ngrams,
};

// ============================================================================
// Reader struct
// ============================================================================

/// Memory-mapped, read-only query layer for the two-file n-gram index.
///
/// Constructed via [`NgramIndexReader::open`] after an
/// [`super::builder::NgramIndexBuilder`] has written `index.skidx` and
/// `index.skpost` to a directory.
pub struct NgramIndexReader {
    /// Decoded header (copied out of mmap for cheap access).
    header: SkidxHeader,
    /// Memory-mapped `.skidx` file (header + entries + file metadata).
    idx_mmap: Mmap,
    /// Memory-mapped `.skpost` file (concatenated posting lists).
    post_mmap: Mmap,
    /// Default BM25F scoring configuration for this reader.
    ///
    /// Can be overridden per-query via [`SearchQuery::bm25f_config`].
    bm25f_config: BM25FConfig,
}

// NgramIndexReader is automatically Send + Sync because all fields
// (SkidxHeader: Copy, Mmap: Send+Sync) satisfy the auto-trait bounds.

impl NgramIndexReader {
    /// Open an existing index from `dir`.
    ///
    /// Validates magic bytes, format version, file sizes, and the CRC32
    /// checksum before returning.
    ///
    /// # Errors
    ///
    /// - [`SearchError::Io`] if the index files cannot be opened.
    /// - [`SearchError::IndexCorrupted`] if validation fails.
    pub fn open(dir: &Path) -> Result<Self> {
        let idx_path = dir.join("index.skidx");
        let post_path = dir.join("index.skpost");

        let idx_file = std::fs::File::open(&idx_path)?;
        let post_file = std::fs::File::open(&post_path)?;

        // SAFETY: The files are not modified after mapping.  If another
        // process truncates or overwrites them concurrently, behaviour is
        // undefined but this is an inherent constraint of mmap-based indexes.
        let idx_mmap = unsafe { Mmap::map(&idx_file) }?;
        let post_mmap = unsafe { Mmap::map(&post_file) }?;

        let header = decode_header(&idx_mmap)?;

        // Validate sizes are internally consistent.
        let entries_bytes = (header.ngram_count as usize)
            .checked_mul(SKIDX_ENTRY_SIZE)
            .ok_or_else(|| {
                SearchError::IndexCorrupted("ngram_count * SKIDX_ENTRY_SIZE overflow".into())
            })?;
        let meta_bytes = (header.file_count as usize)
            .checked_mul(FILE_META_SIZE)
            .ok_or_else(|| {
                SearchError::IndexCorrupted("file_count * FILE_META_SIZE overflow".into())
            })?;
        let expected_idx_size = SKIDX_HEADER_SIZE
            .checked_add(entries_bytes)
            .and_then(|s| s.checked_add(meta_bytes))
            .ok_or_else(|| SearchError::IndexCorrupted("expected_idx_size overflow".into()))?;
        if idx_mmap.len() != expected_idx_size {
            return Err(SearchError::IndexCorrupted(format!(
                "skidx size mismatch: expected {expected_idx_size}, got {}",
                idx_mmap.len()
            )));
        }
        let expected_post_size = usize::try_from(header.postings_file_size).map_err(|_| {
            SearchError::IndexCorrupted(format!(
                "postings_file_size {} exceeds platform usize",
                header.postings_file_size
            ))
        })?;
        if post_mmap.len() != expected_post_size {
            return Err(SearchError::IndexCorrupted(format!(
                "skpost size mismatch: expected {}, got {}",
                header.postings_file_size,
                post_mmap.len()
            )));
        }

        // Verify CRC32 checksum over entries + file metadata.  The slice is
        // contiguous in the mmap so no copy is needed.
        let payload = &idx_mmap[SKIDX_HEADER_SIZE..expected_idx_size];
        let actual_checksum = compute_checksum(payload);
        if actual_checksum != header.checksum {
            return Err(SearchError::IndexCorrupted(format!(
                "checksum mismatch: expected {:#010x}, got {:#010x}",
                header.checksum, actual_checksum
            )));
        }

        Ok(Self {
            header,
            idx_mmap,
            post_mmap,
            bm25f_config: BM25FConfig::default(),
        })
    }

    /// Read the lexical index format version from the first 6 bytes of `index.skidx`.
    ///
    /// Opens only 6 bytes (magic + version) — no mmap, no CRC, no full validation.
    /// Used by `check_staleness` to detect a v2→v3 format mismatch and trigger a
    /// rebuild before `NgramIndexReader::open` hard-errors on the version mismatch.
    ///
    /// # Errors
    ///
    /// - [`SearchError::Io`] if the file cannot be opened.
    /// - [`SearchError::IndexCorrupted`] if the file is too short or has bad magic.
    pub fn lexical_index_version(dir: &Path) -> Result<u16> {
        use std::io::Read;
        let idx_path = dir.join("index.skidx");
        let mut file = std::fs::File::open(&idx_path)?;
        let mut buf = [0u8; 6];
        file.read_exact(&mut buf).map_err(|_| {
            SearchError::IndexCorrupted(
                "lexical_index_version: index.skidx too short (need 6 bytes)".into(),
            )
        })?;
        let magic = &buf[0..4];
        if magic != super::format::SKIDX_MAGIC {
            return Err(SearchError::IndexCorrupted(format!(
                "lexical_index_version: bad magic: expected {:?}, got {:?}",
                super::format::SKIDX_MAGIC,
                magic
            )));
        }
        let version = u16::from_le_bytes([buf[4], buf[5]]);
        Ok(version)
    }

    /// Open an existing index from `dir` with a custom BM25F configuration.
    ///
    /// Identical to [`NgramIndexReader::open`] except the provided `config`
    /// is used as the reader-level default (still overridable per-query via
    /// [`SearchQuery::bm25f_config`]).
    ///
    /// # Errors
    ///
    /// - Same conditions as [`NgramIndexReader::open`].
    /// - [`SearchError::InvalidQuery`] if `config` fails validation.
    pub fn open_with_config(dir: &std::path::Path, config: BM25FConfig) -> Result<Self> {
        config.validate()?;
        let mut reader = Self::open(dir)?;
        reader.bm25f_config = config;
        Ok(reader)
    }

    /// Return summary statistics for this index.
    #[must_use]
    pub fn stats(&self) -> IndexStats {
        IndexStats {
            file_count: self.header.file_count,
            total_ngrams: self.header.ngram_count as u64,
            index_size_bytes: (self.idx_mmap.len() + self.post_mmap.len()) as u64,
            last_updated: None,
        }
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Read the [`FileMetaEntry`] for the file at sequential index `file_index`.
    ///
    /// `file_index` is the zero-based insertion order, not a [`FileId`].
    fn file_meta_at(&self, file_index: u32) -> Result<FileMetaEntry> {
        let entries_end = SKIDX_HEADER_SIZE + (self.header.ngram_count as usize) * SKIDX_ENTRY_SIZE;
        let offset = entries_end + (file_index as usize) * FILE_META_SIZE;
        let end = offset
            .checked_add(FILE_META_SIZE)
            .filter(|&e| e <= self.idx_mmap.len())
            .ok_or_else(|| {
                SearchError::IndexCorrupted(format!(
                    "file_meta_at({file_index}): offset {offset} out of bounds"
                ))
            })?;
        decode_file_meta(&self.idx_mmap[offset..end])
    }

    /// AD-355-7 short-query fallback: emit all indexed files as score-0 candidates.
    ///
    /// Called when `extract_query_ngrams` returns an empty set (queries shorter
    /// than 3 bytes).  Respects `file_filter` (blast-radius allowlist) and
    /// `lang_filter` (`--lang` constraint) so the two dispatch paths are
    /// consistent (PF-006: never silently drop a documented flag on a sub-path).
    ///
    /// Returns candidates in file-id/insertion order; verification in the caller
    /// (e.g. `resolve_paths_and_snippets_verified`) is the only correctness gate.
    /// See the `SearchLayer::search` contract in `types.rs` for details.
    fn short_query_fallback(
        &self,
        query: &SearchQuery,
        lang_filter: Option<u8>,
    ) -> Vec<SearchResult> {
        let limit = query.limit.unwrap_or(20);
        let offset = query.offset.unwrap_or(0);
        let file_count = self.header.file_count as usize;

        (0..file_count)
            .filter(|&doc_id| {
                // Respect the blast-radius file_filter allowlist if present.
                if let Some(ref f) = query.file_filter
                    && !f.contains(&FileId(doc_id as u32))
                {
                    return false;
                }
                // Respect the --lang filter if present (F15/PF-006).
                // Use the existing file_meta_at helper for a clean, bounds-checked
                // decode (avoids raw mmap arithmetic in the fallback path).
                if let Some(lang_id) = lang_filter {
                    match self.file_meta_at(doc_id as u32) {
                        Ok(meta) if meta.lang_id == lang_id => {}
                        _ => return false,
                    }
                }
                true
            })
            .skip(offset)
            .take(limit)
            .map(|doc_id| SearchResult {
                file_id: FileId(doc_id as u32),
                score: 0.0,
                line_range: 0..0,
                match_positions: vec![],
                field: SearchField::Other,
                snippet: None,
            })
            .collect()
    }

    /// Score the candidates accumulated in `tf_per_doc` for a single ngram iteration.
    ///
    /// For each candidate document this method:
    /// 1. Resolves (and caches) the file metadata via `doc_meta_cache`.
    /// 2. Applies the language filter — skips documents whose `lang_id` doesn't match.
    /// 3. Accumulates per-field TF counts into `doc_field_tfs` for [`dominant_field`].
    /// 4. Computes the BM25F contribution and adds it to `doc_scores`.
    /// 5. Transfers any buffered positions from `pos_per_doc` into `doc_positions`.
    #[allow(clippy::too_many_arguments)]
    fn score_ngram_postings(
        &self,
        idf: f64,
        tf_per_doc: &HashMap<u32, [f32; FIELD_COUNT]>,
        pos_per_doc: &mut HashMap<u32, Vec<std::ops::Range<usize>>>,
        lang_filter: Option<u8>,
        scoring_config: &BM25FConfig,
        doc_scores: &mut HashMap<u32, f64>,
        doc_field_tfs: &mut HashMap<u32, [f32; FIELD_COUNT]>,
        doc_positions: &mut HashMap<u32, Vec<std::ops::Range<usize>>>,
        doc_meta_cache: &mut HashMap<u32, FileMetaEntry>,
    ) -> Result<()> {
        for (&doc_id, field_tfs) in tf_per_doc {
            if let std::collections::hash_map::Entry::Vacant(e) = doc_meta_cache.entry(doc_id) {
                let meta = self.file_meta_at(doc_id)?;
                e.insert(meta);
            }
            let meta = &doc_meta_cache[&doc_id];

            if lang_filter.is_some_and(|required_lang| meta.lang_id != required_lang) {
                continue;
            }

            let doc_tfs = doc_field_tfs.entry(doc_id).or_insert([0.0; FIELD_COUNT]);
            for i in 0..FIELD_COUNT {
                doc_tfs[i] += field_tfs[i];
            }

            let contribution = bm25f_score(
                idf,
                field_tfs,
                &meta.field_lengths,
                &self.header.avg_field_lengths,
                scoring_config,
            );
            *doc_scores.entry(doc_id).or_default() += contribution;

            if let Some(positions) = pos_per_doc.remove(&doc_id) {
                doc_positions.entry(doc_id).or_default().extend(positions);
            }
        }
        Ok(())
    }

    /// Retrieve all posting entries for `ngram_key` from the mmap'd posting file.
    fn lookup_postings(&self, ngram_key: u32) -> Result<Vec<super::format::PostingEntry>> {
        let entries_start = SKIDX_HEADER_SIZE;
        let entries_end = entries_start + (self.header.ngram_count as usize) * SKIDX_ENTRY_SIZE;
        let entries_data = &self.idx_mmap[entries_start..entries_end];

        let entry = match lookup_ngram(entries_data, ngram_key)? {
            Some(e) => e,
            None => return Ok(Vec::new()),
        };

        let start = usize::try_from(entry.posting_offset).map_err(|_| {
            SearchError::IndexCorrupted(format!(
                "posting_offset {} exceeds usize",
                entry.posting_offset
            ))
        })?;
        let length = entry.posting_length as usize;
        let end = start.checked_add(length).ok_or_else(|| {
            SearchError::IndexCorrupted(format!("posting slice overflow: {start} + {length}"))
        })?;
        if end > self.post_mmap.len() {
            return Err(SearchError::IndexCorrupted(format!(
                "posting slice [{start}..{end}] out of bounds (skpost len={})",
                self.post_mmap.len()
            )));
        }

        if !length.is_multiple_of(POSTING_ENTRY_SIZE) {
            return Err(SearchError::IndexCorrupted(format!(
                "posting_length {length} not aligned to POSTING_ENTRY_SIZE {POSTING_ENTRY_SIZE}"
            )));
        }
        let data = &self.post_mmap[start..end];
        let n = length / POSTING_ENTRY_SIZE;
        let mut postings = Vec::with_capacity(n);
        for i in 0..n {
            let off = i * POSTING_ENTRY_SIZE;
            postings.push(decode_posting(&data[off..off + POSTING_ENTRY_SIZE])?);
        }
        Ok(postings)
    }
}

// ============================================================================
// SearchLayer implementation
// ============================================================================

impl SearchLayer for NgramIndexReader {
    /// Execute a BM25F-scored n-gram search.
    ///
    /// # Algorithm
    ///
    /// 1. Extract query trigrams via [`extract_query_ngrams`] (sorted by weight desc).
    /// 2. For each trigram, retrieve its posting list.
    /// 3. Accumulate per-document, per-field term frequencies and match positions.
    /// 4. Apply language filter if `query.lang` is set.
    /// 5. Score each document with BM25F using per-field TF, field lengths, and
    ///    average field lengths from the header.
    /// 6. Sort descending by score with [`FileId`] tie-breaking for determinism.
    /// 7. Apply offset/limit (default: 0/20).
    /// 8. Return [`SearchResult`] values with `field` from [`dominant_field`],
    ///    `line_range: 0..0`, and `snippet: None` (deferred to a later wave).
    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        if query.text.is_empty() {
            return Ok(Vec::new());
        }

        let ngrams = extract_query_ngrams(&query.text);

        // Language filter resolved up-front so it is available to BOTH the normal
        // scored path AND the AD-355-7 short-query fallback below.
        // Fix (F15): previously resolved after the `ngrams.is_empty()` guard, so the
        // fallback silently ignored `query.lang` — `skim search "fn" --lang python`
        // would emit non-Python score-0 candidates.  Moving the resolution here
        // ensures both paths honour the language constraint (PF-006: never make a
        // documented flag silently inert on a dispatch sub-path).
        let lang_filter: Option<u8> = query.lang.map(super::format::lang_to_id);

        // AD-355-7: Short-query fallback.
        //
        // Trigram extraction requires ≥3 bytes per token; single- and double-byte
        // tokens (e.g. "fn", "if") produce zero trigrams.  In this case the ngram
        // index cannot generate candidates, but the query is still meaningful.
        //
        // Rather than returning empty results, we emit indexed files as score-0
        // candidates so that the caller's verify step can apply a literal substring
        // filter and return only files that actually contain the query string.
        //
        // SRP note (AD-355-7): the returned score-0 candidates are meaningful ONLY
        // if the caller applies a verification step.  The `NgramIndexReader` has no
        // access to file content — it is a pure index reader.  Any consumer of this
        // trait that trusts the short-query result list without verification will
        // surface arbitrary non-matching files.  The verify-then-truncate obligation
        // is documented in the `SearchLayer::search` contract (types.rs).
        //
        // Candidate bound — callers are expected to set `sq.limit = Some(N)`:
        //   Unset (None): `unwrap_or(20)` default.
        //   Pure-lexical: `Some(max(config.limit * LEXICAL_POOL_K, 100))` (query.rs).
        //   Compound:     `Some(filter_set.len().max(1))` (AD-356-2, query.rs #356).
        //                 (`filter_set` is `blast ∩ ast` on the blast+AST sub-path,
        //                  or the full AST set on the no-blast sub-path — it may be
        //                  strictly smaller than the raw AST set.)
        //   Blast-radius: `Some(max(config.limit * BLAST_POOL_K, 100))` (K=10).
        //
        // NOTE: candidates are selected in raw file-id/insertion order BEFORE the
        // verify step runs.  This deviates from the AD-355-2 verify-then-truncate-LAST
        // invariant: a file that contains the short query but has file_id >= pool_limit
        // will be silently missed.  For the compound path this is mitigated by the
        // file_filter restricting scoring to the AST set (AD-356-1); for other paths
        // this remains an accepted trade-off.
        //
        // Security: no injection, no path traversal; only the user's own indexed
        // files (bounded to sq.limit / file_filter) are returned as score-0 candidates.
        if ngrams.is_empty() {
            return Ok(self.short_query_fallback(query, lang_filter));
        }

        // Resolve scoring config: per-query override takes priority.
        // Validate at the trust boundary so invalid params are rejected early.
        let scoring_config: &BM25FConfig = match &query.bm25f_config {
            Some(cfg) => {
                cfg.validate()?;
                cfg
            }
            None => &self.bm25f_config,
        };

        // doc_id → accumulated BM25F score.
        let mut doc_scores: HashMap<u32, f64> = HashMap::new();
        // doc_id → per-field TF accumulators for dominant_field().
        let mut doc_field_tfs: HashMap<u32, [f32; FIELD_COUNT]> = HashMap::new();
        // doc_id → match positions (collected during the single posting pass).
        let mut doc_positions: HashMap<u32, Vec<std::ops::Range<usize>>> = HashMap::new();
        // Cache decoded FileMetaEntry per doc_id to avoid repeated mmap decoding.
        let mut doc_meta_cache: HashMap<u32, FileMetaEntry> = HashMap::new();

        // Reused across ngram iterations to avoid per-iteration allocation churn.
        let mut tf_per_doc: HashMap<u32, [f32; FIELD_COUNT]> = HashMap::new();
        let mut pos_per_doc: HashMap<u32, Vec<std::ops::Range<usize>>> = HashMap::new();

        for (ngram, _weight) in &ngrams {
            let postings = self.lookup_postings(ngram.key())?;
            let idf = f64::from(idf_for_key(ngram.key()));

            // First sub-pass: accumulate per-field TF counts and candidate positions,
            // skipping doc_ids that are out of range (never valid) or outside the
            // file_filter allowlist.  Checking file_filter here avoids accumulating
            // TF and positions for documents that will be discarded anyway — critical
            // for blast-radius queries where the allowlist is a tiny fraction of the
            // full index.
            tf_per_doc.clear();
            pos_per_doc.clear();
            for p in &postings {
                if p.doc_id >= self.header.file_count {
                    continue; // out-of-range doc_ids are never valid
                }
                if let Some(ref filter) = query.file_filter
                    && !filter.contains(&FileId(p.doc_id))
                {
                    continue; // not in the blast-radius allowlist — skip early
                }
                let field_idx = p.field_id as usize;
                if field_idx < FIELD_COUNT {
                    tf_per_doc.entry(p.doc_id).or_insert([0.0; FIELD_COUNT])[field_idx] += 1.0;
                }
                let pos = p.position as usize;
                pos_per_doc.entry(p.doc_id).or_default().push(pos..pos + 3);
            }

            // Second sub-pass: apply language filter, score, and transfer positions
            // only for documents that survive all filters.
            self.score_ngram_postings(
                idf,
                &tf_per_doc,
                &mut pos_per_doc,
                lang_filter,
                scoring_config,
                &mut doc_scores,
                &mut doc_field_tfs,
                &mut doc_positions,
                &mut doc_meta_cache,
            )?;
        }

        // Defense-in-depth: apply file_filter allowlist a second time before
        // collecting scores.  The first sub-pass (above) already skips posting
        // accumulation for non-allowlisted docs, so in practice this filter is a
        // no-op.  It is kept here to guard against future refactors that change
        // the first-pass filtering logic — any doc that somehow escaped the early
        // guard is still excluded from the final result set.
        let mut scored: Vec<(u32, f64)> = if let Some(ref filter) = query.file_filter {
            doc_scores
                .into_iter()
                .filter(|(doc_id, _)| filter.contains(&FileId(*doc_id)))
                .collect()
        } else {
            doc_scores.into_iter().collect()
        };
        // Sort descending by score; tie-break ascending by FileId for determinism.
        // FileId tie-breaking already guarantees a total order so stable sort is not needed.
        scored.sort_unstable_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        let offset = query.offset.unwrap_or(0);
        let limit = query.limit.unwrap_or(20);

        let results = scored
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(doc_id, score)| {
                let positions = doc_positions.remove(&doc_id).unwrap_or_default();
                let field = doc_field_tfs
                    .get(&doc_id)
                    .map(dominant_field)
                    .unwrap_or(SearchField::Other);
                SearchResult {
                    file_id: FileId(doc_id),
                    score,
                    line_range: 0..0,
                    match_positions: positions,
                    field,
                    snippet: None,
                }
            })
            .collect();

        Ok(results)
    }

    fn name(&self) -> &str {
        "ngram-index"
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "reader_tests.rs"]
mod tests;
