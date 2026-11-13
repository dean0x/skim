//! The file universe of `skim search`: which files a query reads, and what
//! happens to each one.
//!
//! # The set
//!
//! Every git-tracked regular file (read from the git index in process, with no
//! `git` subprocess; gitlinks and skip-worktree entries are dropped), plus the
//! files of an ignore-aware walk of the root (hidden files and ignored files
//! are skipped, links are not followed). There is no language allow-list, no
//! minified gate and no file cap. Outside a repository the walk is the whole
//! set. The caller applies the `-g` and `-t` filters to the listing; then each
//! remaining file is read and admitted, or counted under a [`LeftOut`] reason.
//!
//! A tracked file that is hidden or looks secret-bearing is listed like any
//! other file. [`SENSITIVE_PATH_POLICY`] and [`sensitive_admission`] are the one
//! place that rule lives.
//!
//! # Containment
//!
//! The index and the work tree are untrusted input. A tracked path below a
//! folder that is a link leading out of the root must not be read.
//! [`Containment`] canonicalizes each parent folder once, requires the result
//! to stay inside the canonical root, and opens the file by that canonical
//! parent plus the file name. The open does not follow a link at the last
//! component and does not block on a FIFO. The handle is checked (a regular
//! file, within the size bound) before a byte is read from it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;

use super::walk::{MAX_FILE_BYTES, configure_builder};

/// The number of leading bytes searched for a NUL byte. A NUL byte there makes
/// the file binary.
pub(super) const BINARY_PROBE_BYTES: usize = 8192;

// ============================================================================
// Reasons a candidate is left out
// ============================================================================

/// Why a candidate file is left out of the universe. [`LeftOut::key`] is the
/// `left_out` key of the JSON output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum LeftOut {
    /// A NUL byte in the first [`BINARY_PROBE_BYTES`] bytes.
    Binary,
    /// The content is not strict UTF-8.
    NotUtf8,
    /// More than `MAX_FILE_BYTES` bytes.
    TooLarge,
    /// An I/O error, a permission error, or a handle that is not a regular
    /// file (a FIFO, a device).
    Unreadable,
    /// Tracked, but absent from the work tree.
    Vanished,
    /// The real path leaves the canonical root, or the index names a path that
    /// is not a plain relative path.
    OutsideRoot,
    /// A tracked entry of mode `120000`, or a walked entry that is a link. It
    /// is counted and never read.
    Symlink,
}

impl LeftOut {
    /// Every reason, in the order of the JSON `left_out` object.
    pub(super) const ALL: [LeftOut; 7] = [
        LeftOut::Binary,
        LeftOut::NotUtf8,
        LeftOut::TooLarge,
        LeftOut::Unreadable,
        LeftOut::Vanished,
        LeftOut::OutsideRoot,
        LeftOut::Symlink,
    ];

    /// The name of the reason in the output.
    pub(super) fn key(self) -> &'static str {
        match self {
            LeftOut::Binary => "binary",
            LeftOut::NotUtf8 => "not_utf8",
            LeftOut::TooLarge => "too_large",
            LeftOut::Unreadable => "unreadable",
            LeftOut::Vanished => "vanished",
            LeftOut::OutsideRoot => "outside_root",
            LeftOut::Symlink => "symlink",
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// How many candidates each [`LeftOut`] reason left out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct LeftOutCounts([u64; LeftOut::ALL.len()]);

impl LeftOutCounts {
    /// Count one more candidate under `reason`.
    pub(super) fn add(&mut self, reason: LeftOut) {
        let slot = &mut self.0[reason.index()];
        *slot = slot.saturating_add(1);
    }

    /// The count of one reason.
    pub(super) fn get(&self, reason: LeftOut) -> u64 {
        self.0[reason.index()]
    }

    /// The count over all reasons.
    pub(super) fn total(&self) -> u64 {
        self.0.iter().fold(0u64, |sum, n| sum.saturating_add(*n))
    }

    /// `(reason, count)` for every reason, in output order.
    pub(super) fn iter(&self) -> impl Iterator<Item = (LeftOut, u64)> + '_ {
        LeftOut::ALL.into_iter().map(|r| (r, self.get(r)))
    }
}

// ============================================================================
// Hidden and secret-bearing files
// ============================================================================

/// What the search does with a tracked file whose name is hidden or looks
/// secret-bearing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SensitivePolicy {
    /// List it like any other tracked file.
    List,
}

