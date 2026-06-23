//! Shared I/O utilities for the on-disk store builders.
//!
//! Centralised here so that `ast_index/store/builder.rs`,
//! `index/builder.rs`, and `cochange/builder.rs` all share one
//! implementation and cannot drift apart.

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
