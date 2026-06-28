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
    FILE_META_SIZE, FileMetaEntry, SKIDX_ENTRY_SIZE, SKIDX_HEADER_SIZE, SkidxHeader,
    decode_file_meta, decode_header, decode_postings_varint, idf_for_key, lookup_ngram,
};
use crate::{
    FileId, IndexStats, Result, SearchError, SearchField, SearchLayer, SearchQuery, SearchResult,
    lexical::{BM25FConfig, FIELD_COUNT, bm25f_score, dominant_field},
    ngram::{Ngram, extract_query_ngrams, is_single_token},
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

        // Verify CRC32 checksum over postings + entries + file metadata (#364).
        //
        // Ordering matches builder.rs: postings first, then entries+meta.
        // This catches bit-flips in the .skpost blob that would otherwise
        // yield wrong-but-bounded (doc_id, position) values and silently
        // mis-rank results (Design Constraint: "fail loud", ADR-006).
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(&post_mmap);
        hasher.update(&idx_mmap[SKIDX_HEADER_SIZE..expected_idx_size]);
        let actual_checksum = hasher.finalize();
        if actual_checksum != header.checksum {
            return Err(SearchError::IndexCorrupted(format!(
                "checksum mismatch: expected {:#010x}, got {:#010x}. \
                 The index may be corrupt; rebuild with `skim search index --rebuild`.",
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
    /// Used by `check_staleness` to detect a stale/below-current lexical
    /// FORMAT_VERSION (currently v4) and trigger a rebuild before
    /// `NgramIndexReader::open` hard-errors on the version mismatch.
    /// For example, a v3 index on disk (pre-#358 delta+varint posting codec)
    /// reads version=3 here, which is less than FORMAT_VERSION=4, so the
    /// staleness check fires and a full rebuild is triggered.
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

    /// AD-355-7 / AD-372-4 short-query fallback: emit ALL indexed files as
    /// score-0 candidates (no internal truncation).
    ///
    /// Called when `extract_query_ngrams` returns an empty set (queries shorter
    /// than 3 bytes, e.g. `"fn"`, `"if"`).  Respects `file_filter`
    /// (blast-radius allowlist) and `lang_filter` (`--lang` constraint) so the
    /// two dispatch paths are consistent (PF-006: never silently drop a
    /// documented flag on a sub-path).
    ///
    /// # AD-372-4: Full filtered set, no internal pre-truncation
    ///
    /// The previous implementation applied `.skip(offset).take(limit)` **before**
    /// the caller's verification step, causing files with `file_id >= pool_limit`
    /// to be silently dropped even when they contained the query token.  This
    /// violated the AD-355-2 verify-then-truncate-LAST invariant.
    ///
    /// This method now returns the **complete** filtered candidate set (all files
    /// that pass `file_filter` + `lang_filter`).  The caller
    /// (`resolve_paths_and_snippets_verified`) is the **only** truncation gate —
    /// it applies offset and limit AFTER verification (ADR-001).
    ///
    /// Performance note: this incurs O(file_count) file reads on the verify pass.
    /// A concrete measured SLA (AC #15a) bounds this: `"fn"` over 5,000 indexed
    /// files must complete within 2,000 ms wall-clock (release profile).  If
    /// measurement shows a problem on larger corpora, a K-cap on verify fan-out
    /// is the documented follow-up (not part of #372).
    ///
    /// Security: no injection, no path traversal; only the user's own indexed
    /// files (bounded by `file_filter`) are returned as score-0 candidates.
    fn short_query_fallback(
        &self,
        query: &SearchQuery,
        lang_filter: Option<u8>,
    ) -> Vec<SearchResult> {
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
            // AD-372-4: NO .skip/.take here — the full filtered set is returned.
            // Offset + limit are applied by the caller AFTER verification.
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

    /// First sub-pass of the BM25F scoring loop: accumulate per-document, per-field
    /// TF counts and match positions from `postings` into `tf_per_doc` and
    /// `pos_per_doc`.
    ///
    /// Documents are skipped when:
    /// - `doc_id >= self.header.file_count` (out-of-range; defensive guard).
    /// - `query.file_filter` is set and the doc is not in the allowlist (blast-radius).
    ///
    /// The caller is responsible for calling `tf_per_doc.clear()` and
    /// `pos_per_doc.clear()` before each ngram iteration to reuse the allocations.
    fn accumulate_posting_tfs(
        &self,
        postings: &[super::format::PostingEntry],
        file_filter: Option<&std::collections::HashSet<FileId>>,
        tf_per_doc: &mut HashMap<u32, [f32; FIELD_COUNT]>,
        pos_per_doc: &mut HashMap<u32, Vec<std::ops::Range<usize>>>,
    ) {
        for p in postings {
            if p.doc_id >= self.header.file_count {
                continue; // out-of-range doc_ids are never valid
            }
            if let Some(filter) = file_filter
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
    }

    /// AND-intersection of posting lists: returns the sorted, deduplicated set of
    /// `doc_id`s that appear in **every** query trigram's posting list.
    ///
    /// # AD-372-2: Smallest-posting-list-first galloping intersection
    ///
    /// Intersection is computed by sorting the per-trigram doc_id sets by length
    /// (ascending) and then sweeping the smallest set against each larger set in
    /// turn.  Only doc_ids present in every set survive.  Bound: `O(min_list_len
    /// × n_trigrams)` — bounded by the smallest posting list length regardless of
    /// corpus size.  No allocation per candidate beyond the `Vec` result.
    ///
    /// The superset invariant (correctness guarantee): a file that contains the
    /// literal query token contains all of the token's contiguous trigrams.
    /// Therefore the AND-intersection of the query's trigram posting lists is a
    /// **superset** of the verified result set — every verified file is in the
    /// intersection, never dropped by the intersection.
    ///
    /// # Deduplication
    ///
    /// `decode_postings_varint` returns one `PostingEntry` per
    /// `(doc_id, field_id, position)` tuple.  A single document can appear many
    /// times in one posting list (once per occurrence).  Each list is first reduced
    /// to its sorted-unique `doc_id` set in a single linear pass (adjacent-distinct,
    /// because posting lists are already `doc_id`-major sorted).
    ///
    /// # Errors
    ///
    /// Returns `Err(SearchError::IndexCorrupted)` if any posting list fails to decode.
    fn intersect_posting_doc_ids(&self, ngrams: &[(Ngram, f32)]) -> Result<Vec<u32>> {
        if ngrams.is_empty() {
            return Ok(Vec::new());
        }

        // Build per-trigram sorted-unique doc_id sets.
        // Posting lists are already doc_id-major sorted; one linear pass suffices
        // for adjacent-dedup.
        let mut per_ngram_doc_ids: Vec<Vec<u32>> = Vec::with_capacity(ngrams.len());
        for (ngram, _weight) in ngrams {
            let postings = self.lookup_postings(ngram.key())?;
            let mut doc_ids: Vec<u32> = Vec::with_capacity(postings.len());
            let mut last: Option<u32> = None;
            for p in &postings {
                if last != Some(p.doc_id) {
                    doc_ids.push(p.doc_id);
                    last = Some(p.doc_id);
                }
            }
            // If any trigram has an empty posting list, the intersection is empty.
            if doc_ids.is_empty() {
                return Ok(Vec::new());
            }
            per_ngram_doc_ids.push(doc_ids);
        }

        // Sort by list length ascending so we sweep the smallest set first
        // (AD-372-2: smallest-posting-list-first for minimum work).
        per_ngram_doc_ids.sort_unstable_by_key(|v| v.len());

        // Start with the smallest set, then intersect with each remaining set.
        let mut intersection: Vec<u32> = per_ngram_doc_ids[0].clone();

        for other in &per_ngram_doc_ids[1..] {
            // Linear merge of two sorted slices — O(n + m) per pair.
            let mut result: Vec<u32> = Vec::new();
            let mut i = 0usize;
            let mut j = 0usize;
            while i < intersection.len() && j < other.len() {
                match intersection[i].cmp(&other[j]) {
                    std::cmp::Ordering::Equal => {
                        result.push(intersection[i]);
                        i += 1;
                        j += 1;
                    }
                    std::cmp::Ordering::Less => i += 1,
                    std::cmp::Ordering::Greater => j += 1,
                }
            }
            intersection = result;
            if intersection.is_empty() {
                return Ok(Vec::new());
            }
        }

        Ok(intersection)
    }

    /// Exact-symbol search: AND-intersection of query trigram posting lists,
    /// followed by occurrence-count / token-density ranking (length-norm-free).
    ///
    /// # AD-372-1: Query-shape dispatch — exact-symbol mode
    ///
    /// This method is called when `is_single_token(query.text)` is `true` and
    /// `extract_query_ngrams` produced a non-empty set.  It generates candidates
    /// via AND-intersection (grep-exact, limit/size-independent), then ranks by
    /// an occurrence-count / token-density key (AD-372-6) so large-file definers
    /// are not buried by BM25F length-normalization.
    ///
    /// The intersection is returned in its entirety (no `take` before verify):
    /// the caller (`resolve_paths_and_snippets_verified`) is the only truncation
    /// gate (AD-355-2).  When `query.limit` is `Some(n)`, offset+limit are
    /// applied AFTER ranking.
    ///
    /// # Correctness invariant (AD-372-2)
    ///
    /// A file that contains the literal query token contains every contiguous
    /// trigram of that token.  Therefore the AND-intersection of the query's
    /// trigram posting lists is a **superset** of the verified result set: every
    /// verified file is in the intersection; no true match can be dropped.
    ///
    /// # match_positions (RESOLVED Decision 2: ALL intersected trigrams)
    ///
    /// Positions are collected from **all** intersected trigrams for each
    /// surviving document (not just the highest-weight trigram).  This preserves
    /// byte-identical snippet behavior relative to the UNION path.
    ///
    /// # Errors
    ///
    /// Returns `Err(SearchError::IndexCorrupted)` if any posting list fails to
    /// decode.
    fn search_exact_intersection(
        &self,
        query: &SearchQuery,
        ngrams: &[(Ngram, f32)],
        lang_filter: Option<u8>,
    ) -> Result<Vec<SearchResult>> {
        // Step 1: AND-intersection of posting lists → surviving doc_ids.
        let intersected_ids = self.intersect_posting_doc_ids(ngrams)?;
        if intersected_ids.is_empty() {
            return Ok(Vec::new());
        }

        // Step 2: For each surviving doc_id, gather occurrence count (for
        // ranking) and match positions (for snippets) from ALL intersected
        // trigrams.  Also apply lang_filter and file_filter.
        //
        // Build a lookup set of surviving doc_ids for O(1) membership tests.
        let id_set: std::collections::HashSet<u32> = intersected_ids.iter().copied().collect();

        // Per-doc occurrence count (sum of TFs across all query trigrams).
        let mut doc_occurrence_count: HashMap<u32, usize> = HashMap::new();
        let mut doc_positions: HashMap<u32, Vec<std::ops::Range<usize>>> = HashMap::new();
        let mut doc_meta_cache: HashMap<u32, FileMetaEntry> = HashMap::new();
        let mut doc_field: HashMap<u32, [f32; FIELD_COUNT]> = HashMap::new();

        for (ngram, _weight) in ngrams {
            let postings = self.lookup_postings(ngram.key())?;
            for p in &postings {
                if !id_set.contains(&p.doc_id) {
                    continue; // not in intersection
                }
                // Apply file_filter (blast-radius allowlist) if set.
                if let Some(ref f) = query.file_filter
                    && !f.contains(&FileId(p.doc_id))
                {
                    continue;
                }
                // Resolve and cache file metadata; apply lang_filter.
                if let std::collections::hash_map::Entry::Vacant(e) =
                    doc_meta_cache.entry(p.doc_id)
                {
                    let meta = self.file_meta_at(p.doc_id)?;
                    e.insert(meta);
                }
                let meta = &doc_meta_cache[&p.doc_id];
                if lang_filter.is_some_and(|required| meta.lang_id != required) {
                    continue;
                }

                // Accumulate occurrence count (TF) across all query trigrams.
                *doc_occurrence_count.entry(p.doc_id).or_default() += 1;

                // Collect positions from ALL intersected trigrams (RESOLVED
                // Decision 2) so snippets are byte-identical to the UNION path.
                let pos = p.position as usize;
                doc_positions
                    .entry(p.doc_id)
                    .or_default()
                    .push(pos..pos + 3);

                // Accumulate field TF for dominant-field determination.
                let field_idx = p.field_id as usize;
                if field_idx < FIELD_COUNT {
                    doc_field.entry(p.doc_id).or_insert([0.0; FIELD_COUNT])[field_idx] += 1.0;
                }
            }
        }

        // Step 3: Build ranked result list.
        //
        // AD-372-6: Ranking key = raw occurrence_count (length-norm-free, NOT BM25F).
        //
        // BM25F divides TF by field_length, which buried large-file definers
        // (the root bug: a file with 3 occurrences of "UserService" in a 500-line
        // module scored LOWER than a tiny stub with 1 occurrence because BM25F's
        // field-length normalization term divided by the large module's byte count).
        //
        // The fix: use the raw occurrence count directly.  A file with 10 occurrences
        // of the token ranks higher than a file with 1 occurrence regardless of file
        // size.  This is "length-norm-free" in the sense that large files are not
        // penalized for being large — only raw occurrence frequency matters.
        //
        // Why NOT occurrence/total_tokens?  That would reintroduce a density bias
        // that penalizes long files (a file with 3/83 = 0.036 density ranks BELOW
        // a tiny file with 1/5 = 0.20 density), recreating the length-normalization
        // problem we are eliminating.  Raw count is the correct signal.
        //
        // Tie-break: ascending FileId for determinism (mirrors collect_scored_results).
        //
        // Note: docs that were excluded by file_filter or lang_filter above will
        // have no entry in doc_occurrence_count and are omitted here.
        let mut scored: Vec<(u32, f64)> = doc_occurrence_count
            .into_iter()
            .map(|(doc_id, occ)| {
                // AD-372-6: length-norm-free ranking key = raw occurrence count.
                // Do NOT divide by total_tokens — that reintroduces length normalization
                // and would penalize large files with many occurrences relative to tiny
                // files with a single dense occurrence (recreating the root bug).
                let score = occ as f64;
                (doc_id, score)
            })
            .collect();

        // Sort: descending score, ascending FileId for tie-break (determinism).
        scored.sort_unstable_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        // Step 4: Apply offset + limit LAST (AD-355-2 / AD-372-3).
        // When query.limit is None the full intersection is returned to the caller,
        // which applies its own truncation after verification.
        let offset = query.offset.unwrap_or(0);
        let ranked: Box<dyn Iterator<Item = (u32, f64)>> = if let Some(lim) = query.limit {
            Box::new(scored.into_iter().skip(offset).take(lim))
        } else {
            Box::new(scored.into_iter().skip(offset))
        };

        let results: Vec<SearchResult> = ranked
            .map(|(doc_id, score)| {
                let positions = doc_positions.remove(&doc_id).unwrap_or_default();
                let field = doc_field.get(&doc_id).map(dominant_field).unwrap_or(SearchField::Other);
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

    /// Final phase of scoring: apply defense-in-depth file_filter, sort by score,
    /// apply offset/limit, and assemble [`SearchResult`] values.
    ///
    /// `doc_scores`, `doc_field_tfs`, and `doc_positions` are all consumed here.
    fn collect_scored_results(
        doc_scores: HashMap<u32, f64>,
        doc_field_tfs: HashMap<u32, [f32; FIELD_COUNT]>,
        mut doc_positions: HashMap<u32, Vec<std::ops::Range<usize>>>,
        file_filter: Option<&std::collections::HashSet<FileId>>,
        offset: usize,
        limit: usize,
    ) -> Vec<SearchResult> {
        // Defense-in-depth: re-apply file_filter before collecting scores.
        // The first sub-pass already skips posting accumulation for non-allowlisted
        // docs, so in practice this is a no-op.  It is kept to guard against future
        // refactors that change the first-pass filtering logic.
        let mut scored: Vec<(u32, f64)> = match file_filter {
            Some(filter) => doc_scores
                .into_iter()
                .filter(|(doc_id, _)| filter.contains(&FileId(*doc_id)))
                .collect(),
            None => doc_scores.into_iter().collect(),
        };
        // Sort descending by score; tie-break ascending by FileId for determinism.
        scored.sort_unstable_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        scored
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
            .collect()
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

        // v4: posting list is variable-length encoded (delta+varint, AD-LXPOST-1).
        // The old fixed-stride guard (is_multiple_of 9) assumed 9-byte fixed entries
        // and is removed — varint byte counts are not a multiple of 9.
        // CRC32 integrity over the full .skpost blob is verified in open() (#364).
        let data = &self.post_mmap[start..end];
        decode_postings_varint(data)
    }
}

// ============================================================================
// SearchLayer implementation
// ============================================================================

impl SearchLayer for NgramIndexReader {
    /// Execute a scored n-gram search, dispatching on query shape.
    ///
    /// # AD-372-1: Two-mode dispatch
    ///
    /// The branch order is:
    ///
    /// 1. **Empty query guard** — return immediately with an empty result.
    /// 2. **Extract query trigrams** — if the set is empty (query < 3 bytes),
    ///    route to `short_query_fallback` (AD-355-7 / AD-372-4).
    /// 3. **`is_single_token` branch** — a single contiguous token (≥ 3 bytes,
    ///    no interior whitespace) routes to `search_exact_intersection`, which
    ///    generates candidates via AND-intersection (grep-exact, limit/size-
    ///    independent) and ranks by an occurrence-count / token-density key
    ///    (AD-372-6, length-norm-free).
    /// 4. **Multi-word / default** — the existing BM25F UNION loop; untouched.
    ///
    /// The `is_single_token` check is placed AFTER the `ngrams.is_empty()` guard
    /// so that a 1-2 byte token (e.g. `"fn"`) always enters the short-query
    /// fallback regardless of what `is_single_token` would say about the trimmed
    /// form.  This ensures a 1-2 byte single token never enters the intersection
    /// path with zero trigrams.
    ///
    /// # Short-query semantics (AD-355-7 / AD-372-4)
    ///
    /// `short_query_fallback` now returns the **full** filtered candidate set (no
    /// internal `.take`); the caller's verify-then-truncate-LAST step is the only
    /// gate (ADR-001).
    ///
    /// # Exact-symbol semantics (AD-372-1 / AD-372-6)
    ///
    /// `search_exact_intersection` applies offset + limit after ranking —
    /// callers on the pure-lexical path must set `sq.limit = None` so the
    /// complete intersection is forwarded to `resolve_paths_and_snippets_verified`
    /// (AD-372-3).
    ///
    /// # Multi-word / UNION semantics (unchanged)
    ///
    /// The BM25F UNION loop is byte-identical to the pre-#372 implementation.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::IndexCorrupted`] if any posting list fails to
    /// decode.
    fn search(&self, query: &SearchQuery) -> Result<Vec<SearchResult>> {
        if query.text.is_empty() {
            return Ok(Vec::new());
        }

        let ngrams = extract_query_ngrams(&query.text);

        // Language filter resolved up-front so it is available to ALL dispatch
        // paths (short-query fallback, exact-intersection, and UNION loop).
        // Fix (F15): previously resolved after the `ngrams.is_empty()` guard,
        // so the fallback silently ignored `query.lang`.  Moving resolution here
        // ensures all paths honour the language constraint (PF-006).
        let lang_filter: Option<u8> = query.lang.map(super::format::lang_to_id);

        // AD-355-7 / AD-372-4: Short-query fallback.
        //
        // Trigram extraction requires ≥3 bytes; single- and double-byte tokens
        // produce zero trigrams.  Emit ALL indexed files (filtered by
        // file_filter + lang_filter) as score-0 candidates so the caller's
        // verify step can apply a literal substring filter.
        //
        // AD-372-4: the returned set has NO internal pre-truncation.
        // Offset + limit are applied by the caller AFTER verification.
        // Callers must not set `sq.limit` on this path — they will get the
        // full filtered set regardless.
        if ngrams.is_empty() {
            return Ok(self.short_query_fallback(query, lang_filter));
        }

        // AD-372-1: single-token exact-symbol mode.
        //
        // A single contiguous token (≥3 bytes, no interior whitespace) enters
        // the AND-intersection path.  The intersection is grep-exact and
        // limit/size-independent: every verified file is guaranteed to be in
        // the candidate set (superset invariant, AD-372-2).  Ranked by
        // occurrence-count / token-density (length-norm-free, AD-372-6) so
        // large-file definers are not buried by BM25F field-length normalization.
        //
        // This check is placed AFTER `ngrams.is_empty()` (above) so that a
        // 1-2 byte token always enters the short-query fallback regardless of
        // `is_single_token`'s answer.  A 1-byte query like "a" has
        // is_single_token=false (< 3 bytes), so this guard is redundant for
        // that case, but the ordering makes the invariant explicit.
        if is_single_token(&query.text) {
            return self.search_exact_intersection(query, &ngrams, lang_filter);
        }

        // Multi-word / default: BM25F UNION path (unchanged from pre-#372).
        // Resolve scoring config: per-query override takes priority.
        // Validate at the trust boundary so invalid params are rejected early.
        let scoring_config: &BM25FConfig = match &query.bm25f_config {
            Some(cfg) => {
                cfg.validate()?;
                cfg
            }
            None => &self.bm25f_config,
        };

        // Per-ngram accumulation buffers — reused across iterations to avoid churn.
        let mut tf_per_doc: HashMap<u32, [f32; FIELD_COUNT]> = HashMap::new();
        let mut pos_per_doc: HashMap<u32, Vec<std::ops::Range<usize>>> = HashMap::new();
        // Persistent scoring state across all ngram iterations.
        let mut doc_scores: HashMap<u32, f64> = HashMap::new();
        let mut doc_field_tfs: HashMap<u32, [f32; FIELD_COUNT]> = HashMap::new();
        let mut doc_positions: HashMap<u32, Vec<std::ops::Range<usize>>> = HashMap::new();
        let mut doc_meta_cache: HashMap<u32, FileMetaEntry> = HashMap::new();

        for (ngram, _weight) in &ngrams {
            let postings = self.lookup_postings(ngram.key())?;
            let idf = f64::from(idf_for_key(ngram.key()));

            // Sub-pass 1: accumulate TF counts and match positions per doc.
            // Blast-radius early-out and out-of-range guard are in the helper.
            tf_per_doc.clear();
            pos_per_doc.clear();
            self.accumulate_posting_tfs(
                &postings,
                query.file_filter.as_ref(),
                &mut tf_per_doc,
                &mut pos_per_doc,
            );

            // Sub-pass 2: apply lang filter, score, and transfer positions.
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

        let offset = query.offset.unwrap_or(0);
        let limit = query.limit.unwrap_or(20);

        Ok(Self::collect_scored_results(
            doc_scores,
            doc_field_tfs,
            doc_positions,
            query.file_filter.as_ref(),
            offset,
            limit,
        ))
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
