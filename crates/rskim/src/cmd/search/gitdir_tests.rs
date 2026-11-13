//! Tests of the root walk: where it stops.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use super::{
    MAX_CEILING_ENTRIES, RepoLookup, Repository, RootRules, Walk, dropped_ceilings,
    find_repository, owned_by_current_user, parse_ceilings, resolve_repo_toplevel, walk_up,
};

/// The folder a query takes as its root when the walk starts at `start`.
fn root_from(start: &Path, rules: &RootRules) -> PathBuf {
    walk_up(start, rules).root(start)
}

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
    let found = root_from(&sub, &RootRules::default());
    assert_eq!(found, repo);
}

#[test]
fn walk_without_a_repository_returns_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let start = dir.path().canonicalize().unwrap();
    let found = root_from(&start, &RootRules::default());
    assert_eq!(found, start);
}

#[test]
fn walk_never_steps_into_a_ceiling_folder() {
    let (_dir, repo, sub) = repo_with_subfolder();
    // The ceiling sits between the start and the repository.
    let rules = RootRules {
        ceilings: vec![repo.join("a")],
        ..RootRules::default()
    };
    let found = root_from(&sub, &rules);
    assert_eq!(found, sub, "a ceiling above the start ends the search");
}

#[test]
fn walk_may_start_in_a_ceiling_folder() {
    let (_dir, repo, _sub) = repo_with_subfolder();
    let rules = RootRules {
        ceilings: vec![repo.clone()],
        ..RootRules::default()
    };
    // The start itself is never excluded: it holds the repository.
    assert_eq!(root_from(&repo, &rules), repo);
}

#[test]
fn walk_never_takes_the_home_folder_from_below() {
    let (_dir, repo, sub) = repo_with_subfolder();
    // The repository sits in the "home" folder, the start is below it.
    let rules = RootRules {
        home: Some(repo.clone()),
        ..RootRules::default()
    };
    let found = root_from(&sub, &rules);
    assert_eq!(found, sub, "the home folder is not a root from below");
}

#[test]
fn walk_may_start_in_the_home_folder() {
    let (_dir, repo, _sub) = repo_with_subfolder();
    let rules = RootRules {
        home: Some(repo.clone()),
        ..RootRules::default()
    };
    assert_eq!(root_from(&repo, &rules), repo);
}

#[test]
fn walk_does_not_pass_the_home_folder_to_a_repository_above_it() {
    let (_dir, repo, sub) = repo_with_subfolder();
    let home = repo.join("a");
    let rules = RootRules {
        home: Some(home),
        ..RootRules::default()
    };
    let found = root_from(&sub, &rules);
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
#[cfg(unix)]
fn every_ceiling_read_keeps_both_forms_when_each_is_a_link() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let links: Vec<PathBuf> = (0..MAX_CEILING_ENTRIES)
        .map(|n| {
            let real = base.join(format!("real{n}"));
            std::fs::create_dir(&real).unwrap();
            let link = base.join(format!("link{n}"));
            std::os::unix::fs::symlink(&real, &link).unwrap();
            link
        })
        .collect();
    let value = std::env::join_paths(&links).unwrap();
    let parsed = parse_ceilings(&value);
    let last = links.last().unwrap();
    assert!(
        parsed.contains(last),
        "the last entry read is kept as written"
    );
    assert!(
        parsed.contains(&last.canonicalize().unwrap()),
        "and in its canonical form"
    );
}

#[test]
fn the_ceiling_entries_past_the_bound_are_counted_exactly() {
    let entries = |n: usize| {
        let list: Vec<String> = (0..n).map(|i| format!("/ceiling/{i}")).collect();
        std::ffi::OsString::from(list.join(":"))
    };
    assert_eq!(dropped_ceilings(&entries(MAX_CEILING_ENTRIES)), 0);
    assert_eq!(dropped_ceilings(&entries(MAX_CEILING_ENTRIES + 1)), 1);
    assert_eq!(dropped_ceilings(&entries(MAX_CEILING_ENTRIES + 37)), 37);
    // A relative entry is not read, so it does not count.
    let mut value = entries(MAX_CEILING_ENTRIES);
    value.push(":relative");
    assert_eq!(dropped_ceilings(&value), 0);
}

#[test]
fn a_ceiling_overflow_keeps_the_walk_in_the_start_folder() {
    let (_dir, repo, sub) = repo_with_subfolder();
    let rules = RootRules {
        ceilings_dropped: 1,
        ..RootRules::default()
    };
    // A ceiling that was not read could sit between the start and the
    // repository, so the walk does not go up.
    assert_eq!(root_from(&sub, &rules), sub);
    assert_eq!(walk_up(&sub, &rules), Walk::NotFound);
    // The start folder itself is still a root when it holds the repository.
    assert_eq!(root_from(&repo, &rules), repo);
}

// ============================================================================
// Ownership of an ancestor `.git`
// ============================================================================

/// Rules under which no `.git` entry belongs to the current user.
fn nobody_owns() -> RootRules {
    RootRules {
        owns: |_| false,
        ..RootRules::default()
    }
}

