//! Tests for the old index data cleanup (legacy_cache.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use super::{
    LEGACY_ARTIFACTS, LegacyCleanup, format_megabytes, legacy_index_report,
    remove_legacy_index_data,
};

const PROJECT_FOLDER: &str = "0123456789abcdef";

// ============================================================================
// Helpers
// ============================================================================

/// Create `<base>/search/<name>/` and return it.
fn project_folder(base: &Path, name: &str) -> PathBuf {
    let folder = base.join("search").join(name);
    fs::create_dir_all(&folder).unwrap();
    folder
}

/// Write every listed artifact into `folder`; file `i` holds `i + 1` bytes.
/// Returns the total size.
fn write_all_artifacts(folder: &Path) -> u64 {
    let mut total = 0;
    for (i, name) in LEGACY_ARTIFACTS.iter().enumerate() {
        let len = i + 1;
        fs::write(folder.join(name), vec![b'x'; len]).unwrap();
        total += len as u64;
    }
    total
}

// ============================================================================
// remove_legacy_index_data
// ============================================================================

#[test]
fn test_the_artifact_list_holds_the_twelve_known_names() {
    assert_eq!(
        LEGACY_ARTIFACTS,
        [
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
        ]
    );
}

#[test]
fn test_cleanup_removes_the_twelve_names_and_the_empty_folders() {
    let base = tempdir().unwrap();
    let folder = project_folder(base.path(), PROJECT_FOLDER);
    let total = write_all_artifacts(&folder);

    let cleanup = remove_legacy_index_data(base.path()).unwrap();

    assert_eq!(
        cleanup,
        LegacyCleanup {
            folders: 1,
            files: 12,
            bytes: total,
            kept: 0,
            more: false,
        }
    );
    assert!(
        !base.path().join("search").exists(),
        "the empty project folder and the empty search folder must go"
    );
    assert!(base.path().exists(), "the cache folder itself must stay");
}

#[test]
fn test_cleanup_keeps_an_unknown_file_and_the_folder_that_holds_it() {
    let base = tempdir().unwrap();
    let folder = project_folder(base.path(), PROJECT_FOLDER);
    fs::write(folder.join("index.skidx"), b"old").unwrap();
    fs::write(folder.join("notes.txt"), b"mine").unwrap();
    fs::write(folder.join("index.skidx.bak"), b"mine too").unwrap();

    let cleanup = remove_legacy_index_data(base.path()).unwrap();

    assert_eq!(cleanup.files, 1);
    assert_eq!(cleanup.kept, 1, "the folder that still holds files is kept");
    assert!(!folder.join("index.skidx").exists());
    assert_eq!(fs::read(folder.join("notes.txt")).unwrap(), b"mine");
    assert_eq!(
        fs::read(folder.join("index.skidx.bak")).unwrap(),
        b"mine too"
    );
}

/// A listed name that is a link is not a file the old index wrote: neither the
/// link nor its target is removed.
#[cfg(unix)]
#[test]
fn test_cleanup_keeps_a_symlink_artifact_and_its_target() {
    let base = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let target = outside.path().join("precious.bin");
    fs::write(&target, b"precious").unwrap();
    let folder = project_folder(base.path(), PROJECT_FOLDER);
    let link = folder.join("temporal.db");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let cleanup = remove_legacy_index_data(base.path()).unwrap();

    assert_eq!(cleanup.files, 0);
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link must stay"
    );
    assert_eq!(fs::read(&target).unwrap(), b"precious");
}

/// A listed name that is a folder is not a file the old index wrote.
#[test]
fn test_cleanup_keeps_a_folder_with_a_listed_name() {
    let base = tempdir().unwrap();
    let folder = project_folder(base.path(), PROJECT_FOLDER);
    let imposter = folder.join("index.skpost");
    fs::create_dir_all(&imposter).unwrap();
    fs::write(imposter.join("keep.txt"), b"mine").unwrap();

    let cleanup = remove_legacy_index_data(base.path()).unwrap();

    assert_eq!(cleanup.files, 0);
    assert_eq!(fs::read(imposter.join("keep.txt")).unwrap(), b"mine");
}

/// A `search` entry that is a link is not entered: the old files below the target
/// stay, and the cleanup says that it left something.
#[cfg(unix)]
#[test]
fn test_cleanup_keeps_old_files_below_a_search_symlink() {
    let base = tempdir().unwrap();
    let elsewhere = tempdir().unwrap();
    let folder = project_folder(elsewhere.path(), PROJECT_FOLDER);
    fs::write(folder.join("index.skidx"), b"old").unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("search"), base.path().join("search"))
        .unwrap();

    let cleanup = remove_legacy_index_data(base.path()).unwrap();

    assert_eq!(
        cleanup,
        LegacyCleanup {
            kept: 1,
            ..LegacyCleanup::default()
        }
    );
    assert_eq!(fs::read(folder.join("index.skidx")).unwrap(), b"old");
    assert_eq!(legacy_index_report(base.path()), None);
}

/// Only a real folder with a 16 lowercase hex name is a project folder. Files
/// with listed names below any other child stay.
#[test]
fn test_cleanup_ignores_children_that_are_not_project_folders() {
    let base = tempdir().unwrap();
    let search = base.path().join("search");
    fs::create_dir_all(&search).unwrap();
    let mut untouched = Vec::new();
    for name in [
        "not-a-project",     // wrong name
        "0123456789ABCDEF",  // upper case
        "0123456789abcde",   // 15 digits
        "0123456789abcdef0", // 17 digits
        "0123456789abcdeg",  // not hex
    ] {
        let folder = project_folder(base.path(), name);
        let file = folder.join("index.skidx");
        fs::write(&file, b"old").unwrap();
        untouched.push(file);
    }
    // A FILE with a project folder name.
    fs::write(search.join("fedcba9876543210"), b"not a folder").unwrap();

    let cleanup = remove_legacy_index_data(base.path()).unwrap();

    assert_eq!(cleanup.files, 0);
    assert_eq!(cleanup.kept, 6);
    for file in untouched {
        assert_eq!(fs::read(&file).unwrap(), b"old", "{file:?} must stay");
    }
    assert_eq!(
        fs::read(search.join("fedcba9876543210")).unwrap(),
        b"not a folder"
    );
}

