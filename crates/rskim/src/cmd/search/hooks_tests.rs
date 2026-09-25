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

    let resolved = super::resolve_hooks_dir(&worktree)
        .expect("linked worktree has a .git FILE — resolve_hooks_dir must return Ok");
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

    let resolved = super::resolve_hooks_dir(&project)
        .expect("project with .git FILE (invalid gitdir) — resolve_hooks_dir must return Ok");

    // Must fall back to the safe local path, not the attacker's directory.
    assert_eq!(
        resolved,
        project.join(".git").join("hooks"),
        "security: a gitdir: pointer to a non-git-dir (HEAD only, no objects/refs) \
         must fall back to the safe local hooks path; got {resolved:?}"
    );
}

// ============================================================================
// #413 / AC34(b) — SHARED_HOOKS_SCOPE_MSG constant content guard
// ============================================================================

/// AC34(b) constant guard — the disclosure message printed by both
/// `install_search_hooks` and `remove_search_hooks` when the resolved hooks
/// directory is the shared `<commondir>/hooks` must contain both key phrases.
///
/// This test anchors the string content of `SHARED_HOOKS_SCOPE_MSG` so that
/// the two `eprintln!` call sites in `hooks.rs` cannot drift away from the
/// phrases that AC34(b) requires ("every worktree" and "clone").
///
/// Combined with `test_ac34_multi_worktree_blast_radius` below, which asserts
/// that `is_shared_hooks_dir` returns `true` for a real linked-worktree root,
/// the two tests together prove that the disclosure path is taken and emits the
/// required content — without requiring stderr capture in a unit test.
#[test]
fn test_shared_hooks_scope_msg_contains_required_phrases() {
    let msg = super::SHARED_HOOKS_SCOPE_MSG;
    assert!(
        msg.contains("every worktree"),
        "AC34(b): SHARED_HOOKS_SCOPE_MSG must contain 'every worktree'; got: {msg:?}"
    );
    assert!(
        msg.contains("clone"),
        "AC34(b): SHARED_HOOKS_SCOPE_MSG must contain 'clone'; got: {msg:?}"
    );
    // The message must not inadvertently claim a per-worktree scope.
    assert!(
        !msg.contains("/.git/hooks"),
        "AC34(b): SHARED_HOOKS_SCOPE_MSG must not reference a per-worktree hooks path; \
         got: {msg:?}"
    );
}

// ============================================================================
// #413 / AC34(b)/(c)/(d) — multi-worktree blast radius is real, bounded,
// idempotent across worktrees, and the disclosure predicate fires correctly
// ============================================================================

