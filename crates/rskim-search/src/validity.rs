//! Validity-marker caching for the two-file mmap'd indexes (#376, AD-376-1).
//!
//! # Why this exists
//!
//! Every `NgramIndexReader::open` / `AstIndexReader::open` re-hashes the **entire**
//! posting blob with CRC32 before any query runs (#364, ADR-006 desync guard).
//! On large corpora that fixed per-open cost dominated query latency (median
//! 57 ms / p90 77 ms; the floor scaled with `.skpost` size, not result count).
//!
//! The marker moves that one-shot integrity check **off the per-query hot path**:
//! after one successful verified open, a sidecar file (`index.skverify` for the
//! lexical reader, `ast_index.skverify` for the AST reader) records a compact
//! signature of both index files plus the header's verified CRC32 field.  On a
//! subsequent open, if that on-disk signature proves both files are byte-identical
//! to the previously-verified state, the full CRC32 is skipped.  On any marker
//! miss (absent, unreadable, garbage, or a signature that no longer matches the
//! files) the full CRC32 still runs and, on success, rewrites the marker.
//!
//! # Trust boundary (AD-376-2, ACCEPTED per ADR-006)
//!
//! The signature is `(idx_len, idx_mtime_ns, post_len, post_mtime_ns)` AND the
//! header's already-stored `checksum` **field** (read cheaply from the decoded
//! header — never recomputed here).  `mtime` + `len` detect any rewrite of either
//! file; carrying the checksum field means a marker minted for one index can never
//! validate a file whose header advertises a different checksum.
//!
//! The marker's purpose is **FileId consistency across opens**, i.e. caching a
//! prior successful verification — it is explicitly **NOT** a corruption detector.
//! The full CRC32 remains the desync/mis-rank integrity guard.  KNOWN LIMIT
//! (accepted, out of scope): a content byte-flip that simultaneously preserves
//! `len` AND `mtime` AND the header `checksum` field is served unverified (silent
//! mis-rank).  This is rare and a content-SHA sidecar would be a separate
//! follow-up if ever justified.  AC1 of #376 pins this exact skip so the boundary
//! is measurable, not implicit (PF-007).
//!
//! # On-disk format (AD-376-2)
//!
//! Fixed little-endian binary record (read on the hot path of every query, so a
//! binary record is trivially cheap and adds no parse/dependency cost; human
//! inspectability does not justify hot-path parse cost):
//!
//! ```text
//! [0..8]   idx_len        u64 LE
//! [8..24]  idx_mtime_ns   i128 LE   (nanoseconds relative to UNIX_EPOCH; signed for pre-epoch)
//! [24..32] post_len       u64 LE
//! [32..48] post_mtime_ns  i128 LE
//! [48..52] checksum       u32 LE    (header.checksum field, NOT a recompute)
//! ```
//!
//! # Robustness (AC6)
//!
//! Every read is best-effort: a truncated, garbage, zero-length, or unreadable
//! marker yields `None` and the caller falls through to the full CRC32 — a bad
//! derived cache must never fail `open()`.  Likewise a failed marker **write**
//! during `open()` is swallowed; the next open simply re-verifies.

use std::path::Path;

/// On-disk size of a [`ValidityMarker`] record, in bytes.
///
/// `8 (idx_len) + 16 (idx_mtime_ns) + 8 (post_len) + 16 (post_mtime_ns) + 4 (checksum)`.
pub(crate) const MARKER_SIZE: usize = 52;

/// Compact signature proving byte-identity of an index pair to a prior verified open.
///
/// See the module docs for the trust boundary (AD-376-2): equality of this record
/// between the on-disk marker and the freshly-`stat`'d files is what licenses
/// skipping the full-payload CRC32.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ValidityMarker {
    /// Byte length of the `.skidx` file.
    pub idx_len: u64,
    /// `.skidx` modification time in nanoseconds relative to `UNIX_EPOCH` (signed).
    pub idx_mtime_ns: i128,
    /// Byte length of the `.skpost` file.
    pub post_len: u64,
    /// `.skpost` modification time in nanoseconds relative to `UNIX_EPOCH` (signed).
    pub post_mtime_ns: i128,
    /// The header's verified CRC32 **field** (read, never recomputed — AD-376-2).
    pub checksum: u32,
}

impl ValidityMarker {
    /// Serialise into the fixed [`MARKER_SIZE`]-byte little-endian record.
    fn encode(&self) -> [u8; MARKER_SIZE] {
        let mut buf = [0u8; MARKER_SIZE];
        buf[0..8].copy_from_slice(&self.idx_len.to_le_bytes());
        buf[8..24].copy_from_slice(&self.idx_mtime_ns.to_le_bytes());
        buf[24..32].copy_from_slice(&self.post_len.to_le_bytes());
        buf[32..48].copy_from_slice(&self.post_mtime_ns.to_le_bytes());
        buf[48..52].copy_from_slice(&self.checksum.to_le_bytes());
        buf
    }

