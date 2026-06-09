//! Tests for the staleness detection module (staleness.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

use tempfile::tempdir;

use super::{
    StalenessCheck, auto_refresh_if_stale, check_staleness, read_git_head, resolve_git_dir,
};

// Minimal analytics config for tests — analytics recording is disabled.
const TEST_ANALYTICS: crate::analytics::AnalyticsConfig = crate::analytics::AnalyticsConfig {
    enabled: false,
    input_cost_per_mtok: None,
    session_id: None,
};

// ============================================================================
// Helpers
// ============================================================================

/// Create a minimal git repo structure in `dir` with the given HEAD content.
fn create_fake_git_repo(dir: &std::path::Path, head_content: &str) {
    let git_dir = dir.join(".git");
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(git_dir.join("HEAD"), head_content).unwrap();
}

/// Write a packed-refs file for the git repo in `dir`.
fn write_packed_refs(dir: &std::path::Path, content: &str) {
    let git_dir = dir.join(".git");
    fs::write(git_dir.join("packed-refs"), content).unwrap();
}

/// Create a ref file with SHA under `.git/refs/`.
fn create_ref_file(dir: &std::path::Path, ref_path: &str, sha: &str) {
    let git_dir = dir.join(".git");
    let full_path = git_dir.join(ref_path);
    fs::create_dir_all(full_path.parent().unwrap()).unwrap();
    fs::write(&full_path, format!("{sha}\n")).unwrap();
}

/// Write a minimal valid AST index stub file in `cache_dir`.
///
/// `index_version` reads the first 6 bytes: magic `SKAX` + version u16 LE.
/// Writing version 2 (the current format) prevents the AST self-heal from
/// reporting `NoStoredHead` in unit tests that only stub the lexical index.
fn write_ast_index_stub(cache_dir: &std::path::Path) {
    // b"SKAX" = magic (4 bytes), 0x02 0x00 = version 2 in little-endian.
    fs::write(cache_dir.join("ast_index.skidx"), b"SKAX\x02\x00").unwrap();
}

/// Write a manifest with the given git_head into `cache_dir`.
fn write_manifest_with_head(
    root: &std::path::Path,
    cache_dir: &std::path::Path,
    git_head: Option<&str>,
) {
    use crate::cmd::search::manifest::FileManifest;

    let mut manifest = FileManifest::new(root.to_path_buf(), cache_dir.to_path_buf());
    manifest.set_git_head(git_head.map(str::to_string));
    manifest.save().unwrap();
}

// ============================================================================
// resolve_git_dir
// ============================================================================

#[test]
fn test_resolve_git_dir_returns_git_dir_when_directory() {
    let dir = tempdir().unwrap();
    let git_path = dir.path().join(".git");
    fs::create_dir_all(&git_path).unwrap();

    let result = resolve_git_dir(dir.path());
    assert!(
        result.is_some(),
        "should resolve git dir when .git is a directory"
    );
    assert_eq!(result.unwrap(), git_path);
}

#[test]
fn test_resolve_git_dir_returns_none_when_no_git() {
    let dir = tempdir().unwrap();
    // No .git at all
    assert!(
        resolve_git_dir(dir.path()).is_none(),
        "should return None when no .git present"
    );
}

#[test]
fn test_resolve_git_dir_follows_gitdir_file_for_worktree() {
    let dir = tempdir().unwrap();
    let worktree_dir = dir.path().join("worktree");
    fs::create_dir_all(&worktree_dir).unwrap();

    // Create the actual git dir that the .git file points to
    let actual_git_dir = dir.path().join("actual_git");
    fs::create_dir_all(&actual_git_dir).unwrap();

    // Write .git file (worktree style)
    let git_file_path = worktree_dir.join(".git");
    fs::write(
        &git_file_path,
        format!("gitdir: {}\n", actual_git_dir.display()),
    )
    .unwrap();

    let result = resolve_git_dir(&worktree_dir);
    assert!(result.is_some(), "should follow gitdir: pointer");
    assert_eq!(result.unwrap(), actual_git_dir);
}

// ============================================================================
// read_git_head
// ============================================================================