/// AC34(b)/(c)/(d) — S37 scenario in unit-test form.
///
/// Fixture: primary + wt1 + wt2, with a foreign `post-commit` body and a
/// `pre-push` file planted in `<commondir>/hooks` **before** any skim command.
///
/// **(b)** `is_shared_hooks_dir` returns `true` for both linked-worktree roots,
/// proving the predicate that gates `eprintln!({SHARED_HOOKS_SCOPE_MSG})` in
/// both `install_search_hooks` and `remove_search_hooks`.  Together with
/// `test_shared_hooks_scope_msg_contains_required_phrases`, this establishes
/// that the disclosure string has the required content AND the predicate fires.
///
/// **(c)** After install the planted foreign `post-commit` body is byte-identical
/// to its pre-install content (the skim block was appended, not prepended over
/// it).  The planted `pre-push` file is untouched.  After remove the foreign
/// body is byte-identical to its original form and `pre-push` remains untouched.
///
/// **(d)** Installing a second time from `wt2` and a third time from `primary`
/// each return `changed=false` (the idempotency no-op) and each leave the
/// `# skim-search-start` marker count at exactly 1 per hook file.
#[test]
fn test_ac34_multi_worktree_blast_radius() {
    use std::process::Command;

    let dir = tempdir().unwrap();
    let primary = dir.path().join("primary");
    let wt1 = dir.path().join("wt1");
    let wt2 = dir.path().join("wt2");
    fs::create_dir_all(&primary).unwrap();

    super::super::staleness::create_real_git_repo(&primary, &[("init", &[("a.rs", "fn a(){}\n")])]);
    super::super::staleness::create_real_git_worktree(&primary, &wt1, "b1");
    super::super::staleness::create_real_git_worktree(&primary, &wt2, "b2");

    // Ground-truth shared hooks directory for wt1 (same for wt2 and primary).
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(["rev-parse", "--git-path", "hooks"])
        .current_dir(&wt1)
        .output()
        .expect("git rev-parse --git-path hooks");
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let gt_hooks = {
        let p = std::path::PathBuf::from(&rel);
        let p = if p.is_absolute() { p } else { wt1.join(p) };
        let parent = p.parent().unwrap().canonicalize().unwrap();
        parent.join(p.file_name().unwrap())
    };

    // Precondition: both wt1 and wt2 have a .git FILE (linked worktree).
    assert!(
        wt1.join(".git").is_file(),
        "precondition: wt1/.git must be a file (linked worktree)"
    );
    assert!(
        wt2.join(".git").is_file(),
        "precondition: wt2/.git must be a file (linked worktree)"
    );

    // Plant a foreign post-commit body BEFORE any skim command (AC34(c)).
    let foreign_pc_body = "#!/bin/sh\necho FOREIGN_POST_COMMIT\n";
    let pc_path = gt_hooks.join("post-commit");
    fs::create_dir_all(&gt_hooks).unwrap();
    fs::write(&pc_path, foreign_pc_body).unwrap();

    // Plant a foreign pre-push file that skim must never touch (AC34(c)).
    let foreign_pp_body = "#!/bin/sh\necho FOREIGN_PRE_PUSH\n";
    let pp_path = gt_hooks.join("pre-push");
    fs::write(&pp_path, foreign_pp_body).unwrap();

    // ── AC34(b): is_shared_hooks_dir fires for every linked-worktree root ──
    // Both linked-worktree roots must return true so the eprintln! path is taken.
    assert!(
        super::is_shared_hooks_dir(&wt1, &gt_hooks),
        "AC34(b): is_shared_hooks_dir must be true for wt1 (linked worktree)"
    );
    assert!(
        super::is_shared_hooks_dir(&wt2, &gt_hooks),
        "AC34(b): is_shared_hooks_dir must be true for wt2 (linked worktree)"
    );

    // ── First install (from wt1) ──────────────────────────────────────────────
    let out1 = install_search_hooks(&wt1).unwrap();
    assert!(
        out1.changed,
        "first install from wt1 must report changed=true"
    );

    // Foreign post-commit body must be preserved — skim block appended, not overwriting.
    let pc_after_install1 = fs::read_to_string(&pc_path).unwrap();
    assert!(
        pc_after_install1.starts_with(foreign_pc_body),
        "AC34(c): foreign post-commit body must be byte-identical (as prefix) after 1st install; \
         got: {pc_after_install1:?}"
    );
    assert!(
        pc_after_install1.contains("# skim-search-start"),
        "AC34(c): skim block must be appended after the foreign body"
    );

    // pre-push must be completely untouched.
    assert_eq!(
        fs::read_to_string(&pp_path).unwrap(),
        foreign_pp_body,
        "AC34(c): pre-push file must be byte-identical after 1st install"
    );

    // Exactly one marker block per hook file installed.
    for name in ["post-commit", "post-merge", "post-checkout"] {
        let content =
            fs::read_to_string(gt_hooks.join(name)).expect("hook file must exist after install");
        assert_eq!(
            content.matches("# skim-search-start").count(),
            1,
            "AC34(d): exactly one marker block in {name} after 1st install"
        );
    }

    // ── Second install (from wt2 — DIFFERENT worktree) ───────────────────────
    let out2 = install_search_hooks(&wt2).unwrap();
    assert!(
        !out2.changed,
        "AC34(d): 2nd install from wt2 must report changed=false (idempotent)"
    );
    for name in ["post-commit", "post-merge", "post-checkout"] {
        let count = fs::read_to_string(gt_hooks.join(name))
            .unwrap()
            .matches("# skim-search-start")
            .count();
        assert_eq!(
            count, 1,
            "AC34(d): marker count must still be 1 in {name} after 2nd install from wt2"
        );
    }

    // ── Third install (from primary) ──────────────────────────────────────────
    let out3 = install_search_hooks(&primary).unwrap();
    assert!(
        !out3.changed,
        "AC34(d): 3rd install from primary must report changed=false (idempotent)"
    );
    for name in ["post-commit", "post-merge", "post-checkout"] {
        let count = fs::read_to_string(gt_hooks.join(name))
            .unwrap()
            .matches("# skim-search-start")
            .count();
        assert_eq!(
            count, 1,
            "AC34(d): marker count must still be 1 in {name} after 3rd install from primary"
        );
    }

    // ── Remove (from wt2 — DIFFERENT worktree from installer) ────────────────
    let rm_out = remove_search_hooks(&wt2).unwrap();
    assert!(
        rm_out.changed,
        "AC34(c): remove from wt2 must report changed=true (markers were present)"
    );

    // Marker blocks gone from all three hook names.
    for name in ["post-commit", "post-merge", "post-checkout"] {
        let path = gt_hooks.join(name);
        if path.exists() {
            let content = fs::read_to_string(&path).unwrap();
            assert!(
                !content.contains("# skim-search-start"),
                "AC34(c): skim-search-start must be removed from {name} after remove"
            );
        }
    }

    // Foreign post-commit body must be byte-identical after remove (AC34(c)).
    let pc_after_remove = fs::read_to_string(&pc_path).unwrap();
    assert_eq!(
        pc_after_remove, foreign_pc_body,
        "AC34(c): foreign post-commit body must be byte-identical after remove"
    );

    // pre-push must still be completely untouched (AC34(c)).
    assert_eq!(
        fs::read_to_string(&pp_path).unwrap(),
        foreign_pp_body,
        "AC34(c): pre-push file must be byte-identical after remove"
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
        super::resolve_hooks_dir(&plain).unwrap(),
        plain.join(".git").join("hooks"),
        "AC31(e): a plain repo must resolve to <root>/.git/hooks exactly as before"
    );

    // Non-repo directory (the shape every pre-existing hooks_tests case uses).
    // This directory has no .git and no enclosing git repository (it is inside
    // a system temp dir), so resolve_hooks_dir still returns Some(<root>/.git/hooks).
    let non_repo = dir.path().join("non_repo");
    fs::create_dir_all(&non_repo).unwrap();
    assert_eq!(
        super::resolve_hooks_dir(&non_repo).unwrap(),
        non_repo.join(".git").join("hooks"),
        "AC31(e): a non-repo root must resolve to <root>/.git/hooks exactly as before"
    );
}

