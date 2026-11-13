//! Tests for the old-hook removal (hooks.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;

use tempfile::tempdir;

use super::{
    HookFailure, HookStep, HooksFinding, find_search_hooks, remove_from_dir, remove_search_hooks,
    scan_dir,
};

// ============================================================================
// Helpers
// ============================================================================

/// The block that skim 2.x wrote into the three search hooks.
const HOOK_BLOCK: &str =
    "# skim-search-start\nskim search --update 2>/dev/null &\n# skim-search-end";

const HOOK_NAMES: [&str; 3] = ["post-commit", "post-merge", "post-checkout"];

/// The text of a hook as skim 2.x left it: `existing` first, a newline if it had
/// none, then the block and a newline.
fn with_old_block(existing: &str) -> String {
    let mut text = existing.to_string();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(HOOK_BLOCK);
    text.push('\n');
    text
}

/// Write the hooks skim 2.x created in a repository that had none: a shebang
/// line and the block.
fn plant_fresh_hooks(hooks_dir: &Path) {
    fs::create_dir_all(hooks_dir).unwrap();
    for name in HOOK_NAMES {
        fs::write(hooks_dir.join(name), with_old_block("#!/bin/sh")).unwrap();
    }
}

/// Create a fake git repo with a hooks dir in `dir`.
fn create_git_repo(dir: &Path) {
    fs::create_dir_all(dir.join(".git").join("hooks")).unwrap();
}

fn hooks_dir(dir: &Path) -> std::path::PathBuf {
    dir.join(".git").join("hooks")
}

/// Run `git` in `dir`, isolated from the ambient git configuration, and
/// assert that it succeeds.
fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Create a real git repository in `dir` with one commit per entry of
/// `commit_files`: the message, then the `(path, content)` files to add.
#[allow(clippy::type_complexity)]
fn create_real_git_repo(dir: &Path, commit_files: &[(&str, &[(&str, &str)])]) {
    git(dir, &["init"]);
    git(dir, &["config", "user.email", "test@example.com"]);
    git(dir, &["config", "user.name", "Test"]);
    for (message, files) in commit_files {
        for (name, content) in *files {
            let path = dir.join(name);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&path, content).unwrap();
            git(dir, &["add", name]);
        }
        git(dir, &["commit", "-m", message]);
    }
}

/// Add a linked worktree at `worktree` on a new branch `branch` of the
/// repository at `primary`.
fn create_real_git_worktree(primary: &Path, worktree: &Path, branch: &str) {
    git(primary, &["branch", branch]);
    git(
        primary,
        &["worktree", "add", worktree.to_str().unwrap(), branch],
    );
}

/// The hooks directory git itself names for `worktree`, made absolute.
fn git_hooks_dir(worktree: &Path) -> std::path::PathBuf {
    let out = std::process::Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(["rev-parse", "--git-path", "hooks"])
        .current_dir(worktree)
        .output()
        .expect("git rev-parse --git-path hooks");
    let rel = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let p = std::path::PathBuf::from(&rel);
    let p = if p.is_absolute() { p } else { worktree.join(p) };
    // The directory may not exist yet; canonicalize the existing parent instead.
    let parent = p.parent().unwrap().canonicalize().unwrap();
    parent.join(p.file_name().unwrap())
}

// ============================================================================
// remove_search_hooks
// ============================================================================

#[test]
fn test_remove_strips_the_block_from_every_hook() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    plant_fresh_hooks(&hooks_dir(dir.path()));

    let outcome = remove_search_hooks(dir.path()).unwrap();

    assert!(outcome.changed, "a block was removed, so changed is true");
    for name in HOOK_NAMES {
        assert_eq!(
            fs::read_to_string(hooks_dir(dir.path()).join(name)).unwrap(),
            "#!/bin/sh\n",
            "{name}: only the shebang line must remain"
        );
    }
}

/// Foreign bytes survive removal exactly: blank lines before and after the
/// block, tabs, carriage returns, and a line with no final newline.
///
/// Discriminating: a remover that trims the blank lines in front of the block
/// (or that cuts at a marker found inside a line) changes these bytes.
#[test]
fn test_remove_keeps_every_foreign_byte() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    let before = "#!/bin/sh\n\n# my hook\nif [ -x ./go ]; then\n\t./go \"$@\"\nfi\r\n\n\n";
    let after = "\nexport KEEP=1\r\n\n\ttrailing line without newline";
    let hook = hooks_dir(dir.path()).join("post-commit");
    fs::write(&hook, format!("{before}{HOOK_BLOCK}\n{after}")).unwrap();

    let outcome = remove_search_hooks(dir.path()).unwrap();

    assert!(outcome.changed);
    assert_eq!(
        fs::read_to_string(&hook).unwrap(),
        format!("{before}{after}"),
        "removal must delete the block lines and no other byte"
    );
}