/// A link with a project folder name does not lead the cleanup out of `search`.
#[cfg(unix)]
#[test]
fn test_cleanup_does_not_follow_a_project_folder_symlink() {
    let base = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let victim = outside.path().join("index.skidx");
    fs::write(&victim, b"not ours").unwrap();
    let search = base.path().join("search");
    fs::create_dir_all(&search).unwrap();
    std::os::unix::fs::symlink(outside.path(), search.join(PROJECT_FOLDER)).unwrap();

    let cleanup = remove_legacy_index_data(base.path()).unwrap();

    assert_eq!(cleanup.files, 0);
    assert_eq!(fs::read(&victim).unwrap(), b"not ours");
    assert!(
        fs::symlink_metadata(search.join(PROJECT_FOLDER))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

/// Files of the cache folder that are not below `search` are never candidates,
/// even when they carry a listed name.
#[test]
fn test_cleanup_never_touches_the_rest_of_the_cache_folder() {
    let base = tempdir().unwrap();
    for name in ["analytics.db", "temporal.db", "index.skidx", "a1b2.json"] {
        fs::write(base.path().join(name), name.as_bytes()).unwrap();
    }
    fs::create_dir_all(base.path().join("tee")).unwrap();
    fs::write(base.path().join("tee").join("out.txt"), b"tee").unwrap();
    let folder = project_folder(base.path(), PROJECT_FOLDER);
    write_all_artifacts(&folder);

    remove_legacy_index_data(base.path()).unwrap();

    for name in ["analytics.db", "temporal.db", "index.skidx", "a1b2.json"] {
        assert_eq!(
            fs::read(base.path().join(name)).unwrap(),
            name.as_bytes(),
            "{name} is outside search and must stay"
        );
    }
    assert_eq!(
        fs::read(base.path().join("tee").join("out.txt")).unwrap(),
        b"tee"
    );
}

#[test]
fn test_cleanup_does_nothing_without_a_search_folder() {
    let base = tempdir().unwrap();
    assert_eq!(
        remove_legacy_index_data(base.path()).unwrap(),
        LegacyCleanup::default()
    );
    // A cache folder that does not exist is the same case.
    assert_eq!(
        remove_legacy_index_data(&base.path().join("missing")).unwrap(),
        LegacyCleanup::default()
    );
}

#[test]
fn test_cleanup_removes_an_empty_project_folder_but_not_a_sibling_with_files() {
    let base = tempdir().unwrap();
    let empty = project_folder(base.path(), "aaaaaaaaaaaaaaaa");
    let busy = project_folder(base.path(), "bbbbbbbbbbbbbbbb");
    fs::write(busy.join("notes.txt"), b"mine").unwrap();

    let cleanup = remove_legacy_index_data(base.path()).unwrap();

    assert!(!empty.exists());
    assert!(busy.join("notes.txt").exists());
    assert_eq!(cleanup.kept, 1);
    assert_eq!(cleanup.files, 0);
    assert!(!cleanup.more);
}

#[test]
fn test_cleanup_says_when_entries_past_the_bound_were_not_inspected() {
    let base = tempdir().unwrap();
    let search = base.path().join("search");
    fs::create_dir_all(&search).unwrap();
    for n in 0..=super::MAX_SEARCH_ENTRIES {
        fs::write(search.join(format!("other{n}")), b"").unwrap();
    }

    let cleanup = remove_legacy_index_data(base.path()).unwrap();

    assert!(cleanup.more, "one entry is past the bound");
    assert_eq!(cleanup.kept, super::MAX_SEARCH_ENTRIES as u64);
}

// ============================================================================
// legacy_index_report
// ============================================================================

#[test]
fn test_report_counts_what_cleanup_would_remove() {
    let base = tempdir().unwrap();
    let first = project_folder(base.path(), "aaaaaaaaaaaaaaaa");
    let second = project_folder(base.path(), "bbbbbbbbbbbbbbbb");
    let total = write_all_artifacts(&first) + write_all_artifacts(&second);
    // A folder with only unknown files adds nothing.
    let unrelated = project_folder(base.path(), "cccccccccccccccc");
    fs::write(unrelated.join("notes.txt"), b"mine").unwrap();

    let report = legacy_index_report(base.path());

    assert_eq!(report, Some((2, total)));
    // The report removed nothing.
    assert!(first.join("index.skidx").exists());
    // And it is the number that cleanup then removes.
    let cleanup = remove_legacy_index_data(base.path()).unwrap();
    assert_eq!((cleanup.folders, cleanup.bytes), (2, total));
}

#[test]
fn test_report_is_none_without_old_index_files() {
    let base = tempdir().unwrap();
    assert_eq!(legacy_index_report(base.path()), None);
    let folder = project_folder(base.path(), PROJECT_FOLDER);
    fs::write(folder.join("notes.txt"), b"mine").unwrap();
    assert_eq!(legacy_index_report(base.path()), None);
}

#[test]
fn test_megabytes_use_one_decimal() {
    assert_eq!(format_megabytes(0), "0.0");
    assert_eq!(format_megabytes(1_260_000), "1.3");
    assert_eq!(format_megabytes(12_340_000), "12.3");
}