// ============================================================================
// Subdirectory-root refusal (review finding — AD-413-15 extension)
// ============================================================================

/// A subdirectory of a git repo must not receive a fabricated `.git` entry.
///
/// Pre-fix: `resolve_hooks_dir` fell through to `<root>/.git/hooks` for any
/// root with no `.git`, and `install_search_hooks` then called `create_dir_all`
/// on that path, creating a real `.git` **directory** at the subdirectory level.
/// This permanently disabled `resolve_repo_toplevel` (AC17: it refuses to walk
/// past an existing `.git` entry) and made the temporal layer silently dead
/// (PF-017).
///
/// Post-fix:
/// - `resolve_hooks_dir` returns `None` for a subdirectory root.
/// - `install_search_hooks` returns `Err` naming the enclosing repository.
/// - No `.git` entry is created.
#[test]
fn test_install_hooks_refuses_subdirectory_root() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    super::super::staleness::create_real_git_repo(&repo, &[("init", &[("a.rs", "fn a(){}\n")])]);

    // Create a subdirectory inside the repo — no .git of its own.
    let subdir = repo.join("src");
    fs::create_dir_all(&subdir).unwrap();

    // resolve_hooks_dir must return Err for a subdirectory of a git repo (F9/AD-413-18).
    assert!(
        super::resolve_hooks_dir(&subdir).is_err(),
        "a subdirectory of a git repo must yield Err from resolve_hooks_dir; \
         pre-fix: returned <subdir>/.git/hooks and let create_dir_all fabricate \
         a fake .git directory"
    );

    // install_search_hooks must refuse with an Err naming the enclosing repo.
    let err = install_search_hooks(&subdir).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("subdirectory") || msg.contains("inside"),
        "error must describe the subdirectory constraint; got: {msg}"
    );

    // Most critical invariant: no .git entry must exist at the subdirectory level.
    assert!(
        !subdir.join(".git").exists(),
        "install must not create a .git entry in a subdirectory root; \
         this breaks ancestor adoption (resolve_repo_toplevel AC17) and \
         makes the temporal layer silently dead (PF-017)"
    );

    // remove_search_hooks is symmetric: also refuses on subdirectory roots.
    let remove_err = remove_search_hooks(&subdir).unwrap_err();
    let remove_msg = remove_err.to_string();
    assert!(
        remove_msg.contains("subdirectory") || remove_msg.contains("inside"),
        "remove_search_hooks must also refuse a subdirectory root; got: {remove_msg}"
    );

    // has_search_hooks returns false rather than panicking on a subdirectory root.
    assert!(
        !has_search_hooks(&subdir),
        "has_search_hooks must return false for a subdirectory root"
    );
}

