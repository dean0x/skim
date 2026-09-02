//! Shared I/O utilities for the on-disk store builders and readers.
//!
//! Centralised here so that `ast_index/store/builder.rs`,
//! `index/builder.rs`, `cochange/builder.rs`, and the two integrity probe
//! functions (`AstIndexReader::index_integrity`,
//! `NgramIndexReader::lexical_index_integrity`) all share one implementation
//! and cannot drift apart.

use std::path::Path;

use tempfile::NamedTempFile;

use crate::Result;

/// Atomically write `data` to `path` using a temp file in `dir`.
///
/// Strategy: `NamedTempFile::new_in` (temp file in the same directory as the
/// target, avoiding cross-device rename) → `write_all` → `sync_all` (flush
/// kernel page cache to durable storage) → set `0o600` (owner-only)
/// permissions on Unix → `persist` (atomic rename).
///
/// `0o600` matches the temporal store (`temporal/storage.rs`) so every
/// `.skim/` index artifact is owner-readable only; the index can embed paths
/// and code structure, so it should not be world-readable on shared hosts.
///
/// A reader that finds the target file present can therefore assume it is
/// complete and durably written.  Without a subsequent directory fsync the
/// rename itself may be unordered on some filesystems (e.g. ext4 without
/// `data=journal`) after a power loss, but that is a caller-level concern and
/// consistent with the posture of all three sibling builders.
///
/// # Errors
///
/// Returns [`crate::SearchError::Io`] on any I/O failure (temp file creation,
/// write, sync, chmod, or rename).
pub(crate) fn atomic_write(dir: &Path, path: &Path, data: &[u8]) -> Result<()> {
    let mut tmp = NamedTempFile::new_in(dir)?;
    use std::io::Write as _;
    tmp.write_all(data)?;
    tmp.as_file().sync_all()?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600))?;
    }

    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Steps 1–4 shared by both index integrity probes:
/// open the `.skidx` file, fill `buf` with up to `buf.len()` bytes, validate
/// that at least 6 bytes were read, validate the magic bytes, and return the
/// decoded version word.
///
/// Returns `(file, version, bytes_filled)` where `file` is the still-open
/// handle (usable for `file.metadata()?.len()` in step 6 of the caller,
/// which avoids a TOCTOU gap and surfaces a real I/O error rather than
/// silently substituting 0 on stat failure).
///
/// The caller is responsible for the foreign-version early exit (AD-414-6):
///
/// ```text
/// let (mut file, version, n) =
///     probe_index_header(&idx_path, &mut buf, "foo.skidx", MAGIC)?;
/// if version != FORMAT_VERSION {
///     return Ok(version);  // foreign layout — size checks must not proceed
/// }
/// ```
///
/// # Errors
///
/// - [`crate::SearchError::Io`] if the file at `path` cannot be opened.
/// - [`crate::SearchError::IndexCorrupted`] if fewer than 6 bytes could be
///   read from the file, or the first 4 bytes do not equal `expected_magic`.
pub(crate) fn probe_index_header(
    path: &Path,
    buf: &mut [u8],
    label: &str,
    expected_magic: &[u8; 4],
) -> Result<(std::fs::File, u16, usize)> {
    use std::io::Read as _;

    // Step 1: open and fill the buffer.
    //
    // `Read::read` may return fewer bytes than requested even when more are
    // available, so a single call cannot distinguish "short file" from "short
    // read" — treating the latter as corruption costs the user a full rebuild
    // of a healthy index.  Fill the buffer explicitly; bounded by construction:
    // every iteration either breaks on EOF (0-byte read) or advances `n` by at
    // least one byte toward `buf.len()`.
    let mut file = std::fs::File::open(path)?;
    let mut n = 0usize;
    while n < buf.len() {
        match file.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }

    // Step 2: need at least 6 bytes for magic + version.
    if n < 6 {
        return Err(crate::SearchError::IndexCorrupted(format!(
            "{label} too short: need 6 bytes for magic+version, got {n}"
        )));
    }

    // Step 3: validate magic.
    let magic = &buf[0..4];
    if magic != expected_magic.as_ref() {
        return Err(crate::SearchError::IndexCorrupted(format!(
            "{label} bad magic: expected {:?}, got {:?}",
            expected_magic, magic
        )));
    }

    // Step 4: decode version.
    let version = u16::from_le_bytes([buf[4], buf[5]]);

    Ok((file, version, n))
}
