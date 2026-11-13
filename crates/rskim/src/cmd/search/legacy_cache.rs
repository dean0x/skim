//! Removal of the search index data that older versions of skim left in the cache.
//!
//! Skim 2.x built a search index for each project. Its files sat in one folder
//! per project, `<cache>/search/<id>/`, where `<id>` is 16 lowercase hex digits.
//! Nothing reads that data now. `skim --clear-cache` removes it and `skim doctor`
//! reports it. A query never touches it.
//!
//! # What cleanup removes
//!
//! Only the files in [`LEGACY_ARTIFACTS`] and the temp files of the old atomic
//! writer (see [`is_temp_artifact_name`]), and only when the file is a regular
//! file in a folder `<cache>/search/<id>/` with a valid `<id>`. Then it removes
//! each such folder that is empty, and `<cache>/search` when that is empty.
//! Nothing else goes: not an unknown file, not a link, not a folder with another
//! name, and not anything outside `<cache>/search`.
//!
//! # Links and owners
//!
//! No step follows a link below the cache folder. Each entry is inspected with
//! `symlink_metadata` or `DirEntry::metadata`, which do not follow a link, and
//! each path is built from the cache folder, the fixed name `search`, a validated
//! `<id>` and a listed name.
//!
//! `SKIM_CACHE_DIR` can name a folder that other users write to. So the cleanup
//! enters `<cache>/search`, and each project folder, only when the user that runs
//! it owns that folder, by the rule that git applies to a repository folder
//! (`gix::sec::identity`). A folder that is not owned, or whose owner cannot be
//! read, is counted (`foreign`) and left alone.
//!
//! The std library has no `unlinkat`. A process that can rename entries in a
//! folder that this user owns, between the inspection and the removal, could
//! still redirect that one removal. The checks above remove the cases where the
//! other user made the folder; they do not remove that window.
//!
//! # Bounds
//!
//! One pass reads at most [`MAX_SEARCH_ENTRIES`] entries of `<cache>/search` and
//! works on at most [`MAX_PROJECT_FOLDERS`] project folders. It reads at most
//! [`MAX_FOLDER_ENTRIES`] entries of each project folder. A kept entry (a child
//! that is not a project folder) spends only the first bound, so it cannot stop
//! the cleanup of the project folders that follow it. Every bound that fires is
//! reported in [`Skipped`] with exact counts.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The file names the old index wrote in a project folder.
///
/// The three `temporal.db-*` names are the journal files of the SQLite database
/// `temporal.db`, which the old writer opened in WAL mode. A crash leaves them
/// next to the database. The temp files of the old atomic writer are not listed:
/// their names are random, see [`is_temp_artifact_name`].
pub(crate) const LEGACY_ARTIFACTS: [&str; 15] = [
    "index.skidx",
    "index.skpost",
    "index.skfiles",
    "index.skverify",
    "ast_index.skidx",
    "ast_index.skpost",
    "ast_index.skcache",
    "ast_index.skverify",
    "temporal.db",
    "temporal.db-wal",
    "temporal.db-shm",
    "temporal.db-journal",
    "temporal.db.build_backoff",
    "tracked_union.cache",
    ".skim-build.lock",
];

/// The start of a temp file name from `tempfile::NamedTempFile::new_in`, which the
/// old atomic writer used to write each index file before it renamed it.
const TEMP_NAME_PREFIX: &str = ".tmp";

/// The number of random alphanumeric characters that follow [`TEMP_NAME_PREFIX`].
const TEMP_NAME_RANDOM_LEN: usize = 6;

/// Length of a project folder name: 16 lowercase hex digits.
const PROJECT_FOLDER_NAME_LEN: usize = 16;

/// Most entries of `<cache>/search` one pass reads. Every entry counts, so this
/// bound limits the listing and nothing else.
pub(crate) const MAX_SEARCH_ENTRIES: usize = 65_536;

/// Most project folders one pass works on. Each costs one listing and one removal
/// per file. A cache that holds more is cleaned over several runs.
pub(crate) const MAX_PROJECT_FOLDERS: usize = 4096;

/// Most entries of one project folder that a pass reads. The old writer left at
/// most the listed files and a few temp files.
pub(crate) const MAX_FOLDER_ENTRIES: usize = 1024;

/// Whether `name` is a temp file of the old atomic writer: `.tmp` and six ASCII
/// letters or digits. That is the default name rule of `NamedTempFile::new_in`.
fn is_temp_artifact_name(name: &OsStr) -> bool {
    name.to_str()
        .and_then(|name| name.strip_prefix(TEMP_NAME_PREFIX))
        .is_some_and(|random| {
            random.len() == TEMP_NAME_RANDOM_LEN
                && random.bytes().all(|b| b.is_ascii_alphanumeric())
        })
}