#[test]
fn test_read_git_head_returns_none_when_no_git() {
    let dir = tempdir().unwrap();
    assert!(
        read_git_head(dir.path()).is_none(),
        "should return None when no .git directory"
    );
}

#[test]
fn test_read_git_head_detached_head_raw_sha() {
    let dir = tempdir().unwrap();
    let sha = "abcdef1234567890abcdef1234567890abcdef12";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    let result = read_git_head(dir.path());
    assert_eq!(result.as_deref(), Some(sha));
}

#[test]
fn test_read_git_head_follows_symbolic_ref_to_loose_ref() {
    let dir = tempdir().unwrap();
    let sha = "deadbeef12345678deadbeef12345678deadbeef";
    create_fake_git_repo(dir.path(), "ref: refs/heads/main\n");
    create_ref_file(dir.path(), "refs/heads/main", sha);

    let result = read_git_head(dir.path());
    assert_eq!(result.as_deref(), Some(sha));
}

#[test]
fn test_read_git_head_falls_back_to_packed_refs() {
    let dir = tempdir().unwrap();
    let sha = "cafebabe12345678cafebabe12345678cafebabe";
    create_fake_git_repo(dir.path(), "ref: refs/heads/feature\n");
    // No loose ref file — only packed-refs
    write_packed_refs(
        dir.path(),
        &format!("# pack-refs with: peeled fully-peeled sorted\n{sha} refs/heads/feature\n"),
    );

    let result = read_git_head(dir.path());
    assert_eq!(result.as_deref(), Some(sha));
}

#[test]
fn test_read_git_head_loose_ref_takes_priority_over_packed() {
    let dir = tempdir().unwrap();
    let loose_sha = "1111111111111111111111111111111111111111";
    let packed_sha = "2222222222222222222222222222222222222222";
    create_fake_git_repo(dir.path(), "ref: refs/heads/main\n");
    create_ref_file(dir.path(), "refs/heads/main", loose_sha);
    write_packed_refs(dir.path(), &format!("{packed_sha} refs/heads/main\n"));

    let result = read_git_head(dir.path());
    assert_eq!(
        result.as_deref(),
        Some(loose_sha),
        "loose ref should take priority over packed-refs"
    );
}

#[test]
fn test_read_git_head_rejects_path_traversal_ref() {
    let dir = tempdir().unwrap();
    // Crafted HEAD that tries to escape the git dir via path traversal.
    create_fake_git_repo(dir.path(), "ref: ../../etc/shadow\n");

    let result = read_git_head(dir.path());
    assert!(
        result.is_none(),
        "path traversal ref should be rejected, got {result:?}"
    );
}

#[test]
fn test_read_git_head_accepts_sha256_hash() {
    let dir = tempdir().unwrap();
    // 64-hex SHA-256 detached HEAD
    let sha256 = "a".repeat(64);
    create_fake_git_repo(dir.path(), &format!("{sha256}\n"));

    let result = read_git_head(dir.path());
    assert_eq!(
        result.as_deref(),
        Some(sha256.as_str()),
        "64-char SHA-256 should be accepted as a detached HEAD"
    );
}

// ============================================================================
// check_staleness
// ============================================================================

#[test]
fn test_check_staleness_no_index_returns_no_index() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_fake_git_repo(dir.path(), "ref: refs/heads/main\n");

    let (result, manifest) = check_staleness(&cache_dir, dir.path());
    assert!(
        matches!(result, StalenessCheck::NoIndex),
        "no index.skidx → NoIndex, got {result:?}"
    );
    assert!(manifest.is_none(), "NoIndex should return no manifest");
}

#[test]
fn test_check_staleness_no_stored_head() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let sha = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    // Write manifest without git_head and create fake index file
    write_manifest_with_head(dir.path(), &cache_dir, None);
    // Create a stub index file so NoIndex branch is not triggered
    fs::write(cache_dir.join("index.skidx"), b"stub").unwrap();

    // Git HEAD is present but manifest has no stored HEAD → NoStoredHead
    let (result, _) = check_staleness(&cache_dir, dir.path());
    assert!(
        matches!(result, StalenessCheck::NoStoredHead),
        "git HEAD present + manifest without git_head → NoStoredHead, got {result:?}"
    );
}