/// The policy in force. Changing it is one edit here and one in
/// [`sensitive_admission`].
pub(super) const SENSITIVE_PATH_POLICY: SensitivePolicy = SensitivePolicy::List;

/// What [`sensitive_admission`] decides for a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Admission {
    /// Read the file.
    Admit,
}

/// Decide whether a candidate path is read under `policy`. Under
/// [`SensitivePolicy::List`] every path is admitted.
pub(super) fn sensitive_admission(_rel: &Path, policy: SensitivePolicy) -> Admission {
    match policy {
        SensitivePolicy::List => Admission::Admit,
    }
}

// ============================================================================
// Listing
// ============================================================================

/// How a candidate was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CandidateKind {
    /// A regular file, as far as the listing knows.
    File,
    /// A tracked entry of mode `120000`, or a walked link.
    Symlink,
}

/// One path of the universe, relative to the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Candidate {
    pub(super) rel: PathBuf,
    pub(super) kind: CandidateKind,
}

/// The candidates of a root, sorted in path order, and the candidates the
/// listing itself left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Listing {
    pub(super) candidates: Vec<Candidate>,
    /// Paths the listing could not offer: an unsafe index path
    /// (`outside_root`) or a walk error (`unreadable`).
    pub(super) left_out: LeftOutCounts,
}

/// Why the universe cannot be listed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UniverseError {
    /// The root is a repository whose index cannot be read. There is no
    /// partial universe: a half-listed repository would answer "no match" for
    /// files it never looked at.
    GitIndex { detail: String },
}

impl std::fmt::Display for UniverseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UniverseError::GitIndex { detail } => {
                write!(f, "cannot read the git index: {detail}")
            }
        }
    }
}

impl std::error::Error for UniverseError {}

/// One entry of the git index.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TrackedEntry {
    path: PathBuf,
    symlink: bool,
}

/// List the universe of `root`: the tracked files when `root` has a `.git`
/// entry, plus the ignore-aware walk.
///
/// # Errors
///
/// [`UniverseError::GitIndex`] when `root` is a repository and its index cannot
/// be opened or read.
pub(super) fn list_universe(root: &Path) -> Result<Listing, UniverseError> {
    let mut found: BTreeMap<PathBuf, CandidateKind> = BTreeMap::new();
    let mut left_out = LeftOutCounts::default();

    if root.join(".git").exists() {
        let (safe, unsafe_paths) = split_safe(tracked_entries(root)?);
        for _ in 0..unsafe_paths {
            left_out.add(LeftOut::OutsideRoot);
        }
        for entry in safe {
            let kind = if entry.symlink {
                CandidateKind::Symlink
            } else {
                CandidateKind::File
            };
            found.insert(entry.path, kind);
        }
    }
    walk_entries(root, &mut found, &mut left_out);

    Ok(Listing {
        candidates: found
            .into_iter()
            .map(|(rel, kind)| Candidate { rel, kind })
            .collect(),
        left_out,
    })
}

impl Listing {
    /// Keep the candidates for which `keep` is true. A candidate the filter
    /// drops is not searched and not counted.
    pub(super) fn retain(&mut self, mut keep: impl FnMut(&Path) -> bool) {
        self.candidates.retain(|c| keep(&c.rel));
    }
}

/// Split index entries into those with a plain relative path and the count of
/// the rest. A path that is empty, absolute or holds a `..` component leaves
/// the root; it is counted and never joined to the root.
fn split_safe(entries: Vec<TrackedEntry>) -> (Vec<TrackedEntry>, u64) {
    let (safe, rest): (Vec<_>, Vec<_>) = entries.into_iter().partition(|e| {
        !e.path.as_os_str().is_empty() && crate::cmd::is_repo_relative_safe(&e.path)
    });
    (safe, u64::try_from(rest.len()).unwrap_or(u64::MAX))
}

