//! [`CochangeMatrixReader`] — memory-mapped, read-only query layer for the
//! co-change matrix.
//!
//! # Memory layout
//!
//! The `.skcc` file is memory-mapped in its entirety. The layout is:
//!
//! ```text
//! [SkccHeader: 18 bytes]
//! [FileCommitEntry × file_count:  8 bytes each, sorted by file_id]
//! [PairEntry       × pair_count: 12 bytes each, sorted by (file_a, file_b)]
//! ```
//!
//! # Send + Sync
//!
//! `CochangeMatrixReader` is `Send + Sync` because:
//! - `Mmap` is `Send + Sync` on all platforms supported by `memmap2`.
//! - All fields are read-only after construction.
//! - There is no interior mutation.
//!
//! # SAFETY
//!
//! The mmap is created read-only.  If another process truncates or overwrites
//! `cochange.skcc` concurrently, behaviour is undefined — this is an inherent
//! constraint of mmap-based indexes.

use std::path::Path;

use memmap2::Mmap;

use super::format::{
    FILE_COMMIT_ENTRY_SIZE, HEADER_SIZE, PAIR_ENTRY_SIZE, compute_checksum, decode_file_commit,
    decode_header, lookup_pair,
};
use crate::{FileId, Result, SearchError};

// ============================================================================
// Reader struct
// ============================================================================

/// Memory-mapped, read-only query layer for a co-change matrix.
pub struct CochangeMatrixReader {
    // SAFETY: read-only after construction; see module-level SAFETY note.
    mmap: Mmap,
    /// Byte offset where the file-commit section begins (always `HEADER_SIZE`).
    fc_start: usize,
    /// Byte offset where the file-commit section ends / pair section begins.
    /// Computed once in `open()` with checked arithmetic and cached here so
    /// that `file_commit_slice` and `pairs_slice` never repeat multiplication
    /// that could overflow on 32-bit targets.
    fc_end: usize,
    /// Byte offset where the pair section ends (equals total file size).
    pairs_end: usize,
}

// CochangeMatrixReader is automatically Send + Sync because all fields
// (Mmap: Send+Sync, usize: Send+Sync) satisfy the auto-trait bounds.

impl CochangeMatrixReader {
    /// Open an existing co-change matrix from `dir`.
    ///
    /// Validates magic bytes, format version, file size, and CRC32 checksum
    /// before returning.
    ///
    /// # Errors
    ///
    /// - [`SearchError::Io`] if `cochange.skcc` cannot be opened.
    /// - [`SearchError::IndexCorrupted`] if validation fails.
    #[must_use = "dropping the reader immediately means no queries can be made"]
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("cochange.skcc");
        let file = std::fs::File::open(&path)?;

        // SAFETY: The file is not modified after mapping. See module-level note.
        let mmap = unsafe { Mmap::map(&file) }?;

        let header = decode_header(&mmap)?;

        // Validate size consistency.
        let fc_bytes = (header.file_count as usize)
            .checked_mul(FILE_COMMIT_ENTRY_SIZE)
            .ok_or_else(|| {
                SearchError::IndexCorrupted("file_count * FILE_COMMIT_ENTRY_SIZE overflow".into())
            })?;
        let pair_bytes = (header.pair_count as usize)
            .checked_mul(PAIR_ENTRY_SIZE)
            .ok_or_else(|| {
                SearchError::IndexCorrupted("pair_count * PAIR_ENTRY_SIZE overflow".into())
            })?;
        // Cache validated offsets: HEADER_SIZE → fc_end → pairs_end.
        // These are computed once here with checked arithmetic and stored as
        // struct fields so that `file_commit_slice` and `pairs_slice` never
        // need to repeat potentially-overflowing multiplication.
        let fc_start = HEADER_SIZE;
        let fc_end = fc_start
            .checked_add(fc_bytes)
            .ok_or_else(|| SearchError::IndexCorrupted("fc_end overflow".into()))?;
        let pairs_end = fc_end
            .checked_add(pair_bytes)
            .ok_or_else(|| SearchError::IndexCorrupted("pairs_end overflow".into()))?;
        let expected_size = pairs_end;

        if mmap.len() != expected_size {
            return Err(SearchError::IndexCorrupted(format!(
                "skcc size mismatch: expected {expected_size}, got {}",
                mmap.len()
            )));
        }

        // Verify CRC32 over file_commit ++ pair bytes.
        let payload = &mmap[HEADER_SIZE..expected_size];
        let actual_checksum = compute_checksum(payload);
        if actual_checksum != header.checksum {
            return Err(SearchError::IndexCorrupted(format!(
                "checksum mismatch: expected {:#010x}, got {:#010x}",
                header.checksum, actual_checksum
            )));
        }