/// A foreign script that prints or greps the marker text has the marker inside a
/// longer line. That is not a block, so the hook stays byte for byte.
#[test]
fn test_remove_leaves_marker_text_inside_a_longer_line() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    let text = "#!/bin/sh\necho \"# skim-search-start\"\ngrep -q '# skim-search-end' notes\n";
    let hook = hooks_dir(dir.path()).join("post-merge");
    fs::write(&hook, text).unwrap();

    let outcome = remove_search_hooks(dir.path()).unwrap();

    assert!(!outcome.changed);
    assert_eq!(fs::read_to_string(&hook).unwrap(), text);
    assert_eq!(
        find_search_hooks(dir.path()),
        None,
        "doctor must not report text that removal would keep"
    );
}

/// A start marker with no end marker is not a block that skim wrote.
#[test]
fn test_remove_leaves_a_start_marker_without_an_end_marker() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    let text = "#!/bin/sh\n# skim-search-start\necho mine\n";
    let hook = hooks_dir(dir.path()).join("post-checkout");
    fs::write(&hook, text).unwrap();

    let outcome = remove_search_hooks(dir.path()).unwrap();

    assert!(!outcome.changed);
    assert_eq!(fs::read_to_string(&hook).unwrap(), text);
    assert_eq!(find_search_hooks(dir.path()), None);
}

#[test]
fn test_remove_is_safe_when_no_hooks_exist() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    let outcome = remove_search_hooks(dir.path()).unwrap();
    assert!(!outcome.changed);
}

/// A hook that was not executable must not become executable: git would start to
/// run a script that its owner had switched off.
#[cfg(unix)]
#[test]
fn test_remove_keeps_the_file_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    for (name, mode) in [("post-commit", 0o644), ("post-merge", 0o700)] {
        let hook = hooks_dir(dir.path()).join(name);
        fs::write(&hook, with_old_block("#!/bin/sh\necho mine")).unwrap();
        fs::set_permissions(&hook, fs::Permissions::from_mode(mode)).unwrap();
    }

    remove_search_hooks(dir.path()).unwrap();

    for (name, mode) in [("post-commit", 0o644), ("post-merge", 0o700)] {
        let found = fs::metadata(hooks_dir(dir.path()).join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(found, mode, "{name}: removal must keep the file mode");
    }
}

/// A hook that is a link belongs to whoever made the link. The rewrite replaces
/// the file at the hook path, so it would turn the link into a copy.
#[cfg(unix)]
#[test]
fn test_remove_leaves_a_symlinked_hook_alone() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    let shared = dir.path().join("shared-hook.sh");
    let text = with_old_block("#!/bin/sh\necho shared");
    fs::write(&shared, &text).unwrap();
    let hook = hooks_dir(dir.path()).join("post-commit");
    std::os::unix::fs::symlink(&shared, &hook).unwrap();

    let outcome = remove_search_hooks(dir.path()).unwrap();

    assert!(!outcome.changed);
    assert!(
        fs::symlink_metadata(&hook)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link must still be a link"
    );
    assert_eq!(fs::read_to_string(&shared).unwrap(), text);
    assert_eq!(find_search_hooks(dir.path()), None);
}

/// A file above the read cap is not a hook skim wrote, and removal does not read
/// it whole.
#[test]
fn test_remove_leaves_an_oversized_hook_alone() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    let mut text = with_old_block("#!/bin/sh");
    text.push_str(&"# padding line\n".repeat(80_000));
    assert!(text.len() as u64 > super::MAX_HOOK_BYTES);
    let hook = hooks_dir(dir.path()).join("post-commit");
    fs::write(&hook, &text).unwrap();

    let outcome = remove_search_hooks(dir.path()).unwrap();

    assert!(!outcome.changed);
    assert_eq!(fs::read_to_string(&hook).unwrap(), text);
}

