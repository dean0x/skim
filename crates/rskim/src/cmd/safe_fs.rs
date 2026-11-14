//! File helpers for paths that skim does not own: files of a work tree, hook
//! scripts and instruction files.
//!
//! The same three rules hold for every caller, so there is one copy of each:
//!
//! - [`open_no_follow`] opens for reading and refuses a link at the last
//!   component. On unix it does not block on a FIFO.
//! - [`read_regular_bounded`] opens that way, checks the opened handle (a
//!   regular file, within the size bound) and then reads at most the bound.
//!   The text step, [`BoundedFile::into_text`], is strict UTF-8: it never
//!   replaces a byte.
//! - [`write_atomic`] writes a new file next to the target and renames it over
//!   the target, so a reader sees the old file or the new file and never half
//!   of one.
//!
//! The parent folders of a path are not checked here. A caller that must stay
//! inside a root checks them before it opens.

use std::fs::{File, Permissions};
use std::io::{self, Read, Write as _};
use std::path::Path;

use tempfile::NamedTempFile;

// ============================================================================
// Open without following a link
// ============================================================================

/// Why [`open_no_follow`] failed.
#[derive(Debug)]
pub(crate) enum OpenFailure {
    /// The last component is a link.
    Symlink,
    /// The path is gone.
    Missing,
    /// Any other failure of the open.
    Other(io::Error),
}

/// Open for reading. On unix the open refuses a link at the last component
/// (`O_NOFOLLOW`) and does not block on a FIFO (`O_NONBLOCK`); the caller
/// checks the handle with `fstat` before it reads. `O_NONBLOCK` has no effect
/// on a regular file, so no read of a checked handle is non-blocking.
#[cfg(unix)]
pub(crate) fn open_no_follow(path: &Path) -> Result<File, OpenFailure> {
    use std::os::unix::fs::OpenOptionsExt as _;

    File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(code) if code == libc::ELOOP => OpenFailure::Symlink,
            _ if e.kind() == io::ErrorKind::NotFound => OpenFailure::Missing,
            _ => OpenFailure::Other(e),
        })
}

/// Open for reading where `O_NOFOLLOW` does not exist: check the link status
/// first, then open, then let the caller check the handle.
#[cfg(not(unix))]
pub(crate) fn open_no_follow(path: &Path) -> Result<File, OpenFailure> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => return Err(OpenFailure::Symlink),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(OpenFailure::Missing),
        Err(e) => return Err(OpenFailure::Other(e)),
    }
    File::open(path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            OpenFailure::Missing
        } else {
            OpenFailure::Other(e)
        }
    })
}

// ============================================================================
// Bounded read
// ============================================================================

/// Why a bounded read gave no bytes or no text.
#[derive(Debug)]
pub(crate) enum ReadFailure {
    /// The last component is a link.
    Symlink,
    /// The path is gone.
    Missing,
    /// The path is a folder.
    Folder,
    /// The path is not a regular file (a FIFO, a device, a socket).
    NotRegular,
    /// The file holds more than the bound.
    TooLarge,
    /// The bytes are not strict UTF-8.
    NotUtf8,
    /// The open failed for a reason that is none of the above.
    Open(io::Error),
    /// The check of the opened handle (`fstat`) failed.
    Inspect(io::Error),
    /// A read of the opened handle failed.
    Read(io::Error),
}

impl From<OpenFailure> for ReadFailure {
    fn from(failure: OpenFailure) -> Self {
        match failure {
            OpenFailure::Symlink => Self::Symlink,
            OpenFailure::Missing => Self::Missing,
            OpenFailure::Other(e) => Self::Open(e),
        }
    }
}

/// A regular file read within a bound: its bytes and the permissions it had
/// when it was opened.
#[derive(Debug)]
pub(crate) struct BoundedFile {
    pub(crate) bytes: Vec<u8>,
    pub(crate) permissions: Permissions,
}

impl BoundedFile {
    /// The text and the permissions of the file.
    ///
    /// # Errors
    ///
    /// [`ReadFailure::NotUtf8`] when the bytes are not strict UTF-8. No byte is
    /// replaced.
    pub(crate) fn into_text(self) -> Result<(String, Permissions), ReadFailure> {
        String::from_utf8(self.bytes)
            .map(|text| (text, self.permissions))
            .map_err(|_| ReadFailure::NotUtf8)
    }
}