    /// Decode from a byte slice, or `None` if it is not exactly [`MARKER_SIZE`]
    /// bytes.  A wrong-length (truncated / zero-length / over-long) buffer is a
    /// marker miss, not an error (AC6).
    fn decode(data: &[u8]) -> Option<Self> {
        // Exact-length match: a truncated or padded marker is treated as a miss.
        let bytes: &[u8; MARKER_SIZE] = data.try_into().ok()?;
        Some(Self {
            idx_len: u64::from_le_bytes(bytes[0..8].try_into().ok()?),
            idx_mtime_ns: i128::from_le_bytes(bytes[8..24].try_into().ok()?),
            post_len: u64::from_le_bytes(bytes[24..32].try_into().ok()?),
            post_mtime_ns: i128::from_le_bytes(bytes[32..48].try_into().ok()?),
            checksum: u32::from_le_bytes(bytes[48..52].try_into().ok()?),
        })
    }
}

/// Convert a file's modification time to nanoseconds relative to `UNIX_EPOCH`.
///
/// STD-ONLY (AD-376-2): uses [`std::fs::Metadata::modified`].  Returns `None`
/// when the platform does not expose mtime, mirroring the best-effort posture of
/// the whole marker path.  A `SystemTime` before the epoch yields a negative
/// value, which is why the on-disk field is signed (`i128`).
fn mtime_ns(meta: &std::fs::Metadata) -> Option<i128> {
    let modified = meta.modified().ok()?;
    match modified.duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => i128::try_from(d.as_nanos()).ok(),
        // Pre-epoch mtime: represent as a negative offset.
        Err(e) => i128::try_from(e.duration().as_nanos()).ok().map(|n| -n),
    }
}

/// Build the current signature for an index pair by `stat`-ing both files and
/// pairing them with the header's verified `checksum` field.
///
/// Returns `None` if either file cannot be `stat`'d or its mtime is unavailable;
/// the caller then simply runs the full CRC32 (best-effort, AC6).  `checksum` is
/// the header field the caller already decoded — it is **never** recomputed here.
pub(crate) fn current_signature(
    idx_path: &Path,
    post_path: &Path,
    header_checksum: u32,
) -> Option<ValidityMarker> {
    let idx_meta = std::fs::metadata(idx_path).ok()?;
    let post_meta = std::fs::metadata(post_path).ok()?;
    Some(ValidityMarker {
        idx_len: idx_meta.len(),
        idx_mtime_ns: mtime_ns(&idx_meta)?,
        post_len: post_meta.len(),
        post_mtime_ns: mtime_ns(&post_meta)?,
        checksum: header_checksum,
    })
}

/// Read and decode the on-disk marker at `marker_path`.
///
/// Best-effort: returns `None` on any I/O error or if the bytes do not form a
/// well-formed [`MARKER_SIZE`]-byte record (absent, truncated, zero-length,
/// garbage, or unreadable — all marker misses per AC6).
pub(crate) fn read_marker(marker_path: &Path) -> Option<ValidityMarker> {
    let data = std::fs::read(marker_path).ok()?;
    ValidityMarker::decode(&data)
}

/// Best-effort write of `marker` to `marker_path` (atomic, owner-only 0o600 on
/// Unix) via [`crate::io_util::atomic_write`].
///
/// A failure here is intentionally swallowed (AC6): the marker is a derived
/// cache, so a failed write must never fail `open()`; the next open re-verifies
/// and re-attempts the write.  `dir` is the parent directory used for the
/// same-filesystem temp file.
pub(crate) fn write_marker_best_effort(dir: &Path, marker_path: &Path, marker: &ValidityMarker) {
    // Ignore the result deliberately — derived-cache write is best-effort.
    let _ = crate::io_util::atomic_write(dir, marker_path, &marker.encode());
}

/// Best-effort removal of the marker at `marker_path` (AD-376-4).
///
/// Called on rebuild / forced rewrite so a partial or aborted rebuild cannot
/// leave a stale marker that would validate the wrong bytes.  A missing file or
/// any removal error is ignored — the (len, mtime, checksum) signature already
/// self-invalidates on rewrite; this unlink is purely defensive.
pub(crate) fn unlink_marker_best_effort(marker_path: &Path) {
    let _ = std::fs::remove_file(marker_path);
}

#[cfg(test)]
#[path = "validity_tests.rs"]
mod tests;