/// A hook that is not UTF-8 text (a compiled program) is left as it is, and the
/// other hooks are still cleaned.
#[test]
fn test_remove_leaves_a_binary_hook_alone_and_cleans_the_others() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    let mut binary = b"\xff\xfe\x00 not text\n".to_vec();
    binary.extend_from_slice(HOOK_BLOCK.as_bytes());
    let hook = hooks_dir(dir.path()).join("post-commit");
    fs::write(&hook, &binary).unwrap();
    let other = hooks_dir(dir.path()).join("post-merge");
    fs::write(&other, with_old_block("#!/bin/sh")).unwrap();

    let outcome = remove_search_hooks(dir.path()).unwrap();

    assert!(outcome.changed);
    assert_eq!(fs::read(&hook).unwrap(), binary);
    assert_eq!(fs::read_to_string(&other).unwrap(), "#!/bin/sh\n");
}

// ============================================================================
// find_search_hooks
// ============================================================================

#[test]
fn test_find_reports_the_directory_while_a_block_is_present() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    assert_eq!(find_search_hooks(dir.path()), None, "no hooks yet");

    fs::write(
        hooks_dir(dir.path()).join("post-merge"),
        with_old_block("#!/bin/sh\necho mine"),
    )
    .unwrap();
    assert_eq!(
        find_search_hooks(dir.path()),
        Some(HooksFinding {
            dir: hooks_dir(dir.path()).canonicalize().unwrap(),
            block_found: true,
            unreadable: Vec::new(),
        })
    );

    remove_search_hooks(dir.path()).unwrap();
    assert_eq!(
        find_search_hooks(dir.path()),
        None,
        "after removal no block is found"
    );
}

// ============================================================================
// A hook that cannot be read does not stop the others
// ============================================================================

/// A reader that fails for the hook named `broken` and reads every other hook as
/// `read_hook` does. A process may run as root, and root reads a file of mode 000,
/// so a stand-in failure is the only one that every environment can raise.
fn reader_failing_on(
    broken: &'static str,
) -> impl Fn(&Path) -> Result<Option<super::Hook>, HookFailure> {
    move |path: &Path| {
        if path.file_name().is_some_and(|name| name == broken) {
            Err(HookFailure {
                path: path.to_path_buf(),
                step: HookStep::Read,
                kind: std::io::ErrorKind::PermissionDenied,
            })
        } else {
            super::read_hook(path)
        }
    }
}

/// The first hook cannot be read. The two after it are still cleaned, and the
/// failed one is listed and left as it was.
#[test]
fn test_remove_cleans_the_other_hooks_when_the_first_cannot_be_read() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    plant_fresh_hooks(&hooks_dir(dir.path()));

    let removal = remove_from_dir(&hooks_dir(dir.path()), &reader_failing_on("post-commit"));

    assert!(removal.changed, "the two readable hooks lost their block");
    assert_eq!(removal.failures.len(), 1, "{:?}", removal.failures);
    assert_eq!(
        removal.failures[0].path,
        hooks_dir(dir.path()).join("post-commit")
    );
    assert_eq!(
        fs::read_to_string(hooks_dir(dir.path()).join("post-commit")).unwrap(),
        with_old_block("#!/bin/sh"),
        "the unreadable hook stays as it was"
    );
    for name in ["post-merge", "post-checkout"] {
        assert_eq!(
            fs::read_to_string(hooks_dir(dir.path()).join(name)).unwrap(),
            "#!/bin/sh\n",
            "{name}: a hook after the failed one must still be cleaned"
        );
    }
}

/// A block removed from an earlier hook is still reported when a later hook
/// cannot be read.
#[test]
fn test_remove_keeps_the_changed_flag_when_a_later_hook_cannot_be_read() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    plant_fresh_hooks(&hooks_dir(dir.path()));

    let removal = remove_from_dir(&hooks_dir(dir.path()), &reader_failing_on("post-checkout"));

    assert!(removal.changed);
    assert_eq!(removal.failures.len(), 1, "{:?}", removal.failures);
}

/// Doctor must not read an unreadable hook as clean, and must still see the
/// block of the hooks after it.
#[test]
fn test_scan_reports_an_unreadable_hook_and_still_finds_the_block_after_it() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    plant_fresh_hooks(&hooks_dir(dir.path()));

    let scan = scan_dir(&hooks_dir(dir.path()), &reader_failing_on("post-commit"));

    assert!(
        scan.block_found,
        "post-merge and post-checkout hold a block"
    );
    assert_eq!(scan.unreadable.len(), 1, "{:?}", scan.unreadable);
    assert_eq!(
        scan.unreadable[0].path,
        hooks_dir(dir.path()).join("post-commit")
    );
}

