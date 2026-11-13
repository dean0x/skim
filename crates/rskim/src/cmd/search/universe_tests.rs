//! Tests for the file universe: the listing, the left-out reasons, the
//! containment of reads and the sensitive-file seam.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use super::*;

// ============================================================================
// Helpers
// ============================================================================

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap())
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository with a canonical path, so that the root and the temp folder do
/// not differ by a symlink such as `/var` on macOS.
struct Repo {
    _dir: TempDir,
    root: PathBuf,
}

impl Repo {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git(&root, &["init", "-q"]);
        Repo { _dir: dir, root }
    }

    fn write(&self, rel: &str, content: &[u8]) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn track(&self, rel: &str, content: &[u8]) {
        self.write(rel, content);
        git(&self.root, &["add", "-f", rel]);
    }
}

fn pool() -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(2)
        .build()
        .unwrap()
}

fn listed(listing: &Listing) -> Vec<(String, CandidateKind)> {
    listing
        .candidates
        .iter()
        .map(|c| (c.rel.to_string_lossy().into_owned(), c.kind))
        .collect()
}

fn names(listing: &Listing) -> Vec<String> {
    listed(listing).into_iter().map(|(n, _)| n).collect()
}

fn candidate(rel: &str) -> Candidate {
    Candidate {
        rel: PathBuf::from(rel),
        kind: CandidateKind::File,
    }
}

/// Read `rel` of `root` through a fresh [`Containment`].
fn read(root: &Path, rel: &str) -> Read {
    let candidates = [candidate(rel)];
    let containment = Containment::new(root, &candidates, &pool()).unwrap();
    containment.read(&candidates[0])
}

// ============================================================================
// Listing
// ============================================================================

#[test]
fn the_universe_is_the_tracked_files_plus_the_untracked_not_ignored_visible_files() {
    let repo = Repo::new();
    repo.track("src/main.rs", b"fn main() {}\n");
    repo.track("Cargo.lock", b"lock\n");
    repo.track("notes.txt", b"text\n");
    repo.track("run", b"#!/bin/sh\n");
    repo.write("untracked.rs", b"fn u() {}\n");
    repo.write(".gitignore", b"ignored.txt\n");
    repo.write("ignored.txt", b"ignored\n");
    repo.write(".hidden-untracked", b"hidden\n");

    let listing = list_universe(&repo.root).unwrap();

    assert_eq!(
        names(&listing),
        [
            "Cargo.lock",
            "notes.txt",
            "run",
            "src/main.rs",
            "untracked.rs"
        ]
    );
    assert_eq!(listing.left_out, LeftOutCounts::default());
}

#[test]
fn a_tracked_file_is_listed_though_it_is_hidden_or_ignored() {
    let repo = Repo::new();
    repo.track(".env", b"TOKEN=x\n");
    repo.track(".config/app.toml", b"k = 1\n");
    repo.write(".gitignore", b"vendor/\n");
    repo.track("vendor/lib.rs", b"fn v() {}\n");

    let listing = list_universe(&repo.root).unwrap();

    assert_eq!(
        names(&listing),
        [".config/app.toml", ".env", "vendor/lib.rs"]
    );
}

#[test]
fn a_tracked_symlink_is_a_symlink_candidate() {
    let repo = Repo::new();
    repo.track("real.rs", b"fn r() {}\n");
    std::os::unix::fs::symlink("real.rs", repo.root.join("link.rs")).unwrap();
    git(&repo.root, &["add", "link.rs"]);

    let listing = list_universe(&repo.root).unwrap();

    assert_eq!(
        listed(&listing),
        [
            ("link.rs".to_string(), CandidateKind::Symlink),
            ("real.rs".to_string(), CandidateKind::File),
        ]
    );
}