#[test]
fn test_check_staleness_current_when_heads_match() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let sha = "bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    write_manifest_with_head(dir.path(), &cache_dir, Some(sha));
    fs::write(cache_dir.join("index.skidx"), b"stub").unwrap();
    // AST stub required so self-heal does not trigger before HEAD comparison.
    write_ast_index_stub(&cache_dir);

    let (result, _) = check_staleness(&cache_dir, dir.path());
    assert!(
        matches!(result, StalenessCheck::Current),
        "matching HEADs → Current, got {result:?}"
    );
}

#[test]
fn test_check_staleness_head_changed() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let stored_sha = "cccc3333cccc3333cccc3333cccc3333cccc3333";
    let current_sha = "dddd4444dddd4444dddd4444dddd4444dddd4444";
    create_fake_git_repo(dir.path(), &format!("{current_sha}\n"));

    write_manifest_with_head(dir.path(), &cache_dir, Some(stored_sha));
    fs::write(cache_dir.join("index.skidx"), b"stub").unwrap();
    // AST stub required so self-heal does not trigger before HEAD comparison.
    write_ast_index_stub(&cache_dir);

    let (result, _) = check_staleness(&cache_dir, dir.path());
    match result {
        StalenessCheck::HeadChanged { stored, current } => {
            assert_eq!(stored, stored_sha);
            assert_eq!(current, current_sha);
        }
        other => panic!("expected HeadChanged, got {other:?}"),
    }
}

#[test]
fn test_check_staleness_non_git_project_is_current() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    // No .git directory — non-git project
    write_manifest_with_head(dir.path(), &cache_dir, None);
    fs::write(cache_dir.join("index.skidx"), b"stub").unwrap();
    // AST stub required so self-heal does not trigger before HEAD comparison.
    write_ast_index_stub(&cache_dir);

    // Non-git: stored HEAD = None, current HEAD = None → Current (no rebuild loop).
    let (result, _) = check_staleness(&cache_dir, dir.path());
    assert!(
        matches!(result, StalenessCheck::Current),
        "non-git project (no stored HEAD, no current HEAD) → Current, got {result:?}"
    );
}

#[test]
fn test_check_staleness_unreadable_git_is_current() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let stored_sha = "eeee5555eeee5555eeee5555eeee5555eeee5555";

    // Manifest records a HEAD (was a git repo at build time), but .git is absent now.
    write_manifest_with_head(dir.path(), &cache_dir, Some(stored_sha));
    fs::write(cache_dir.join("index.skidx"), b"stub").unwrap();
    // AST stub required so self-heal does not trigger before HEAD comparison.
    write_ast_index_stub(&cache_dir);
    // No .git directory — simulates git becoming unreadable.

    // stored HEAD = Some, current HEAD = None → Current (don't trigger rebuild).
    let (result, _) = check_staleness(&cache_dir, dir.path());
    assert!(
        matches!(result, StalenessCheck::Current),
        "stored HEAD present + git unreadable → Current, got {result:?}"
    );
}

#[test]
fn test_check_staleness_git_appeared_triggers_rebuild() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let current_sha = "ffff6666ffff6666ffff6666ffff6666ffff6666";

    // Manifest has no stored HEAD (was built as a non-git project), but now .git exists.
    write_manifest_with_head(dir.path(), &cache_dir, None);
    fs::write(cache_dir.join("index.skidx"), b"stub").unwrap();
    create_fake_git_repo(dir.path(), &format!("{current_sha}\n"));

    // stored HEAD = None, current HEAD = Some → NoStoredHead (rebuild to record HEAD).
    let (result, _) = check_staleness(&cache_dir, dir.path());
    assert!(
        matches!(result, StalenessCheck::NoStoredHead),
        "git appeared since last build → NoStoredHead, got {result:?}"
    );
}

// ============================================================================
// check_staleness — AST self-heal manifest passthrough (Issue 2 fix guard)
// ============================================================================

