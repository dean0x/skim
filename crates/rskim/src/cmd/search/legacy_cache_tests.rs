//! Tests for the old index data cleanup (legacy_cache.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use super::{
    CleanupLines, LEGACY_ARTIFACTS, LegacyCleanup, LegacyFinding, LegacyReport, MAX_FOLDER_ENTRIES,
    MAX_PROJECT_FOLDERS, MAX_SEARCH_ENTRIES, Pass, Skipped, cleanup_lines, format_megabytes,
    is_temp_artifact_name, legacy_index_report, owned_by_current_user, remove_legacy_index_data,
    remove_with, report_with,
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

/// A pass for the user that runs the tests, with the given bounds.
fn pass(max_entries: usize, max_folders: usize) -> Pass {
    Pass {
        owns: owned_by_current_user,
        max_entries,
        max_folders,
    }
}

/// Stand-in ownership rules: a test cannot give a folder to another user.
/// Another user owns the `search` folder.
fn owns_all_but_search(path: &Path) -> bool {
    path.file_name() != Some(OsStr::new("search"))
}

/// Another user owns the project folder `aaaaaaaaaaaaaaaa`.
fn owns_all_but_the_first_project_folder(path: &Path) -> bool {
    path.file_name() != Some(OsStr::new("aaaaaaaaaaaaaaaa"))
}

/// `count` files in `search` that cleanup keeps.
fn write_foreign_files(search: &Path, count: usize) {
    fs::create_dir_all(search).unwrap();
    for n in 0..count {
        fs::write(search.join(format!("other{n}")), b"").unwrap();
    }
}

// ============================================================================
// The names cleanup removes
// ============================================================================

#[test]
fn test_the_artifact_list_holds_the_known_names() {
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
            "temporal.db-wal",
            "temporal.db-shm",
            "temporal.db-journal",
            "temporal.db.build_backoff",
            "tracked_union.cache",
            ".skim-build.lock",
        ]
    );
}

/// The old atomic writer named its temp files with the default rule of
/// `NamedTempFile::new_in`: `.tmp` and six random letters or digits.
#[test]
fn test_the_temp_file_rule_is_dot_tmp_and_six_alphanumeric_characters() {
    for name in [".tmpAb12Z9", ".tmp000000", ".tmpzzzzzz", ".tmpZZZZZZ"] {
        assert!(is_temp_artifact_name(OsStr::new(name)), "{name}");
    }
    for name in [
        ".tmpAb12Z",       // 5 characters
        ".tmpAb12Z99",     // 7 characters
        ".tmpAb12Z-",      // not alphanumeric
        ".tmpAb12 9",      // a space
        "tmpAb12Z9",       // no dot
        ".TMPAb12Z9",      // upper case prefix
        ".tmp",            // no random part
        ".tmpAb12\u{e9}9", // not ASCII
        "x.tmpAb12Z9",     // not at the start
    ] {
        assert!(!is_temp_artifact_name(OsStr::new(name)), "{name}");
    }
}

/// A crash leaves the journal files of the database and the temp file of a write
/// in progress. Without their names the folder stays, and every later run says
/// that it holds something that is not old index data.
#[test]
fn test_cleanup_removes_the_journal_files_and_temp_files_a_crash_leaves() {
    let base = tempdir().unwrap();
    let folder = project_folder(base.path(), PROJECT_FOLDER);
    for name in [
        "temporal.db",
        "temporal.db-wal",
        "temporal.db-shm",
        "temporal.db-journal",
        ".tmpAb12Z9",
    ] {
        fs::write(folder.join(name), name.as_bytes()).unwrap();
    }

    let cleanup = remove_legacy_index_data(base.path()).unwrap();

    assert_eq!(
        cleanup,
        LegacyCleanup {
            folders: 1,
            files: 5,
            bytes: [
                "temporal.db",
                "temporal.db-wal",
                "temporal.db-shm",
                "temporal.db-journal",
                ".tmpAb12Z9",
            ]
            .iter()
            .map(|name| name.len() as u64)
            .sum(),
            freed: 1,
            ..LegacyCleanup::default()
        }
    );
    assert!(
        !base.path().join("search").exists(),
        "the project folder and the search folder must go"
    );
}