/// With no readable block anywhere, an unreadable hook alone is still a finding.
#[test]
fn test_scan_reports_an_unreadable_hook_when_no_block_is_found() {
    let dir = tempdir().unwrap();
    create_git_repo(dir.path());
    fs::write(hooks_dir(dir.path()).join("post-merge"), "#!/bin/sh\n").unwrap();

    let scan = scan_dir(&hooks_dir(dir.path()), &reader_failing_on("post-commit"));

    assert!(!scan.block_found);
    assert_eq!(scan.unreadable.len(), 1, "{:?}", scan.unreadable);
}

/// A failure names the hook, the step and the cause, and quotes the control
/// bytes that a repository-controlled path can hold.
#[test]
fn test_hook_failure_quotes_control_bytes_in_the_path() {
    let failure = HookFailure {
        path: Path::new("/r\u{1b}[31m\n/.git/hooks/post-commit").to_path_buf(),
        step: HookStep::Rewrite,
        kind: std::io::ErrorKind::PermissionDenied,
    };

    let text = failure.to_string();

    assert_eq!(
        text,
        "could not rewrite hook \"/r\\u{1b}[31m\\n/.git/hooks/post-commit\" (permission denied)"
    );
    assert!(
        !text.bytes().any(|b| b < 0x20 && b != 0x09),
        "no raw control byte: {text:?}"
    );
}

// ============================================================================
// #413 / AD-413-15 / AC31 — hooks route to the SHARED commondir directory
// ============================================================================

/// AC31(b)/(c) — in a linked worktree, `resolve_hooks_dir` returns the SHARED
/// `<commondir>/hooks` directory (git's own answer), and remove and find agree on
/// it. Nothing is written under the per-worktree gitdir.
///
/// Ground truth is taken from git itself (`git -C <wt> rev-parse --git-path hooks`),
/// not assumed, so a routing change that stops matching git fails this test.
#[test]
fn test_hooks_route_to_shared_commondir_in_linked_worktree() {
    let dir = tempdir().unwrap();
    let primary = dir.path().join("primary");
    let worktree = dir.path().join("wt1");
    fs::create_dir_all(&primary).unwrap();

    create_real_git_repo(&primary, &[("init", &[("a.rs", "fn a(){}\n")])]);
    create_real_git_worktree(&primary, &worktree, "b1");
    let gt_hooks = git_hooks_dir(&worktree);

    let resolved = super::resolve_hooks_dir(&worktree)
        .expect("linked worktree has a .git FILE — resolve_hooks_dir must return Ok");
    // `.git` is a FILE in a linked worktree — a hand-built `<root>/.git/hooks` is not a directory.
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

    // Old hooks live in the SHARED directory.
    plant_fresh_hooks(&gt_hooks);

    // AC34(a): the block is visible from a DIFFERENT worktree of the same clone.
    assert!(
        find_search_hooks(&primary).is_some(),
        "AC34(a): a block in the shared hooks dir must be visible from the primary"
    );

    // Remove reports truthfully and works from any worktree (AC31(a)/AC34(c)).
    assert!(
        remove_search_hooks(&worktree).unwrap().changed,
        "AC31(a): remove must report changed=true when a block was actually removed"
    );
    assert!(
        !remove_search_hooks(&primary).unwrap().changed,
        "AC31(a): a second remove must report changed=false so no false success line is printed"
    );
    for name in HOOK_NAMES {
        assert_eq!(
            fs::read_to_string(gt_hooks.join(name)).unwrap(),
            "#!/bin/sh\n",
            "{name}: the shared hook must lose its block"
        );
    }
    assert_eq!(find_search_hooks(&worktree), None);

    // AC31(c): nothing was created per-worktree.
    // AD-413-12: delegate to resolve_git_dir — the single `gitdir:` parser.
    let wt_gitdir = super::super::gitdir::resolve_git_dir(&worktree).unwrap();
    assert!(
        !wt_gitdir.join("hooks").exists(),
        "AC31(c): no per-worktree hooks directory may be created ({wt_gitdir:?})"
    );
}

