//! Tests for the staleness detection module (staleness.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

use tempfile::tempdir;

use super::{
    StalenessCheck, auto_refresh_if_stale, check_staleness, read_git_head, resolve_git_dir,
    temporal_db_is_stale,
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

/// Write a minimal valid lexical index stub file in `cache_dir`.
///
/// `lexical_index_version` reads the first 6 bytes: magic `SKIX` + version u16 LE.
/// Writing the current FORMAT_VERSION prevents the lexical self-heal from
/// reporting `NoStoredHead` in unit tests that only want to exercise the
/// HEAD-comparison or AST-self-heal logic paths
/// (Finding 9, ADR-006, #355 cycle-2, #358 Item 2).
///
/// The version bytes are derived from `rskim_search::LEXICAL_INDEX_FORMAT_VERSION`
/// so this stub automatically tracks future FORMAT_VERSION bumps without requiring
/// a manual edit (Finding 8 / #358 cycle-3: hardcoded literal bytes are a
/// maintenance trap that silently exercises the self-heal path on the next bump).
fn write_lexical_index_stub(cache_dir: &std::path::Path) {
    let version_bytes = rskim_search::LEXICAL_INDEX_FORMAT_VERSION.to_le_bytes();
    let mut stub = Vec::with_capacity(6);
    stub.extend_from_slice(b"SKIX");
    stub.extend_from_slice(&version_bytes);
    fs::write(cache_dir.join("index.skidx"), &stub).unwrap();
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

    // Write manifest without git_head and create a valid-format index stub.
    write_manifest_with_head(dir.path(), &cache_dir, None);
    // Valid lexical stub so the lexical self-heal does not short-circuit.
    write_lexical_index_stub(&cache_dir);

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
    // Valid lexical stub (v4 magic) so lexical self-heal does not short-circuit.
    write_lexical_index_stub(&cache_dir);
    // AST stub required so AST self-heal does not trigger before HEAD comparison.
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
    // Valid lexical stub (v4 magic) so lexical self-heal does not short-circuit.
    write_lexical_index_stub(&cache_dir);
    // AST stub required so AST self-heal does not trigger before HEAD comparison.
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
    // Valid lexical stub (v4 magic) so lexical self-heal does not short-circuit.
    write_lexical_index_stub(&cache_dir);
    // AST stub required so AST self-heal does not trigger before HEAD comparison.
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
    // Valid lexical stub (v4 magic) so lexical self-heal does not short-circuit.
    write_lexical_index_stub(&cache_dir);
    // AST stub required so AST self-heal does not trigger before HEAD comparison.
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
    // Valid lexical stub (v4 magic) + valid AST stub so both self-heal checks pass,
    // allowing the HEAD-comparison logic (None, Some) → NoStoredHead to fire.
    write_lexical_index_stub(&cache_dir);
    write_ast_index_stub(&cache_dir);
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

    // Write a manifest with a real HEAD plus a valid lexical stub.
    // A valid lexical stub is required so the lexical self-heal does NOT trigger;
    // only the absent AST index should cause NoStoredHead here (AST self-heal).
    write_manifest_with_head(dir.path(), &cache_dir, Some(sha));
    write_lexical_index_stub(&cache_dir);
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

    // Valid lexical stub so the lexical self-heal does not short-circuit the AST check.
    write_manifest_with_head(dir.path(), &cache_dir, Some(sha));
    write_lexical_index_stub(&cache_dir);
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
// check_staleness — lexical self-heal (#355 Finding 9 / ADR-006)
// ============================================================================

/// When the lexical index has a below-FORMAT_VERSION magic (v2 = bigram),
/// check_staleness must return NoStoredHead to trigger a full rebuild AND must
/// still return the loaded manifest (so --stats shows the real git HEAD).
///
/// PF-007 discriminating: if the lexical version check were absent, a v2 lexical
/// index with a matching HEAD would return Current instead of NoStoredHead, and the
/// next query would get a hard error from NgramIndexReader::open.  This test fails
/// the moment that check is removed.
#[test]
fn test_check_staleness_lexical_below_version_triggers_rebuild_returns_manifest() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let sha = "eeff5566eeff5566eeff5566eeff5566eeff5566";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    write_manifest_with_head(dir.path(), &cache_dir, Some(sha));
    // Write a v2 lexical stub (bigram-era format, below current v4).
    // magic = b"SKIX", version = 2 (LE u16).
    fs::write(cache_dir.join("index.skidx"), b"SKIX\x02\x00").unwrap();
    // Valid AST stub so AST self-heal does not co-trigger.
    write_ast_index_stub(&cache_dir);

    let (result, manifest) = check_staleness(&cache_dir, dir.path());

    // Must report stale (lexical v2 < v4 → self-heal required).
    assert!(
        matches!(result, StalenessCheck::NoStoredHead),
        "v2 lexical index must trigger NoStoredHead rebuild; got {result:?}"
    );

    // Manifest must be returned so --stats can show the real git HEAD.
    assert!(
        manifest.is_some(),
        "check_staleness must return manifest even when lexical index is below version"
    );
    assert_eq!(
        manifest.unwrap().stored_git_head(),
        Some(sha),
        "--stats must show real HEAD when only the lexical format version is outdated"
    );
}

/// Finding 8 / ADR-006: a v3 lexical stub (pre-varint-compression, the specific
/// format version this ticket (#358 Item 2) upgrades from) must also trigger
/// `NoStoredHead` so the staleness check self-heals via full rebuild.
///
/// The generic `v < LEXICAL_INDEX_FORMAT_VERSION` guard (staleness.rs)
/// covers v3 (3 < 4) via the same code path as v2, so migration is functional;
/// this test adds a v3-specific end-to-end regression case so the #358-owned
/// v3→v4 boundary is directly guarded at the integration level (applies ADR-006
/// self-heal intent; avoids PF-007 by asserting the exact `NoStoredHead`
/// discriminating observable, not just exit-0).
///
/// PF-007 compliance: asserts `StalenessCheck::NoStoredHead` and that the
/// manifest is returned (mirroring the sibling v2 test's exact assertions).
#[test]
fn test_check_staleness_lexical_v3_below_version_triggers_rebuild_returns_manifest() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let sha = "aabb1122aabb1122aabb1122aabb1122aabb1122";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    write_manifest_with_head(dir.path(), &cache_dir, Some(sha));
    // Write a v3 lexical stub (pre-varint-compression format, below current v4).
    // magic = b"SKIX", version = 3 (LE u16).
    fs::write(cache_dir.join("index.skidx"), b"SKIX\x03\x00").unwrap();
    // Valid AST stub so AST self-heal does not co-trigger.
    write_ast_index_stub(&cache_dir);

    let (result, manifest) = check_staleness(&cache_dir, dir.path());

    // Must report stale (lexical v3 < v4 → self-heal required, same guard as v2).
    assert!(
        matches!(result, StalenessCheck::NoStoredHead),
        "v3 lexical index must trigger NoStoredHead rebuild; got {result:?}"
    );

    // Manifest must be returned so --stats can show the real git HEAD.
    assert!(
        manifest.is_some(),
        "check_staleness must return manifest even when lexical index is v3 (below v4)"
    );
    assert_eq!(
        manifest.unwrap().stored_git_head(),
        Some(sha),
        "--stats must show real HEAD when only the lexical format is at v3 (below v4)"
    );
}