        Ok(Self {
            mmap,
            fc_start,
            fc_end,
            pairs_end,
        })
    }

    // -----------------------------------------------------------------------
    // Public query API
    // -----------------------------------------------------------------------

    /// Return the co-change count for the pair `(a, b)`.
    ///
    /// Canonicalises the pair to `(min, max)` before lookup, so the caller
    /// can pass IDs in either order. Returns `0` for absent pairs.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::IndexCorrupted`] if the pair data is malformed.
    #[must_use = "ignoring the co-change count discards the query result"]
    pub fn pair_count(&self, a: FileId, b: FileId) -> Result<u32> {
        if a == b {
            return Ok(0);
        }
        let (lo, hi) = canonicalize(a, b);
        let pairs_data = self.pairs_slice();
        lookup_pair(pairs_data, lo, hi).map(|opt| opt.unwrap_or(0))
    }

    /// Compute Jaccard similarity between files `a` and `b`.
    ///
    /// `Jaccard(a, b) = count_ab / (count_a + count_b - count_ab)`
    ///
    /// Returns `0.0` for self-pairs, absent pairs, and zero denominators.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::IndexCorrupted`] if the underlying data is malformed.
    #[must_use = "ignoring the Jaccard similarity discards the query result"]
    pub fn jaccard(&self, a: FileId, b: FileId) -> Result<f64> {
        if a == b {
            return Ok(0.0);
        }
        let count_ab = self.pair_count(a, b)?;
        if count_ab == 0 {
            return Ok(0.0);
        }
        let count_a = self.file_commits(a)?;
        let count_b = self.file_commits(b)?;
        let denominator = u64::from(count_a) + u64::from(count_b) - u64::from(count_ab);
        if denominator == 0 {
            return Ok(0.0);
        }
        Ok(f64::from(count_ab) / denominator as f64)
    }

    /// Return all co-change partners for `file_id`, sorted by co-change count
    /// descending.
    ///
    /// # Complexity
    ///
    /// **O(pair_count)** — performs a full linear scan over all pair entries.
    /// At the `MAX_PAIRS` cap of 2,000,000 pairs this reads up to ~24 MB per
    /// call.  Pairs are stored sorted by `(file_a, file_b)`, so `file_a`
    /// matches form a contiguous range — a future optimisation could use
    /// binary search on the `file_a` dimension to reduce this to
    /// O(log(pair_count) + k) where k is the number of matching pairs.
    ///
    /// Returns an empty Vec for unknown `file_id` values.
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::IndexCorrupted`] if the pair data is malformed.
    pub fn pairs_for_file(&self, file_id: FileId) -> Result<Vec<(FileId, u32)>> {
        let id = file_id.0;
        let pairs_data = self.pairs_slice();
        let n = pairs_data.len() / PAIR_ENTRY_SIZE;
        let mut results: Vec<(FileId, u32)> = Vec::new();

        for i in 0..n {
            let offset = i * PAIR_ENTRY_SIZE;
            let entry = super::format::decode_pair(&pairs_data[offset..offset + PAIR_ENTRY_SIZE])?;
            if entry.file_a == id {
                results.push((FileId(entry.file_b), entry.count));
            } else if entry.file_b == id {
                results.push((FileId(entry.file_a), entry.count));
            }
        }

        // Sort by count descending; tie-break by FileId ascending for determinism.
        results.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        Ok(results)
    }

    /// Return the number of commits in which `file_id` appeared.
    ///
    /// Returns `0` for unknown `file_id` values (not present in the matrix).
    ///
    /// # Errors
    ///
    /// Returns [`SearchError::IndexCorrupted`] if the file-commit data is malformed.
    pub fn file_commits(&self, file_id: FileId) -> Result<u32> {
        let id = file_id.0;
        let fc_data = self.file_commit_slice();

        // Binary search over FileCommitEntry sorted by file_id.
        let n = fc_data.len() / FILE_COMMIT_ENTRY_SIZE;
        let mut lo = 0usize;
        let mut hi = n;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let offset = mid * FILE_COMMIT_ENTRY_SIZE;
            let entry = decode_file_commit(&fc_data[offset..offset + FILE_COMMIT_ENTRY_SIZE])?;
            match entry.file_id.cmp(&id) {
                std::cmp::Ordering::Equal => return Ok(entry.commit_count),
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        Ok(0)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Slice of file-commit entries within the mmap.
    ///
    /// Uses offsets cached at `open()` time — no arithmetic at call site.
    fn file_commit_slice(&self) -> &[u8] {
        &self.mmap[self.fc_start..self.fc_end]
    }

    /// Slice of pair entries within the mmap.
    ///
    /// Uses offsets cached at `open()` time — no arithmetic at call site.
    fn pairs_slice(&self) -> &[u8] {
        &self.mmap[self.fc_end..self.pairs_end]
    }
}

// ============================================================================
// Private helpers
// ============================================================================

/// Return `(min(a,b), max(a,b))` as `(u32, u32)`.
#[inline]
fn canonicalize(a: FileId, b: FileId) -> (u32, u32) {
    (a.0.min(b.0), a.0.max(b.0))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "reader_tests.rs"]
mod tests;