/// The entries of the git index of `root`, one per path, in index order.
/// Gitlinks and skip-worktree entries are dropped.
fn tracked_entries(root: &Path) -> Result<Vec<TrackedEntry>, UniverseError> {
    use gix::bstr::ByteSlice as _;

    let fail = |what: &str, e: &dyn std::fmt::Display| UniverseError::GitIndex {
        detail: format!("{what}: {e}"),
    };
    let repo = gix::open(root).map_err(|e| fail("open", &e))?;
    // A repository with nothing staged has no index file: that is an empty
    // universe of tracked files, not a failure.
    let index = repo
        .index_or_empty()
        .map_err(|e| fail("read the index", &e))?;
    let state = &*index;

    let mut by_path: BTreeMap<PathBuf, bool> = BTreeMap::new();
    for entry in state.entries() {
        if entry.mode == gix::index::entry::Mode::COMMIT
            || entry
                .flags
                .contains(gix::index::entry::Flags::SKIP_WORKTREE)
        {
            continue;
        }
        let raw = entry.path(state);
        let path = PathBuf::from(raw.to_os_str_lossy().into_owned());
        by_path.insert(path, entry.mode == gix::index::entry::Mode::SYMLINK);
    }
    Ok(by_path
        .into_iter()
        .map(|(path, symlink)| TrackedEntry { path, symlink })
        .collect())
}

/// Add the files of the ignore-aware walk to `found`. A path the index
/// already lists keeps its entry. A walk error counts as `unreadable`.
fn walk_entries(
    root: &Path,
    found: &mut BTreeMap<PathBuf, CandidateKind>,
    left_out: &mut LeftOutCounts,
) {
    let mut builder = WalkBuilder::new(root);
    configure_builder(&mut builder);
    for item in builder.build() {
        let entry = match item {
            Ok(entry) => entry,
            Err(e) => {
                crate::debug_log!("skim search: walk error: {e}");
                left_out.add(LeftOut::Unreadable);
                continue;
            }
        };
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        let kind = if file_type.is_symlink() {
            CandidateKind::Symlink
        } else if file_type.is_file() {
            CandidateKind::File
        } else {
            continue;
        };
        if let Ok(rel) = entry.path().strip_prefix(root) {
            found.entry(rel.to_path_buf()).or_insert(kind);
        }
    }
}

// ============================================================================
// Reading
// ============================================================================

/// What reading one candidate gave.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Read {
    /// The text of an admitted file.
    Text(String),
    /// The file is left out, for this reason.
    LeftOut(LeftOut),
    /// Not a file at all: a folder where the index lists a file (a nested
    /// repository). It is not counted.
    NotAFile,
}

/// The canonical root and the canonical form of every parent folder of the
/// candidates, so that each folder is canonicalized once.
#[derive(Debug)]
pub(super) struct Containment {
    canonical_root: PathBuf,
    parents: HashMap<PathBuf, Result<PathBuf, io::ErrorKind>>,
}

impl Containment {
    /// Canonicalize `root` and the parent folder of each candidate. The
    /// folders are canonicalized on `pool`; the number of folders bounds the
    /// work.
    ///
    /// # Errors
    ///
    /// The I/O error of canonicalizing `root`.
    pub(super) fn new(
        root: &Path,
        candidates: &[Candidate],
        pool: &rayon::ThreadPool,
    ) -> io::Result<Self> {
        use rayon::prelude::*;

        let canonical_root = root.canonicalize()?;
        let folders: BTreeSet<&Path> = candidates
            .iter()
            .filter(|c| c.kind == CandidateKind::File)
            .map(|c| parent_of(&c.rel))
            .collect();
        let folders: Vec<&Path> = folders.into_iter().collect();
        let parents = pool.install(|| {
            folders
                .par_iter()
                .map(|folder| {
                    let real = root.join(folder).canonicalize().map_err(|e| e.kind());
                    (folder.to_path_buf(), real)
                })
                .collect()
        });
        Ok(Containment {
            canonical_root,
            parents,
        })
    }

