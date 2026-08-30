//! Tests for the git hooks module (hooks.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

use tempfile::tempdir;

use super::{has_search_hooks, install_search_hooks, remove_search_hooks};

// ============================================================================
// Helpers
// ============================================================================

/// Create a fake git repo with a hooks dir in `dir`.
fn create_git_repo(dir: &std::path::Path) {
    let hooks_dir = dir.join(".git").join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap();
}

/// Read content of a hook file.
fn read_hook(dir: &std::path::Path, name: &str) -> String {
    fs::read_to_string(dir.join(".git").join("hooks").join(name)).unwrap()
}

// ============================================================================
// install_search_hooks
// ============================================================================

#[test]
fn test_install_creates_hooks_with_markers() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());

    install_search_hooks(dir.path()).unwrap();

    for hook in &["post-commit", "post-merge", "post-checkout"] {
        let content = read_hook(dir.path(), hook);
        assert!(
            content.contains("# skim-search-start"),
            "{hook} should contain start marker"
        );
        assert!(
            content.contains("# skim-search-end"),
            "{hook} should contain end marker"
        );
        assert!(
            content.contains("skim search --update"),
            "{hook} should call skim search --update"
        );
    }
}

#[test]
fn test_install_creates_shebang_when_hook_missing() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());

    install_search_hooks(dir.path()).unwrap();

    let content = read_hook(dir.path(), "post-commit");
    assert!(
        content.starts_with("#!/bin/sh"),
        "new hook should start with #!/bin/sh"
    );
}

#[test]
fn test_install_is_idempotent() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());

    install_search_hooks(dir.path()).unwrap();
    install_search_hooks(dir.path()).unwrap(); // Second call must be a no-op

    let content = read_hook(dir.path(), "post-commit");
    // Should not have duplicate markers
    let start_count = content.matches("# skim-search-start").count();
    assert_eq!(start_count, 1, "install twice should not duplicate markers");
}

#[test]
fn test_install_preserves_existing_content() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());

    let hook_path = dir.path().join(".git").join("hooks").join("post-commit");
    fs::write(&hook_path, "#!/bin/sh\necho 'existing hook'\n").unwrap();

    install_search_hooks(dir.path()).unwrap();

    let content = read_hook(dir.path(), "post-commit");
    assert!(
        content.contains("existing hook"),
        "existing content must be preserved"
    );
    assert!(
        content.contains("# skim-search-start"),
        "skim block must be appended"
    );
}

#[test]
fn test_install_creates_hooks_dir_if_missing() {
    let dir = tempdir().unwrap();
    // Create .git but NOT hooks/
    fs::create_dir_all(dir.path().join(".git")).unwrap();

    install_search_hooks(dir.path()).unwrap();

    let hooks_dir = dir.path().join(".git").join("hooks");
    assert!(hooks_dir.exists(), "hooks dir should be created");
    assert!(
        hooks_dir.join("post-commit").exists(),
        "post-commit hook should exist"
    );
}

// ============================================================================
// remove_search_hooks
// ============================================================================

#[test]
fn test_remove_strips_skim_block() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    install_search_hooks(dir.path()).unwrap();

    remove_search_hooks(dir.path()).unwrap();

    for hook in &["post-commit", "post-merge", "post-checkout"] {
        let path = dir.path().join(".git").join("hooks").join(hook);
        if path.exists() {
            let content = fs::read_to_string(&path).unwrap();
            assert!(
                !content.contains("# skim-search-start"),
                "{hook}: start marker should be removed"
            );
            assert!(
                !content.contains("# skim-search-end"),
                "{hook}: end marker should be removed"
            );
        }
    }
}

#[test]
fn test_remove_preserves_non_skim_content() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());

    let hook_path = dir.path().join(".git").join("hooks").join("post-commit");
    fs::write(&hook_path, "#!/bin/sh\necho 'my hook'\n").unwrap();

    install_search_hooks(dir.path()).unwrap();
    remove_search_hooks(dir.path()).unwrap();

    let content = fs::read_to_string(&hook_path).unwrap();
    assert!(
        content.contains("my hook"),
        "non-skim content should be preserved after removal"
    );
}

