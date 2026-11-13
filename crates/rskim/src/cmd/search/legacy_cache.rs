//! Removal of the search index data that older versions of skim left in the cache.
//!
//! Skim 2.x built a search index for each project. Its files sat in one folder
//! per project, `<cache>/search/<id>/`, where `<id>` is 16 lowercase hex digits.
//! Nothing reads that data now. `skim --clear-cache` removes it and `skim doctor`
//! reports it. A query never touches it.
//!
//! # What cleanup removes
//!
//! Only the files in [`LEGACY_ARTIFACTS`], and only when the file is a regular
//! file in a folder `<cache>/search/<id>/` with a valid `<id>`. Then it removes
//! each such folder that is empty, and `<cache>/search` when that is empty.
//! Nothing else goes: not an unknown file, not a link, not a folder with another
//! name, and not anything outside `<cache>/search`.
//!
//! No step follows a link below the cache folder. Each entry is inspected with
//! `symlink_metadata` or `DirEntry::file_type`, which do not follow a link, and
//! each path is built from the cache folder, the fixed name `search`, a validated
//! `<id>` and a listed name. The std library has no `unlinkat`, so a person who
//! can swap a folder for a link between the inspection and the removal could
//! still redirect that one removal. That person already owns the cache folder.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The file names the old index wrote in a project folder, and no other.
pub(crate) const LEGACY_ARTIFACTS: [&str; 12] = [
    "index.skidx",
    "index.skpost",
    "index.skfiles",
    "index.skverify",
    "ast_index.skidx",
    "ast_index.skpost",
    "ast_index.skcache",
    "ast_index.skverify",
    "temporal.db",
    "temporal.db.build_backoff",
    "tracked_union.cache",
    ".skim-build.lock",
];

/// Length of a project folder name: 16 lowercase hex digits.
const PROJECT_FOLDER_NAME_LEN: usize = 16;

/// Most entries of `<cache>/search` one pass inspects. A cache that holds more
/// project folders is cleaned over several runs.
const MAX_SEARCH_ENTRIES: usize = 4096;

/// What a cleanup removed and what it left.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct LegacyCleanup {
    /// Project folders from which at least one file was removed.
    pub folders: u64,
    /// Files removed.
    pub files: u64,
    /// Bytes in the files removed.
    pub bytes: u64,
    /// Things under `<cache>/search` left in place: a `search` entry that is not
    /// a real folder, a child that is not a project folder, and a project folder
    /// that still holds something.
    pub kept: u64,
    /// `<cache>/search` holds more than [`MAX_SEARCH_ENTRIES`] entries. The
    /// entries past the bound were not inspected; a next run cleans them.
    pub more: bool,
}

/// What `<cache>/search` is.
enum SearchDir {
    Absent,
    Folder(PathBuf),
    /// A link or a file: not ours to enter.
    NotAFolder,
}

/// The project folders of `<cache>/search`, and how many other entries it holds.
struct SearchScan {
    project_folders: Vec<PathBuf>,
    others: u64,
    /// More entries follow the first [`MAX_SEARCH_ENTRIES`]; they were not
    /// inspected.
    more: bool,
}

fn classify_search_dir(base: &Path) -> io::Result<SearchDir> {
    let search = base.join("search");
    match fs::symlink_metadata(&search) {
        Ok(meta) if meta.is_dir() => Ok(SearchDir::Folder(search)),
        Ok(_) => Ok(SearchDir::NotAFolder),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(SearchDir::Absent),
        Err(e) => Err(e),
    }
}