/// Missing test 6 / P2-3 / AD-413-18: `resolve_hooks_dir` on a subdirectory must
/// return `Err(ancestor)` where `ancestor` is the canonical path of the enclosing
/// repository, not merely `Err(_)`.
///
/// Discriminating: the `Err` payload equals the canonical enclosing repo path so
/// callers can embed it in a human-readable hint ("re-run with `--root <payload>`").
#[test]
fn test_resolve_hooks_dir_subdirectory_err_payload_is_ancestor_canonical() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    super::super::staleness::create_real_git_repo(&repo, &[("init", &[("a.rs", "fn a(){}\n")])]);
    let repo_canonical = repo.canonicalize().unwrap();

    let subdir = repo.join("lib");
    fs::create_dir_all(&subdir).unwrap();

    // The Err payload must be the canonical ancestor path (P2-3 / AD-413-18).
    let result = super::resolve_hooks_dir(&subdir);
    assert_eq!(
        result,
        Err(repo_canonical.clone()),
        "resolve_hooks_dir must return Err(enclosing_repo_canonical) for a subdirectory; \
         callers embed this path in the '--root <path>' hint (P2-3)"
    );

    // The install error message must name the canonical repo path so the user can
    // copy-paste the `--root` flag without quoting surprises (P3-4).
    let err = install_search_hooks(&subdir).unwrap_err();
    let msg = err.to_string();
    let repo_display = repo_canonical.display().to_string();
    assert!(
        msg.contains(&repo_display),
        "install error must contain the canonical repo path '{repo_display}'; got: {msg}"
    );
}