#[test]
fn a_walked_link_is_a_symlink_candidate_and_is_not_followed() {
    let repo = Repo::new();
    repo.track("real.rs", b"fn r() {}\n");
    std::os::unix::fs::symlink("real.rs", repo.root.join("untracked-link.rs")).unwrap();
    fs::create_dir(repo.root.join("realdir")).unwrap();
    repo.write("realdir/inner.rs", b"fn i() {}\n");
    std::os::unix::fs::symlink("realdir", repo.root.join("dirlink")).unwrap();

    let listing = list_universe(&repo.root).unwrap();

    let by_name = listed(&listing);
    assert!(
        by_name.contains(&("untracked-link.rs".to_string(), CandidateKind::Symlink)),
        "{by_name:?}"
    );
    assert!(
        by_name.contains(&("dirlink".to_string(), CandidateKind::Symlink)),
        "{by_name:?}"
    );
    assert!(
        !by_name.iter().any(|(n, _)| n.starts_with("dirlink/")),
        "a linked folder is not walked: {by_name:?}"
    );
}

#[test]
fn a_gitlink_and_a_skip_worktree_entry_are_not_listed() {
    let repo = Repo::new();
    repo.track("kept.rs", b"fn k() {}\n");
    repo.track("sparse.rs", b"fn s() {}\n");
    git(
        &repo.root,
        &["update-index", "--skip-worktree", "sparse.rs"],
    );
    // A skip-worktree file is not materialized in the work tree.
    fs::remove_file(repo.root.join("sparse.rs")).unwrap();
    git(
        &repo.root,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000,4b825dc642cb6eb9a060e54bf8d69288fbee4904,sub",
        ],
    );

    let listing = list_universe(&repo.root).unwrap();

    assert_eq!(names(&listing), ["kept.rs"]);
}

#[test]
fn outside_a_repository_the_walk_is_the_whole_universe() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("a.txt"), "a\n").unwrap();
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/b.rs"), "fn b() {}\n").unwrap();
    fs::write(root.join(".hidden"), "h\n").unwrap();

    let listing = list_universe(&root).unwrap();

    assert_eq!(names(&listing), ["a.txt", "sub/b.rs"]);
}

#[test]
fn a_repository_with_nothing_staged_lists_its_walk() {
    let repo = Repo::new();
    repo.write("fresh.rs", b"fn f() {}\n");

    let listing = list_universe(&repo.root).unwrap();

    assert_eq!(names(&listing), ["fresh.rs"]);
}

#[test]
fn a_root_inside_a_repository_lists_the_tracked_files_below_it() {
    let repo = Repo::new();
    repo.track("top.rs", b"fn t() {}\n");
    repo.track("sub/a.rs", b"fn a() {}\n");
    repo.track("sub/.env", b"TOKEN=x\n");
    repo.write(".gitignore", b"sub/vendor/\n");
    repo.track("sub/vendor/lib.rs", b"fn v() {}\n");
    repo.write("sub/untracked.txt", b"u\n");

    let listing = list_universe(&repo.root.join("sub")).unwrap();

    // The hidden and the ignored tracked files below the root are listed, with
    // paths relative to the root; a tracked file above the root is not.
    assert_eq!(
        names(&listing),
        [".env", "a.rs", "untracked.txt", "vendor/lib.rs"]
    );
    assert_eq!(listing.left_out, LeftOutCounts::default());
}

#[test]
fn an_unreadable_git_index_fails_with_no_partial_universe() {
    let repo = Repo::new();
    repo.track("a.rs", b"fn a() {}\n");
    fs::write(repo.root.join(".git/index"), b"this is not an index").unwrap();

    let err = list_universe(&repo.root).unwrap_err();

    assert!(matches!(err, UniverseError::GitIndex { .. }), "{err:?}");
}

#[test]
fn index_paths_that_leave_the_root_are_counted_and_never_listed() {
    let entries = vec![
        TrackedEntry {
            path: PathBuf::from("ok.rs"),
            symlink: false,
        },
        TrackedEntry {
            path: PathBuf::from("../escape.rs"),
            symlink: false,
        },
        TrackedEntry {
            path: PathBuf::from("a/../../b.rs"),
            symlink: false,
        },
        TrackedEntry {
            path: PathBuf::from("/etc/passwd"),
            symlink: false,
        },
        TrackedEntry {
            path: PathBuf::new(),
            symlink: false,
        },
    ];

    let (safe, unsafe_count) = split_safe(entries);

    assert_eq!(safe.len(), 1);
    assert_eq!(safe[0].path, PathBuf::from("ok.rs"));
    assert_eq!(unsafe_count, 4);
}