fn is_project_folder_name(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        name.len() == PROJECT_FOLDER_NAME_LEN
            && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

fn scan_search_dir(search: &Path) -> io::Result<SearchScan> {
    let mut scan = SearchScan {
        project_folders: Vec::new(),
        others: 0,
        more: false,
    };
    for (seen, entry) in fs::read_dir(search)?.enumerate() {
        if seen == MAX_SEARCH_ENTRIES {
            scan.more = true;
            break;
        }
        let entry = entry?;
        // `file_type` does not follow a link, so a link to a folder is not a folder here.
        let is_real_folder = entry.file_type().is_ok_and(|kind| kind.is_dir());
        if is_real_folder && is_project_folder_name(&entry.file_name()) {
            scan.project_folders.push(entry.path());
        } else {
            scan.others += 1;
        }
    }
    Ok(scan)
}

/// The regular files in `folder` that carry a listed name, with their sizes.
fn artifacts_in(folder: &Path) -> Vec<(PathBuf, u64)> {
    LEGACY_ARTIFACTS
        .iter()
        .filter_map(|name| {
            let path = folder.join(name);
            let meta = fs::symlink_metadata(&path).ok()?;
            meta.is_file().then_some((path, meta.len()))
        })
        .collect()
}

/// Remove the old search index data under the cache folder `base`.
///
/// See the module doc for exactly what goes. When `<base>/search` is a link or a
/// file, nothing is removed and `kept` is 1.
///
/// # Errors
///
/// Returns `Err` when `<base>/search` cannot be inspected or listed. A file or
/// folder that cannot be removed stays and is counted in `kept`.
pub(crate) fn remove_legacy_index_data(base: &Path) -> io::Result<LegacyCleanup> {
    let mut cleanup = LegacyCleanup::default();
    let search = match classify_search_dir(base)? {
        SearchDir::Absent => return Ok(cleanup),
        SearchDir::NotAFolder => {
            cleanup.kept = 1;
            return Ok(cleanup);
        }
        SearchDir::Folder(search) => search,
    };
    let scan = scan_search_dir(&search)?;
    cleanup.kept += scan.others;
    cleanup.more = scan.more;
    for folder in &scan.project_folders {
        let mut removed_here = 0_u64;
        for (path, bytes) in artifacts_in(folder) {
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
        if fs::remove_dir(folder).is_err() {
            cleanup.kept += 1;
        }
    }
    // Same rule for `<base>/search`: it goes only when nothing is left in it.
    let _ = fs::remove_dir(&search);
    Ok(cleanup)
}

/// The project folders with old index files, and their size in bytes, under the
/// cache folder `base`. `None` when there are none. It removes nothing.
///
/// Counts the same files that [`remove_legacy_index_data`] removes.
pub(crate) fn legacy_index_report(base: &Path) -> Option<(u64, u64)> {
    let Ok(SearchDir::Folder(search)) = classify_search_dir(base) else {
        return None;
    };
    let scan = scan_search_dir(&search).ok()?;
    let (mut folders, mut bytes) = (0_u64, 0_u64);
    for folder in &scan.project_folders {
        let artifacts = artifacts_in(folder);
        if !artifacts.is_empty() {
            folders += 1;
            bytes += artifacts.iter().map(|(_, len)| len).sum::<u64>();
        }
    }
    (folders > 0).then_some((folders, bytes))
}

/// `bytes` as megabytes (10^6 bytes) with one decimal, for the notices.
pub(crate) fn format_megabytes(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1_000_000.0)
}

/// Remove the old index data under the cache folder `base` and say what happened.
///
/// For `skim --clear-cache`. A failure is a note on stderr and does not change
/// the exit code: the parser cache was cleared before this ran.
pub(crate) fn clear_legacy_index_data(base: &Path) {
    match remove_legacy_index_data(base) {
        Ok(cleanup) => {
            if cleanup.files > 0 {
                println!(
                    "Removed old search index data ({} folders, {} MB)",
                    cleanup.folders,
                    format_megabytes(cleanup.bytes)
                );
            }
            if cleanup.kept > 0 {
                eprintln!(
                    "Note: {} entries under {:?} are not old index files and were left in place",
                    cleanup.kept,
                    base.join("search")
                );
            }
            if cleanup.more {
                eprintln!(
                    "Note: {:?} holds more than {MAX_SEARCH_ENTRIES} entries; \
                     run skim --clear-cache again to clean the rest",
                    base.join("search")
                );
            }
        }
        Err(e) => eprintln!("Note: could not remove old search index data: {e}"),
    }
}

#[cfg(test)]
#[path = "legacy_cache_tests.rs"]
mod tests;