/// When the lexical index exists and the manifest has a real git HEAD, but the
/// AST index is absent, check_staleness must return NoStoredHead (to trigger
/// rebuild) AND return the loaded manifest — NOT None.
///
/// Previously check_staleness returned (NoStoredHead, None) in this case,
/// causing `--stats` to report "git HEAD: (none)" even though the HEAD was
/// recorded in the manifest. The HEAD was there; only the AST index was missing.
#[test]
fn test_check_staleness_ast_stale_still_returns_manifest() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let sha = "aabb1122aabb1122aabb1122aabb1122aabb1122";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    // Write a manifest with a real HEAD, plus a lexical index stub.
    write_manifest_with_head(dir.path(), &cache_dir, Some(sha));
    fs::write(cache_dir.join("index.skidx"), b"stub").unwrap();
    // Deliberately NO ast_index.skidx — simulates missing AST index.

    let (result, manifest) = check_staleness(&cache_dir, dir.path());

    // Outcome must be stale (rebuild triggered).
    assert!(
        !matches!(result, StalenessCheck::Current),
        "missing AST index must trigger stale outcome, got {result:?}"
    );
    assert!(
        matches!(result, StalenessCheck::NoStoredHead),
        "missing AST index should return NoStoredHead, got {result:?}"
    );

    // The manifest must be Some — the real HEAD must be accessible to display consumers.
    assert!(
        manifest.is_some(),
        "check_staleness must return the manifest even when AST is stale (Issue 2 fix)"
    );
    assert_eq!(
        manifest.unwrap().stored_git_head(),
        Some(sha),
        "--stats must show the real git HEAD even when only the AST index is missing"
    );
}

/// Same as above but with a below-FORMAT_VERSION AST stub instead of absent file.
#[test]
fn test_check_staleness_ast_below_version_still_returns_manifest() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let sha = "ccdd3344ccdd3344ccdd3344ccdd3344ccdd3344";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    write_manifest_with_head(dir.path(), &cache_dir, Some(sha));
    fs::write(cache_dir.join("index.skidx"), b"stub").unwrap();
    // Write a v1 AST stub (below current AST_INDEX_FORMAT_VERSION).
    let stub: [u8; 6] = [b'S', b'K', b'A', b'X', 1, 0];
    fs::write(cache_dir.join("ast_index.skidx"), stub).unwrap();

    let (result, manifest) = check_staleness(&cache_dir, dir.path());

    assert!(
        !matches!(result, StalenessCheck::Current),
        "below-version AST index must trigger stale outcome, got {result:?}"
    );

    assert!(
        manifest.is_some(),
        "check_staleness must return the manifest for below-version AST index"
    );
    assert_eq!(
        manifest.unwrap().stored_git_head(),
        Some(sha),
        "--stats must show real HEAD when only the AST format version is outdated"
    );
}

// ============================================================================
// auto_refresh_if_stale
// ============================================================================

/// Helper: build a real index in `cache_dir` for project at `root`.
///
/// The git HEAD recorded in the manifest is whatever `read_git_head` returns
/// at build time — create `.git` with the desired HEAD before calling this.
/// For non-git projects (no `.git`), the manifest stores `git_head: None`.
fn build_index_in(root: &std::path::Path, cache_dir: &std::path::Path) {
    use crate::cmd::search::index::build_index;
    use crate::cmd::search::types::IndexConfig;

    let config = IndexConfig {
        root: root.to_path_buf(),
        max_files: None,
        force: false,
        cache_dir_override: Some(cache_dir.to_path_buf()),
    };
    build_index(&config).unwrap();
}

#[test]
fn test_auto_refresh_returns_false_when_current() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    let sha = "1234567890abcdef1234567890abcdef12345678";

    // Set up git with the SHA, then build — manifest records this HEAD.
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));
    build_index_in(dir.path(), &cache_dir);

    let analytics = TEST_ANALYTICS;
    let (refreshed, _manifest) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();

    assert!(
        !refreshed,
        "index is current — should not trigger a rebuild"
    );
}

#[test]
fn test_auto_refresh_returns_manifest_when_current() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    let sha = "abcdef1234567890abcdef1234567890abcdef12";

    create_fake_git_repo(dir.path(), &format!("{sha}\n"));
    build_index_in(dir.path(), &cache_dir);

    let analytics = TEST_ANALYTICS;
    let (_refreshed, manifest) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();

    // The returned manifest should reflect the stored HEAD.
    assert_eq!(
        manifest.stored_git_head(),
        Some(sha),
        "returned manifest should have the correct stored HEAD"
    );
}