// ============================================================================
// check_staleness — manifest binary self-heal (#380, AD-380-2 / AC-4)
// ============================================================================

/// Write a v3 JSONL `index.skfiles` (the immediate-predecessor format #373
/// produced) directly into `cache_dir`, bypassing the binary writer. Starts with
/// `{`, never the SKFM magic, so `version_matches` reports a mismatch.
fn write_v3_jsonl_manifest(
    root: &std::path::Path,
    cache_dir: &std::path::Path,
    git_head: Option<&str>,
) {
    use std::io::Write as _;
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let path = cache_dir.join("index.skfiles");
    let mut f = fs::File::create(&path).unwrap();
    let header = serde_json::json!({
        "version": 3,
        "root": canonical.to_string_lossy(),
        "git_head": git_head,
    });
    writeln!(f, "{header}").unwrap();
    let entry = serde_json::json!({
        "path": "src/lib.rs",
        "sha256": "a".repeat(64),
        "lang": "rust",
        "field_map": [[0, 10, 0]],
        "mtime": 1_700_000_000u64,
        "size": 42u64,
    });
    writeln!(f, "{entry}").unwrap();
}

/// AC-4 (#380), GIT root: a v3 JSONL `index.skfiles` with otherwise-current
/// lexical + AST stubs and a matching git HEAD MUST trigger a full rebuild
/// (`NoStoredHead`) — the binary 3→4 bump is detected via `version_matches`
/// even though the git HEAD is unchanged.
///
/// PF-007 discriminating: without the `manifest_stale` gate, a v3 JSONL manifest
/// with a matching HEAD would (after the binary loader cold-starts it) reach the
/// HEAD compare and could mis-report; the version gate forces `NoStoredHead`.
#[test]
fn test_check_staleness_manifest_v3_jsonl_triggers_rebuild_git_root() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let sha = "ccdd3344ccdd3344ccdd3344ccdd3344ccdd3344";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    // Current lexical + AST stubs so ONLY the manifest version is stale.
    write_lexical_index_stub(&cache_dir);
    write_ast_index_stub(&cache_dir);
    // v3 JSONL manifest (no SKFM magic).
    write_v3_jsonl_manifest(dir.path(), &cache_dir, Some(sha));

    // version_matches must report the v3 JSONL manifest as below-current.
    assert!(
        !crate::cmd::search::manifest::FileManifest::version_matches(&cache_dir).unwrap(),
        "v3 JSONL manifest must NOT be accepted as current (AC-4 negative)"
    );

    let (result, _manifest) = check_staleness(&cache_dir, dir.path());
    assert!(
        matches!(result, StalenessCheck::NoStoredHead),
        "v3 JSONL manifest must trigger NoStoredHead rebuild on a git root; got {result:?}"
    );
}

/// AC-4 (#380), NON-GIT root: the manifest version self-heal MUST fire
/// independent of git HEAD state — a v3 JSONL manifest under a non-git root
/// (no `.git`) still triggers a rebuild. `check_staleness` must detect the
/// below-current FORMAT_VERSION before reaching any HEAD comparison.
#[test]
fn test_check_staleness_manifest_v3_jsonl_triggers_rebuild_non_git_root() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    // Deliberately NO .git — non-git root.

    write_lexical_index_stub(&cache_dir);
    write_ast_index_stub(&cache_dir);
    write_v3_jsonl_manifest(dir.path(), &cache_dir, None);

    assert!(
        !crate::cmd::search::manifest::FileManifest::version_matches(&cache_dir).unwrap(),
        "v3 JSONL manifest must NOT be accepted as current on a non-git root (AC-4)"
    );

    let (result, _manifest) = check_staleness(&cache_dir, dir.path());
    assert!(
        matches!(result, StalenessCheck::NoStoredHead),
        "v3 JSONL manifest must trigger NoStoredHead rebuild independent of git HEAD \
         (non-git root); got {result:?}"
    );
}

/// AC-4 (#380): a CURRENT binary (v4) manifest with current lexical + AST stubs
/// and a matching HEAD must NOT be flagged stale by the manifest gate — the
/// self-heal must be specific to below-current versions (no false rebuild loop).
#[test]
fn test_check_staleness_binary_v4_manifest_is_current() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let sha = "11aa22bb11aa22bb11aa22bb11aa22bb11aa22bb";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    write_lexical_index_stub(&cache_dir);
    write_ast_index_stub(&cache_dir);
    // Current binary manifest via the real writer.
    write_manifest_with_head(dir.path(), &cache_dir, Some(sha));

    assert!(
        crate::cmd::search::manifest::FileManifest::version_matches(&cache_dir).unwrap(),
        "current binary (v4) manifest must be accepted as current (AC-4)"
    );

    let (result, _manifest) = check_staleness(&cache_dir, dir.path());
    // The manifest written by `write_manifest_with_head` is empty (no entries),
    // and the project root has only `.git` (ignored), so the working-tree scan is
    // clean → the verdict is `Current`. Crucially it is NOT `NoStoredHead`: the
    // manifest-version gate must not false-trigger on a current v4 manifest.
    assert!(
        !matches!(result, StalenessCheck::NoStoredHead),
        "current v4 manifest must not trigger the version self-heal; got {result:?}"
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

/// AC7 / AC14 — Temporal hook integration: temporal rebuild called from
/// auto_refresh_if_stale does NOT cause lexical search to fail.
///
/// This is the discriminating integration test for the hook wiring in
/// staleness.rs. It exercises the SAME code path that AC7 protects — the
/// call `rebuild_temporal(root, cache_dir, head, now)` inside
/// `auto_refresh_if_stale` — and verifies that:
/// 1. auto_refresh_if_stale returns Ok even when the temporal rebuild
///    degrades gracefully (non-git root: temporal.db not written, no panic).
/// 2. The returned manifest is valid (the lexical refresh succeeded).
///
/// A fake git repo is not needed here — the non-git path exercises the
/// graceful-degradation arm of rebuild_temporal, which is the live failure
/// mode the AC7 hook path must handle.
#[test]
fn test_auto_refresh_hook_temporal_failure_does_not_fail_lexical() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // Build an initial non-git index so the next call is a "NoIndex" rebuild.
    // (NoIndex triggers build_index, which then calls rebuild_temporal.)
    // We don't call build_index_in first — NoIndex triggers the rebuild arm.

    let analytics = TEST_ANALYTICS;
    // auto_refresh_if_stale on a fresh non-git dir: NoIndex → build_index → rebuild_temporal.
    // rebuild_temporal will fail gracefully (no git) and must NOT propagate the error.
    let result = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics);

    assert!(
        result.is_ok(),
        "auto_refresh_if_stale must succeed even when rebuild_temporal \
         degrades on non-git root (AC7 / AC14 hook integration)"
    );

    // The returned manifest must be valid (lexical index was built).
    let (_refreshed, manifest) = result.unwrap();
    // Non-git project: stored_git_head is None (no git repo).
    assert_eq!(
        manifest.stored_git_head(),
        None,
        "non-git project manifest must have no stored HEAD"
    );

    // temporal.db must NOT be created (rebuild_temporal returned Ok early on non-git root).
    let temporal_db_path = cache_dir.join("temporal.db");
    assert!(
        !temporal_db_path.exists(),
        "temporal.db must not be created when rebuild_temporal degrades on non-git root (AC14)"
    );
}