/// The report counts the same files.
#[test]
fn test_report_counts_the_journal_files_and_temp_files() {
    let base = tempdir().unwrap();
    let folder = project_folder(base.path(), PROJECT_FOLDER);
    fs::write(folder.join("temporal.db-wal"), b"12345").unwrap();
    fs::write(folder.join(".tmpAb12Z9"), b"123").unwrap();

    assert_eq!(
        legacy_index_report(base.path()),
        Some(LegacyFinding::Found(LegacyReport {
            folders: 1,
            bytes: 8,
            ..LegacyReport::default()
        }))
    );
}

/// A name that only looks like a temp file, or is not a regular file, stays.
#[cfg(unix)]
#[test]
fn test_cleanup_keeps_a_temp_look_alike_that_is_not_a_regular_file() {
    let base = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let target = outside.path().join("precious.bin");
    fs::write(&target, b"precious").unwrap();
    let folder = project_folder(base.path(), PROJECT_FOLDER);
    std::os::unix::fs::symlink(&target, folder.join(".tmpLink01")).unwrap();
    fs::create_dir_all(folder.join(".tmpDir001")).unwrap();
    fs::write(folder.join(".tmpShort"), b"mine").unwrap();

    let cleanup = remove_legacy_index_data(base.path()).unwrap();

    assert_eq!(cleanup.files, 0);
    assert_eq!(fs::read(&target).unwrap(), b"precious");
    assert!(folder.join(".tmpDir001").is_dir());
    assert_eq!(fs::read(folder.join(".tmpShort")).unwrap(), b"mine");
}

// ============================================================================
// remove_legacy_index_data
// ============================================================================