/// Security (AD-413-3 extension): a crafted `gitdir:` pointer that resolves to a
/// directory which is NOT a real git directory (`HEAD` present but `objects/` and
/// `refs/` absent) must NOT be used as a write destination.  `resolve_hooks_dir`
/// must fall back to the safe local `<root>/.git/hooks` instead.
///
/// The attacker scenario:
/// - A tarball is extracted and contains a `.git` FILE (git itself won't check
///   one out, but archive tools will) pointing at an arbitrary target directory.
/// - skim must not rewrite files in that arbitrary directory.
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

/// The remover never rewrites a file below a `gitdir:` pointer that fails the
/// write-path gate, even when that file holds the old block.
///
/// Discriminating: without the gate the remover would resolve to
/// `<target>/hooks` and strip the block from the file planted there.
#[test]
fn test_remove_never_rewrites_through_a_gitdir_that_fails_the_gate() {
    let dir = tempdir().unwrap();
    let project = dir.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let target = dir.path().join("attacker_target");
    fs::create_dir_all(target.join("hooks")).unwrap();
    fs::write(target.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let victim = target.join("hooks").join("post-commit");
    let text = with_old_block("#!/bin/sh\necho not-ours");
    fs::write(&victim, &text).unwrap();
    fs::write(
        project.join(".git"),
        format!("gitdir: {}\n", target.display()),
    )
    .unwrap();

    let outcome = remove_search_hooks(&project).unwrap();

    assert!(!outcome.changed);
    assert_eq!(
        fs::read_to_string(&victim).unwrap(),
        text,
        "a file below a gitdir that fails the gate must stay byte for byte"
    );
    assert_eq!(find_search_hooks(&project), None);
}

// ============================================================================
// #413 / AC34(b) — SHARED_HOOKS_SCOPE_MSG constant content guard
// ============================================================================

/// AC34(b) constant guard — the disclosure message printed by
/// `remove_search_hooks` when the resolved hooks directory is the shared
/// `<commondir>/hooks` must contain both key phrases.
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
// #413 / AC34(b)/(c) — multi-worktree blast radius is real, bounded, and the
// disclosure predicate fires correctly
// ============================================================================

/// AC34(b)/(c) — S37 scenario in unit-test form.
///
/// Fixture: primary + wt1 + wt2, with a foreign `post-commit` body (carrying the
/// old block) and a `pre-push` file planted in `<commondir>/hooks`.
///
/// **(b)** `is_shared_hooks_dir` returns `true` for both linked-worktree roots,
/// proving the predicate that gates `eprintln!({SHARED_HOOKS_SCOPE_MSG})` in
/// `remove_search_hooks`.
///
/// **(c)** After a remove from a different worktree the foreign `post-commit` body
/// is byte-identical to its original form and `pre-push` is untouched.
#[test]
fn test_ac34_multi_worktree_blast_radius() {
    let dir = tempdir().unwrap();
    let primary = dir.path().join("primary");
    let wt1 = dir.path().join("wt1");
    let wt2 = dir.path().join("wt2");
    fs::create_dir_all(&primary).unwrap();

    create_real_git_repo(&primary, &[("init", &[("a.rs", "fn a(){}\n")])]);
    create_real_git_worktree(&primary, &wt1, "b1");
    create_real_git_worktree(&primary, &wt2, "b2");
    let gt_hooks = git_hooks_dir(&wt1);

    // Precondition: both wt1 and wt2 have a .git FILE (linked worktree).
    assert!(wt1.join(".git").is_file(), "wt1/.git must be a file");
    assert!(wt2.join(".git").is_file(), "wt2/.git must be a file");

    // Plant a foreign post-commit body that carries the old block, and a foreign
    // pre-push file that skim must never touch.
    let foreign_pc_body = "#!/bin/sh\necho FOREIGN_POST_COMMIT\n";
    let pc_path = gt_hooks.join("post-commit");
    fs::create_dir_all(&gt_hooks).unwrap();
    fs::write(&pc_path, with_old_block(foreign_pc_body)).unwrap();
    let foreign_pp_body = "#!/bin/sh\necho FOREIGN_PRE_PUSH\n";
    let pp_path = gt_hooks.join("pre-push");
    fs::write(&pp_path, foreign_pp_body).unwrap();

    // AC34(b): is_shared_hooks_dir fires for every linked-worktree root.
    assert!(
        super::is_shared_hooks_dir(&wt1),
        "AC34(b): is_shared_hooks_dir must be true for wt1 (linked worktree)"
    );
    assert!(
        super::is_shared_hooks_dir(&wt2),
        "AC34(b): is_shared_hooks_dir must be true for wt2 (linked worktree)"
    );

    // Remove from wt2 — a DIFFERENT worktree from the one that is checked out first.
    let outcome = remove_search_hooks(&wt2).unwrap();
    assert!(outcome.changed, "the block was present in the shared hooks");

    assert_eq!(
        fs::read_to_string(&pc_path).unwrap(),
        foreign_pc_body,
        "AC34(c): foreign post-commit body must be byte-identical after remove"
    );
    assert_eq!(
        fs::read_to_string(&pp_path).unwrap(),
        foreign_pp_body,
        "AC34(c): pre-push file must be byte-identical after remove"
    );
}

/// AC31(e) / AC5b monotonicity — a plain repo and a non-repo directory keep the
/// `<root>/.git/hooks` path, byte for byte.
#[test]
fn test_resolve_hooks_dir_unchanged_for_plain_repo_and_non_repo() {
    let dir = tempdir().unwrap();

    // Plain repo: `.git` is a directory with no `commondir`.
    let plain = dir.path().join("plain");
    fs::create_dir_all(&plain).unwrap();
    create_real_git_repo(&plain, &[("init", &[("a.rs", "fn a(){}\n")])]);
    assert!(
        !plain.join(".git").join("commondir").exists(),
        "precondition: a plain repo has no commondir"
    );
    assert_eq!(
        super::resolve_hooks_dir(&plain).unwrap(),
        plain.join(".git").join("hooks"),
        "AC31(e): a plain repo must resolve to <root>/.git/hooks exactly as before"
    );

    // Non-repo directory (the shape the simple cases above use). This directory
    // has no .git and no enclosing git repository (it is inside a system temp
    // dir), so resolve_hooks_dir still returns Ok(<root>/.git/hooks).
    let non_repo = dir.path().join("non_repo");
    fs::create_dir_all(&non_repo).unwrap();
    assert_eq!(
        super::resolve_hooks_dir(&non_repo).unwrap(),
        non_repo.join(".git").join("hooks"),
        "AC31(e): a non-repo root must resolve to <root>/.git/hooks exactly as before"
    );
}

// ============================================================================
// Subdirectory-root refusal
// ============================================================================

/// A subdirectory of a git repo holds no hooks of its own: removal refuses, names
/// the enclosing repository, and creates no `.git` entry.
#[test]
fn test_remove_hooks_refuses_subdirectory_root() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    create_real_git_repo(&repo, &[("init", &[("a.rs", "fn a(){}\n")])]);

    // Create a subdirectory inside the repo — no .git of its own.
    let subdir = repo.join("src");
    fs::create_dir_all(&subdir).unwrap();

    assert!(
        super::resolve_hooks_dir(&subdir).is_err(),
        "a subdirectory of a git repo must yield Err from resolve_hooks_dir"
    );

    let err = remove_search_hooks(&subdir).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("subdirectory") || msg.contains("inside"),
        "error must describe the subdirectory constraint; got: {msg}"
    );
    assert!(
        !subdir.join(".git").exists(),
        "removal must not create a .git entry in a subdirectory root"
    );
    assert_eq!(
        find_search_hooks(&subdir),
        None,
        "find returns None rather than panicking on a subdirectory root"
    );
}