// ============================================================================
// Hook integration: auto_refresh_if_stale on a real git repo populates
// temporal.db (ticket #289 core contract: temporal.db was never written
// outside tests before this feature).
// ============================================================================

/// Shared helper: create a real git repo with commits.
///
/// Delegates to the canonical `staleness::create_real_git_repo` helper so
/// staleness_tests.rs, temporal_build_tests.rs, and mod.rs tests all share one
/// implementation (avoids three-copy drift, #357 cycle-2 findings 9/14).
/// Named identically to the counterpart in temporal_build_tests.rs and mod.rs
/// so a reader scanning the three test files sees the same shared helper (#357
/// cycle-2 finding 3).
fn create_real_git_repo(dir: &std::path::Path, commit_files: &[(&str, &[(&str, &str)])]) -> String {
    super::create_real_git_repo(dir, commit_files)
}

/// AC (hook wiring): auto_refresh_if_stale on a real git repo MUST populate
/// temporal.db — this is the ticket's core contract (#289: temporal.db was
/// never written outside direct rebuild_temporal calls before this feature).
///
/// Discriminating: temporal.db EXISTS after auto_refresh_if_stale on a real
/// git repo; META_GIT_HEAD stored in temporal.db equals the repo HEAD; and
/// top_hotspots returns a non-empty list (data was indexed).
///
/// If rebuild_temporal were removed from the hook, every test in
/// temporal_build_tests.rs would still pass because they call rebuild_temporal
/// directly. This test is the ONLY one that drives the hook wiring end-to-end.
#[test]
fn test_auto_refresh_hook_populates_temporal_db_on_real_git_repo() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // Create a real git repo with a few commits so temporal data is non-trivial.
    let head = create_real_git_repo(
        dir.path(),
        &[
            ("feat: add auth", &[("src/auth.rs", "fn authenticate() {}")]),
            ("feat: add parser", &[("src/parser.rs", "fn parse() {}")]),
            (
                "fix: fix auth bug",
                &[("src/auth.rs", "fn authenticate() { // fixed }")],
            ),
        ],
    );
    assert_eq!(head.len(), 40, "HEAD must be a 40-char SHA");

    let analytics = TEST_ANALYTICS;

    // This is the call under test: auto_refresh_if_stale must build the index
    // (NoIndex → build_index) AND populate temporal.db (via rebuild_temporal hook).
    let result = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics);
    assert!(
        result.is_ok(),
        "auto_refresh_if_stale must succeed on a real git repo"
    );

    let (refreshed, manifest) = result.unwrap();
    assert!(
        refreshed,
        "index must have been built (NoIndex → refreshed=true)"
    );
    assert_eq!(
        manifest.stored_git_head(),
        Some(head.as_str()),
        "manifest must record the current HEAD"
    );

    // The critical contract: temporal.db MUST exist after the hook runs.
    let temporal_db_path = cache_dir.join("temporal.db");
    assert!(
        temporal_db_path.exists(),
        "temporal.db must be created by the auto_refresh_if_stale hook on a real git repo \
         (ticket #289 core contract: temporal.db was never written before this feature)"
    );

    // And it must contain valid data.
    let db = rskim_search::TemporalDb::open(&temporal_db_path).unwrap();
    let stored_head = db
        .get_meta(rskim_search::META_GIT_HEAD)
        .unwrap()
        .expect("META_GIT_HEAD must be set in temporal.db after hook runs");
    assert_eq!(
        stored_head, head,
        "META_GIT_HEAD in temporal.db must match the repo HEAD"
    );

    let hotspots = db.top_hotspots(20).unwrap();
    assert!(
        !hotspots.is_empty(),
        "temporal.db must contain hotspot data after rebuild (data was indexed, not empty)"
    );
}

/// AC14: Lexical query results must be unchanged when temporal hook succeeds.
///
/// Verifies the "temporal success must not alter lexical output" contract on
/// the success arm (not just the failure arm tested by
/// test_auto_refresh_hook_temporal_failure_does_not_fail_lexical).
///
/// Strategy: build the index twice (same repo, same HEAD) — once before any
/// temporal data exists, and once after. The manifest must record the same HEAD
/// and the index must produce consistent results. Direct lexical output comparison
/// is infeasible in a unit test (requires running a full query), so this test
/// verifies the manifest invariant: the lexical manifest is identical regardless
/// of whether temporal.db is populated, confirming no cross-contamination.
#[test]
fn test_auto_refresh_temporal_success_does_not_affect_lexical_manifest() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    let head = create_real_git_repo(
        dir.path(),
        &[
            ("feat: first", &[("lib.rs", "pub fn foo() {}")]),
            ("feat: second", &[("main.rs", "fn main() {}")]),
        ],
    );

    let analytics = TEST_ANALYTICS;

    // First refresh: builds index + populates temporal.db.
    let (refreshed1, manifest1) =
        auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(refreshed1, "first refresh must build the index");

    // Second refresh: index is current — must not rebuild, manifest unchanged.
    let (refreshed2, manifest2) =
        auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(
        !refreshed2,
        "second refresh must not rebuild (index is current)"
    );

    // Manifests from both calls must record the same HEAD (lexical is stable).
    assert_eq!(
        manifest1.stored_git_head(),
        manifest2.stored_git_head(),
        "lexical manifest HEAD must be identical before and after temporal population (AC14)"
    );
    assert_eq!(
        manifest1.stored_git_head(),
        Some(head.as_str()),
        "manifest must record the current repo HEAD"
    );
}

// ============================================================================
// temporal_db_is_stale — unit tests (AD-TMP-2/3)
// ============================================================================