/// Read all of `reader`, which must hold at most `max` bytes.
///
/// The read stops after `max + 1` bytes, so an endless reader costs a bounded
/// read. `size_hint` is the size the reader is expected to have; it only sizes
/// the buffer.
///
/// # Errors
///
/// [`ReadFailure::TooLarge`] when the reader holds more than `max` bytes, and
/// [`ReadFailure::Read`] when a read fails.
pub(crate) fn read_capped<R: Read>(
    reader: R,
    max: u64,
    size_hint: u64,
) -> Result<Vec<u8>, ReadFailure> {
    let mut bytes = Vec::with_capacity(usize::try_from(size_hint.min(max)).unwrap_or(0));
    reader
        .take(max.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(ReadFailure::Read)?;
    if u64::try_from(bytes.len()).is_ok_and(|n| n <= max) {
        Ok(bytes)
    } else {
        Err(ReadFailure::TooLarge)
    }
}

/// Open `path` without following a link at the last component and without
/// blocking, check the handle, then read it within `max` bytes.
///
/// The size check on the handle skips the read of a file that is already too
/// large. The bound on the read itself still holds for a file that grew since
/// that check.
///
/// # Errors
///
/// A [`ReadFailure`] that names the reason: a link, a missing path, a folder,
/// a file that is not regular, a file larger than `max`, or an I/O error.
pub(crate) fn read_regular_bounded(path: &Path, max: u64) -> Result<BoundedFile, ReadFailure> {
    let file = open_no_follow(path)?;
    let meta = file.metadata().map_err(ReadFailure::Inspect)?;
    if meta.is_dir() {
        return Err(ReadFailure::Folder);
    }
    // A FIFO or a device is not text; reading it could block.
    if !meta.is_file() {
        return Err(ReadFailure::NotRegular);
    }
    if meta.len() > max {
        return Err(ReadFailure::TooLarge);
    }
    let bytes = read_capped(file, max, meta.len())?;
    Ok(BoundedFile {
        bytes,
        permissions: meta.permissions(),
    })
}

// ============================================================================
// Atomic write
// ============================================================================

/// Atomically write `content` to `path`, through a temporary file in the same
/// folder (so the rename stays on one filesystem).
///
/// The temporary name is random and the file is created exclusively, so no
/// name is known before the write and nothing at an earlier name is followed
/// or replaced. The folder of `path` must exist; this function creates none.
/// The rename replaces whatever is at `path`; a link at `path` is replaced, not
/// followed.
///
/// The file gets `permissions` when given. With `None` it keeps the tempfile
/// default, mode 0600 on unix, so a new file is private to its owner.
///
/// The data reaches the disk before the rename, so a power loss cannot leave
/// the old content of the target as an empty file.
///
/// # Errors
///
/// The I/O error of creating, writing, syncing or renaming. The target is not
/// changed when this returns an error.
pub(crate) fn write_atomic(
    path: &Path,
    content: &str,
    permissions: Option<Permissions>,
) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;

    let mut tmp = NamedTempFile::new_in(parent)?;
    tmp.write_all(content.as_bytes())?;
    if let Some(permissions) = permissions {
        std::fs::set_permissions(tmp.path(), permissions)?;
    }
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::rc::Rc;

    use tempfile::TempDir;

    use super::*;

    /// A reader that never ends. It counts the bytes it hands out and fails
    /// once it has handed out `hard_stop`, so a read without a bound ends in
    /// an error and not in a hang.
    struct Endless {
        served: Rc<Cell<u64>>,
        hard_stop: u64,
    }

    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.served.get() >= self.hard_stop {
                return Err(io::Error::other("read past the hard stop"));
            }
            let room = usize::try_from(self.hard_stop - self.served.get()).unwrap_or(usize::MAX);
            let n = buf.len().min(room);
            buf[..n].fill(b'a');
            self.served.set(self.served.get() + n as u64);
            Ok(n)
        }
    }

    // ------------------------------------------------------------------
    // read_capped
    // ------------------------------------------------------------------

    #[test]
    fn read_capped_stops_an_endless_reader_at_the_bound_and_reports_too_large() {
        let served = Rc::new(Cell::new(0));
        let reader = Endless {
            served: Rc::clone(&served),
            hard_stop: 1 << 20,
        };

        let result = read_capped(reader, 16, 0);

        assert!(
            matches!(result, Err(ReadFailure::TooLarge)),
            "got {result:?}"
        );
        assert_eq!(
            served.get(),
            17,
            "the read must stop one byte past the bound"
        );
    }

    #[test]
    fn read_capped_accepts_a_reader_of_exactly_the_bound() {
        let result = read_capped(&b"0123456789abcdef"[..], 16, 16);

        assert_eq!(result.unwrap(), b"0123456789abcdef");
    }

    #[test]
    fn read_capped_refuses_a_reader_one_byte_past_the_bound() {
        let result = read_capped(&b"0123456789abcdefg"[..], 16, 17);

        assert!(matches!(result, Err(ReadFailure::TooLarge)));
    }

    // ------------------------------------------------------------------
    // read_regular_bounded
    // ------------------------------------------------------------------

    #[test]
    fn read_regular_bounded_returns_the_bytes_of_a_regular_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("note.txt");
        fs::write(&path, "hello\n").unwrap();

        let file = read_regular_bounded(&path, 1024).unwrap();

        assert_eq!(file.bytes, b"hello\n");
    }

    #[test]
    fn read_regular_bounded_refuses_a_file_larger_than_the_bound() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("big.txt");
        fs::write(&path, "x".repeat(33)).unwrap();

        let result = read_regular_bounded(&path, 32);

        assert!(
            matches!(result, Err(ReadFailure::TooLarge)),
            "got {result:?}"
        );
    }

    #[test]
    fn read_regular_bounded_reports_a_missing_path() {
        let dir = TempDir::new().unwrap();

        let result = read_regular_bounded(&dir.path().join("absent"), 32);

        assert!(
            matches!(result, Err(ReadFailure::Missing)),
            "got {result:?}"
        );
    }

    #[test]
    fn read_regular_bounded_reports_a_folder() {
        let dir = TempDir::new().unwrap();

        let result = read_regular_bounded(dir.path(), 32);

        assert!(matches!(result, Err(ReadFailure::Folder)), "got {result:?}");
    }

    #[test]
    fn into_text_refuses_bytes_that_are_not_utf8_and_replaces_none() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("latin1.txt");
        fs::write(&path, [b'a', 0xE9, b'b']).unwrap();

        let file = read_regular_bounded(&path, 32).unwrap();
        assert_eq!(
            file.bytes,
            [b'a', 0xE9, b'b'],
            "the bytes are returned as they are"
        );

        let text = file.into_text();
        assert!(matches!(text, Err(ReadFailure::NotUtf8)), "got {text:?}");
    }

    #[test]
    fn into_text_returns_the_text_and_the_permissions() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("note.txt");
        fs::write(&path, "caf\u{e9}\n").unwrap();

        let (text, permissions) = read_regular_bounded(&path, 32)
            .unwrap()
            .into_text()
            .unwrap();

        assert_eq!(text, "caf\u{e9}\n");
        assert_eq!(permissions, fs::metadata(&path).unwrap().permissions());
    }

    #[cfg(unix)]
    #[test]
    fn read_regular_bounded_does_not_follow_a_link_at_the_last_component() {
        let dir = TempDir::new().unwrap();
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, "outside\n").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let result = read_regular_bounded(&link, 1024);

        assert!(
            matches!(result, Err(ReadFailure::Symlink)),
            "got {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_regular_bounded_does_not_block_on_a_fifo() {
        let dir = TempDir::new().unwrap();
        let fifo = dir.path().join("pipe.txt");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success(), "mkfifo failed");

        // A blocking open of a FIFO with no writer never returns, so the open
        // runs on its own thread and the test waits for it with a timeout.
        let (sender, receiver) = std::sync::mpsc::channel();
        let path = fifo.clone();
        std::thread::spawn(move || {
            let _ = sender.send(read_regular_bounded(&path, 1024));
        });
        let result = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the open of a FIFO must not block");

        assert!(
            matches!(result, Err(ReadFailure::NotRegular)),
            "got {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn read_regular_bounded_returns_the_permissions_of_the_file() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("script.sh");
        fs::write(&path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&path, Permissions::from_mode(0o750)).unwrap();

        let file = read_regular_bounded(&path, 1024).unwrap();

        assert_eq!(file.permissions.mode() & 0o777, 0o750);
    }

    // ------------------------------------------------------------------
    // write_atomic
    // ------------------------------------------------------------------

    #[test]
    fn write_atomic_replaces_the_content_and_leaves_no_other_file() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        fs::write(&path, "old\n").unwrap();

        write_atomic(&path, "new\n", None).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["CLAUDE.md"], "no temporary file may stay behind");
    }

    #[test]
    fn write_atomic_does_not_create_the_folder_of_the_target() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("missing").join("CLAUDE.md");

        let result = write_atomic(&path, "new\n", None);

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(!dir.path().join("missing").exists());
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_creates_a_new_file_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");

        write_atomic(&path, "new\n", None).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_gives_the_file_the_permissions_it_is_given() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("post-commit");
        fs::write(&path, "old\n").unwrap();

        write_atomic(&path, "new\n", Some(Permissions::from_mode(0o755))).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_replaces_a_link_at_the_target_and_leaves_its_destination_alone() {
        let dir = TempDir::new().unwrap();
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, "outside\n").unwrap();
        let link = dir.path().join("CLAUDE.md");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        write_atomic(&link, "new\n", None).unwrap();

        assert_eq!(fs::read_to_string(&outside).unwrap(), "outside\n");
        assert!(
            !fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read_to_string(&link).unwrap(), "new\n");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_uses_no_fixed_temporary_name() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, "outside\n").unwrap();
        // Files at the names that a fixed-suffix writer would use.
        let planted_file = dir.path().join("CLAUDE.md.tmp");
        fs::write(&planted_file, "planted\n").unwrap();
        let planted_link = dir.path().join("CLAUDE.tmp");
        std::os::unix::fs::symlink(&outside, &planted_link).unwrap();

        write_atomic(&path, "new\n", None).unwrap();

        assert_eq!(fs::read_to_string(&planted_file).unwrap(), "planted\n");
        assert_eq!(fs::read_to_string(&outside).unwrap(), "outside\n");
        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
    }
}