#[test]
fn retain_drops_the_candidates_a_filter_refuses() {
    let repo = Repo::new();
    repo.track("a.rs", b"a\n");
    repo.track("b.txt", b"b\n");
    let mut listing = list_universe(&repo.root).unwrap();

    listing.retain(|rel| rel.extension().is_some_and(|e| e == "rs"));

    assert_eq!(names(&listing), ["a.rs"]);
}

// ============================================================================
// Left-out reasons
// ============================================================================

#[test]
fn a_text_file_is_read_in_full() {
    let repo = Repo::new();
    repo.write("a.txt", "line one\nline two\n".as_bytes());
    assert_eq!(
        read(&repo.root, "a.txt"),
        Read::Text("line one\nline two\n".to_string())
    );
}

#[test]
fn a_nul_byte_in_the_probe_makes_a_file_binary_and_a_later_one_does_not() {
    let repo = Repo::new();
    repo.write("bin.dat", b"abc\0def");
    assert_eq!(read(&repo.root, "bin.dat"), Read::LeftOut(LeftOut::Binary));

    // The probe is the first BINARY_PROBE_BYTES bytes: a NUL after it is text.
    let mut late = vec![b'a'; BINARY_PROBE_BYTES];
    late.push(0);
    repo.write("late.dat", &late);
    assert!(matches!(read(&repo.root, "late.dat"), Read::Text(_)));

    let mut last_in_probe = vec![b'a'; BINARY_PROBE_BYTES - 1];
    last_in_probe.push(0);
    repo.write("edge.dat", &last_in_probe);
    assert_eq!(read(&repo.root, "edge.dat"), Read::LeftOut(LeftOut::Binary));
}

#[test]
fn a_file_that_is_not_utf8_is_not_utf8() {
    let repo = Repo::new();
    repo.write("latin.txt", b"caf\xe9\n");
    assert_eq!(
        read(&repo.root, "latin.txt"),
        Read::LeftOut(LeftOut::NotUtf8)
    );
}

#[test]
fn a_file_over_the_size_bound_is_too_large_and_one_at_the_bound_is_read() {
    let repo = Repo::new();
    let at_bound = usize::try_from(MAX_FILE_BYTES).unwrap();
    repo.write("at.txt", &vec![b'x'; at_bound]);
    repo.write("over.txt", &vec![b'x'; at_bound + 1]);
    // Too large wins over binary: the size is known before any byte is read.
    repo.write("over.bin", &vec![0u8; at_bound + 1]);

    assert!(matches!(read(&repo.root, "at.txt"), Read::Text(_)));
    assert_eq!(
        read(&repo.root, "over.txt"),
        Read::LeftOut(LeftOut::TooLarge)
    );
    assert_eq!(
        read(&repo.root, "over.bin"),
        Read::LeftOut(LeftOut::TooLarge)
    );
}

#[test]
fn a_tracked_file_that_is_gone_is_vanished() {
    let repo = Repo::new();
    repo.track("gone.rs", b"fn g() {}\n");
    fs::remove_file(repo.root.join("gone.rs")).unwrap();
    assert_eq!(
        read(&repo.root, "gone.rs"),
        Read::LeftOut(LeftOut::Vanished)
    );

    // The whole folder is gone.
    repo.track("dir/inner.rs", b"fn i() {}\n");
    fs::remove_dir_all(repo.root.join("dir")).unwrap();
    assert_eq!(
        read(&repo.root, "dir/inner.rs"),
        Read::LeftOut(LeftOut::Vanished)
    );
}

#[test]
fn a_symlink_candidate_is_counted_and_never_read() {
    let repo = Repo::new();
    repo.write("real.txt", b"secret\n");
    let link = Candidate {
        rel: PathBuf::from("real.txt"),
        kind: CandidateKind::Symlink,
    };
    let containment = Containment::new(&repo.root, std::slice::from_ref(&link), &pool()).unwrap();
    assert_eq!(containment.read(&link), Read::LeftOut(LeftOut::Symlink));
}

