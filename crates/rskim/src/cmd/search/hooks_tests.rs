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