fn is_artifact_name(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| LEGACY_ARTIFACTS.contains(&name))
        || is_temp_artifact_name(name)
}

fn is_project_folder_name(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        name.len() == PROJECT_FOLDER_NAME_LEN
            && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// Whether the user that runs skim owns `path`: the rule that git applies to a
/// repository folder. A path that cannot be examined is not owned.
fn owned_by_current_user(path: &Path) -> bool {
    gix::sec::identity::is_path_owned_by_current_user(path).unwrap_or(false)
}

/// Who a pass acts for and where its bounds lie. Tests set a stand-in ownership
/// rule, because a test cannot give a folder to another user, and small bounds.
#[derive(Clone, Copy)]
struct Pass {
    /// Whether the user that runs the cleanup owns the folder at the path.
    owns: fn(&Path) -> bool,
    max_entries: usize,
    max_folders: usize,
}

impl Pass {
    fn standard() -> Self {
        Self {
            owns: owned_by_current_user,
            max_entries: MAX_SEARCH_ENTRIES,
            max_folders: MAX_PROJECT_FOLDERS,
        }
    }
}

/// What a pass did not inspect because a bound fired. The counts are exact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Skipped {
    /// Project folders past the first [`MAX_PROJECT_FOLDERS`].
    pub folders: u64,
    /// `<cache>/search` holds more than [`MAX_SEARCH_ENTRIES`] entries; the rest
    /// were not read.
    pub listing: bool,
    /// Project folders that hold more than [`MAX_FOLDER_ENTRIES`] entries; the
    /// entries past that bound were not read.
    pub crowded: u64,
}

impl Skipped {
    pub(crate) fn any(&self) -> bool {
        self.folders > 0 || self.listing || self.crowded > 0
    }

    /// One clause for each bound that fired, with the exact counts.
    pub(crate) fn clauses(&self) -> Vec<String> {
        let mut clauses = Vec::new();
        if self.folders > 0 {
            clauses.push(format!(
                "{} project folders past the first {MAX_PROJECT_FOLDERS} were not inspected",
                self.folders
            ));
        }
        if self.listing {
            clauses.push(format!(
                "only the first {MAX_SEARCH_ENTRIES} entries of the folder were read"
            ));
        }
        if self.crowded > 0 {
            clauses.push(format!(
                "{} project folders hold more than {MAX_FOLDER_ENTRIES} entries and only the first {MAX_FOLDER_ENTRIES} of each were read",
                self.crowded
            ));
        }
        clauses
    }
}

/// What a cleanup removed and what it left.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LegacyCleanup {
    /// Project folders from which at least one file was removed.
    pub folders: u64,
    /// Files removed.
    pub files: u64,
    /// Bytes in the files removed.
    pub bytes: u64,
    /// Places this pass freed inside its bounds: each project folder removed, and
    /// each file removed from a project folder that a bound cut short. A later
    /// pass reads past the bounds only as far as this count allows.
    pub freed: u64,
    /// Things under `<cache>/search` left in place: a `search` entry that is not
    /// a real folder, a child that is not a project folder, and a project folder
    /// that still holds something.
    pub kept: u64,
    /// Folders that another user owns, left alone: `<cache>/search` itself or a
    /// project folder.
    pub foreign: u64,
    /// What the bounds left uninspected.
    pub skipped: Skipped,
}

/// What `<cache>/search` is.
enum SearchDir {
    Absent,
    Folder(PathBuf),
    /// A link or a file: not ours to enter.
    NotAFolder,
    /// A real folder that another user owns: not ours to enter.
    Foreign,
}

/// The project folders of `<cache>/search`, and what else it holds.
struct SearchScan {
    /// At most [`Pass::max_folders`] folders that the current user owns.
    project_folders: Vec<PathBuf>,
    /// Entries that are not project folders: any other name, a link, a file.
    others: u64,
    /// Project folders that another user owns.
    foreign: u64,
    skipped: Skipped,
}

fn classify_search_dir(base: &Path, owns: fn(&Path) -> bool) -> io::Result<SearchDir> {
    let search = base.join("search");
    match fs::symlink_metadata(&search) {
        Ok(meta) if meta.is_dir() && owns(&search) => Ok(SearchDir::Folder(search)),
        Ok(meta) if meta.is_dir() => Ok(SearchDir::Foreign),
        Ok(_) => Ok(SearchDir::NotAFolder),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(SearchDir::Absent),
        Err(e) => Err(e),
    }
}