/// `resolve_hooks_dir` on a subdirectory must return `Err(ancestor)` where
/// `ancestor` is the canonical path of the enclosing repository, not merely
/// `Err(_)`; the removal error names that repository.
#[test]
fn test_resolve_hooks_dir_subdirectory_err_payload_is_ancestor_canonical() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    create_real_git_repo(&repo, &[("init", &[("a.rs", "fn a(){}\n")])]);
    let repo_canonical = repo.canonicalize().unwrap();

    let subdir = repo.join("lib");
    fs::create_dir_all(&subdir).unwrap();

    let result = super::resolve_hooks_dir(&subdir);
    assert_eq!(
        result,
        Err(repo_canonical.clone()),
        "resolve_hooks_dir must return Err(enclosing_repo_canonical) for a subdirectory"
    );

    let err = remove_search_hooks(&subdir).unwrap_err();
    let msg = err.to_string();
    let repo_display = repo_canonical.display().to_string();
    assert!(
        msg.contains(&repo_display),
        "remove error must contain the canonical repo path '{repo_display}'; got: {msg}"
    );
}

/// Submodule case — AC34(b) correctness:
///
/// A git submodule's `.git` is a file whose `gitdir:` pointer resolves to
/// `<superproject>/.git/modules/<name>`.  That gitdir is a self-contained private
/// ref store with no `commondir` file, so the hooks directory is NOT shared across
/// worktrees.
///
/// (a) `is_shared_hooks_dir` must be false — the "shared by every worktree of
///     this clone" disclosure must NOT fire for a submodule root.
///
/// (b) The block is removed from the submodule's private gitdir, not from the
///     superproject's hooks directory.
///
/// Uses a real `git submodule add` fixture — a fake `.git` file would not exercise
/// the relative-pointer code path in `resolve_git_dir`.
#[test]
fn test_remove_hooks_submodule_no_shared_disclosure() {
    use std::process::Command;

    let dir = tempdir().unwrap();
    let super_root = dir.path().join("superproject");
    let sub_origin = dir.path().join("sub_origin");

    // Build the sub-repository that will be added as a submodule.
    fs::create_dir_all(&sub_origin).unwrap();
    create_real_git_repo(&sub_origin, &[("init", &[("lib.rs", "fn f(){}\n")])]);

    // Build the superproject.
    fs::create_dir_all(&super_root).unwrap();
    create_real_git_repo(&super_root, &[("init", &[("README.md", "readme\n")])]);

    // `protocol.file.allow=always` is required when adding a local-path submodule
    // from within git 2.38+ (file protocol is restricted by default).
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
    assert!(
        out.status.success(),
        "git submodule add failed ({}): {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let sub_root = super_root.join("sub");
    assert!(
        sub_root.join(".git").is_file(),
        "submodule .git must be a file, not a directory (precondition)"
    );

    // The old block sits in the submodule's private hooks directory, and the
    // superproject has one of its own that removal must not touch.
    let sub_hooks = super::resolve_hooks_dir(&sub_root).unwrap();
    plant_fresh_hooks(&sub_hooks);
    let super_hooks = hooks_dir(&super_root);
    plant_fresh_hooks(&super_hooks);

    let outcome = remove_search_hooks(&sub_root)
        .expect("remove_search_hooks must succeed on a submodule root");

    assert!(outcome.changed, "the submodule hooks held the block");
    assert!(
        !super::is_shared_hooks_dir(&sub_root),
        "submodule hooks dir is private (no commondir); \
         the 'shared by every worktree' notice must not fire"
    );
    assert_eq!(outcome.dir, sub_hooks.canonicalize().unwrap());
    assert!(
        !outcome.dir.to_string_lossy().contains(".."),
        "the reported directory must be normalized; got {:?}",
        outcome.dir
    );
    for name in HOOK_NAMES {
        assert_eq!(
            fs::read_to_string(sub_hooks.join(name)).unwrap(),
            "#!/bin/sh\n",
            "{name}: the submodule hook must lose its block"
        );
        assert_eq!(
            fs::read_to_string(super_hooks.join(name)).unwrap(),
            with_old_block("#!/bin/sh"),
            "{name}: the superproject hook must stay as it is"
        );
    }
}

// ============================================================================
// Control characters in a repository path are not forwarded raw
// ============================================================================

/// The refusal for a subdirectory names the folder and the enclosing repository.
/// When the repository folder holds an ESC byte, the error must carry it quoted:
/// a raw ESC would reach the terminal of the user.
///
/// The test builds the one case that makes `remove_search_hooks` return this
/// error: a folder with no `.git` entry inside a repository whose folder name
/// holds ESC and a CSI sequence.
#[test]
fn test_hooks_refusal_quotes_control_chars_in_the_repository_path() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo\x1b[31mred");
    fs::create_dir_all(&repo).unwrap();
    create_real_git_repo(&repo, &[("init", &[("a.rs", "fn a(){}\n")])]);
    let subdir = repo.join("src");
    fs::create_dir_all(&subdir).unwrap();

    let message = remove_search_hooks(&subdir).unwrap_err().to_string();

    assert!(
        message.contains("is a subdirectory of the git repository"),
        "the removal path must return the subdirectory refusal; got: {message:?}"
    );
    assert!(
        !message.bytes().any(|b| b < 0x20 && b != 0x09),
        "the error must not contain a raw control byte; got: {message:?}"
    );
    assert!(
        message.contains("repo\\u{1b}[31mred"),
        "the ESC byte must appear quoted as \\u{{1b}}; got: {message:?}"
    );
}