#[test]
fn test_auto_refresh_rebuilds_on_head_changed() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    let old_sha = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";
    let new_sha = "bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222";

    // Build index with old HEAD recorded.
    create_fake_git_repo(dir.path(), &format!("{old_sha}\n"));
    build_index_in(dir.path(), &cache_dir);

    // Advance HEAD to simulate a new commit.
    let git_dir = dir.path().join(".git");
    fs::write(git_dir.join("HEAD"), format!("{new_sha}\n")).unwrap();

    let analytics = TEST_ANALYTICS;
    let (refreshed, manifest) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();

    assert!(refreshed, "HEAD changed — index should be rebuilt");
    assert_eq!(
        manifest.stored_git_head(),
        Some(new_sha),
        "manifest after rebuild should record the new HEAD"
    );
}

#[test]
fn test_auto_refresh_rebuilds_on_no_stored_head() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    let sha = "cccc3333cccc3333cccc3333cccc3333cccc3333";

    // Build index as a non-git project — manifest stores git_head: None.
    build_index_in(dir.path(), &cache_dir);

    // Now add a .git to simulate git appearing after the last build.
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    let analytics = TEST_ANALYTICS;
    let (refreshed, manifest) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();

    assert!(
        refreshed,
        "no stored HEAD + git present — index should be rebuilt"
    );
    assert_eq!(
        manifest.stored_git_head(),
        Some(sha),
        "manifest after rebuild should record the current HEAD"
    );
}

#[test]
fn test_auto_refresh_non_git_project_no_rebuild_loop() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    // Non-git project: no .git directory.
    build_index_in(dir.path(), &cache_dir);

    let analytics = TEST_ANALYTICS;
    let (first_refreshed, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    let (second_refreshed, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();

    assert!(
        !first_refreshed,
        "non-git project should not rebuild on first query"
    );
    assert!(
        !second_refreshed,
        "non-git project should not rebuild on second query (no infinite loop)"
    );
}

// ============================================================================
// Display impl for StalenessCheck
// ============================================================================

#[test]
fn test_display_current() {
    assert_eq!(StalenessCheck::Current.to_string(), "current");
}

#[test]
fn test_display_no_stored_head() {
    assert_eq!(
        StalenessCheck::NoStoredHead.to_string(),
        "stale (no HEAD recorded)"
    );
}

#[test]
fn test_display_no_index() {
    assert_eq!(StalenessCheck::NoIndex.to_string(), "no index");
}

#[test]
fn test_display_head_changed_full_sha() {
    // Full 40-char SHAs — both are truncated to 8 chars in the output.
    let stored = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111".to_string();
    let current = "bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222".to_string();
    let s = StalenessCheck::HeadChanged { stored, current }.to_string();
    assert_eq!(s, "stale (HEAD changed: aaaa1111…→bbbb2222…)");
}

#[test]
fn test_display_head_changed_short_stored_sha() {
    // Stored SHA shorter than 8 bytes — .get(..8) returns None, falls back to
    // the full string. This guards against panicking on short/corrupt content.
    let stored = "abc".to_string();
    let current = "bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222".to_string();
    let s = StalenessCheck::HeadChanged { stored, current }.to_string();
    // stored is printed in full ("abc"), current is truncated to 8 chars.
    assert_eq!(s, "stale (HEAD changed: abc…→bbbb2222…)");
}

#[test]
fn test_display_head_changed_short_current_sha() {
    // Current SHA shorter than 8 bytes.
    let stored = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111".to_string();
    let current = "xy".to_string();
    let s = StalenessCheck::HeadChanged { stored, current }.to_string();
    assert_eq!(s, "stale (HEAD changed: aaaa1111…→xy…)");
}

#[test]
fn test_display_head_changed_exactly_8_chars() {
    // Exactly 8 characters — .get(..8) succeeds and returns the full string.
    let stored = "12345678".to_string();
    let current = "abcdef01".to_string();
    let s = StalenessCheck::HeadChanged { stored, current }.to_string();
    assert_eq!(s, "stale (HEAD changed: 12345678…→abcdef01…)");
}