/// temporal_db_is_stale returns true when temporal.db is absent.
#[test]
fn test_temporal_db_is_stale_when_absent() {
    let dir = tempdir().unwrap();
    // No temporal.db in dir — must report stale.
    assert!(
        temporal_db_is_stale(dir.path(), "abc1234"),
        "absent temporal.db must be reported stale"
    );
}

/// temporal_db_is_stale returns false when temporal.db exists and META_GIT_HEAD
/// matches current_head.
#[test]
fn test_temporal_db_is_not_stale_when_head_matches() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("temporal.db");
    let head = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";

    // Create a temporal.db with matching META_GIT_HEAD.
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    db.sync(&[], &[], &[], head).unwrap();
    drop(db);

    assert!(
        !temporal_db_is_stale(dir.path(), head),
        "temporal.db with matching META_GIT_HEAD must NOT be stale"
    );
}

/// temporal_db_is_stale returns true when temporal.db exists but META_GIT_HEAD
/// is different from current_head (HEAD-divergent / "deadbeef" case).
///
/// PF-007 discriminating: the value MUST transition from the planted stale SHA to
/// the real HEAD after auto_refresh_if_stale rebuilds temporal. This unit test
/// guards the predicate; the integration test below guards the self-heal.
#[test]
fn test_temporal_db_is_stale_when_head_diverges() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("temporal.db");
    let planted_head = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let real_head = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";

    // Create a temporal.db with a stale (planted) META_GIT_HEAD.
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    db.sync(&[], &[], &[], planted_head).unwrap();
    drop(db);

    assert!(
        temporal_db_is_stale(dir.path(), real_head),
        "temporal.db with diverged META_GIT_HEAD must be reported stale (deadbeef case)"
    );
}

// ============================================================================
// #357 BUG B — auto_refresh_if_stale self-heals stale temporal.db when
// lexical index is Current (AD-TMP-2)
// ============================================================================