fn scan_search_dir(search: &Path, pass: Pass) -> io::Result<SearchScan> {
    let mut scan = SearchScan {
        project_folders: Vec::new(),
        others: 0,
        foreign: 0,
        skipped: Skipped::default(),
    };
    for (seen, entry) in fs::read_dir(search)?.enumerate() {
        if seen == pass.max_entries {
            scan.skipped.listing = true;
            break;
        }
        let entry = entry?;
        if !is_project_folder_name(&entry.file_name()) {
            scan.others += 1;
            continue;
        }
        // `DirEntry::metadata` does not follow a link, so a link to a folder is
        // not a folder here.
        match entry.metadata() {
            Ok(meta) if meta.is_dir() && !(pass.owns)(&entry.path()) => scan.foreign += 1,
            Ok(meta) if meta.is_dir() => {
                if scan.project_folders.len() < pass.max_folders {
                    scan.project_folders.push(entry.path());
                } else {
                    scan.skipped.folders += 1;
                }
            }
            _ => scan.others += 1,
        }
    }
    Ok(scan)
}

/// The regular files in `folder` that the old index wrote, with their sizes, and
/// whether the folder holds more entries than the bound lets a pass read.
fn artifacts_in(folder: &Path) -> (Vec<(PathBuf, u64)>, bool) {
    let Ok(entries) = fs::read_dir(folder) else {
        return (Vec::new(), false);
    };
    let mut entries = entries.fuse();
    let mut found = Vec::new();
    for entry in entries.by_ref().take(MAX_FOLDER_ENTRIES) {
        let Ok(entry) = entry else { continue };
        if !is_artifact_name(&entry.file_name()) {
            continue;
        }
        // Not a link, not a folder: `DirEntry::metadata` does not follow a link.
        if let Ok(meta) = entry.metadata()
            && meta.is_file()
        {
            found.push((entry.path(), meta.len()));
        }
    }
    (found, entries.next().is_some())
}

/// Remove the old search index data under the cache folder `base`.
///
/// See the module doc for exactly what goes. When `<base>/search` is a link or a
/// file, nothing is removed and `kept` is 1. When another user owns it, nothing
/// is removed and `foreign` is 1.
///
/// # Errors
///
/// Returns `Err` when `<base>/search` cannot be inspected or listed. A file or
/// folder that cannot be removed stays and is counted in `kept`.
pub(crate) fn remove_legacy_index_data(base: &Path) -> io::Result<LegacyCleanup> {
    remove_with(base, Pass::standard())
}

fn remove_with(base: &Path, pass: Pass) -> io::Result<LegacyCleanup> {
    match classify_search_dir(base, pass.owns)? {
        SearchDir::Absent => Ok(LegacyCleanup::default()),
        SearchDir::NotAFolder => Ok(LegacyCleanup {
            kept: 1,
            ..LegacyCleanup::default()
        }),
        SearchDir::Foreign => Ok(LegacyCleanup {
            foreign: 1,
            ..LegacyCleanup::default()
        }),
        SearchDir::Folder(search) => remove_in(&search, pass),
    }
}

/// The cleanup of the folder `search`, which is `<cache>/search` and is ours.
fn remove_in(search: &Path, pass: Pass) -> io::Result<LegacyCleanup> {
    let mut cleanup = LegacyCleanup::default();
    let scan = scan_search_dir(search, pass)?;
    cleanup.kept += scan.others;
    cleanup.foreign += scan.foreign;
    cleanup.skipped = scan.skipped;
    for folder in &scan.project_folders {
        let (artifacts, crowded) = artifacts_in(folder);
        cleanup.skipped.crowded += u64::from(crowded);
        let mut removed_here = 0_u64;
        for (path, bytes) in artifacts {
            if fs::remove_file(&path).is_ok() {
                removed_here += 1;
                cleanup.files += 1;
                cleanup.bytes += bytes;
            }
        }
        if removed_here > 0 {
            cleanup.folders += 1;
        }
        // `remove_dir` removes an empty folder and fails on any other.
        match fs::remove_dir(folder) {
            Ok(()) => cleanup.freed += 1,
            Err(_) => {
                cleanup.kept += 1;
                if crowded {
                    cleanup.freed += removed_here;
                }
            }
        }
    }
    // Same rule for `<base>/search`: it goes only when nothing is left in it.
    let _ = fs::remove_dir(search);
    Ok(cleanup)
}

/// What [`legacy_index_report`] found.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LegacyReport {
    /// Project folders with old index files, among the entries inspected.
    pub folders: u64,
    /// Bytes in those files.
    pub bytes: u64,
    /// Folders that another user owns, left alone: `<cache>/search` itself or a
    /// project folder.
    pub foreign: u64,
    /// What the bounds left uninspected. When any bound fired, `folders` and
    /// `bytes` are lower bounds.
    pub skipped: Skipped,
}

/// What [`legacy_index_report`] says about the cache folder.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum LegacyFinding {
    /// Old index files, or something the report could not inspect.
    Found(LegacyReport),
    /// `<cache>/search` could not be inspected or listed, so the report cannot
    /// say whether old index data is there.
    Unreadable(io::ErrorKind),
}