#[test]
fn test_cleanup_removes_the_listed_names_and_the_empty_folders() {
    let base = tempdir().unwrap();
    let folder = project_folder(base.path(), PROJECT_FOLDER);
    let total = write_all_artifacts(&folder);

    let cleanup = remove_legacy_index_data(base.path()).unwrap();

    assert_eq!(
        cleanup,
        LegacyCleanup {
            folders: 1,
            files: LEGACY_ARTIFACTS.len() as u64,
            bytes: total,
            freed: 1,
            ..LegacyCleanup::default()
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
    assert_eq!(cleanup.freed, 0, "a kept folder frees no place");
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
    assert_eq!(cleanup.freed, 1);
    assert_eq!(cleanup.files, 0);
    assert!(!cleanup.skipped.any());
}

// ============================================================================
// Bounds
// ============================================================================

#[test]
fn test_cleanup_says_when_the_listing_bound_fired() {
    let base = tempdir().unwrap();
    let search = base.path().join("search");
    write_foreign_files(&search, 6);

    let cleanup = remove_with(base.path(), pass(5, MAX_PROJECT_FOLDERS)).unwrap();

    assert_eq!(
        cleanup,
        LegacyCleanup {
            kept: 5,
            skipped: Skipped {
                listing: true,
                ..Skipped::default()
            },
            ..LegacyCleanup::default()
        }
    );
}

/// Kept entries spend the listing bound only. They must not stop the cleanup of
/// the project folders, and the project folders past the bound are counted
/// exactly and cleaned by the next run.
#[test]
fn test_kept_entries_do_not_use_up_the_project_folder_bound() {
    let base = tempdir().unwrap();
    write_foreign_files(&base.path().join("search"), 10);
    for name in ["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb", "cccccccccccccccc"] {
        fs::write(
            project_folder(base.path(), name).join("index.skidx"),
            b"old",
        )
        .unwrap();
    }

    let first = remove_with(base.path(), pass(100, 2)).unwrap();

    assert_eq!(first.folders, 2, "two project folders are cleaned");
    assert_eq!(first.files, 2);
    assert_eq!(first.kept, 10, "the kept files are counted, not skipped");
    assert_eq!(first.freed, 2);
    assert_eq!(
        first.skipped,
        Skipped {
            folders: 1,
            ..Skipped::default()
        }
    );

    let second = remove_with(base.path(), pass(100, 2)).unwrap();

    assert_eq!((second.folders, second.files), (1, 1));
    assert!(!second.skipped.any(), "the next run reaches the rest");
}

/// A run that frees no place meets the same entries again. It must not promise
/// that a new run cleans the rest.
#[test]
fn test_a_run_that_freed_nothing_names_the_folder_for_a_check_by_hand() {
    let base = tempdir().unwrap();
    for name in ["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb", "cccccccccccccccc"] {
        let folder = project_folder(base.path(), name);
        fs::write(folder.join("index.skidx"), b"old").unwrap();
        fs::write(folder.join("notes.txt"), b"mine").unwrap();
    }

    let cleanup = remove_with(base.path(), pass(100, 2)).unwrap();
    let lines = cleanup_lines(base.path(), &Ok(cleanup));

    assert_eq!(lines.notes.len(), 2, "{:?}", lines.notes);
    let search = base.path().join("search");
    assert_eq!(
        lines.notes[1],
        format!(
            "Note: not every entry under {search:?} was inspected: 1 project folders past the first {MAX_PROJECT_FOLDERS} were not inspected; this run freed no place inside the bounds, so a new run stops at the same place; check {search:?} by hand"
        )
    );
    assert!(!lines.notes[1].contains("again"), "{}", lines.notes[1]);
}

/// A folder that a bound cut short frees a place for each file removed from it.
#[test]
fn test_a_folder_that_holds_more_than_the_bound_is_cleaned_over_two_runs() {
    let base = tempdir().unwrap();
    let folder = project_folder(base.path(), PROJECT_FOLDER);
    let count = MAX_FOLDER_ENTRIES + 6;
    for n in 0..count {
        fs::write(folder.join(format!(".tmp{n:06}")), b"x").unwrap();
    }

    let first = remove_legacy_index_data(base.path()).unwrap();

    assert_eq!(first.files, MAX_FOLDER_ENTRIES as u64);
    assert_eq!(first.kept, 1, "the folder still holds the rest");
    assert_eq!(first.freed, MAX_FOLDER_ENTRIES as u64);
    assert_eq!(
        first.skipped,
        Skipped {
            crowded: 1,
            ..Skipped::default()
        }
    );
    let lines = cleanup_lines(base.path(), &Ok(first));
    assert!(
        lines
            .notes
            .iter()
            .any(|note| note.ends_with("run skim --clear-cache again to clean the rest")),
        "{:?}",
        lines.notes
    );

    let second = remove_legacy_index_data(base.path()).unwrap();

    assert_eq!(second.files, 6);
    assert!(!second.skipped.any());
    assert!(!base.path().join("search").exists());
}

// ============================================================================
// Folders of another user
// ============================================================================

#[test]
fn test_the_user_who_made_a_folder_owns_it() {
    let dir = tempdir().unwrap();

    assert!(owned_by_current_user(dir.path()));
}

/// A path that cannot be examined is not owned.
#[test]
fn test_a_path_that_cannot_be_examined_is_not_owned() {
    let dir = tempdir().unwrap();

    assert!(!owned_by_current_user(&dir.path().join("missing")));
}

/// `SKIM_CACHE_DIR` can name a folder that other users write to. A `search`
/// folder of another user is not entered: nothing in it is removed.
#[test]
fn test_cleanup_does_not_enter_a_search_folder_that_another_user_owns() {
    let base = tempdir().unwrap();
    let folder = project_folder(base.path(), PROJECT_FOLDER);
    fs::write(folder.join("index.skidx"), b"old").unwrap();
    // A second project folder: a pass that entered `search` would count both.
    project_folder(base.path(), "aaaaaaaaaaaaaaaa");
    let other_users = Pass {
        owns: owns_all_but_search,
        ..pass(MAX_SEARCH_ENTRIES, MAX_PROJECT_FOLDERS)
    };

    let cleanup = remove_with(base.path(), other_users).unwrap();

    assert_eq!(
        cleanup,
        LegacyCleanup {
            foreign: 1,
            ..LegacyCleanup::default()
        }
    );
    assert_eq!(fs::read(folder.join("index.skidx")).unwrap(), b"old");
    assert_eq!(
        report_with(base.path(), other_users),
        Some(LegacyFinding::Found(LegacyReport {
            foreign: 1,
            ..LegacyReport::default()
        }))
    );
}

/// A project folder of another user inside our `search` folder is not entered
/// either: its files stay and the folder is counted. Our own project folders are
/// still cleaned, and the folder of the other user does not spend the bound on
/// project folders.
#[test]
fn test_cleanup_does_not_enter_a_project_folder_that_another_user_owns() {
    let base = tempdir().unwrap();
    let foreign = project_folder(base.path(), "aaaaaaaaaaaaaaaa");
    let ours = project_folder(base.path(), "bbbbbbbbbbbbbbbb");
    let also_ours = project_folder(base.path(), "cccccccccccccccc");
    for folder in [&foreign, &ours, &also_ours] {
        fs::write(folder.join("index.skidx"), b"old").unwrap();
    }
    let other_user_in_first = Pass {
        owns: owns_all_but_the_first_project_folder,
        ..pass(MAX_SEARCH_ENTRIES, 2)
    };

    let report = report_with(base.path(), other_user_in_first);
    let cleanup = remove_with(base.path(), other_user_in_first).unwrap();

    assert_eq!(
        cleanup,
        LegacyCleanup {
            folders: 2,
            files: 2,
            bytes: 6,
            freed: 2,
            kept: 0,
            foreign: 1,
            skipped: Skipped::default(),
        }
    );
    assert_eq!(fs::read(foreign.join("index.skidx")).unwrap(), b"old");
    assert!(!ours.exists() && !also_ours.exists());
    assert_eq!(
        report,
        Some(LegacyFinding::Found(LegacyReport {
            folders: 2,
            bytes: 6,
            foreign: 1,
            skipped: Skipped::default(),
        }))
    );
}

// ============================================================================
// Unreadable
// ============================================================================

/// A cache folder that is a file: `<base>/search` cannot be inspected. Cleanup
/// fails with the error, and the report says that it could not look.
#[test]
fn test_report_says_when_the_search_folder_cannot_be_inspected() {
    let dir = tempdir().unwrap();
    let base = dir.path().join("not-a-folder");
    fs::write(&base, b"a file").unwrap();

    assert_eq!(
        legacy_index_report(&base),
        Some(LegacyFinding::Unreadable(io::ErrorKind::NotADirectory))
    );
    assert_eq!(
        remove_legacy_index_data(&base).unwrap_err().kind(),
        io::ErrorKind::NotADirectory
    );
}

/// `<base>/search` exists but cannot be listed.
#[cfg(unix)]
#[test]
fn test_report_says_when_the_search_folder_cannot_be_listed() {
    use std::os::unix::fs::PermissionsExt;

    let base = tempdir().unwrap();
    let search = base.path().join("search");
    fs::create_dir_all(&search).unwrap();
    fs::set_permissions(&search, fs::Permissions::from_mode(0o000)).unwrap();
    let result = if fs::read_dir(&search).is_ok() {
        // The user that runs the tests can list any folder (root): the case
        // cannot be made here.
        None
    } else {
        Some(legacy_index_report(base.path()))
    };
    fs::set_permissions(&search, fs::Permissions::from_mode(0o755)).unwrap();

    if let Some(report) = result {
        assert_eq!(
            report,
            Some(LegacyFinding::Unreadable(io::ErrorKind::PermissionDenied))
        );
    }
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

    assert_eq!(
        report,
        Some(LegacyFinding::Found(LegacyReport {
            folders: 2,
            bytes: total,
            ..LegacyReport::default()
        }))
    );
    // The report removed nothing.
    assert!(first.join("index.skidx").exists());
    // And it is the number that cleanup then removes.
    let cleanup = remove_legacy_index_data(base.path()).unwrap();
    assert_eq!((cleanup.folders, cleanup.bytes), (2, total));
}

#[test]
fn test_report_says_when_the_listing_bound_fired() {
    let base = tempdir().unwrap();
    write_foreign_files(&base.path().join("search"), 6);

    // No old file is among the first entries, yet one entry went uninspected:
    // the report must not claim there is nothing.
    assert_eq!(
        report_with(base.path(), pass(5, MAX_PROJECT_FOLDERS)),
        Some(LegacyFinding::Found(LegacyReport {
            skipped: Skipped {
                listing: true,
                ..Skipped::default()
            },
            ..LegacyReport::default()
        }))
    );
}

#[test]
fn test_report_says_how_many_project_folders_the_bound_left_out() {
    let base = tempdir().unwrap();
    for name in ["aaaaaaaaaaaaaaaa", "bbbbbbbbbbbbbbbb", "cccccccccccccccc"] {
        fs::write(
            project_folder(base.path(), name).join("index.skidx"),
            b"old",
        )
        .unwrap();
    }

    assert_eq!(
        report_with(base.path(), pass(100, 2)),
        Some(LegacyFinding::Found(LegacyReport {
            folders: 2,
            bytes: 6,
            skipped: Skipped {
                folders: 1,
                ..Skipped::default()
            },
            ..LegacyReport::default()
        }))
    );
}

#[test]
fn test_report_says_when_a_project_folder_holds_more_than_the_bound() {
    let base = tempdir().unwrap();
    let folder = project_folder(base.path(), PROJECT_FOLDER);
    for n in 0..=MAX_FOLDER_ENTRIES {
        fs::write(folder.join(format!(".tmp{n:06}")), b"x").unwrap();
    }

    let Some(LegacyFinding::Found(report)) = legacy_index_report(base.path()) else {
        panic!("a folder full of temp files must be reported");
    };

    assert_eq!(report.folders, 1);
    assert_eq!(report.bytes, MAX_FOLDER_ENTRIES as u64);
    assert_eq!(
        report.skipped,
        Skipped {
            crowded: 1,
            ..Skipped::default()
        }
    );
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

// ============================================================================
// The text of the notes
// ============================================================================

fn lines_of(cleanup: LegacyCleanup) -> CleanupLines {
    cleanup_lines(Path::new("/c"), &Ok(cleanup))
}

#[test]
fn test_cleanup_says_nothing_when_it_removed_nothing_and_left_nothing() {
    assert_eq!(lines_of(LegacyCleanup::default()), CleanupLines::default());
}

#[test]
fn test_cleanup_says_what_it_removed() {
    let lines = lines_of(LegacyCleanup {
        folders: 2,
        files: 7,
        bytes: 12_340_000,
        ..LegacyCleanup::default()
    });

    assert_eq!(
        lines.removed.as_deref(),
        Some("Removed old search index data (2 folders, 12.3 MB)")
    );
    assert!(lines.notes.is_empty(), "{:?}", lines.notes);
}

/// The note is the only disclosure of the data that stays.
#[test]
fn test_cleanup_says_how_many_entries_it_left_in_place() {
    let lines = lines_of(LegacyCleanup {
        kept: 3,
        ..LegacyCleanup::default()
    });

    assert_eq!(
        lines.notes,
        ["Note: 3 entries under \"/c/search\" are not old index files and were left in place"]
    );
}

#[test]
fn test_cleanup_says_how_many_folders_belong_to_another_user() {
    let lines = lines_of(LegacyCleanup {
        foreign: 2,
        ..LegacyCleanup::default()
    });

    assert_eq!(
        lines.notes,
        ["Note: 2 folders at or under \"/c/search\" belong to another user and were left in place"]
    );
}

#[test]
fn test_cleanup_names_each_bound_that_fired_and_the_remedy_when_a_place_was_freed() {
    let lines = lines_of(LegacyCleanup {
        freed: 1,
        skipped: Skipped {
            folders: 7,
            listing: true,
            crowded: 2,
        },
        ..LegacyCleanup::default()
    });

    assert_eq!(
        lines.notes,
        [format!(
            "Note: not every entry under \"/c/search\" was inspected: 7 project folders past the first {MAX_PROJECT_FOLDERS} were not inspected; only the first {MAX_SEARCH_ENTRIES} entries of the folder were read; 2 project folders hold more than {MAX_FOLDER_ENTRIES} entries and only the first {MAX_FOLDER_ENTRIES} of each were read; run skim --clear-cache again to clean the rest"
        )]
    );
}

#[test]
fn test_cleanup_names_the_folder_when_no_place_was_freed() {
    let lines = lines_of(LegacyCleanup {
        skipped: Skipped {
            listing: true,
            ..Skipped::default()
        },
        ..LegacyCleanup::default()
    });

    assert_eq!(
        lines.notes,
        [format!(
            "Note: not every entry under \"/c/search\" was inspected: only the first {MAX_SEARCH_ENTRIES} entries of the folder were read; this run freed no place inside the bounds, so a new run stops at the same place; check \"/c/search\" by hand"
        )]
    );
}

#[test]
fn test_cleanup_says_why_it_could_not_remove_the_data() {
    let failure = Err(io::Error::new(io::ErrorKind::PermissionDenied, "denied"));

    let lines = cleanup_lines(Path::new("/c"), &failure);

    assert_eq!(
        lines,
        CleanupLines {
            removed: None,
            notes: vec!["Note: could not remove old search index data: denied".to_string()],
        }
    );
}