#[test]
fn the_walk_does_not_adopt_an_ancestor_repository_that_another_user_owns() {
    let (_dir, repo, sub) = repo_with_subfolder();

    let walk = walk_up(&sub, &nobody_owns());

    assert_eq!(walk, Walk::NotOwned(repo.clone()));
    assert_eq!(walk.root(&sub), sub, "the start folder is the root");
    // The same tree is adopted when the user owns it.
    assert_eq!(walk_up(&sub, &RootRules::default()), Walk::Found(repo));
}

#[test]
fn the_owner_check_receives_the_git_entry_and_not_its_folder() {
    let (_dir, repo, sub) = repo_with_subfolder();
    let rules = RootRules {
        owns: |path| path.ends_with("repo/.git"),
        ..RootRules::default()
    };

    assert_eq!(walk_up(&sub, &rules), Walk::Found(repo));
}

#[test]
fn the_git_entry_of_the_start_folder_is_not_checked_for_an_owner() {
    let (_dir, repo, _sub) = repo_with_subfolder();
    // The user chose the start folder, so its own repository is used.
    assert_eq!(walk_up(&repo, &nobody_owns()), Walk::Found(repo));
}

#[test]
fn the_real_owner_check_accepts_a_git_entry_of_the_current_user() {
    let (_dir, repo, _sub) = repo_with_subfolder();
    assert!(owned_by_current_user(&repo.join(".git")));
}

#[test]
fn the_real_owner_check_refuses_a_git_entry_it_cannot_examine() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("none").join(".git");
    assert!(!owned_by_current_user(&missing));
}

// ============================================================================
// The repository lookup
// ============================================================================

/// A canonical temp folder with a real `repo/.git` (a HEAD file) and
/// `repo/a/b` below it.
fn repo_with_head() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (dir, repo, sub) = repo_with_subfolder();
    std::fs::write(repo.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
    (dir, repo, sub)
}

#[test]
fn a_root_with_a_git_entry_is_its_own_repository() {
    let (_dir, repo, _sub) = repo_with_head();
    assert_eq!(
        find_repository(&repo, &RootRules::default()),
        RepoLookup::Within(Repository {
            top: repo,
            prefix: PathBuf::new()
        })
    );
}

#[test]
fn a_folder_inside_a_repository_names_the_top_and_the_path_below_it() {
    let (_dir, repo, sub) = repo_with_head();
    assert_eq!(
        find_repository(&sub, &RootRules::default()),
        RepoLookup::Within(Repository {
            top: repo,
            prefix: PathBuf::from("a/b")
        })
    );
}

#[test]
fn a_folder_with_no_repository_above_it_is_outside() {
    let dir = tempfile::tempdir().unwrap();
    let start = dir.path().canonicalize().unwrap();
    assert_eq!(
        find_repository(&start, &RootRules::default()),
        RepoLookup::Outside
    );
}

#[test]
fn a_repository_that_another_user_owns_is_outside() {
    let (_dir, _repo, sub) = repo_with_head();
    assert_eq!(find_repository(&sub, &nobody_owns()), RepoLookup::Outside);
}

#[test]
fn a_ceiling_overflow_leaves_a_folder_below_a_repository_outside() {
    let (_dir, _repo, sub) = repo_with_head();
    let rules = RootRules {
        ceilings_dropped: 1,
        ..RootRules::default()
    };
    assert_eq!(find_repository(&sub, &rules), RepoLookup::Outside);
}

#[test]
fn an_enclosing_repository_with_no_head_file_is_unresolved() {
    // `repo/.git` is a folder with no HEAD: the state of a damaged copy.
    let (_dir, _repo, sub) = repo_with_subfolder();
    match find_repository(&sub, &RootRules::default()) {
        RepoLookup::Unresolved(detail) => assert!(detail.contains("HEAD"), "{detail}"),
        other => panic!("expected Unresolved, got {other:?}"),
    }
}

#[test]
fn an_enclosing_git_pointer_that_cannot_be_read_is_unresolved() {
    let (_dir, repo, sub) = repo_with_subfolder();
    std::fs::remove_dir_all(repo.join(".git")).unwrap();
    // A `.git` file with no `gitdir:` line.
    std::fs::write(repo.join(".git"), "not a pointer\n").unwrap();
    assert!(matches!(
        find_repository(&sub, &RootRules::default()),
        RepoLookup::Unresolved(_)
    ));
}

#[test]
fn the_toplevel_for_the_hook_removal_is_none_for_every_unusable_repository() {
    let (_dir, repo, sub) = repo_with_head();
    assert_eq!(resolve_repo_toplevel(&sub), Some(repo));
    // A root that has a `.git` entry of its own is never re-pointed.
    let (_dir2, own, _sub2) = repo_with_head();
    assert_eq!(resolve_repo_toplevel(&own), None);
    // No HEAD: no action on a repository that cannot be read.
    let (_dir3, _damaged, damaged_sub) = repo_with_subfolder();
    assert_eq!(resolve_repo_toplevel(&damaged_sub), None);
}

#[test]
fn the_filesystem_root_is_never_taken_from_below() {
    // An ancestor with no parent ends the walk, even when it holds a `.git`.
    let rules = RootRules::default();
    assert!(rules.stops_at(Path::new("/")));
}