#[test]
fn test_remove_is_safe_when_no_hooks_exist() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    // No hooks installed — remove should succeed without error.
    remove_search_hooks(dir.path()).unwrap();
}

// ============================================================================
// has_search_hooks
// ============================================================================

#[test]
fn test_has_hooks_returns_false_before_install() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    assert!(!has_search_hooks(dir.path()));
}

#[test]
fn test_has_hooks_returns_true_after_install() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    install_search_hooks(dir.path()).unwrap();
    assert!(has_search_hooks(dir.path()));
}

#[test]
fn test_has_hooks_returns_false_after_remove() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    install_search_hooks(dir.path()).unwrap();
    remove_search_hooks(dir.path()).unwrap();
    // After removal, no hooks should be detected.
    assert!(!has_search_hooks(dir.path()));
}

// ============================================================================
// #413 / AD-413-15 / AC31 — hooks route to the SHARED commondir directory
// ============================================================================

/// AC31(b)/(c) — in a linked worktree, `resolve_hooks_dir` returns the SHARED
/// `<commondir>/hooks` directory (git's own answer), and install/remove/has all
/// agree on it.  Nothing is written under the per-worktree gitdir.
///
/// Ground truth is taken from git itself (`git -C <wt> rev-parse --git-path hooks`),
/// not assumed, so a routing change that stops matching git fails this test.
///
/// Discriminating: pre-#413 the three call sites hand-built
/// `<root>/.git/hooks`, and `<root>/.git` is a FILE in a linked worktree, so
/// `install_search_hooks` died with `Not a directory (os error 20)` and
/// `remove_search_hooks` silently no-opped while reporting success.
#[test]
fn test_hooks_route_to_shared_commondir_in_linked_worktree() {
    use std::process::Command;

    let dir = tempdir().unwrap();
    let primary = dir.path().join("primary");
    let worktree = dir.path().join("wt1");
    fs::create_dir_all(&primary).unwrap();

    super::super::staleness::create_real_git_repo(&primary, &[("init", &[("a.rs", "fn a(){}\n")])]);
    super::super::staleness::create_real_git_worktree(&primary, &worktree, "b1");

    // Ground truth: git's own hooks path for this worktree, absolutised.
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(["rev-parse", "--git-path", "hooks"])
        .current_dir(&worktree)
        .output()
        .expect("git rev-parse --git-path hooks");
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let gt_hooks = {
        let p = std::path::PathBuf::from(&rel);
        let p = if p.is_absolute() { p } else { worktree.join(p) };
        // The directory may not exist yet; canonicalize the existing parent instead.
        let parent = p.parent().unwrap().canonicalize().unwrap();
        parent.join(p.file_name().unwrap())
    };

    let resolved = super::resolve_hooks_dir(&worktree);
    // `.git` is a FILE in a linked worktree — the pre-#413 path is not a directory.
    assert!(
        worktree.join(".git").is_file(),
        "precondition: a linked worktree's .git must be a file"
    );
    assert_eq!(
        resolved.canonicalize().unwrap_or(resolved.clone()),
        gt_hooks,
        "AC31(b): resolved hooks dir must equal `git rev-parse --git-path hooks`; \
         resolved={resolved:?}, git={gt_hooks:?}"
    );

    // Install must succeed (pre-fix: ENOTDIR) and write into the SHARED dir.
    install_search_hooks(&worktree).unwrap();
    for name in ["post-commit", "post-merge", "post-checkout"] {
        let body = fs::read_to_string(gt_hooks.join(name)).unwrap_or_else(|e| {
            panic!("AC31(b): {name} must exist in the shared hooks dir {gt_hooks:?}: {e}")
        });
        assert_eq!(
            body.matches("# skim-search-start").count(),
            1,
            "AC31(b): exactly one marker block must be present in {name}"
        );
    }

    // AC31(c): nothing written per-worktree.
    // AD-413-12: delegate to resolve_git_dir — the single `gitdir:` parser.
    let wt_gitdir = super::super::staleness::resolve_git_dir(&worktree).unwrap();
    assert!(
        !wt_gitdir.join("hooks").exists(),
        "AC31(c): no per-worktree hooks directory may be created ({wt_gitdir:?})"
    );

    // The install is observable from a DIFFERENT worktree of the same clone (AC34(a)).
    assert!(
        has_search_hooks(&primary),
        "AC34(a): a hook installed from a linked worktree must be visible from the primary"
    );

    // Remove reports truthfully and is reversible from the primary (AC31(a)/AC34(c)).
    assert!(
        remove_search_hooks(&primary).unwrap().changed,
        "AC31(a): remove must report changed=true when a marker block was actually removed"
    );
    assert!(
        !remove_search_hooks(&primary).unwrap().changed,
        "AC31(a): a second remove must report changed=false so no false success line is printed"
    );
    assert!(!has_search_hooks(&worktree));
}