/// The project folders with old index files, and their size in bytes, under the
/// cache folder `base`. `None` when there are none, no folder of another user and
/// no entry went uninspected. It removes nothing.
///
/// Counts the same files that [`remove_legacy_index_data`] removes, inside the
/// same bounds.
pub(crate) fn legacy_index_report(base: &Path) -> Option<LegacyFinding> {
    report_with(base, Pass::standard())
}

fn report_with(base: &Path, pass: Pass) -> Option<LegacyFinding> {
    let search = match classify_search_dir(base, pass.owns) {
        Ok(SearchDir::Folder(search)) => search,
        Ok(SearchDir::Foreign) => {
            return Some(LegacyFinding::Found(LegacyReport {
                foreign: 1,
                ..LegacyReport::default()
            }));
        }
        Ok(SearchDir::Absent | SearchDir::NotAFolder) => return None,
        Err(e) => return Some(LegacyFinding::Unreadable(e.kind())),
    };
    report_in(&search, pass)
}

/// The report on the folder `search`, which is `<cache>/search` and is ours.
fn report_in(search: &Path, pass: Pass) -> Option<LegacyFinding> {
    let scan = match scan_search_dir(search, pass) {
        Ok(scan) => scan,
        Err(e) => return Some(LegacyFinding::Unreadable(e.kind())),
    };
    let mut report = LegacyReport {
        foreign: scan.foreign,
        skipped: scan.skipped,
        ..LegacyReport::default()
    };
    for folder in &scan.project_folders {
        let (artifacts, crowded) = artifacts_in(folder);
        report.skipped.crowded += u64::from(crowded);
        if !artifacts.is_empty() {
            report.folders += 1;
            report.bytes += artifacts.iter().map(|(_, len)| len).sum::<u64>();
        }
    }
    let nothing_to_say = report.folders == 0 && report.foreign == 0 && !report.skipped.any();
    (!nothing_to_say).then_some(LegacyFinding::Found(report))
}

/// `bytes` as megabytes (10^6 bytes) with one decimal, for the notices.
pub(crate) fn format_megabytes(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1_000_000.0)
}

/// The lines that [`clear_legacy_index_data`] prints.
#[derive(Debug, Default, PartialEq, Eq)]
struct CleanupLines {
    /// What was removed, for stdout.
    removed: Option<String>,
    /// The notes for stderr.
    notes: Vec<String>,
}

/// The remedy for a bound that fired. A new run helps only when this run freed a
/// place inside a bound (see [`LegacyCleanup::freed`]): an entry past the bound
/// takes that place. Kept entries free none, so a run that only met kept entries
/// would stop at the same place again.
fn rerun_remedy(cleanup: &LegacyCleanup, search: &Path) -> String {
    if cleanup.freed > 0 {
        "run skim --clear-cache again to clean the rest".to_string()
    } else {
        format!(
            "this run freed no place inside the bounds, so a new run stops at the same place; check {search:?} by hand"
        )
    }
}

fn cleanup_lines(base: &Path, result: &io::Result<LegacyCleanup>) -> CleanupLines {
    let search = base.join("search");
    let cleanup = match result {
        Ok(cleanup) => cleanup,
        Err(e) => {
            return CleanupLines {
                removed: None,
                notes: vec![format!("Note: could not remove old search index data: {e}")],
            };
        }
    };
    let mut lines = CleanupLines::default();
    if cleanup.files > 0 {
        lines.removed = Some(format!(
            "Removed old search index data ({} folders, {} MB)",
            cleanup.folders,
            format_megabytes(cleanup.bytes)
        ));
    }
    if cleanup.kept > 0 {
        lines.notes.push(format!(
            "Note: {} entries under {search:?} are not old index files and were left in place",
            cleanup.kept
        ));
    }
    if cleanup.foreign > 0 {
        lines.notes.push(format!(
            "Note: {} folders at or under {search:?} belong to another user and were left in place",
            cleanup.foreign
        ));
    }
    if cleanup.skipped.any() {
        lines.notes.push(format!(
            "Note: not every entry under {search:?} was inspected: {}; {}",
            cleanup.skipped.clauses().join("; "),
            rerun_remedy(cleanup, &search)
        ));
    }
    lines
}

/// Remove the old index data under the cache folder `base` and say what happened.
///
/// For `skim --clear-cache`. A failure is a note on stderr and does not change
/// the exit code: the parser cache was cleared before this ran.
pub(crate) fn clear_legacy_index_data(base: &Path) {
    let lines = cleanup_lines(base, &remove_legacy_index_data(base));
    if let Some(removed) = lines.removed {
        println!("{removed}");
    }
    for note in lines.notes {
        eprintln!("{note}");
    }
}

#[cfg(test)]
#[path = "legacy_cache_tests.rs"]
mod tests;