    /// Read one candidate and admit it or name the reason it is left out.
    pub(super) fn read(&self, candidate: &Candidate) -> Read {
        if candidate.kind == CandidateKind::Symlink {
            return Read::LeftOut(LeftOut::Symlink);
        }
        let Admission::Admit = sensitive_admission(&candidate.rel, SENSITIVE_PATH_POLICY);
        let Some(name) = candidate.rel.file_name() else {
            return Read::LeftOut(LeftOut::OutsideRoot);
        };
        let folder = match self.parents.get(parent_of(&candidate.rel)) {
            Some(Ok(real)) if real.starts_with(&self.canonical_root) => real,
            Some(Ok(_)) => return Read::LeftOut(LeftOut::OutsideRoot),
            Some(Err(io::ErrorKind::NotFound)) => return Read::LeftOut(LeftOut::Vanished),
            Some(Err(_)) | None => return Read::LeftOut(LeftOut::Unreadable),
        };
        read_file(&folder.join(name))
    }
}

/// The folder part of a root-relative path; the empty path for a top-level
/// file.
fn parent_of(rel: &Path) -> &Path {
    rel.parent().unwrap_or(Path::new(""))
}

/// Open `path` without following a link at the last component and without
/// blocking, check the handle, then read it within the size bound.
fn read_file(path: &Path) -> Read {
    let file = match open_no_follow(path) {
        Ok(file) => file,
        Err(OpenFailure::Symlink) => return Read::LeftOut(LeftOut::Symlink),
        Err(OpenFailure::Missing) => return Read::LeftOut(LeftOut::Vanished),
        Err(OpenFailure::Other) => return Read::LeftOut(LeftOut::Unreadable),
    };
    let meta = match file.metadata() {
        Ok(meta) => meta,
        Err(_) => return Read::LeftOut(LeftOut::Unreadable),
    };
    if meta.is_dir() {
        return Read::NotAFile;
    }
    // A FIFO or a device is not text; reading it could block.
    if !meta.is_file() {
        return Read::LeftOut(LeftOut::Unreadable);
    }
    if meta.len() > MAX_FILE_BYTES {
        return Read::LeftOut(LeftOut::TooLarge);
    }
    // A bounded read: a file that grew since the metadata call is cut at the
    // bound, then refused.
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
    if file
        .take(MAX_FILE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Read::LeftOut(LeftOut::Unreadable);
    }
    if !u64::try_from(bytes.len()).is_ok_and(|n| n <= MAX_FILE_BYTES) {
        return Read::LeftOut(LeftOut::TooLarge);
    }
    if bytes[..bytes.len().min(BINARY_PROBE_BYTES)].contains(&0) {
        return Read::LeftOut(LeftOut::Binary);
    }
    match String::from_utf8(bytes) {
        Ok(text) => Read::Text(text),
        Err(_) => Read::LeftOut(LeftOut::NotUtf8),
    }
}

/// Why an open failed, in the terms the universe counts.
enum OpenFailure {
    /// The last component is a link.
    Symlink,
    /// The path is gone.
    Missing,
    Other,
}

/// Open for reading. On unix the open refuses a link at the last component
/// (`O_NOFOLLOW`) and does not block on a FIFO (`O_NONBLOCK`); the caller
/// checks the handle with `fstat` before it reads. `O_NONBLOCK` has no effect
/// on a regular file, so no read of a checked handle is non-blocking.
#[cfg(unix)]
fn open_no_follow(path: &Path) -> Result<File, OpenFailure> {
    use std::os::unix::fs::OpenOptionsExt as _;

    File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .map_err(|e| match e.raw_os_error() {
            Some(code) if code == libc::ELOOP => OpenFailure::Symlink,
            _ if e.kind() == io::ErrorKind::NotFound => OpenFailure::Missing,
            _ => OpenFailure::Other,
        })
}

/// Open for reading where `O_NOFOLLOW` does not exist: check the link status
/// first, then open, then let the caller check the handle.
#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> Result<File, OpenFailure> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => return Err(OpenFailure::Symlink),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(OpenFailure::Missing),
        Err(_) => return Err(OpenFailure::Other),
    }
    File::open(path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            OpenFailure::Missing
        } else {
            OpenFailure::Other
        }
    })
}

#[cfg(test)]
#[path = "universe_tests.rs"]
mod tests;