/// BUG B discriminating (via auto_refresh_if_stale directly): when the lexical
/// index is Current and temporal.db is deleted, a second call to
/// auto_refresh_if_stale recreates temporal.db with the correct META_GIT_HEAD
/// and non-empty hotspots. Lexical was NOT rebuilt (refreshed==false).
///
/// PF-007: assert temporal.db recreation + exact HEAD match.
/// This test FAILS on the pre-fix code because the Current early-return skipped
/// the temporal staleness check entirely.
#[test]
fn test_bug_b_auto_refresh_self_heals_deleted_temporal_db() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // Create a real git repo with a few commits.
    let head = create_real_git_repo(
        dir.path(),
        &[
            ("feat: add auth", &[("src/auth.rs", "fn authenticate() {}")]),
            ("feat: add parser", &[("src/parser.rs", "fn parse() {}")]),
            (
                "fix: fix auth bug",
                &[("src/auth.rs", "fn authenticate() { // fixed }")],
            ),
        ],
    );
    assert_eq!(head.len(), 40, "HEAD must be a 40-char SHA");

    let analytics = TEST_ANALYTICS;

    // First call: builds lexical+AST+temporal.
    let (refreshed1, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(refreshed1, "first call must build the index");

    let temporal_db_path = cache_dir.join("temporal.db");
    assert!(
        temporal_db_path.exists(),
        "temporal.db must exist after first call (setup invariant)"
    );

    // Delete temporal.db — lexical stays Current (HEAD unchanged).
    fs::remove_file(&temporal_db_path).unwrap();
    assert!(
        !temporal_db_path.exists(),
        "temporal.db deleted (test setup)"
    );

    // Second call: lexical is Current, temporal.db is missing.
    // BUG B fix: must self-heal temporal.db before the Current early-return.
    let (refreshed2, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(
        !refreshed2,
        "lexical must NOT be rebuilt (index is Current) even during temporal self-heal"
    );

    // Discriminating: temporal.db must be recreated.
    assert!(
        temporal_db_path.exists(),
        "temporal.db must be self-healed by auto_refresh_if_stale on Current branch (#357 BUG B)"
    );

    let db = rskim_search::TemporalDb::open(&temporal_db_path).unwrap();

    // Discriminating: META_GIT_HEAD must equal the current HEAD.
    let stored_head = db
        .get_meta(rskim_search::META_GIT_HEAD)
        .unwrap()
        .expect("META_GIT_HEAD must be set in self-healed temporal.db");
    assert_eq!(
        stored_head, head,
        "META_GIT_HEAD in self-healed temporal.db must match repo HEAD (#357 BUG B)"
    );

    // Discriminating: hotspots must be non-empty.
    let hotspots = db.top_hotspots(20).unwrap();
    assert!(
        !hotspots.is_empty(),
        "self-healed temporal.db must contain non-empty hotspot data (#357 BUG B)"
    );
}

/// BUG B HEAD-divergent: when temporal.db exists with a planted stale SHA but the
/// lexical index is Current, auto_refresh_if_stale self-heals temporal.db so that
/// META_GIT_HEAD transitions from the stale value to the real HEAD.
///
/// PF-007 discriminating: the value MUST change from planted_head to real head.
#[test]
fn test_bug_b_auto_refresh_self_heals_head_divergent_temporal_db() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    let head = create_real_git_repo(
        dir.path(),
        &[
            ("feat: add module", &[("src/lib.rs", "pub fn foo() {}")]),
            ("feat: add binary", &[("src/main.rs", "fn main() {}")]),
        ],
    );
    assert_eq!(head.len(), 40, "HEAD must be a 40-char SHA");

    let analytics = TEST_ANALYTICS;

    // First call: builds everything.
    auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();

    let temporal_db_path = cache_dir.join("temporal.db");
    assert!(
        temporal_db_path.exists(),
        "temporal.db must exist after first call"
    );

    // Plant a stale META_GIT_HEAD to simulate the HEAD-divergent case.
    let planted_head = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    {
        let db = rskim_search::TemporalDb::open(&temporal_db_path).unwrap();
        db.set_meta(rskim_search::META_GIT_HEAD, planted_head)
            .unwrap();
        drop(db);
    }

    // Verify the plant took effect.
    {
        let db = rskim_search::TemporalDb::open(&temporal_db_path).unwrap();
        let stored = db.get_meta(rskim_search::META_GIT_HEAD).unwrap();
        assert_eq!(
            stored.as_deref(),
            Some(planted_head),
            "planted HEAD must be set"
        );
    }

    // Second call: lexical is Current; temporal.db exists but HEAD-divergent.
    // BUG B fix: must detect and self-heal the divergent temporal.db.
    let (refreshed2, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(!refreshed2, "lexical must NOT be rebuilt on Current branch");

    // Discriminating: META_GIT_HEAD must transition from planted_head to real head.
    let db = rskim_search::TemporalDb::open(&temporal_db_path).unwrap();
    let healed_head = db
        .get_meta(rskim_search::META_GIT_HEAD)
        .unwrap()
        .expect("META_GIT_HEAD must be set after self-heal");
    assert_ne!(
        healed_head, planted_head,
        "META_GIT_HEAD must have changed from planted stale value"
    );
    assert_eq!(
        healed_head, head,
        "META_GIT_HEAD must equal the real repo HEAD after self-heal (#357 BUG B HEAD-divergent)"
    );
}

/// BUG B no-rebuild-loop: when temporal.db is Current (META_GIT_HEAD == current HEAD),
/// two consecutive auto_refresh_if_stale calls must NOT rewrite temporal.db.
///
/// PF-007 discriminating: compare temporal.db mtime before and after the second call.
/// Guards against an over-eager temporal staleness gate.
#[test]
fn test_bug_b_no_rebuild_loop_when_temporal_is_current() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    let _head = create_real_git_repo(
        dir.path(),
        &[
            ("feat: init", &[("src/lib.rs", "pub fn hello() {}")]),
            ("fix: update", &[("src/lib.rs", "pub fn hello() { // v2 }")]),
        ],
    );

    let analytics = TEST_ANALYTICS;

    // First call: builds everything including temporal.
    auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();

    let temporal_db_path = cache_dir.join("temporal.db");
    assert!(
        temporal_db_path.exists(),
        "temporal.db must exist after first call"
    );

    // Capture mtime before the second call.
    let mtime_before = fs::metadata(&temporal_db_path).unwrap().modified().unwrap();

    // Small delay to ensure mtime would differ if temporal.db were rewritten.
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Second call: both lexical and temporal are Current.
    // Must NOT rebuild temporal.db (mtime must stay unchanged).
    let (refreshed2, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(
        !refreshed2,
        "second call must not rebuild lexical (Current)"
    );

    let mtime_after = fs::metadata(&temporal_db_path).unwrap().modified().unwrap();

    assert_eq!(
        mtime_before, mtime_after,
        "temporal.db mtime must be unchanged when temporal is already Current (no rebuild loop, #357 BUG B)"
    );
}

// ============================================================================
// #357 API CONTRACT — degenerate git repo no-rebuild-loop
// (LOCKED DECISION 2026-06-24, plan lines 14/146/349)
// ============================================================================

/// API CONTRACT (degenerate git repo no-loop, LOCKED DECISION 2026-06-24):
///
/// Two sub-cases:
///
/// **Case A — unborn branch (no commits, HEAD=None)**:
/// `read_git_head` returns `None`; the guard `if let Some(ref head) = current_head`
/// in auto_refresh_if_stale short-circuits before calling `rebuild_temporal`.
/// temporal.db is never written — both-absent is the stable state.
///
/// **Case B — one commit (HEAD readable)**:
/// `rebuild_temporal` is called, writes a present-but-empty temporal.db (zero
/// hotspot rows + META_GIT_HEAD set, LOCKED DECISION 2026-06-24). On the second
/// call, `temporal_db_is_stale` reads META_GIT_HEAD == current HEAD → returns
/// false → rebuild is SKIPPED. temporal.db mtime is STABLE: this is the
/// discriminating observable the unborn-branch sub-case cannot provide.
///
/// PF-007 discriminating for Case B: mtime unchanged between two consecutive
/// auto_refresh calls proves the no-rebuild-loop contract is enforced on the
/// empty-history-but-readable-HEAD path.  Case A proves no error/hang on the
/// unborn-branch path (#357 cycle-2 finding 13: strengthen the tautological
/// both-absent assertion with a truly-discriminating second sub-case).
#[test]
fn test_bug_b_degenerate_repo_empty_history_no_rebuild_loop() {
    use std::process::Command;

    // ── Case A: unborn branch (no commits, HEAD = None) ──────────────────────
    {
        let dir = tempdir().unwrap();
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&cache_dir).unwrap();

        // git init — HEAD points to refs/heads/main or master (unborn branch).
        // No commits → read_git_head returns None.
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(dir.path())
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir.path())
            .output()
            .expect("git config name");

        // Build the lexical index (HEAD=None; manifest has no stored HEAD).
        build_index_in(dir.path(), &cache_dir);

        let analytics = TEST_ANALYTICS;

        let result1 = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics);
        assert!(result1.is_ok(), "Case A: first call must return Ok");
        let (refreshed1, _) = result1.unwrap();
        assert!(!refreshed1, "Case A: lexical must not be rebuilt (Current)");

        let temporal_db_path = cache_dir.join("temporal.db");
        let exists_after_first = temporal_db_path.exists();

        let result2 = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics);
        assert!(result2.is_ok(), "Case A: second call must return Ok");
        let (refreshed2, _) = result2.unwrap();
        assert!(!refreshed2, "Case A: second call must not rebuild lexical");

        let exists_after_second = temporal_db_path.exists();
        // Stability assertion: both-absent is the expected stable state.
        assert_eq!(
            exists_after_first, exists_after_second,
            "Case A: temporal.db existence must be STABLE (no flapping on unborn repo)"
        );
    }

    // ── Case B: one commit (HEAD readable) — discriminating no-loop assertion ─
    // rebuild_temporal writes a present-but-empty temporal.db with META_GIT_HEAD.
    // On the second auto_refresh call, temporal_db_is_stale reads META_GIT_HEAD ==
    // current HEAD → false → temporal.db is NOT rewritten.  Verified via mtime.
    {
        let dir = tempdir().unwrap();
        let cache_dir = dir.path().join("cache");
        fs::create_dir_all(&cache_dir).unwrap();

        // One commit makes HEAD readable.
        create_real_git_repo(dir.path(), &[("init", &[("README", "hello")])]);

        let analytics = TEST_ANALYTICS;

        // First call: NoIndex → build lexical + write empty temporal.db.
        let (refreshed1, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
        assert!(refreshed1, "Case B: first call must build index (NoIndex)");

        let temporal_db_path = cache_dir.join("temporal.db");
        assert!(
            temporal_db_path.exists(),
            "Case B: temporal.db must be created on first call (LOCKED DECISION 2026-06-24)"
        );

        // Verify the DB has META_GIT_HEAD set (so the staleness gate sees Current).
        let db = rskim_search::TemporalDb::open(&temporal_db_path).unwrap();
        let stored_head = db.get_meta(rskim_search::META_GIT_HEAD).unwrap();
        assert!(
            stored_head.is_some(),
            "Case B: META_GIT_HEAD must be set in the empty temporal.db (no-loop key)"
        );
        drop(db);

        // Capture mtime before the second call.
        let mtime_before = fs::metadata(&temporal_db_path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Second call: both lexical and temporal are Current.
        // MUST NOT rewrite temporal.db — mtime must be unchanged.
        let (refreshed2, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
        assert!(
            !refreshed2,
            "Case B: second call must not rebuild lexical (Current)"
        );

        let mtime_after = fs::metadata(&temporal_db_path).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "Case B: temporal.db mtime must be UNCHANGED on second call \
             (no-rebuild-loop on empty-history repo, LOCKED DECISION 2026-06-24)"
        );
    }
}

// ============================================================================
// #379 — Working-tree staleness (uncommitted edits with unchanged git HEAD)
// ============================================================================
//
// These tests exercise the metadata-scan staleness path added in #379. The scan
// runs ONLY after the cheap HEAD compare yields a Current-equivalent verdict
// (AD-379-5), compares each indexed file's mtime AND size against the manifest
// (AD-379-2), and triggers a FULL rebuild (AD-379-4) on any change/add/remove.

/// Helper: read the indexed file set (normalized rel-paths) from the manifest.
fn manifest_paths(root: &std::path::Path, cache_dir: &std::path::Path) -> Vec<String> {
    use crate::cmd::search::manifest::FileManifest;
    let m = FileManifest::load(root.to_path_buf(), cache_dir.to_path_buf()).unwrap();
    m.sorted_paths().iter().map(|s| s.to_string()).collect()
}

/// Helper: restore a file's mtime to a fixed second-resolution value via filetime,
/// modeling the "same-second edit" boundary (AC9 / AC9a / AD-379-2).
fn set_mtime_secs(path: &std::path::Path, secs: i64) {
    let ft = filetime::FileTime::from_unix_time(secs, 0);
    filetime::set_file_mtime(path, ft).unwrap();
}

/// AC4 (API contract): an in-place edit to a tracked file (HEAD unchanged) makes
/// `check_staleness` return `WorkingTreeChanged` with EXACT counts
/// `{ changed: 1, added: 0, removed: 0 }`, AND it MUST still return `Some(manifest)`
/// so `--stats` can display the real HEAD.
///
/// Discriminating: a single edited file produces exactly `changed == 1`.
#[test]
fn test_check_staleness_working_tree_changed_exact_counts() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // One commit so HEAD is readable and stable across the edit.
    create_real_git_repo(
        dir.path(),
        &[("init", &[("src/lib.rs", "fn alpha() {}\n")])],
    );
    build_index_in(dir.path(), &cache_dir);

    // Edit in place WITHOUT committing — HEAD stays the same.
    fs::write(
        dir.path().join("src/lib.rs"),
        "fn alpha_edited_longer() {}\n",
    )
    .unwrap();

    let (result, manifest) = check_staleness(&cache_dir, dir.path());
    match result {
        StalenessCheck::WorkingTreeChanged {
            changed,
            added,
            removed,
        } => {
            assert_eq!(changed, 1, "exactly one file edited");
            assert_eq!(added, 0, "no files added");
            assert_eq!(removed, 0, "no files removed");
        }
        other => panic!("expected WorkingTreeChanged, got {other:?}"),
    }
    assert!(
        manifest.is_some(),
        "WorkingTreeChanged MUST carry the loaded manifest for --stats (AC4)"
    );
}

