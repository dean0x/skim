//! DI seam: [`AstPostingSource`] trait and its production implementation.
//!
//! Separating the trait from the engine lets tests inject in-memory fakes
//! without depending on disk I/O or a real AST index.

use crate::ast_index::store::AstIndexReader;
use crate::{
    Result,
    ast_index::{AstBigram, AstFileMetaEntry, AstPosting, AstTrigram},
};

/// Dependency-injection seam: implemented by [`AstIndexReader`] and test fakes.
///
/// **Value-type coupling is intentional and bounded.** `AstPosting` and
/// `AstFileMetaEntry` are deliberately treated as the stable query-layer value
/// contract — they are `Copy`, gix-free, and mmap-free — mirroring the
/// "free of gix types" note on `CommitInfo` in `types.rs`.
///
/// **Finiteness contract.** `avg_node_count()` MUST return a finite, non-NaN
/// value. `node_count` values in `AstFileMetaEntry` MUST be non-negative.
/// The production reader validates these at header/entry decode time; custom
/// implementations must uphold the same contract.
///
/// **Count contract (C4).** `count` in every returned [`AstPosting`] MUST be
/// `>= 1`. Sources returning `count == 0` break the all-scores-positive
/// invariant (`count >= 1 → tf > 0 → score > 0`) relied on by BM25 and the
/// `debug_assert!` in [`crate::ast_index::AstQueryEngine::run_ngram_set`].
pub trait AstPostingSource: Send + Sync {
    /// Look up postings for an [`AstBigram`]; `Ok(vec![])` when absent (C2).
    fn lookup_bigram(&self, b: AstBigram) -> Result<Vec<AstPosting>>;
    /// Look up postings for an [`AstTrigram`]; `Ok(vec![])` when absent (C2).
    fn lookup_trigram(&self, t: AstTrigram) -> Result<Vec<AstPosting>>;
    /// Per-file metadata for `doc_id`; `Err(IndexCorrupted)` when out of range.
    fn file_meta(&self, doc_id: u32) -> Result<AstFileMetaEntry>;
    /// Average per-file node count across the corpus. MUST be finite and non-NaN.
    fn avg_node_count(&self) -> f32;
    /// Total number of files in the index.
    fn file_count(&self) -> u32;
    /// Partial decode — returns `(lang_id, node_count)` for `doc_id`.
    ///
    /// This is the hot-path accessor called by `score_postings` (#286 P1).
    /// The default implementation delegates to `file_meta` so test fakes
    /// compiled against the trait before this method existed continue to work.
    /// The production [`AstIndexReader`] overrides with a fast path that
    /// decodes only bytes `[0..5]` of the 15-byte on-disk record.
    ///
    /// **Contract**: for any in-range `doc_id`, the returned `(u8, u32)` equals
    /// `(file_meta(doc_id)?.lang_id, file_meta(doc_id)?.node_count)`.  For an
    /// out-of-range `doc_id` it returns the same `Err(IndexCorrupted)` as
    /// `file_meta`.
    fn file_lang_and_node_count(&self, doc_id: u32) -> Result<(u8, u32)> {
        let m = self.file_meta(doc_id)?;
        Ok((m.lang_id, m.node_count))
    }
}

impl AstPostingSource for AstIndexReader {
    fn lookup_bigram(&self, b: AstBigram) -> Result<Vec<AstPosting>> {
        AstIndexReader::lookup_bigram(self, b)
    }
    fn lookup_trigram(&self, t: AstTrigram) -> Result<Vec<AstPosting>> {
        AstIndexReader::lookup_trigram(self, t)
    }
    fn file_meta(&self, doc_id: u32) -> Result<AstFileMetaEntry> {
        AstIndexReader::file_meta(self, doc_id)
    }
    fn avg_node_count(&self) -> f32 {
        AstIndexReader::avg_node_count(self)
    }
    fn file_count(&self) -> u32 {
        AstIndexReader::file_count(self)
    }
    /// Override with the fast path: decode only `lang_id` + `node_count` (5 bytes).
    fn file_lang_and_node_count(&self, doc_id: u32) -> Result<(u8, u32)> {
        AstIndexReader::file_lang_and_node_count(self, doc_id)
    }
}