#[test]
fn a_path_below_a_folder_that_links_out_of_the_root_is_outside_root() {
    let repo = Repo::new();
    repo.track("sub/file.txt", b"inside\n");
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("file.txt"), "KNOWN-OUTSIDE-LINE\n").unwrap();
    fs::remove_dir_all(repo.root.join("sub")).unwrap();
    std::os::unix::fs::symlink(outside.path(), repo.root.join("sub")).unwrap();

    assert_eq!(
        read(&repo.root, "sub/file.txt"),
        Read::LeftOut(LeftOut::OutsideRoot)
    );
}

#[test]
fn a_folder_link_that_stays_inside_the_root_is_read() {
    let repo = Repo::new();
    repo.write("real/file.txt", b"inside\n");
    std::os::unix::fs::symlink("real", repo.root.join("alias")).unwrap();

    assert_eq!(
        read(&repo.root, "alias/file.txt"),
        Read::Text("inside\n".to_string())
    );
}

#[test]
fn a_last_component_link_swapped_in_after_the_listing_is_not_followed() {
    let repo = Repo::new();
    repo.track("swap.txt", b"original\n");
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("target.txt");
    fs::write(&target, "KNOWN-OUTSIDE-LINE\n").unwrap();

    let candidates = [candidate("swap.txt")];
    let containment = Containment::new(&repo.root, &candidates, &pool()).unwrap();
    // The swap happens after the folder was checked and before the open.
    fs::remove_file(repo.root.join("swap.txt")).unwrap();
    std::os::unix::fs::symlink(&target, repo.root.join("swap.txt")).unwrap();

    assert_eq!(
        containment.read(&candidates[0]),
        Read::LeftOut(LeftOut::Symlink)
    );
}

#[test]
fn a_fifo_in_place_of_a_file_is_unreadable_and_does_not_block() {
    let repo = Repo::new();
    let fifo = repo.root.join("pipe.txt");
    let made = Command::new("mkfifo").arg(&fifo).status().unwrap();
    assert!(made.success());

    let started = std::time::Instant::now();
    assert_eq!(
        read(&repo.root, "pipe.txt"),
        Read::LeftOut(LeftOut::Unreadable)
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn a_folder_in_place_of_a_file_is_not_a_file_and_is_not_counted() {
    let repo = Repo::new();
    fs::create_dir(repo.root.join("nested")).unwrap();
    assert_eq!(read(&repo.root, "nested"), Read::NotAFile);
}

#[test]
fn left_out_counts_add_get_and_total() {
    let mut a = LeftOutCounts::default();
    a.add(LeftOut::Binary);
    a.add(LeftOut::Binary);
    a.add(LeftOut::Symlink);
    a.add(LeftOut::Vanished);

    assert_eq!(a.get(LeftOut::Binary), 2);
    assert_eq!(a.get(LeftOut::Vanished), 1);
    assert_eq!(a.total(), 4);
    let keys: Vec<&str> = a.iter().map(|(r, _)| r.key()).collect();
    assert_eq!(
        keys,
        [
            "binary",
            "not_utf8",
            "too_large",
            "unreadable",
            "vanished",
            "outside_root",
            "symlink"
        ]
    );
}

// ============================================================================
// Hidden and secret-bearing files
// ============================================================================

#[test]
fn the_sensitive_policy_lists_hidden_and_secret_bearing_names() {
    assert_eq!(SENSITIVE_PATH_POLICY, SensitivePolicy::List);
    for name in [
        ".env",
        ".env.production",
        "config/credentials.json",
        "id_rsa",
        ".npmrc",
        ".ssh/id_ed25519",
        "secrets.yaml",
        "server.pem",
    ] {
        assert_eq!(
            sensitive_admission(Path::new(name), SENSITIVE_PATH_POLICY),
            Admission::Admit,
            "{name}"
        );
    }
}

#[test]
fn a_tracked_env_file_is_read_under_the_default_policy() {
    let repo = Repo::new();
    repo.track(".env", b"TOKEN=abc\n");
    let listing = list_universe(&repo.root).unwrap();
    let containment = Containment::new(&repo.root, &listing.candidates, &pool()).unwrap();

    let env = listing
        .candidates
        .iter()
        .find(|c| c.rel == Path::new(".env"))
        .unwrap();
    assert_eq!(containment.read(env), Read::Text("TOKEN=abc\n".to_string()));
}
