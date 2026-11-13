//! Tests of the root walk: where it stops.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use super::{MAX_CEILING_ENTRIES, RootRules, discover_project_root_with, parse_ceilings};

/// A canonical temp folder with `repo/.git` and `repo/a/b` below it.
fn repo_with_subfolder() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let repo = base.join("repo");
    let sub = repo.join("a").join("b");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(&sub).unwrap();
    (dir, repo, sub)
}

#[test]
fn walk_finds_the_enclosing_repository() {
    let (_dir, repo, sub) = repo_with_subfolder();
    let found = discover_project_root_with(&sub, &RootRules::default());
    assert_eq!(found, repo);
}

#[test]
fn walk_without_a_repository_returns_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let start = dir.path().canonicalize().unwrap();
    let found = discover_project_root_with(&start, &RootRules::default());
    assert_eq!(found, start);
}

#[test]
fn walk_never_steps_into_a_ceiling_folder() {
    let (_dir, repo, sub) = repo_with_subfolder();
    // The ceiling sits between the start and the repository.
    let rules = RootRules {
        ceilings: vec![repo.join("a")],
        home: None,
    };
    let found = discover_project_root_with(&sub, &rules);
    assert_eq!(found, sub, "a ceiling above the start ends the search");
}

#[test]
fn walk_may_start_in_a_ceiling_folder() {
    let (_dir, repo, _sub) = repo_with_subfolder();
    let rules = RootRules {
        ceilings: vec![repo.clone()],
        home: None,
    };
    // The start itself is never excluded: it holds the repository.
    assert_eq!(discover_project_root_with(&repo, &rules), repo);
}

#[test]
fn walk_never_takes_the_home_folder_from_below() {
    let (_dir, repo, sub) = repo_with_subfolder();
    // The repository sits in the "home" folder, the start is below it.
    let rules = RootRules {
        ceilings: Vec::new(),
        home: Some(repo.clone()),
    };
    let found = discover_project_root_with(&sub, &rules);
    assert_eq!(found, sub, "the home folder is not a root from below");
}

#[test]
fn walk_may_start_in_the_home_folder() {
    let (_dir, repo, _sub) = repo_with_subfolder();
    let rules = RootRules {
        ceilings: Vec::new(),
        home: Some(repo.clone()),
    };
    assert_eq!(discover_project_root_with(&repo, &rules), repo);
}

#[test]
fn walk_does_not_pass_the_home_folder_to_a_repository_above_it() {
    let (_dir, repo, sub) = repo_with_subfolder();
    let home = repo.join("a");
    let rules = RootRules {
        ceilings: Vec::new(),
        home: Some(home),
    };
    let found = discover_project_root_with(&sub, &rules);
    assert_eq!(found, sub, "the walk ends at the home folder");
}

#[test]
fn ceilings_ignore_empty_and_relative_entries() {
    let value = std::ffi::OsString::from("::relative/dir:/abs/ceiling:");
    let parsed = parse_ceilings(&value);
    assert!(parsed.contains(&PathBuf::from("/abs/ceiling")));
    assert!(parsed.iter().all(|p| p.is_absolute()));
}

#[test]
#[cfg(unix)]
fn ceilings_keep_the_canonical_form_of_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let real = base.join("real");
    std::fs::create_dir(&real).unwrap();
    let link = base.join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let parsed = parse_ceilings(link.as_os_str());
    assert!(parsed.contains(&link), "the spelling as given is kept");
    assert!(parsed.contains(&real), "the canonical form is added");
}

#[test]
fn ceilings_are_bounded() {
    let many: Vec<String> = (0..MAX_CEILING_ENTRIES * 2)
        .map(|n| format!("/ceiling/{n}"))
        .collect();
    let value = std::ffi::OsString::from(many.join(":"));
    assert!(parse_ceilings(&value).len() <= MAX_CEILING_ENTRIES);
}

#[test]
fn the_filesystem_root_is_never_taken_from_below() {
    // An ancestor with no parent ends the walk, even when it holds a `.git`.
    let rules = RootRules::default();
    assert!(rules.stops_at(Path::new("/")));
}