/// AC1 / AC5 (behavior contract): editing a tracked file triggers ONE rebuild via
/// `auto_refresh_if_stale` (refreshed == true), and the post-edit manifest reflects
/// the new file set. Forbids exit-0-only assertions (PF-007): we assert refreshed
/// AND that the rebuilt manifest re-indexed the edited path.
#[test]
fn test_auto_refresh_rebuilds_on_working_tree_edit() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    create_real_git_repo(
        dir.path(),
        &[("init", &[("src/lib.rs", "fn original_token() {}\n")])],
    );
    build_index_in(dir.path(), &cache_dir);

    // Capture the manifest mtime so we can prove exactly one rebuild happened.
    let manifest_path = cache_dir.join("index.skfiles");
    let mtime_before = fs::metadata(&manifest_path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));

    // In-place edit (HEAD unchanged) introducing a new token, longer than before.
    fs::write(
        dir.path().join("src/lib.rs"),
        "fn original_token() {}\nfn brand_new_marker() {}\n",
    )
    .unwrap();

    let analytics = TEST_ANALYTICS;
    let (refreshed, manifest) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();

    assert!(refreshed, "in-place edit must trigger a rebuild (AC1/AC5)");
    // Manifest was rewritten exactly once (mtime advanced).
    let mtime_after = fs::metadata(&manifest_path).unwrap().modified().unwrap();
    assert_ne!(
        mtime_before, mtime_after,
        "manifest must be rewritten by the rebuild (single build side-effect)"
    );
    // The edited file is still indexed (the manifest reflects post-edit state).
    assert!(
        manifest.lookup("src/lib.rs").is_some(),
        "rebuilt manifest must include the edited file"
    );
}

/// AC2: a NEW tracked file (non-dotfile, not gitignored) appears in the indexed
/// set on the next query. Discriminating: pre-fix the file is absent until HEAD
/// moves — here HEAD never moves, so only the working-tree scan can surface it.
#[test]
fn test_auto_refresh_indexes_new_working_tree_file() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    create_real_git_repo(dir.path(), &[("init", &[("src/a.rs", "fn a() {}\n")])]);
    build_index_in(dir.path(), &cache_dir);

    // Add a brand-new source file WITHOUT committing.
    fs::write(dir.path().join("src/b.rs"), "fn b() {}\n").unwrap();

    let analytics = TEST_ANALYTICS;
    let (refreshed, _manifest) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(
        refreshed,
        "a new working-tree file must trigger a rebuild (AC2)"
    );

    let paths = manifest_paths(dir.path(), &cache_dir);
    assert!(
        paths.iter().any(|p| p == "src/b.rs"),
        "new file src/b.rs must be indexed after refresh; got {paths:?}"
    );
}

/// AC3: a DELETED tracked file disappears from the indexed set; a rename
/// (delete A + add B in the same window) reflects both A's absence and B's
/// presence. Discriminating: pre-fix the deleted path is still returned.
#[test]
fn test_auto_refresh_reflects_delete_and_rename() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    create_real_git_repo(
        dir.path(),
        &[(
            "init",
            &[
                ("src/old.rs", "fn renamed_me() {}\n"),
                ("src/keep.rs", "fn keep() {}\n"),
            ],
        )],
    );
    build_index_in(dir.path(), &cache_dir);

    // Rename old.rs -> new.rs (delete + add) WITHOUT committing.
    fs::remove_file(dir.path().join("src/old.rs")).unwrap();
    fs::write(dir.path().join("src/new.rs"), "fn renamed_me() {}\n").unwrap();

    let analytics = TEST_ANALYTICS;
    let (refreshed, _manifest) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(refreshed, "delete+add must trigger a rebuild (AC3)");

    let paths = manifest_paths(dir.path(), &cache_dir);
    assert!(
        !paths.iter().any(|p| p == "src/old.rs"),
        "deleted src/old.rs must be gone after refresh; got {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p == "src/new.rs"),
        "added src/new.rs must be present after refresh; got {paths:?}"
    );
}