/// Submodule case — AC34(b) correctness (Findings 3+4 of the #413 review):
///
/// A git submodule's `.git` is a file whose `gitdir:` pointer resolves to
/// `<superproject>/.git/modules/<name>`.  That gitdir is a self-contained private
/// ref store with no `commondir` file, so the hooks directory is NOT shared across
/// worktrees.
///
/// (a) `install_search_hooks` must return `shared: false` — the "shared by every
///     worktree of this clone" disclosure must NOT fire for a submodule root.
///     The pre-fix path-inequality gate (`hooks_dir != root/.git/hooks`) fired
///     for submodules because their resolved dir differs from the hand-built path.
///
/// (b) The hooks are installed into the submodule's private gitdir, not the
///     superproject's hooks directory.
///
/// (b2) After `canonicalize()` (the display-site normalization), the path contains
///      no `..` components.  The raw pointer written by git is relative
///      (`gitdir: ../.git/modules/sub`), so `resolve_git_dir` returns a joined,
///      non-normalized path without the display-site fix.
///
/// Uses a real `git submodule add` fixture — a fake `.git` file would not exercise
/// the relative-pointer code path in `resolve_git_dir`.
#[test]
fn test_install_hooks_submodule_no_shared_disclosure_and_normalized_path() {
    use std::process::Command;

    let dir = tempdir().unwrap();
    let super_root = dir.path().join("superproject");
    let sub_origin = dir.path().join("sub_origin");

    // Build the sub-repository that will be added as a submodule.
    fs::create_dir_all(&sub_origin).unwrap();
    super::super::staleness::create_real_git_repo(
        &sub_origin,
        &[("init", &[("lib.rs", "fn f(){}\n")])],
    );

    // Build the superproject.
    fs::create_dir_all(&super_root).unwrap();
    super::super::staleness::create_real_git_repo(
        &super_root,
        &[("init", &[("README.md", "readme\n")])],
    );

    // Add sub_origin as a submodule named "sub" inside the superproject.
    // `protocol.file.allow=always` is required when adding a local-path submodule
    // from within git 2.38+ (file protocol is restricted by default since CVE-2022-39253).
    let out = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args([
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--name",
            "sub",
            sub_origin.to_str().unwrap(),
            "sub",
        ])
        .current_dir(&super_root)
        .output()
        .expect("git submodule add (spawn)");
    if !out.status.success() {
        // git submodule may be unavailable in CI — skip gracefully.
        eprintln!(
            "test_install_hooks_submodule: git submodule add failed ({}); skipping.\nstderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        return;
    }

    let sub_root = super_root.join("sub");

    // Precondition: the submodule checkout must have a `.git` FILE (not a directory).
    assert!(
        sub_root.join(".git").is_file(),
        "submodule .git must be a file, not a directory (precondition)"
    );

    // (a) install_search_hooks must return shared: false — submodule gitdir has no
    // commondir, so the hooks dir is private, not clone-wide-shared.
    let outcome = install_search_hooks(&sub_root)
        .expect("install_search_hooks must succeed on a submodule root");
    assert!(
        !outcome.shared,
        "submodule hooks dir is private (no commondir); \
         outcome.shared must be false, not 'shared by every worktree'"
    );

    // (b) Hooks must be installed in the submodule's private gitdir, not the
    // superproject's hooks directory.
    let super_hooks = super_root.join(".git").join("hooks");
    assert!(
        !outcome.dir.starts_with(&super_hooks),
        "submodule hooks must NOT be in the superproject hooks dir ({super_hooks:?}); \
         got: {:?}",
        outcome.dir
    );
    for name in ["post-commit", "post-merge", "post-checkout"] {
        assert!(
            outcome.dir.join(name).exists(),
            "hook {name} must be installed in the submodule gitdir {:?}",
            outcome.dir
        );
    }

    // (b2) The canonicalized display path must not contain `..`.
    // `resolve_git_dir` joins the relative pointer as-is (e.g.
    // `sub_root.join("../.git/modules/sub")`).  The display-site
    // `canonicalize()` normalizes this before printing.
    let display_dir = outcome
        .dir
        .canonicalize()
        .expect("submodule hooks dir must exist after install (for canonicalize)");
    let display_str = display_dir.to_string_lossy();
    assert!(
        !display_str.contains(".."),
        "canonicalized display path must not contain '..'; got: {display_str}"
    );
}

// ============================================================================
// Missing test 7 / F8 — control characters in gitdir pointer are not forwarded raw
// ============================================================================

/// Missing test 7 / F8 / AD-413-3 extension: a `gitdir:` pointer containing
/// ASCII control characters (`\r`, `\n`, ESC, NUL) must not appear raw in any
/// error or warning message emitted by `install_search_hooks` or
/// `remove_search_hooks`.
///
/// The F8 fix uses `.display()` (not `{:?}`) for the `--root` hint and wraps the
/// subdirectory path in `{:?}` for the body — neither form should emit raw ESC/CR
/// bytes into the user's terminal (where they would be interpreted as ANSI escapes
/// or CR-rewrite).
///
/// Discriminating: the emitted error message contains no byte in `0x01..=0x1F`
/// other than `\t` (0x09) when a crafted `.git` file contains a control character
/// in its `gitdir:` line.
#[test]
fn test_hooks_refusal_quotes_control_chars_in_gitdir() {
    let dir = tempdir().unwrap();
    let project = dir.path().join("project");
    fs::create_dir_all(&project).unwrap();

    // Craft a .git FILE whose gitdir: line contains a CR+LF and an ESC byte.
    // These are the most dangerous control characters for terminal output.
    let evil_target = dir.path().join("evil\x1b[31mred\x1b[0m");
    let gitdir_line = format!("gitdir: {}\r\n", evil_target.display());
    fs::write(project.join(".git"), gitdir_line.as_bytes()).unwrap();

    // install_search_hooks and remove_search_hooks may return Ok or Err; what
    // matters is that no error message contains a raw control byte (excluding tab).
    let install_err = install_search_hooks(&project)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();
    let remove_err = remove_search_hooks(&project)
        .err()
        .map(|e| e.to_string())
        .unwrap_or_default();

    for (label, msg) in [("install", &install_err), ("remove", &remove_err)] {
        for (i, byte) in msg.bytes().enumerate() {
            // Allow tab (0x09). Reject all other ASCII control characters.
            if byte < 0x20 && byte != 0x09 {
                panic!(
                    "{label} error message must not contain raw control byte 0x{byte:02X} \
                     at position {i}; message: {:?}",
                    &msg[..msg.len().min(200)]
                );
            }
        }
    }
}