/// Security (AD-413-3 extension): a crafted `gitdir:` pointer that resolves to a
/// directory which is NOT a real git directory (`HEAD` present but `objects/` and
/// `refs/` absent) must NOT be used as a write destination.  `resolve_hooks_dir`
/// must fall back to the safe local `<root>/.git/hooks` instead.
///
/// This tests the write-path sanity gate added in the fix for the hooks.rs security
/// finding (hooks.rs `looks_like_git_dir`).  The attacker scenario:
/// - A tarball is extracted and contains a `.git` FILE (git itself won't check
///   one out, but archive tools will) pointing at an arbitrary target directory.
/// - skim must not create executables in that arbitrary directory.
#[test]
fn test_resolve_hooks_dir_rejects_crafted_gitdir_with_no_git_structure() {
    let dir = tempdir().unwrap();
    let project = dir.path().join("project");
    fs::create_dir_all(&project).unwrap();

    // A target that has a HEAD file but NOT objects/ or refs/ — just enough to
    // look superficially like a git dir but not enough to pass looks_like_git_dir.
    let target = dir.path().join("attacker_target");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    // Deliberately omitting objects/ and refs/ subdirectories.

    // Plant a .git FILE that points to the attacker's directory.
    let gitdir_line = format!("gitdir: {}\n", target.display());
    fs::write(project.join(".git"), &gitdir_line).unwrap();

    let resolved = super::resolve_hooks_dir(&project);

    // Must fall back to the safe local path, not the attacker's directory.
    assert_eq!(
        resolved,
        project.join(".git").join("hooks"),
        "security: a gitdir: pointer to a non-git-dir (HEAD only, no objects/refs) \
         must fall back to the safe local hooks path; got {resolved:?}"
    );
}

/// AC31(e) / AC5b monotonicity — a plain repo and a non-repo directory keep the
/// pre-#413 `<root>/.git/hooks` path, byte for byte.
#[test]
fn test_resolve_hooks_dir_unchanged_for_plain_repo_and_non_repo() {
    let dir = tempdir().unwrap();

    // Plain repo: `.git` is a directory with no `commondir`.
    let plain = dir.path().join("plain");
    fs::create_dir_all(&plain).unwrap();
    super::super::staleness::create_real_git_repo(&plain, &[("init", &[("a.rs", "fn a(){}\n")])]);
    assert!(
        !plain.join(".git").join("commondir").exists(),
        "precondition: a plain repo has no commondir"
    );
    assert_eq!(
        super::resolve_hooks_dir(&plain),
        plain.join(".git").join("hooks"),
        "AC31(e): a plain repo must resolve to <root>/.git/hooks exactly as before"
    );

    // Non-repo directory (the shape every pre-existing hooks_tests case uses).
    let non_repo = dir.path().join("non_repo");
    fs::create_dir_all(&non_repo).unwrap();
    assert_eq!(
        super::resolve_hooks_dir(&non_repo),
        non_repo.join(".git").join("hooks"),
        "AC31(e): a non-repo root must resolve to <root>/.git/hooks exactly as before"
    );
}