/// AC7 (negative regression): on a CLEAN tree, calling `auto_refresh_if_stale`
/// twice returns `refreshed == false` every time AND index.skfiles mtime is
/// unchanged across calls. Guards the clean-tree false-positive regression.
#[test]
fn test_auto_refresh_clean_tree_no_rebuild_idempotent() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    create_real_git_repo(
        dir.path(),
        &[("init", &[("src/lib.rs", "fn clean() {}\n")])],
    );
    build_index_in(dir.path(), &cache_dir);

    let manifest_path = cache_dir.join("index.skfiles");
    let mtime0 = fs::metadata(&manifest_path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));

    let analytics = TEST_ANALYTICS;
    let (r1, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    let (r2, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();

    assert!(!r1, "clean tree: first call must not rebuild (AC7)");
    assert!(!r2, "clean tree: second call must not rebuild (AC7)");

    let mtime_final = fs::metadata(&manifest_path).unwrap().modified().unwrap();
    assert_eq!(
        mtime0, mtime_final,
        "clean tree: index.skfiles mtime must be unchanged across calls (AC7)"
    );
}

/// AC8 (short-circuit): the working-tree scan MUST NOT run on the HeadChanged
/// branch. A HEAD-changed repo WITH a working-tree edit returns HeadChanged
/// (NOT WorkingTreeChanged), proving the scan is gated behind a Current HEAD.
#[test]
fn test_check_staleness_head_changed_short_circuits_working_tree_scan() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    create_real_git_repo(dir.path(), &[("init", &[("src/lib.rs", "fn a() {}\n")])]);
    build_index_in(dir.path(), &cache_dir);

    // Edit the working tree AND advance HEAD to a different SHA.
    fs::write(dir.path().join("src/lib.rs"), "fn a_changed_more() {}\n").unwrap();
    let git_dir = dir.path().join(".git");
    fs::write(
        git_dir.join("HEAD"),
        "bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222\n",
    )
    .unwrap();

    let (result, _) = check_staleness(&cache_dir, dir.path());
    assert!(
        matches!(result, StalenessCheck::HeadChanged { .. }),
        "HEAD-changed must short-circuit before the working-tree scan (AC8), got {result:?}"
    );
}

/// AC9 (pinned boundary): a content edit that preserves BOTH mtime AND size
/// (same-length byte swap with mtime restored via filetime) MUST NOT reindex.
///
/// AD-379-2: a same-size + same-second swap is deliberately undetectable without
/// SHA, kept off the hot path. This is an intentional, documented boundary.
#[test]
fn test_auto_refresh_same_mtime_and_size_does_not_reindex() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    let file_rel = "src/lib.rs";
    let original = "fn aaaa() {}\n"; // fixed length
    create_real_git_repo(dir.path(), &[("init", &[(file_rel, original)])]);

    // Pin the file mtime to a fixed second BEFORE building so the manifest records it.
    let abs = dir.path().join(file_rel);
    set_mtime_secs(&abs, 1_700_000_000);
    build_index_in(dir.path(), &cache_dir);

    // Same-length byte swap (size identical), then restore the exact same mtime.
    let swapped = "fn bbbb() {}\n"; // same byte length as `original`
    assert_eq!(swapped.len(), original.len(), "swap must preserve size");
    fs::write(&abs, swapped).unwrap();
    set_mtime_secs(&abs, 1_700_000_000);

    let analytics = TEST_ANALYTICS;
    let (refreshed, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(
        !refreshed,
        "same-size + same-second swap must NOT reindex (AD-379-2 pinned boundary, AC9)"
    );
}

/// AC9a (size closes the same-second hole): an edit that changes the file SIZE
/// but preserves second-resolution mtime (restored via filetime) MUST trigger a
/// rebuild. Discriminating against Open Decision 2: an mtime-only comparator
/// would return false here and miss the edit.
#[test]
fn test_auto_refresh_size_change_with_preserved_mtime_reindexes() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    let file_rel = "src/lib.rs";
    let original = "fn short() {}\n";
    create_real_git_repo(dir.path(), &[("init", &[(file_rel, original)])]);

    let abs = dir.path().join(file_rel);
    set_mtime_secs(&abs, 1_700_000_000);
    build_index_in(dir.path(), &cache_dir);

    // Edit that CHANGES the size, then restore the SAME second-resolution mtime.
    let longer = "fn short() {}\nfn size_growth_marker() {}\n";
    assert_ne!(longer.len(), original.len(), "edit must change size");
    fs::write(&abs, longer).unwrap();
    set_mtime_secs(&abs, 1_700_000_000);

    let analytics = TEST_ANALYTICS;
    let (refreshed, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(
        refreshed,
        "size change with preserved mtime MUST reindex (size comparison, AC9a)"
    );

    // Post-edit manifest carries a populated size for the file.
    use crate::cmd::search::manifest::FileManifest;
    let m = FileManifest::load(dir.path().to_path_buf(), cache_dir.to_path_buf()).unwrap();
    assert!(
        m.lookup(file_rel).and_then(|e| e.size).is_some(),
        "rebuilt manifest must carry a populated size (AC9a)"
    );
}

/// AC12: a NON-git directory (no .git) with an indexed file MUST trigger a
/// rebuild on the next query when the working tree changes. Discriminating:
/// pre-fix the `(None, None)` branch returned Current unconditionally (AD-379-3).
#[test]
fn test_auto_refresh_non_git_working_tree_change_reindexes() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // Non-git project (no .git). Write a source file and build.
    fs::write(dir.path().join("lib.rs"), "fn ng_original() {}\n").unwrap();
    build_index_in(dir.path(), &cache_dir);

    // Edit the file (size grows) — no git involved.
    fs::write(
        dir.path().join("lib.rs"),
        "fn ng_original() {}\nfn ng_added() {}\n",
    )
    .unwrap();

    let analytics = TEST_ANALYTICS;
    let (refreshed, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(
        refreshed,
        "non-git working-tree change MUST reindex (AD-379-3, AC12)"
    );
}

/// AC13: manifest has a stored HEAD but `read_git_head` returns None (corrupt
/// .git/HEAD). A working-tree edit MUST trigger a rebuild. Discriminating:
/// pre-fix the `(Some, None)` branch returned Current unconditionally (AD-379-6).
#[test]
fn test_auto_refresh_corrupt_head_with_working_tree_change_reindexes() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // Real repo so the manifest records a stored HEAD at build time.
    create_real_git_repo(
        dir.path(),
        &[("init", &[("src/lib.rs", "fn ch_original() {}\n")])],
    );
    build_index_in(dir.path(), &cache_dir);

    // Corrupt HEAD so read_git_head returns None (not a valid ref or SHA).
    let git_dir = dir.path().join(".git");
    fs::write(git_dir.join("HEAD"), "garbage-not-a-ref\n").unwrap();
    assert!(
        read_git_head(dir.path()).is_none(),
        "corrupt HEAD must make read_git_head return None (test precondition)"
    );

    // Edit the working tree (size grows).
    fs::write(
        dir.path().join("src/lib.rs"),
        "fn ch_original() {}\nfn ch_added() {}\n",
    )
    .unwrap();

    let analytics = TEST_ANALYTICS;
    let (refreshed, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(
        refreshed,
        "corrupt-HEAD + working-tree edit MUST reindex (AD-379-6, AC13)"
    );
}

/// AC14 (stampede collapse): two sequential `auto_refresh_if_stale` calls after a
/// single edit — the first rebuilds, the second observes the now-refreshed index
/// and returns `refreshed == false` WITHOUT a second build. Exactly one rebuild
/// side-effect across the pair (asserted via a single manifest mtime change).
#[test]
fn test_auto_refresh_working_tree_change_single_rebuild_across_pair() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    create_real_git_repo(dir.path(), &[("init", &[("src/lib.rs", "fn s() {}\n")])]);
    build_index_in(dir.path(), &cache_dir);

    fs::write(
        dir.path().join("src/lib.rs"),
        "fn s() {}\nfn second_marker() {}\n",
    )
    .unwrap();

    let manifest_path = cache_dir.join("index.skfiles");
    let analytics = TEST_ANALYTICS;

    // First call rebuilds.
    let (r1, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(r1, "first call must rebuild on the edit (AC14)");
    let mtime_after_first = fs::metadata(&manifest_path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Second call: index is now Current (manifest carries fresh mtime+size).
    let (r2, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(
        !r2,
        "second call must NOT rebuild — index already refreshed (AC14 / AD-379-8)"
    );
    let mtime_after_second = fs::metadata(&manifest_path).unwrap().modified().unwrap();
    assert_eq!(
        mtime_after_first, mtime_after_second,
        "exactly one rebuild across the pair (manifest mtime stable after 2nd call, AC14)"
    );
}

/// AC10 (no version bump / forward-compat): a pre-#379 manifest whose entries
/// have `mtime: None` and `size: None` (serde default) MUST load, and the first
/// query MUST trigger one rebuild that repopulates mtime AND size — WITHOUT a
/// FORMAT_VERSION bump (header stays version 3 here).
#[test]
fn test_auto_refresh_pre_379_manifest_self_heals_populates_mtime_size() {
    use crate::cmd::search::manifest::{FileManifest, ManifestEntry};

    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // Build a real index so lexical/AST stubs + git HEAD are valid and Current.
    create_real_git_repo(dir.path(), &[("init", &[("src/lib.rs", "fn p() {}\n")])]);
    build_index_in(dir.path(), &cache_dir);

    // Rewrite the manifest to model a pre-#379 build: same paths but mtime/size None.
    // Keep the stored HEAD so the HEAD compare yields Current (only the scan can fire).
    let head = read_git_head(dir.path());
    let loaded = FileManifest::load(dir.path().to_path_buf(), cache_dir.to_path_buf()).unwrap();
    let paths: Vec<String> = loaded
        .sorted_paths()
        .iter()
        .map(|s| s.to_string())
        .collect();
    let mut downgraded = FileManifest::new(dir.path().to_path_buf(), cache_dir.to_path_buf());
    downgraded.set_git_head(head);
    for p in &paths {
        let e = loaded.lookup(p).unwrap();
        downgraded.insert(ManifestEntry {
            path: e.path.clone(),
            sha256: e.sha256.clone(),
            lang: e.lang.clone(),
            field_map: e.field_map.clone(),
            mtime: None, // pre-#379: absent
            size: None,  // pre-#379: absent
        });
    }
    downgraded.save().unwrap();

    // First query: the None mtime/size forces a changed verdict → one rebuild.
    let analytics = TEST_ANALYTICS;
    let (refreshed, _) = auto_refresh_if_stale(dir.path(), &cache_dir, &analytics).unwrap();
    assert!(
        refreshed,
        "pre-#379 manifest (mtime/size None) must self-heal via one rebuild (AC10)"
    );

    // The rewritten manifest now carries populated mtime AND size.
    let healed = FileManifest::load(dir.path().to_path_buf(), cache_dir.to_path_buf()).unwrap();
    let entry = healed
        .lookup("src/lib.rs")
        .expect("file must still be indexed");
    assert!(entry.mtime.is_some(), "rebuild must populate mtime (AC10)");
    assert!(entry.size.is_some(), "rebuild must populate size (AC10)");
}

/// AC16 (at-cap determinism): two `walk_metadata` invocations over the same tree
/// at a small injected cap MUST return byte-identical ordered path sets (the
/// sort-before-truncate guarantee, AD-379-7). Without it, truncated sets could
/// differ run-to-run and oscillate the staleness verdict into a rebuild loop.
#[test]
fn test_walk_metadata_at_cap_is_deterministic() {
    use crate::cmd::search::walk::{normalize_rel_path, walk_metadata};

    let dir = tempdir().unwrap();
    // Create more files than the injected cap so truncation actually engages.
    for i in 0..20 {
        fs::write(dir.path().join(format!("f{i:02}.rs")), "fn x() {}\n").unwrap();
    }

    let cap = 5usize;
    let (a, _) = walk_metadata(dir.path(), cap).unwrap();
    let (b, _) = walk_metadata(dir.path(), cap).unwrap();

    let a_paths: Vec<String> = a.iter().map(|e| normalize_rel_path(&e.rel_path)).collect();
    let b_paths: Vec<String> = b.iter().map(|e| normalize_rel_path(&e.rel_path)).collect();

    assert!(a_paths.len() <= cap, "walk must respect the cap");
    assert_eq!(
        a_paths, b_paths,
        "at-cap path sets must be byte-identical across runs (sort-before-truncate, AD-379-7/AC16)"
    );
}

// ============================================================================
// Display impl for StalenessCheck
// ============================================================================

#[test]
fn test_display_current() {
    assert_eq!(StalenessCheck::Current.to_string(), "current");
}

/// #379: the WorkingTreeChanged Display surfaces the exact `--stats` phrasing
/// required by AC6 (text + JSON both render via this Display).
#[test]
fn test_display_working_tree_changed() {
    let s = StalenessCheck::WorkingTreeChanged {
        changed: 2,
        added: 1,
        removed: 3,
    }
    .to_string();
    assert_eq!(
        s,
        "stale (working tree changed: 2 modified, 1 added, 3 removed)"
    );
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
