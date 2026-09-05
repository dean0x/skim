//! Tests for the staleness detection module (staleness.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;

use tempfile::tempdir;

use super::{
    HeadState, ReanchorPolicy, StalenessCheck, TEMPORAL_META_READ_COUNT, auto_refresh_if_stale,
    check_staleness, git_head_state, read_git_head, resolve_git_dir, temporal_db_is_stale,
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

/// Build a `temporal.db` in `dir/temporal.db` with the given `head` and
/// `data_version`.
///
/// Opens the DB, calls `sync()` with an empty history (simulating a
/// freshly-initialised DB), then plants `data_version` directly using
/// [`super::plant_meta_raw`] — bypassing the `TemporalDb::set_meta` guard
/// that protects the key in production code (AD-408-3).
///
/// Returns the `PathBuf` of `dir/temporal.db` so callers that need to
/// re-open or further modify the file can do so without re-deriving the path.
fn plant_db_at_data_version(
    dir: &std::path::Path,
    head: &str,
    version: &str,
) -> std::path::PathBuf {
    let db_path = dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    db.sync(&[], &[], &[], head, false).unwrap();
    drop(db);
    super::plant_meta_raw(&db_path, rskim_search::META_DATA_VERSION, version);
    db_path
}

/// Write a minimal valid AST index stub file in `cache_dir`.
///
/// `index_version` reads the first 6 bytes: magic `SKAX` + version u16 LE.
/// Writing the current `AST_INDEX_FORMAT_VERSION` prevents the AST self-heal
/// from reporting `NoStoredHead` in unit tests that only stub the lexical index.
///
/// The version bytes are derived from `rskim_search::AST_INDEX_FORMAT_VERSION`
/// so this stub automatically tracks future FORMAT_VERSION bumps without requiring
/// a manual edit — the same maintenance-safety pattern used by `write_lexical_index_stub`.
fn write_ast_index_stub(cache_dir: &std::path::Path) {
    // Write a minimum-valid AST index: 48-byte header with all-zero fields.
    //
    // AD-414-6: index_integrity now validates the full header size and the
    // .skidx expected size (HEADER_SIZE + bigram_count*BIGRAM_ENTRY_SIZE +
    // trigram_count*TRIGRAM_ENTRY_SIZE + file_count*FILE_META_SIZE).
    // With all counts = 0, expected = 48; the stub file must be exactly 48 bytes.
    //
    // E-9 case: postings_file_size = 0 → index_integrity returns Ok(version)
    // before checking for .skpost, so no ast_index.skpost is needed.
    //
    // All f32 fields are 0.0 (finite, >= 0.0) — pass decode_header validation.
    let version = rskim_search::AST_INDEX_FORMAT_VERSION;
    let mut header = [0u8; 48];
    header[0..4].copy_from_slice(b"SKAX");
    header[4..6].copy_from_slice(&version.to_le_bytes());
    // Remaining fields (bigram_count, trigram_count, file_count,
    // postings_file_size, avg_bigram/trigram/node/max_depth, checksum) stay 0.
    fs::write(cache_dir.join("ast_index.skidx"), header).unwrap();
    // No ast_index.skpost created: E-9 early-return skips the .skpost probe.
}

/// Write a minimal valid lexical index stub file in `cache_dir`.
///
/// `lexical_index_integrity` reads the first 6 bytes for the version check.
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
    // Write a minimum-valid lexical index: 62-byte header + empty index.skpost.
    //
    // AD-414-6: lexical_index_integrity now validates the full header size and
    // the .skidx expected size (HEADER_SIZE + ngram_count*ENTRY_SIZE +
    // file_count*FILE_META_SIZE). With all counts = 0, expected = 62; the stub
    // .skidx must be exactly 62 bytes.
    //
    // Step 7 of lexical_index_integrity always checks for .skpost — even when
    // postings_file_size = 0 — so an empty file must exist; unlike the AST path,
    // there is no early-return for a zero postings_file_size.
    //
    // All f32 fields are 0.0 (finite, >= 0.0) — pass decode_header validation.
    let version = rskim_search::LEXICAL_INDEX_FORMAT_VERSION;
    let mut header = [0u8; 62];
    header[0..4].copy_from_slice(b"SKIX");
    header[4..6].copy_from_slice(&version.to_le_bytes());
    // Remaining fields (ngram_count, file_count, postings_file_size,
    // avg_doc_length, avg_field_lengths[8], checksum) stay 0.
    fs::write(cache_dir.join("index.skidx"), header).unwrap();
    // Empty .skpost: postings_file_size = 0 in header → expected 0 bytes.
    fs::write(cache_dir.join("index.skpost"), b"").unwrap();
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

/// Write a v5-format binary manifest (`index.skfiles`) into `cache_dir`,
/// containing exactly one entry for `rel_path` with:
///   - `sha256` = the caller-supplied SHA (use the REAL file SHA so a SHA-check
///     cache HIT fires under a regression that skips the version gate)
///   - `lang` = "rust"
///   - `field_map` = one all-SymbolName (discriminant = 2) span `[0, source_len)`
///     (the unconditional SymbolName classification from before AD-411-1)
///
/// The manifest header uses `version = 5` (LE u32), which `decode_header`
/// rejects (`version 5 ≠ FORMAT_VERSION 7`) → `FileManifest::load` returns an
/// empty manifest → no cache hit → fresh `classify_source` → AD-411-1 semantics.
///
/// If the version gate in `decode_header` were removed (regression), the struct
/// would parse successfully, `manifest.lookup(rel_path)` would find the entry,
/// the SHA would match the real file content, and `read_and_classify` would reuse
/// the stale SymbolName field_map via a SHA-check cache hit — bypassing
/// `classify_source` entirely. The end-to-end test asserts FunctionSignature
/// (disc = 1) in the rebuilt field_map, which is impossible under that regression.
fn write_v5_manifest_with_symbolname_and_sha(
    root: &std::path::Path,
    cache_dir: &std::path::Path,
    rel_path: &str,
    sha256: &str,
    source_len: usize,
) {
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let root_str = canonical_root.to_string_lossy();
    let root_bytes = root_str.as_bytes();

    let mut buf = Vec::<u8>::new();

    // Fixed header: magic "SKFM" + version = 5 (stale) + entry_count = 1
    buf.extend_from_slice(b"SKFM");
    buf.extend_from_slice(&5u32.to_le_bytes());
    buf.extend_from_slice(&1u32.to_le_bytes());

    // Variable header: root (length-prefixed) + git_head absent
    buf.extend_from_slice(&(root_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(root_bytes);
    buf.push(0u8); // git_head_present = false (None)

    // Entry: path
    let path_bytes = rel_path.as_bytes();
    buf.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(path_bytes);
    // Entry: sha256 — the REAL file SHA so SHA-check cache HIT fires under regression
    let sha_bytes = sha256.as_bytes();
    buf.extend_from_slice(&(sha_bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(sha_bytes);
    // Entry: lang = "rust"
    buf.extend_from_slice(&4u32.to_le_bytes());
    buf.extend_from_slice(b"rust");
    // Entry: field_map — one SymbolName (disc = 2) span [0, source_len)
    // This is the stale pre-AD-411-1 classification that must NOT be reused.
    buf.extend_from_slice(&1u32.to_le_bytes()); // field_map_count = 1
    buf.extend_from_slice(&0u32.to_le_bytes()); // start = 0
    buf.extend_from_slice(&(source_len as u32).to_le_bytes()); // end = source_len
    buf.push(2u8); // discriminant = 2 (SymbolName — old unconditional pre-AD-411-1 field)
    // Entry: mtime absent, size absent
    buf.push(0u8); // mtime_present = 0 (None)
    buf.push(0u8); // size_present  = 0 (None)

    // v5 skip section (empty)
    buf.extend_from_slice(&0u32.to_le_bytes()); // skip_count = 0

    fs::write(cache_dir.join("index.skfiles"), &buf).unwrap();
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
    // AC5a precondition: no commondir file (ensures the commondir ladder is not in play).
    assert!(
        !dir.path().join(".git").join("commondir").exists(),
        "AC5a: no commondir file must be present for the no-redirect case"
    );

    let result = read_git_head(dir.path());
    assert_eq!(result.as_deref(), Some(sha));
}

#[test]
fn test_read_git_head_follows_symbolic_ref_to_loose_ref() {
    let dir = tempdir().unwrap();
    let sha = "deadbeef12345678deadbeef12345678deadbeef";
    create_fake_git_repo(dir.path(), "ref: refs/heads/main\n");
    create_ref_file(dir.path(), "refs/heads/main", sha);
    // AC5a precondition: no commondir file.
    assert!(
        !dir.path().join(".git").join("commondir").exists(),
        "AC5a: no commondir file must be present for the no-redirect case"
    );

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
    // AC5a precondition: no commondir file.
    assert!(
        !dir.path().join(".git").join("commondir").exists(),
        "AC5a: no commondir file must be present for the no-redirect case"
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
    // AC5a precondition: no commondir file.
    assert!(
        !dir.path().join(".git").join("commondir").exists(),
        "AC5a: no commondir file must be present for the no-redirect case"
    );

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
    // AC5a precondition: no commondir file.
    assert!(
        !dir.path().join(".git").join("commondir").exists(),
        "AC5a: no commondir file must be present for the no-redirect case"
    );

    let result = read_git_head(dir.path());
    assert!(
        result.is_none(),
        "path traversal ref should be rejected, got {result:?}"
    );
}

/// AC9 / S9 — a ref path starting with `refs/` but containing `..` components
/// (three levels, e.g. `refs/../../../outside-sha`) escapes the git directory.
/// The old `starts_with("refs/")` guard did not normalise `..` and would read
/// the out-of-tree file and persist its SHA.  AD-413-6 closes the hole.
///
/// MANDATORY PRECONDITION (PF-007): `git_dir.join(ref_path)` must be an existing
/// file — without it the test is vacuous (the old code returns `None` because the
/// file is absent, not because the guard fired).
#[test]
fn test_read_git_head_rejects_ref_path_escaping_the_git_dir() {
    // Build:  <parent>/
    //           repo/.git/refs/  ← project root with a real .git/refs dir
    //           outside-sha      ← the escape target planted at parent level
    let parent = tempdir().unwrap();
    let project_root = parent.path().join("repo");
    let git_dir = project_root.join(".git");
    fs::create_dir_all(git_dir.join("refs")).unwrap();

    // Plant the escape SHA outside the project root.
    let escape_sha = "2".repeat(40);
    let escape_target = parent.path().join("outside-sha");
    fs::write(&escape_target, format!("{escape_sha}\n")).unwrap();

    // The ref path starts with "refs/" (passes the old prefix-only guard) but
    // has three ".." components that walk up past the git dir and the project root.
    let ref_path = "refs/../../../outside-sha";

    // MANDATORY PRECONDITION: assert the escape target is actually reachable via
    // git_dir.join(ref_path) so the test fails pre-fix, not vacuously.
    assert!(
        git_dir.join(ref_path).exists(),
        "precondition: git_dir.join(ref_path) must reach the escape target — \
         if this fails the OS cannot resolve the path and the test is vacuous"
    );
    // AC5a precondition: no commondir file (case vi of six no-commondir sub-cases).
    assert!(
        !git_dir.join("commondir").exists(),
        "AC5a: no commondir file must be present for the no-redirect case"
    );

    fs::write(git_dir.join("HEAD"), format!("ref: {ref_path}\n")).unwrap();

    let result = read_git_head(&project_root);
    assert!(
        result.is_none(),
        "ref path escaping the git dir via '..` must be rejected, got {result:?}"
    );
    // Belt-and-suspenders: the escape SHA must never appear in the result.
    assert_ne!(
        result.as_deref(),
        Some(escape_sha.as_str()),
        "escape SHA must not be returned"
    );
}

#[test]
fn test_read_git_head_accepts_sha256_hash() {
    let dir = tempdir().unwrap();
    // 64-hex SHA-256 detached HEAD
    let sha256 = "a".repeat(64);
    create_fake_git_repo(dir.path(), &format!("{sha256}\n"));
    // AC5a precondition: no commondir file (case iv of six no-commondir sub-cases).
    assert!(
        !dir.path().join(".git").join("commondir").exists(),
        "AC5a: no commondir file must be present for the no-redirect case"
    );

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
    // Write a v2 lexical stub (bigram-era format, below current v7).
    // magic = b"SKIX", version = 2 (LE u16).
    fs::write(cache_dir.join("index.skidx"), b"SKIX\x02\x00").unwrap();
    // Valid AST stub so AST self-heal does not co-trigger.
    write_ast_index_stub(&cache_dir);

    let (result, manifest) = check_staleness(&cache_dir, dir.path());

    // Must report stale (lexical v2 < v7 → self-heal required).
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
    // Write a v3 lexical stub (pre-varint-compression format, below current v7).
    // magic = b"SKIX", version = 3 (LE u16).
    fs::write(cache_dir.join("index.skidx"), b"SKIX\x03\x00").unwrap();
    // Valid AST stub so AST self-heal does not co-trigger.
    write_ast_index_stub(&cache_dir);

    let (result, manifest) = check_staleness(&cache_dir, dir.path());

    // Must report stale (lexical v3 < v7 → self-heal required, same guard as v2).
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

/// AC-P2-3 / #392: a v4 lexical stub (pre-token_position format, the version this
/// #392/#380-Phase-2 change upgrades FROM) must trigger `NoStoredHead` so the
/// staleness check self-heals via full rebuild under the v7 binary — the generic
/// `v < LEXICAL_INDEX_FORMAT_VERSION` guard (staleness.rs), same code path as v2/v3.
///
/// PF-007: asserts the exact `NoStoredHead` observable + manifest returned.
#[test]
fn test_check_staleness_lexical_v4_below_version_triggers_rebuild_returns_manifest() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let sha = "ccdd3344ccdd3344ccdd3344ccdd3344ccdd3344";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    write_manifest_with_head(dir.path(), &cache_dir, Some(sha));
    // Write a v4 lexical stub (pre-token_position format, below current v7).
    // magic = b"SKIX", version = 4 (LE u16).
    fs::write(cache_dir.join("index.skidx"), b"SKIX\x04\x00").unwrap();
    write_ast_index_stub(&cache_dir);

    let (result, manifest) = check_staleness(&cache_dir, dir.path());

    assert!(
        matches!(result, StalenessCheck::NoStoredHead),
        "v4 lexical index must trigger NoStoredHead rebuild under v7 binary; got {result:?}"
    );
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

/// AD-411-5 / ADR-006: a v5 lexical stub (pre-AD-411-1 field_id semantic change)
/// must trigger `NoStoredHead` so the staleness check self-heals via full rebuild
/// under the v7 binary.  The stored field_ids are semantically incorrect in v5:
/// identifier bytes all carry SymbolName (unconditional) rather than the new
/// declaration-name-aware tier (FunctionSignature / TypeDefinition / ImportExport
/// / SymbolName / FunctionBody per context).  A v5 on-disk index is therefore
/// silently mis-ranked rather than corrupted; clean rejection + self-heal is the
/// correct response (same `v < LEXICAL_INDEX_FORMAT_VERSION` guard in staleness.rs).
///
/// PF-007 compliance: asserts the exact `NoStoredHead` observable + manifest
/// returned (mirrors the sibling v2/v3/v4 test structure exactly).
#[test]
fn test_check_staleness_lexical_v5_below_version_triggers_rebuild_returns_manifest() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let sha = "eeff9900eeff9900eeff9900eeff9900eeff9900";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    write_manifest_with_head(dir.path(), &cache_dir, Some(sha));
    // Write a v5 lexical stub (pre-AD-411-1 field_id semantic, below current v7).
    // magic = b"SKIX", version = 5 (LE u16).
    fs::write(cache_dir.join("index.skidx"), b"SKIX\x05\x00").unwrap();
    write_ast_index_stub(&cache_dir);

    let (result, manifest) = check_staleness(&cache_dir, dir.path());

    // Must report stale (lexical v5 < v7 → self-heal required).
    assert!(
        matches!(result, StalenessCheck::NoStoredHead),
        "v5 lexical index must trigger NoStoredHead rebuild under v7 binary; got {result:?}"
    );
    assert!(
        manifest.is_some(),
        "check_staleness must return manifest even when lexical index is below version (v5)"
    );
    assert_eq!(
        manifest.unwrap().stored_git_head(),
        Some(sha),
        "--stats must show real HEAD when only the lexical format version is outdated (v5→v7)"
    );
}

/// AD-411-7 / ADR-006: a v6 lexical stub (pre-token_length posting field, the
/// format version the v6→v7 bump in #411 alignment-fix upgrades FROM) must trigger
/// `NoStoredHead` so the staleness check self-heals via full rebuild under the v7
/// binary.  A v6 on-disk index lacks the `delta_token_length` 5th varint per
/// posting entry; `decode_postings_varint` would desync or read across entry
/// boundaries if the v7 binary tried to use it — clean rejection + self-heal is
/// the only safe response (ADR-006, same `v < LEXICAL_INDEX_FORMAT_VERSION` guard
/// in staleness.rs that covers v2/v3/v4/v5).
///
/// This test complements `test_v6_header_rejected_with_please_rebuild_message` in
/// `format_tests.rs` (which exercises the low-level `decode_header` rejection in
/// isolation) by covering the end-to-end `check_staleness` integration path,
/// confirming that `StalenessCheck::NoStoredHead` is returned AND the manifest is
/// preserved for `--stats` display.
///
/// PF-007 compliance: asserts the exact `NoStoredHead` observable + manifest
/// returned (mirrors the sibling v2/v3/v4/v5 test structure exactly).
#[test]
fn test_check_staleness_lexical_v6_below_version_triggers_rebuild_returns_manifest() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let sha = "aabb6677aabb6677aabb6677aabb6677aabb6677";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    write_manifest_with_head(dir.path(), &cache_dir, Some(sha));
    // Write a v6 lexical stub (pre-AD-411-7 token_length posting field, below
    // current v7). magic = b"SKIX", version = 6 (LE u16).
    fs::write(cache_dir.join("index.skidx"), b"SKIX\x06\x00").unwrap();
    write_ast_index_stub(&cache_dir);

    let (result, manifest) = check_staleness(&cache_dir, dir.path());

    // Must report stale (lexical v6 < v7 → self-heal required).
    assert!(
        matches!(result, StalenessCheck::NoStoredHead),
        "v6 lexical index must trigger NoStoredHead rebuild under v7 binary; got {result:?}"
    );
    assert!(
        manifest.is_some(),
        "check_staleness must return manifest even when lexical index is below version (v6)"
    );
    assert_eq!(
        manifest.unwrap().stored_git_head(),
        Some(sha),
        "--stats must show real HEAD when only the lexical format version is outdated (v6→v7)"
    );
}

// ============================================================================
// AD-411-1 end-to-end reclassification: v5 manifest self-heal (PF-007)
// ============================================================================

/// AD-411-1 / PF-007 discriminating end-to-end regression test.
///
/// ## What is being tested
///
/// A v5-format manifest (pre-AD-411-1, all identifier bytes unconditionally
/// SymbolName, discriminant = 2) is planted with the **real file SHA** so that
/// a SHA-check cache HIT would fire under a regression (missing `decode_header`
/// version gate).
///
/// After `auto_refresh_if_stale` detects both the stale v5 manifest AND the
/// stale v5 lexical index (both below current FORMAT_VERSION / LEXICAL_FORMAT_VERSION),
/// `build_index` is called with the v5 manifest rejected to empty →
/// `read_and_classify` gets a cache MISS → `classify_source` runs fresh →
/// `authenticate` in `fn authenticate()` maps to FunctionSignature (disc = 1),
/// NOT the old unconditional SymbolName (disc = 2).
///
/// ## Why the regression payload matters
///
/// Without the REAL SHA in the planted entry, a regression (no version check)
/// would still cause a cache miss (SHA mismatch), `classify_source` would still
/// run, and the test would pass despite the bug — making it a false green.
/// Planting the real SHA turns a cache HIT into the regression trigger: the
/// stale SymbolName field_map is served from the v5 manifest under the bug, and
/// FunctionSignature can never appear in the rebuilt index.
///
/// ## v6→v7 lexical gate coverage
///
/// Replacing the v5 lexical stub with `b"SKIX\x05\x00"` (v5, below current v7)
/// also exercises the v6→v7 boundary of the lexical staleness ladder, which was
/// flagged as missing a classification assertion.  The compound
/// `lexical_stale || manifest_stale` guard in `check_staleness` fires for BOTH
/// independently, so this single test covers both format-version gates.
///
/// ## PF-007 compliance
///
/// Asserts the discriminating observables:
/// - `outcome.refreshed()` (rebuild fired)
/// - `FunctionSignature` (disc = 1) present in rebuilt field_map
/// - `FunctionBody` (disc = 4) present for body blocks / call sites
///
/// Both would be absent if the stale v5 SymbolName cache were incorrectly reused.
#[test]
fn test_v5_manifest_stale_reclassifies_with_new_ad411_field_semantics() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // Source: one function declaration (name → FunctionSignature under new semantics)
    // + one body block and call site (→ FunctionBody). Pre-AD-411-1 all identifiers
    // were unconditionally SymbolName (disc = 2).
    let source = "fn authenticate() {}\nfn main() { authenticate(); }\n";
    fs::write(dir.path().join("auth.rs"), source).unwrap();

    // ── Step 1: build a current-version (v7 manifest + v7 lexical) index ──
    // This establishes the real SHA for auth.rs so the planted v5 manifest can
    // contain a matching SHA — triggering a cache HIT under the regression.
    build_index_in(dir.path(), &cache_dir);

    use crate::cmd::search::manifest::FileManifest;
    let initial_manifest = FileManifest::load(dir.path().to_path_buf(), cache_dir.clone())
        .expect("freshly built manifest must load without error");
    let real_sha = initial_manifest
        .lookup("auth.rs")
        .expect("auth.rs must be indexed after initial build")
        .sha256
        .clone();

    // ── Step 2: overwrite manifest with v5-format + SymbolName + real SHA ──
    // decode_header rejects version = 5 (≠ 7) → empty manifest → no cache hit.
    // Under regression (missing version check): parse succeeds → SHA matches →
    // stale SymbolName (disc = 2) served as a cache hit from the v5 manifest,
    // bypassing classify_source entirely.
    write_v5_manifest_with_symbolname_and_sha(
        dir.path(),
        &cache_dir,
        "auth.rs",
        &real_sha,
        source.len(),
    );

    // ── Step 3: downgrade lexical index to v5 (pre-AD-411-7 format, below v7) ──
    // Guards the v6→v7 lexical gate: the compound `lexical_stale || manifest_stale`
    // in check_staleness fires for BOTH independently (covers the flagged missing
    // v6→v7 self-heal classification assertion).
    fs::write(cache_dir.join("index.skidx"), b"SKIX\x05\x00").unwrap();
    // Keep AST stub current so only lexical + manifest gates fire.
    write_ast_index_stub(&cache_dir);

    // ── Step 4: self-heal via auto_refresh_if_stale ────────────────────────
    // check_staleness: lexical v5 < v7 → stale; manifest v5 ≠ v7 → stale →
    // NoStoredHead → build_index with empty manifest → classify_source fresh.
    let analytics = TEST_ANALYTICS;
    let result = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    );
    assert!(
        result.is_ok(),
        "auto_refresh_if_stale must succeed on v5 self-heal: {:?}",
        result.err()
    );
    let (outcome, rebuilt_manifest, _) = result.unwrap();
    assert!(
        outcome.refreshed(),
        "v5 manifest + v5 lexical must trigger a rebuild \
         (PF-007: test must assert a discriminating observable, not just check exit 0)"
    );

    // ── Step 5: verify AD-411-1 field semantics in the rebuilt manifest ────
    let entry = rebuilt_manifest
        .lookup("auth.rs")
        .expect("auth.rs must be present in rebuilt manifest after self-heal");

    // FunctionSignature (disc = 1): `authenticate` in `fn authenticate()` is
    // the `name:` child of `function_item` (priority 4) → FunctionSignature
    // via map_identifier_to_field (AD-411-1).
    // Pre-AD-411-1 (v5) semantics: unconditionally SymbolName (disc = 2).
    // Under regression: stale SymbolName served from cache → disc = 1 absent.
    let has_fn_sig = entry.field_map.iter().any(|(_, _, d)| *d == 1);
    assert!(
        has_fn_sig,
        "rebuilt manifest must contain FunctionSignature (disc = 1) for the function \
         declaration name after stale-v5 self-heal (manifest v5 → v7, lexical v5 → v7); \
         SymbolName (disc = 2) only would mean \
         the stale v5 field_map cache was incorrectly reused instead of fresh \
         classify_source (AD-411-1 regression, per PF-007). field_map={:?}",
        entry.field_map
    );

    // FunctionBody (disc = 4): body blocks (`{}`, `{ authenticate(); }`) and the
    // call site `authenticate()` in main map to FunctionBody under AD-411-1.
    // Combined with FunctionSignature above, this proves context-aware
    // classify_source ran — the stale v5 entry had only a single SymbolName span
    // covering all bytes; no SymbolName-only rebuild can produce FunctionBody here.
    let has_fn_body = entry.field_map.iter().any(|(_, _, d)| *d == 4);
    assert!(
        has_fn_body,
        "rebuilt manifest must contain FunctionBody (disc = 4) for body blocks and \
         call sites after stale-v5 self-heal (manifest v5 → v7, lexical v5 → v7; proves AD-411-1 context-aware classify_source \
         ran, not stale SymbolName-only cache). field_map={:?}",
        entry.field_map
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
    let (outcome, _manifest, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    assert!(
        !outcome.refreshed(),
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
    let (_outcome, manifest, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

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
    let (outcome, manifest, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    assert!(
        outcome.refreshed(),
        "HEAD changed — index should be rebuilt"
    );
    assert!(
        !outcome.is_first_build(),
        "HEAD changed is an incremental rebuild — outcome must be Incremental, not FirstBuild (AC-405-8)"
    );
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
    let (outcome, manifest, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    assert!(
        outcome.refreshed(),
        "no stored HEAD + git present — index should be rebuilt"
    );
    assert!(
        !outcome.is_first_build(),
        "NoStoredHead is an incremental rebuild — outcome must be Incremental, not FirstBuild (AC-405-8)"
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
    let (first_outcome, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    let (second_outcome, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    assert!(
        !first_outcome.refreshed(),
        "non-git project should not rebuild on first query"
    );
    assert!(
        !second_outcome.refreshed(),
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
    let result = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    );

    assert!(
        result.is_ok(),
        "auto_refresh_if_stale must succeed even when rebuild_temporal \
         degrades on non-git root (AC7 / AC14 hook integration)"
    );

    // The returned manifest must be valid (lexical index was built).
    let (_outcome, manifest, _) = result.unwrap();
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

/// Shared helper: create a linked git worktree.
///
/// Delegates to `staleness::create_real_git_worktree`.
fn create_real_git_worktree(
    primary: &std::path::Path,
    worktree: &std::path::Path,
    branch: &str,
) -> String {
    super::create_real_git_worktree(primary, worktree, branch)
}

/// Shared fixture: create a primary git repo + one linked worktree, returning all
/// four handles the caller needs.
///
/// Encapsulates the 6-line linked-worktree preamble that was duplicated across
/// 13 test functions (ADR-001 deduplication):
/// ```
///     let dir = tempdir().unwrap();
///     let primary = dir.path().join("primary");
///     let worktree = dir.path().join("wt1");
///     fs::create_dir_all(&primary).unwrap();
///     let head = create_real_git_repo(&primary, &[("init", &[("a.rs", "fn a(){}\n")])]);
///     create_real_git_worktree(&primary, &worktree, branch);
/// ```
///
/// Returns `(dir, primary, worktree, head_sha)` where:
/// - `dir` — the `TempDir` root; must be kept alive for the test duration
/// - `primary` — path to the primary repo (`<dir>/primary`)
/// - `worktree` — path to the linked worktree (`<dir>/wt1`)
/// - `head_sha` — 40-char initial commit SHA shared by both primary and worktree
fn worktree_fixture(
    branch: &str,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    std::path::PathBuf,
    String,
) {
    let dir = tempdir().unwrap();
    let primary = dir.path().join("primary");
    let worktree = dir.path().join("wt1");
    fs::create_dir_all(&primary).unwrap();
    let head = create_real_git_repo(&primary, &[("init", &[("a.rs", "fn a(){}\n")])]);
    create_real_git_worktree(&primary, &worktree, branch);
    (dir, primary, worktree, head)
}

/// Shared helper: build `temporal.db` directly (bypassing the lexical pipeline).
///
/// Used by the AD-413-16 anchor tests, which need a `temporal.db` that carries a
/// real `meta.git_toplevel` row without paying for a full index build.
fn build_temporal_for_test(root: &std::path::Path, cache_dir: &std::path::Path, head: &str) {
    use crate::cmd::search::temporal_build::{current_epoch_secs, rebuild_temporal};
    rebuild_temporal(root, cache_dir, head, current_epoch_secs()).expect("rebuild_temporal");
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
    let result = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    );
    assert!(
        result.is_ok(),
        "auto_refresh_if_stale must succeed on a real git repo"
    );

    let (outcome, manifest, _) = result.unwrap();
    assert!(
        outcome.is_first_build(),
        "index must have been built (NoIndex → FirstBuild)"
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
    let (refreshed1, manifest1, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(refreshed1.refreshed(), "first refresh must build the index");

    // Second refresh: index is current — must not rebuild, manifest unchanged.
    let (refreshed2, manifest2, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(
        !refreshed2.refreshed(),
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
        temporal_db_is_stale(dir.path(), "abc1234", None),
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
    db.sync(&[], &[], &[], head, false).unwrap();
    drop(db);

    assert!(
        !temporal_db_is_stale(dir.path(), head, None),
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
    db.sync(&[], &[], &[], planted_head, false).unwrap();
    drop(db);

    assert!(
        temporal_db_is_stale(dir.path(), real_head, None),
        "temporal.db with diverged META_GIT_HEAD must be reported stale (deadbeef case)"
    );
}

// ============================================================================
// AC5 / AC6 / AC7 — temporal data-version gate (AD-408-4)
// ============================================================================

/// AC5: A temporal.db whose git_head MATCHES current_head but has NO
/// data_version row is reported stale (pre-fix DB that lacks the ghost filter).
///
/// AD-408-4 discriminating: the gate must catch pre-fix DBs even when the
/// HEAD matches.  Without the data-version gate, temporal_db_is_stale would
/// return false for such a DB and ghost rows would never be evicted.
#[test]
fn test_temporal_db_data_version_absent_is_stale() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("temporal.db");
    let head = "bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222";

    // Create a DB and set META_GIT_HEAD but NOT data_version (pre-fix state).
    // Plant git_head via raw SQL — set_meta guards version-attestation keys (AD-408-3).
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    drop(db);
    super::plant_meta_raw(&db_path, rskim_search::META_GIT_HEAD, head);

    assert!(
        temporal_db_is_stale(dir.path(), head, None),
        "AC5: temporal.db with matching HEAD but no data_version must be stale \
         (pre-fix DB contains ghost rows)"
    );
}

/// AC5: A temporal.db written by `sync()` carries the data_version row and is
/// NOT reported stale (post-fix DB).
///
/// Also verifies that `sync()` is the version-attesting write path (AD-408-3):
/// an empty-history DB written via sync still gets the data_version row.
#[test]
fn test_temporal_db_after_sync_data_version_is_not_stale() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("temporal.db");
    let head = "cccc3333cccc3333cccc3333cccc3333cccc3333";

    // Write via sync() — the only version-attesting path.
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    db.sync(&[], &[], &[], head, false).unwrap();
    drop(db);

    assert!(
        !temporal_db_is_stale(dir.path(), head, None),
        "AC5: temporal.db written by sync() (with data_version) must NOT be stale"
    );
}

/// AC5: A non-integer data_version value is treated as stale.
///
/// AD-408-4: the gate must parse the stored value as an integer; an
/// unparseable value is treated as stale so corrupt rows trigger a self-heal.
#[test]
fn test_temporal_db_data_version_non_integer_is_stale() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("temporal.db");
    let head = "dddd4444dddd4444dddd4444dddd4444dddd4444";

    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    db.sync(&[], &[], &[], head, false).unwrap();
    drop(db);
    // Overwrite data_version with a non-integer via raw SQL — set_meta guards it (AD-408-3).
    super::plant_meta_raw(&db_path, rskim_search::META_DATA_VERSION, "not-a-number");

    assert!(
        temporal_db_is_stale(dir.path(), head, None),
        "AC5: non-integer data_version must be treated as stale (numeric parse required)"
    );
}

/// AC6: No rebuild loop — after a single sync, two consecutive
/// temporal_db_is_stale calls both return false.
///
/// Also verifies that an empty-history DB (sync with empty slices) carries
/// the data_version row and is NOT perpetually flagged stale (AD-408-3).
#[test]
fn test_temporal_db_data_version_no_rebuild_loop() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("temporal.db");
    let head = "eeee5555eeee5555eeee5555eeee5555eeee5555";

    // Write an empty-history DB via sync().
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    db.sync(&[], &[], &[], head, false).unwrap();
    drop(db);

    // First post-sync check.
    assert!(
        !temporal_db_is_stale(dir.path(), head, None),
        "AC6: first post-sync check must be Current (no rebuild loop)"
    );
    // Second post-sync check — must still be Current.
    assert!(
        !temporal_db_is_stale(dir.path(), head, None),
        "AC6: second post-sync check must be Current (no oscillation)"
    );
}

/// AC7: Forward compatibility — a temporal.db whose stored data_version is
/// GREATER than TEMPORAL_DATA_VERSION is NOT flagged stale.
///
/// AD-408-4: the gate uses `stored < current` (not `!=`) so a DB written by a
/// newer binary is not needlessly rebuilt by an older post-fix binary.
#[test]
fn test_temporal_db_data_version_forward_compat() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("temporal.db");
    let head = "ffff6666ffff6666ffff6666ffff6666ffff6666";

    // Write via sync() then overwrite data_version with a future version.
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    db.sync(&[], &[], &[], head, false).unwrap();
    drop(db);
    // Store a version much higher than the current one via raw SQL — set_meta guards it (AD-408-3).
    let future_version = u64::from(rskim_search::TEMPORAL_DATA_VERSION) + 999;
    super::plant_meta_raw(
        &db_path,
        rskim_search::META_DATA_VERSION,
        &future_version.to_string(),
    );

    assert!(
        !temporal_db_is_stale(dir.path(), head, None),
        "AC7: data_version > TEMPORAL_DATA_VERSION must NOT be stale (forward compat: \
         gate uses stored < current, not stored != current)"
    );
}

/// AD-408-4: A temporal.db whose stored data_version is a VALID INTEGER strictly
/// less than `TEMPORAL_DATA_VERSION` is reported stale — this is the primary
/// numeric rebuild trigger for a versioned-but-outdated pre-fix DB (e.g. a DB
/// written by a binary that produced version 0 before the ghost-filter was
/// introduced).
///
/// Applies ADR-006: the gate uses `stored < current` so any lower integer
/// version forces a self-heal rebuild.  An inverted or mis-wired numeric
/// comparison for this branch would pass all other data-version tests
/// (forward_compat only guards the `>` direction) while silently leaving ghost
/// rows in older-versioned DBs.
///
/// PF-007 discriminating: `data_version = "0"` (valid integer, present,
/// strictly less than TEMPORAL_DATA_VERSION) MUST return true.  Without the
/// `n < current` check the predicate would return false and ghost rows would
/// never be evicted from pre-fix DBs.
#[test]
fn test_temporal_db_data_version_lower_integer_is_stale() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("temporal.db");
    let head = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";

    // Write via sync() to establish a current-format DB, then overwrite
    // data_version with "0" — a valid integer strictly less than the current
    // TEMPORAL_DATA_VERSION, simulating a versioned-but-outdated pre-fix DB.
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    db.sync(&[], &[], &[], head, false).unwrap();
    drop(db);
    // Overwrite data_version with "0" via raw SQL — set_meta guards it (AD-408-3).
    super::plant_meta_raw(&db_path, rskim_search::META_DATA_VERSION, "0");

    assert!(
        temporal_db_is_stale(dir.path(), head, None),
        "AD-408-4: data_version=\"0\" (valid integer < TEMPORAL_DATA_VERSION) must be \
         stale — the `stored < current` numeric gate must fire for any lower version \
         (self-heal trigger for versioned-but-outdated pre-fix DBs, applies ADR-006)"
    );
}

// ============================================================================
// T-8: AD-414-14 — Check 3: shallow→full transition
// ============================================================================

/// T-8a: When the stored is_shallow flag is "1" AND .git/shallow is absent
/// (shallow→full transition: `git fetch --unshallow` removed it), the DB is
/// reported stale so the now-reachable history can be ingested.
///
/// AD-414-14: this gate fires only when `git_dir` is `Some`; passing `None`
/// skips it (backward compat with pre-AD-414-14 callers / tests).
#[test]
fn test_temporal_db_shallow_to_full_is_stale() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("temporal.db");
    let head = "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111";

    // Create a current-format DB via sync().
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    db.sync(&[], &[], &[], head, false).unwrap();
    drop(db);

    // Plant is_shallow = "1" to simulate a DB built on a shallow clone.
    super::plant_meta_raw(&db_path, rskim_search::META_IS_SHALLOW, "1");

    // Provide a fake git_dir WITHOUT a "shallow" file — simulates an unshallowed repo.
    let fake_git_dir = dir.path().join(".git");
    fs::create_dir_all(&fake_git_dir).unwrap();
    // No "shallow" file → transition detected.

    assert!(
        temporal_db_is_stale(dir.path(), head, Some(&fake_git_dir)),
        "T-8a: is_shallow=1 + absent .git/shallow must be reported stale \
         (shallow→full transition, AD-414-14)"
    );
}

/// T-8b: When the stored is_shallow flag is "1" AND .git/shallow STILL EXISTS
/// (the repo is still shallow), the DB is NOT stale on that account.
#[test]
fn test_temporal_db_still_shallow_is_not_stale() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("temporal.db");
    let head = "bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222";

    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    db.sync(&[], &[], &[], head, false).unwrap();
    drop(db);

    // Plant is_shallow = "1".
    super::plant_meta_raw(&db_path, rskim_search::META_IS_SHALLOW, "1");

    // Provide a fake git_dir WITH a "shallow" file — repo is still shallow.
    let fake_git_dir = dir.path().join(".git");
    fs::create_dir_all(&fake_git_dir).unwrap();
    fs::write(fake_git_dir.join("shallow"), b"abc1234\n").unwrap();

    assert!(
        !temporal_db_is_stale(dir.path(), head, Some(&fake_git_dir)),
        "T-8b: is_shallow=1 + present .git/shallow must NOT be stale \
         (repo is still shallow, no transition, AD-414-14)"
    );
}

/// T-8c: When git_dir is None, Check 3 is skipped entirely (backward compat).
/// A DB with is_shallow="1" and no shallow file is NOT stale when git_dir=None.
#[test]
fn test_temporal_db_check3_skipped_when_git_dir_none() {
    let dir = tempdir().unwrap();
    let db_path = dir.path().join("temporal.db");
    let head = "cccc3333cccc3333cccc3333cccc3333cccc3333";

    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    db.sync(&[], &[], &[], head, false).unwrap();
    drop(db);

    // Plant is_shallow = "1" — would trigger stale if git_dir were supplied.
    super::plant_meta_raw(&db_path, rskim_search::META_IS_SHALLOW, "1");
    // No .git/shallow file anywhere — but git_dir is None so check is skipped.

    assert!(
        !temporal_db_is_stale(dir.path(), head, None),
        "T-8c: Check 3 must be skipped when git_dir=None (backward compat)"
    );
}

/// T-8d (AD-414-14 regression guard): Check 3 in a LINKED WORKTREE must probe
/// the COMMON-DIR `shallow` file, not the per-worktree gitdir.
///
/// Regression scenario: `temporal_db_is_stale` receives `git_dir` =
/// `<primary>/.git/worktrees/wt1` (a per-worktree directory that never contains
/// `shallow`).  If the probe used `git_dir.join("shallow")` directly it would
/// always find the file absent → permanent false-positive stale → unbounded
/// rebuild loop.  The fix (`resolve_common_dir`) reads the `commondir` pointer
/// inside the worktree gitdir and resolves to `<primary>/.git` where `shallow`
/// actually lives.
///
/// Sub-cases:
/// - shallow file present in commondir → NOT stale (repo still shallow, correct)
/// - shallow file absent from commondir → stale (shallow→full transition, correct)
#[test]
fn test_temporal_db_check3_linked_worktree_probes_commondir() {
    // Build a real primary repo + linked worktree so git populates the
    // per-worktree `commondir` pointer file automatically.
    let (_dir, primary, worktree, head) = worktree_fixture("wt-check3-shallow");

    // Resolve the linked worktree's gitdir  (<primary>/.git/worktrees/wt-check3-shallow).
    // This is what staleness.rs passes into temporal_db_is_stale via resolve_git_dir().
    let linked_gitdir = resolve_git_dir(&worktree)
        .expect("linked worktree must have a resolvable gitdir (test setup invariant)");
    // Verify this is a per-worktree path (sanity: must end with worktrees/<name>)
    assert!(
        linked_gitdir
            .components()
            .any(|c| c.as_os_str() == "worktrees"),
        "test setup: linked gitdir must contain a 'worktrees' component: {linked_gitdir:?}"
    );

    // Set up a temporal.db in a cache dir.
    let cache_dir = _dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    let db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    db.sync(&[], &[], &[], &head, false).unwrap();
    drop(db);
    // Plant is_shallow="1" — simulates a DB built on a shallow clone.
    super::plant_meta_raw(&db_path, rskim_search::META_IS_SHALLOW, "1");

    // Write a non-empty `shallow` file into the PRIMARY .git (the commondir).
    // resolve_common_dir(linked_gitdir) → <primary>/.git, so the probe must
    // look there, NOT inside linked_gitdir itself.
    let primary_git = primary.join(".git");
    let shallow_path = primary_git.join("shallow");
    fs::write(&shallow_path, b"abc1234\n").unwrap();

    // T-8d-1: shallow file present in commondir → NOT stale.
    assert!(
        !temporal_db_is_stale(&cache_dir, &head, Some(&linked_gitdir)),
        "T-8d-1: is_shallow=1 + shallow present in commondir (linked worktree) must NOT \
         be stale — Check 3 must resolve commondir, not probe the per-worktree gitdir \
         (AD-414-14 regression guard)"
    );

    // Remove the shallow file — simulates `git fetch --unshallow`.
    fs::remove_file(&shallow_path).unwrap();

    // T-8d-2: shallow file gone from commondir → stale (shallow→full transition).
    assert!(
        temporal_db_is_stale(&cache_dir, &head, Some(&linked_gitdir)),
        "T-8d-2: is_shallow=1 + shallow absent from commondir (linked worktree) MUST be \
         stale — unshallow must be detected via commondir probe (AD-414-14 regression guard)"
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
    let (refreshed1, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(refreshed1.refreshed(), "first call must build the index");

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
    let (refreshed2, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(
        !refreshed2.refreshed(),
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
    auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    let temporal_db_path = cache_dir.join("temporal.db");
    assert!(
        temporal_db_path.exists(),
        "temporal.db must exist after first call"
    );

    // Plant a stale META_GIT_HEAD to simulate the HEAD-divergent case.
    // Raw SQL — set_meta guards version-attestation keys (AD-408-3). The DB
    // already exists (built by the earlier auto_refresh call), so the `meta`
    // table is present.
    let planted_head = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    super::plant_meta_raw(&temporal_db_path, rskim_search::META_GIT_HEAD, planted_head);

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
    let (refreshed2, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(
        !refreshed2.refreshed(),
        "lexical must NOT be rebuilt on Current branch"
    );

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
    auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

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
    let (refreshed2, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(
        !refreshed2.refreshed(),
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

        let result1 = auto_refresh_if_stale(
            dir.path(),
            &cache_dir,
            &analytics,
            ReanchorPolicy::Refuse,
            None,
        );
        assert!(result1.is_ok(), "Case A: first call must return Ok");
        let (refreshed1, _, _) = result1.unwrap();
        assert!(
            !refreshed1.refreshed(),
            "Case A: lexical must not be rebuilt (Current)"
        );

        let temporal_db_path = cache_dir.join("temporal.db");
        let exists_after_first = temporal_db_path.exists();

        let result2 = auto_refresh_if_stale(
            dir.path(),
            &cache_dir,
            &analytics,
            ReanchorPolicy::Refuse,
            None,
        );
        assert!(result2.is_ok(), "Case A: second call must return Ok");
        let (refreshed2, _, _) = result2.unwrap();
        assert!(
            !refreshed2.refreshed(),
            "Case A: second call must not rebuild lexical"
        );

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
        let (outcome1, _, _) = auto_refresh_if_stale(
            dir.path(),
            &cache_dir,
            &analytics,
            ReanchorPolicy::Refuse,
            None,
        )
        .unwrap();
        assert!(
            outcome1.is_first_build(),
            "Case B: first call must build index (NoIndex → FirstBuild)"
        );

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
        let (outcome2, _, _) = auto_refresh_if_stale(
            dir.path(),
            &cache_dir,
            &analytics,
            ReanchorPolicy::Refuse,
            None,
        )
        .unwrap();
        assert!(
            !outcome2.refreshed(),
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
    let (refreshed, manifest, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    assert!(
        refreshed.refreshed(),
        "in-place edit must trigger a rebuild (AC1/AC5)"
    );
    assert!(
        !refreshed.is_first_build(),
        "working-tree edit is an incremental rebuild — outcome must be Incremental, not FirstBuild (AC-405-8)"
    );
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
    let (refreshed, _manifest, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(
        refreshed.refreshed(),
        "a new working-tree file must trigger a rebuild (AC2)"
    );
    assert!(
        !refreshed.is_first_build(),
        "new working-tree file is an incremental rebuild — outcome must be Incremental, not FirstBuild (AC-405-8)"
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
    let (refreshed, _manifest, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(
        refreshed.refreshed(),
        "delete+add must trigger a rebuild (AC3)"
    );
    assert!(
        !refreshed.is_first_build(),
        "delete+add is an incremental rebuild — outcome must be Incremental, not FirstBuild (AC-405-8)"
    );

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
    let (r1, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    let (r2, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    assert!(
        !r1.refreshed(),
        "clean tree: first call must not rebuild (AC7)"
    );
    assert!(
        !r2.refreshed(),
        "clean tree: second call must not rebuild (AC7)"
    );

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
    let (refreshed, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(
        !refreshed.refreshed(),
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
    let (refreshed, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(
        refreshed.refreshed(),
        "size change with preserved mtime MUST reindex (size comparison, AC9a)"
    );
    assert!(
        !refreshed.is_first_build(),
        "size-change reindex is an incremental rebuild — outcome must be Incremental, not FirstBuild (AC-405-8)"
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
    let (refreshed, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(
        refreshed.refreshed(),
        "non-git working-tree change MUST reindex (AD-379-3, AC12)"
    );
    assert!(
        !refreshed.is_first_build(),
        "non-git working-tree change is an incremental rebuild — outcome must be Incremental, not FirstBuild (AC-405-8)"
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
    let (refreshed, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(
        refreshed.refreshed(),
        "corrupt-HEAD + working-tree edit MUST reindex (AD-379-6, AC13)"
    );
    assert!(
        !refreshed.is_first_build(),
        "corrupt-HEAD reindex is an incremental rebuild — outcome must be Incremental, not FirstBuild (AC-405-8)"
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
    let (r1, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(r1.refreshed(), "first call must rebuild on the edit (AC14)");
    assert!(
        !r1.is_first_build(),
        "working-tree change rebuild is incremental — must be Incremental, not FirstBuild (AC-405-8)"
    );
    let mtime_after_first = fs::metadata(&manifest_path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Second call: index is now Current (manifest carries fresh mtime+size).
    let (r2, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(
        !r2.refreshed(),
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
    let (refreshed, _, _) = auto_refresh_if_stale(
        dir.path(),
        &cache_dir,
        &analytics,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(
        refreshed.refreshed(),
        "pre-#379 manifest (mtime/size None) must self-heal via one rebuild (AC10)"
    );
    assert!(
        !refreshed.is_first_build(),
        "pre-#379 self-heal is an incremental rebuild — outcome must be Incremental, not FirstBuild (AC-405-8)"
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
    let (a, _) = walk_metadata(dir.path(), cap, None).unwrap();
    let (b, _) = walk_metadata(dir.path(), cap, None).unwrap();

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

// ============================================================================
// #413 — linked worktree HEAD resolution and temporal data
// ============================================================================

/// AC1 / S1 — Linked-worktree HEAD resolves via the commondir loose ref.
///
/// Preconditions: the per-worktree gitdir has no local `refs/heads/<branch>` file
/// and no `packed-refs` (the SHA lives only in the common dir's `refs/heads/<branch>`).
/// Discriminating: pre-fix this returned `None`; post-fix it returns the exact GT SHA.
#[test]
fn test_read_git_head_resolves_commondir_loose_ref_in_real_worktree() {
    let (_dir, _primary, worktree, gt) = worktree_fixture("b1");
    assert_eq!(gt.len(), 40, "GT must be a 40-char SHA");

    // Verify worktree HEAD equals the primary HEAD at creation (fixture invariant).
    let wt_sha = read_git_head(&worktree).expect("worktree HEAD must be readable");
    assert_eq!(
        wt_sha, gt,
        "worktree HEAD must equal primary HEAD at branch creation"
    );

    // Precondition: per-worktree refs/ must be empty and packed-refs absent.
    // AD-413-12: delegate to resolve_git_dir — the single `gitdir:` parser.
    let wt_gitdir = resolve_git_dir(&worktree).unwrap();
    let wt_refs = wt_gitdir.join("refs").join("heads");
    if wt_refs.exists() {
        let entries: Vec<_> = fs::read_dir(&wt_refs).unwrap().collect();
        assert!(
            entries.is_empty(),
            "precondition: wt refs/heads must be empty"
        );
    }
    assert!(
        !wt_gitdir.join("packed-refs").exists(),
        "precondition: wt packed-refs absent"
    );

    // The key assertion: read_git_head resolves via commondir.
    let result = read_git_head(&worktree);
    assert_eq!(
        result.as_deref(),
        Some(gt.as_str()),
        "linked worktree HEAD must resolve to the primary branch SHA via commondir"
    );
}

/// AC2 / S2 — After `git pack-refs --all`, HEAD still resolves via commondir packed-refs.
///
/// Preconditions (asserted): the commondir loose ref for `b1` is absent after packing;
/// `packed-refs` in the commondir contains `refs/heads/b1`.
#[test]
fn test_read_git_head_resolves_commondir_packed_refs_after_pack_refs() {
    use std::process::Command;

    let (_dir, primary, worktree, gt) = worktree_fixture("b1");

    // Pack all refs so the loose ref disappears.
    Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(["pack-refs", "--all"])
        .current_dir(&primary)
        .output()
        .expect("git pack-refs");

    // Precondition: loose ref for b1 must be absent in the commondir.
    let primary_git = primary.join(".git");
    let loose_b1 = primary_git.join("refs").join("heads").join("b1");
    assert!(
        !loose_b1.exists(),
        "precondition: loose b1 must be absent after pack-refs --all"
    );

    // Precondition: packed-refs must contain b1.
    let packed = fs::read_to_string(primary_git.join("packed-refs")).unwrap_or_default();
    assert!(
        packed.contains("refs/heads/b1"),
        "precondition: packed-refs must contain refs/heads/b1; got: {packed}"
    );

    // Key assertion: still resolves.
    let result = read_git_head(&worktree);
    assert_eq!(
        result.as_deref(),
        Some(gt.as_str()),
        "packed-refs path must resolve linked worktree HEAD after pack-refs --all"
    );
}

/// AC3 / S3 — Resolves when `commondir` contains an ABSOLUTE path.
///
/// Precondition (asserted): the commondir file starts with `/`.
#[test]
fn test_read_git_head_handles_absolute_commondir() {
    let (_dir, primary, worktree, gt) = worktree_fixture("b1");

    // Rewrite the commondir to the canonical absolute path.
    // AD-413-12: delegate to resolve_git_dir — the single `gitdir:` parser.
    let wt_gitdir = resolve_git_dir(&worktree).unwrap();
    let abs_common = primary.join(".git").canonicalize().unwrap();
    assert!(
        abs_common.is_absolute(),
        "precondition: abs_common must be absolute"
    );
    fs::write(
        wt_gitdir.join("commondir"),
        format!("{}\n", abs_common.display()),
    )
    .unwrap();

    let result = read_git_head(&worktree);
    assert_eq!(
        result.as_deref(),
        Some(gt.as_str()),
        "absolute commondir path must resolve linked worktree HEAD"
    );
}

/// AC4 / S4 — Slashed branch names (`wave/probe-413`) resolve, both loose and packed.
#[test]
fn test_read_git_head_resolves_slashed_branch_loose_and_packed() {
    use std::process::Command;

    let dir = tempdir().unwrap();
    let primary = dir.path().join("primary");
    let worktree_loose = dir.path().join("wt-loose");
    let worktree_packed = dir.path().join("wt-packed");
    fs::create_dir_all(&primary).unwrap();

    let gt = create_real_git_repo(&primary, &[("init", &[("a.rs", "fn a(){}\n")])]);

    // Sub-case 1: loose ref (wave/probe-413 in refs/heads/wave/probe-413).
    let _ = create_real_git_worktree(&primary, &worktree_loose, "wave/probe-413");
    let result_loose = read_git_head(&worktree_loose);
    assert_eq!(
        result_loose.as_deref(),
        Some(gt.as_str()),
        "S4 loose: slashed branch must resolve via commondir loose ref"
    );

    // Sub-case 2: packed-refs (after git pack-refs --all).
    let _ = create_real_git_worktree(&primary, &worktree_packed, "wave/probe-413-packed");
    Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(["pack-refs", "--all"])
        .current_dir(&primary)
        .output()
        .expect("git pack-refs");

    // Precondition: loose ref must be absent.
    let loose_wave = primary.join(".git").join("refs").join("heads").join("wave");
    if loose_wave.exists() {
        let probe_file = loose_wave.join("probe-413-packed");
        assert!(
            !probe_file.exists(),
            "precondition: loose wave/probe-413-packed must be absent after pack-refs --all"
        );
    }

    let result_packed = read_git_head(&worktree_packed);
    assert_eq!(
        result_packed.as_deref(),
        Some(gt.as_str()),
        "S4 packed: slashed branch must resolve via commondir packed-refs"
    );
}

/// AC6 / S6 — Per-worktree ref namespaces (`refs/bisect/`, `refs/worktree/`,
/// `refs/rewritten/`) are never redirected to the commondir.
///
/// Case 1: per-worktree file present → resolved SHA is shaA (from the local file), NOT shaB.
/// Case 2: per-worktree file removed → resolved SHA is None, NOT shaB from commondir.
#[test]
fn test_read_git_head_per_worktree_ref_namespace_is_not_redirected() {
    let (_dir, primary, worktree, _gt) = worktree_fixture("b1");

    // Locate the per-worktree gitdir.
    // AD-413-12: delegate to resolve_git_dir — the single `gitdir:` parser.
    let wt_gitdir = resolve_git_dir(&worktree).unwrap();

    let sha_a = "a".repeat(40);
    let sha_b = "b".repeat(40);

    for namespace in &["refs/bisect", "refs/worktree", "refs/rewritten"] {
        let per_wt_dir = wt_gitdir.join(namespace);
        fs::create_dir_all(&per_wt_dir).unwrap();
        let per_wt_ref = per_wt_dir.join("testref");
        let primary_dir = primary.join(".git").join(namespace);
        fs::create_dir_all(&primary_dir).unwrap();
        let primary_ref = primary_dir.join("testref");

        fs::write(&per_wt_ref, format!("{sha_a}\n")).unwrap();
        fs::write(&primary_ref, format!("{sha_b}\n")).unwrap();

        // Point HEAD at a per-worktree ref.
        let head_ref = format!("{namespace}/testref");
        fs::write(wt_gitdir.join("HEAD"), format!("ref: {head_ref}\n")).unwrap();

        // Case 1: per-worktree file present — must return shaA.
        let result = read_git_head(&worktree);
        assert_eq!(
            result.as_deref(),
            Some(sha_a.as_str()),
            "{namespace}: case 1 must return shaA from the per-worktree ref, not shaB"
        );

        // Case 2: per-worktree file removed — must return None (NOT shaB).
        fs::remove_file(&per_wt_ref).unwrap();
        let result2 = read_git_head(&worktree);
        assert!(
            result2.is_none(),
            "{namespace}: case 2 (per-wt ref removed) must return None, not shaB; got {result2:?}"
        );
        assert_ne!(
            result2.as_deref(),
            Some(sha_b.as_str()),
            "{namespace}: shaB from commondir must never be returned for a per-worktree namespace"
        );

        // Clean up for the next namespace iteration.
        fs::remove_file(&primary_ref).unwrap();
    }
}

/// AC7 / S7 — Detached HEAD in a linked worktree still resolves (raw-SHA branch).
#[test]
fn test_read_git_head_detached_head_in_linked_worktree_still_resolves() {
    use std::process::Command;

    let (_dir, _primary, worktree, gt) = worktree_fixture("b1");

    // Detach HEAD in the linked worktree.
    Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(["checkout", "--detach"])
        .current_dir(&worktree)
        .output()
        .expect("git checkout --detach");

    let result = read_git_head(&worktree);
    assert_eq!(
        result.as_deref(),
        Some(gt.as_str()),
        "detached HEAD in linked worktree must still resolve to the commit SHA"
    );
}

/// AC8 (a) / S8a — `commondir` points at a directory with a valid SHA file but NO `HEAD`:
/// that SHA must NOT be returned.
#[test]
fn test_read_git_head_rejects_commondir_pointing_at_non_git_dir() {
    let (dir, _primary, worktree, _gt) = worktree_fixture("b1");
    let fake_common = dir.path().join("fake_common");

    // AD-413-12: delegate to resolve_git_dir — the single `gitdir:` parser.
    let wt_gitdir = resolve_git_dir(&worktree).unwrap();

    // Create a fake commondir with a valid-looking SHA file but NO HEAD.
    let planted_sha = "c".repeat(40);
    let refs_heads = fake_common.join("refs").join("heads");
    fs::create_dir_all(&refs_heads).unwrap();
    fs::write(refs_heads.join("b1"), format!("{planted_sha}\n")).unwrap();
    // Deliberately NO HEAD file.

    // Rewrite commondir to point at this fake directory.
    fs::write(
        wt_gitdir.join("commondir"),
        format!("{}\n", fake_common.display()),
    )
    .unwrap();

    let result = read_git_head(&worktree);
    assert!(
        result.is_none(),
        "commondir without HEAD must return None; got {result:?}"
    );
    assert_ne!(
        result.as_deref(),
        Some(planted_sha.as_str()),
        "the planted SHA from a commondir without HEAD must never be returned"
    );
}

/// AC8 (b) / S8b — `commondir` points at a non-existent path (dangling): no HEAD.
#[test]
fn test_read_git_head_rejects_dangling_commondir() {
    let (dir, _primary, worktree, _gt) = worktree_fixture("b1");

    // AD-413-12: delegate to resolve_git_dir — the single `gitdir:` parser.
    let wt_gitdir = resolve_git_dir(&worktree).unwrap();

    // Dangling path — does not exist.
    let dangling = dir.path().join("does-not-exist");
    fs::write(
        wt_gitdir.join("commondir"),
        format!("{}\n", dangling.display()),
    )
    .unwrap();

    let result = read_git_head(&worktree);
    assert!(
        result.is_none(),
        "dangling commondir must return None; got {result:?}"
    );
}

/// AC8 (c) / S8c — `commondir` contains 8 KiB of newline-free junk: no HEAD,
/// no panic, no OOM.
#[test]
fn test_read_git_head_rejects_oversized_commondir() {
    let (_dir, _primary, worktree, _gt) = worktree_fixture("b1");

    // AD-413-12: delegate to resolve_git_dir — the single `gitdir:` parser.
    let wt_gitdir = resolve_git_dir(&worktree).unwrap();

    // 8192 bytes of 'x' with no newline — exceeds the 4096-byte bounded read.
    let junk = "x".repeat(8192);
    fs::write(wt_gitdir.join("commondir"), junk.as_bytes()).unwrap();

    let result = read_git_head(&worktree);
    assert!(
        result.is_none(),
        "oversized junk commondir must return None; got {result:?}"
    );
}

/// AC8 (d) / S8d — `commondir` is a regular file rather than a directory: no HEAD.
#[test]
fn test_read_git_head_rejects_commondir_is_a_file() {
    let (dir, _primary, worktree, _gt) = worktree_fixture("b1");
    let file_common = dir.path().join("file_that_is_not_a_dir");

    // AD-413-12: delegate to resolve_git_dir — the single `gitdir:` parser.
    let wt_gitdir = resolve_git_dir(&worktree).unwrap();

    // commondir points at a regular file (not a directory).
    fs::write(&file_common, b"this is a file, not a dir\n").unwrap();
    fs::write(
        wt_gitdir.join("commondir"),
        format!("{}\n", file_common.display()),
    )
    .unwrap();

    let result = read_git_head(&worktree);
    assert!(
        result.is_none(),
        "commondir pointing at a regular file must return None; got {result:?}"
    );
}

/// AC11 / S11 — `check_staleness` reports `HeadChanged` after a commit in a linked worktree.
///
/// Pre-fix: the verdict was `WorkingTreeChanged` (the new file appeared as an untracked
/// addition before the commit was recognized). Post-fix: `HeadChanged`.
#[test]
fn test_check_staleness_reports_head_changed_in_linked_worktree() {
    use std::process::Command;

    let (dir, _primary, worktree, old_head) = worktree_fixture("b1");
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // Write a manifest with old_head so check_staleness has something to compare.
    write_manifest_with_head(&worktree, &cache_dir, Some(&old_head));
    write_lexical_index_stub(&cache_dir);
    write_ast_index_stub(&cache_dir);

    // Make a commit in the linked worktree.
    fs::write(worktree.join("c.rs"), "fn c(){}\n").unwrap();
    Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(["add", "c.rs"])
        .current_dir(&worktree)
        .output()
        .expect("git add c.rs");
    Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args([
            "-c",
            "user.email=t@e.com",
            "-c",
            "user.name=T",
            "commit",
            "-m",
            "add c",
        ])
        .current_dir(&worktree)
        .output()
        .expect("git commit");

    let new_head = {
        let out = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(["rev-parse", "HEAD"])
            .current_dir(&worktree)
            .output()
            .expect("git rev-parse HEAD");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert_ne!(old_head, new_head, "HEAD must advance after commit");

    let (verdict, _manifest) = check_staleness(&cache_dir, &worktree);
    assert!(
        matches!(&verdict, StalenessCheck::HeadChanged { stored, current }
            if stored == &old_head && current == &new_head),
        "AC11: verdict must be HeadChanged {{ stored: old, current: new }}; got {verdict:?}"
    );
}

/// AC12 / S12 — After AC11's commit-and-update, `temporal.db` `meta.git_head` equals the new GT.
#[test]
fn test_temporal_db_resyncs_when_worktree_branch_advances() {
    use std::process::Command;

    let (dir, _primary, worktree, old_head) = worktree_fixture("b1");
    // First build.
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    auto_refresh_if_stale(
        &worktree,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    let temporal_db_path = cache_dir.join("temporal.db");
    let db = rskim_search::TemporalDb::open(&temporal_db_path).unwrap();
    let head_before = db
        .get_meta(rskim_search::META_GIT_HEAD)
        .unwrap()
        .unwrap_or_default();
    drop(db);
    assert_eq!(
        head_before, old_head,
        "meta.git_head must equal old HEAD after first build"
    );

    // Make a commit in the worktree.
    fs::write(worktree.join("c.rs"), "fn c(){}\n").unwrap();
    Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(["add", "c.rs"])
        .current_dir(&worktree)
        .output()
        .expect("git add c.rs");
    Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args([
            "-c",
            "user.email=t@e.com",
            "-c",
            "user.name=T",
            "commit",
            "-m",
            "add c",
        ])
        .current_dir(&worktree)
        .output()
        .expect("git commit");
    let new_head = {
        let out = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(["rev-parse", "HEAD"])
            .current_dir(&worktree)
            .output()
            .expect("git rev-parse");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert_ne!(old_head, new_head, "HEAD must advance after commit");

    // Trigger update.
    auto_refresh_if_stale(
        &worktree,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    let db2 = rskim_search::TemporalDb::open(&temporal_db_path).unwrap();
    let head_after = db2
        .get_meta(rskim_search::META_GIT_HEAD)
        .unwrap()
        .unwrap_or_default();
    assert_eq!(
        head_after, new_head,
        "AC12: meta.git_head must equal new GT after --update"
    );
    assert_ne!(
        head_after, head_before,
        "AC12: meta.git_head must have changed"
    );
}

/// AC13 / S13 — A frozen manifest (git_head = None) recovers on the first query.
///
/// Constructed with `write_manifest_with_head(.., None)` on a real linked worktree.
/// The staleness verdict must be `NoStoredHead`; afterwards the manifest's stored HEAD
/// equals GT and `meta.git_head` equals GT.
#[test]
fn test_frozen_manifest_without_head_recovers_on_next_query_in_worktree() {
    let (dir, _primary, worktree, gt) = worktree_fixture("b1");
    // Set up with a real index + temporal.db, then wipe the manifest's git_head.
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    auto_refresh_if_stale(
        &worktree,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    write_lexical_index_stub(&cache_dir);
    write_ast_index_stub(&cache_dir);
    write_manifest_with_head(&worktree, &cache_dir, None);

    // Staleness check must see NoStoredHead.
    let (verdict, _) = check_staleness(&cache_dir, &worktree);
    assert!(
        matches!(verdict, StalenessCheck::NoStoredHead),
        "AC13: verdict must be NoStoredHead when manifest has no git_head; got {verdict:?}"
    );

    // One auto-refresh must recover the stored HEAD.
    auto_refresh_if_stale(
        &worktree,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    // Verify the manifest now has the GT stored HEAD.
    let (verdict2, manifest) = check_staleness(&cache_dir, &worktree);
    assert!(
        matches!(verdict2, StalenessCheck::Current),
        "AC13: after recovery, check_staleness must be Current; got {verdict2:?}"
    );
    let stored = manifest.unwrap().stored_git_head().map(str::to_string);
    assert_eq!(
        stored.as_deref(),
        Some(gt.as_str()),
        "AC13: stored git_head must equal GT after recovery"
    );

    // Verify temporal.db also has the GT HEAD.
    let db = rskim_search::TemporalDb::open(&cache_dir.join("temporal.db")).unwrap();
    let meta_head = db
        .get_meta(rskim_search::META_GIT_HEAD)
        .unwrap()
        .unwrap_or_default();
    assert_eq!(
        meta_head, gt,
        "AC13: meta.git_head in temporal.db must equal GT after recovery"
    );
}

/// AC14 / S14 — A divergent `meta.git_head` self-heals on the next query.
///
/// Plants `deadbeef...` as `meta.git_head` and verifies that one auto-refresh
/// corrects it to the GT SHA.
#[test]
fn test_temporal_db_with_divergent_recorded_head_self_heals_in_worktree() {
    let (dir, _primary, worktree, gt) = worktree_fixture("b1");
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    auto_refresh_if_stale(
        &worktree,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    let temporal_db_path = cache_dir.join("temporal.db");
    let stale_head = "deadbeef".repeat(5); // 40-char pseudo-SHA
    super::plant_meta_raw(&temporal_db_path, rskim_search::META_GIT_HEAD, &stale_head);

    // Confirm the stale value was planted.
    {
        let db = rskim_search::TemporalDb::open(&temporal_db_path).unwrap();
        let planted = db
            .get_meta(rskim_search::META_GIT_HEAD)
            .unwrap()
            .unwrap_or_default();
        assert_eq!(
            planted, stale_head,
            "precondition: planted stale head must match"
        );
    }

    // One query triggers the AD-TMP-2 self-heal.
    auto_refresh_if_stale(
        &worktree,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    let db2 = rskim_search::TemporalDb::open(&temporal_db_path).unwrap();
    let healed = db2
        .get_meta(rskim_search::META_GIT_HEAD)
        .unwrap()
        .unwrap_or_default();
    assert_eq!(
        healed, gt,
        "AC14: meta.git_head must equal GT after divergent-head self-heal"
    );
}

/// AC17 / S17 — Three HEAD states are distinguishable.
///
/// Tests all six representative roots described in S17:
/// (1) bare tempdir — not_a_repo
/// (2) `mkdir .git` empty — not_a_repo (a .git dir with no HEAD is not a repo)
/// (3) `.git` file → nonexistent gitdir — not_a_repo
/// (4) `.git/HEAD` = `garbage` — unresolved
/// (5) real linked worktree — resolved
/// (6) real subdirectory of a repo — resolved (via ancestor walk)
///
/// Hermeticity note: cases (1)–(4) are created under a fresh tempdir that has no
/// ancestor repository.  The NOGIT precondition is asserted inside the test.
#[test]
fn test_git_head_state_distinguishes_not_a_repo_from_unresolved() {
    let dir = tempdir().unwrap();

    // NOGIT precondition: no ancestor of dir.path() must contain .git.
    {
        let mut d = dir.path().canonicalize().unwrap();
        loop {
            assert!(
                !d.join(".git").exists(),
                "NOGIT precondition failed: ancestor {d:?} contains .git"
            );
            let parent = match d.parent() {
                Some(p) if p != d => p.to_path_buf(),
                _ => break,
            };
            d = parent;
        }
    }

    // (1) bare tempdir — not_a_repo
    let bare = dir.path().join("bare");
    fs::create_dir_all(&bare).unwrap();
    assert_eq!(
        git_head_state(&bare),
        HeadState::NotARepo,
        "(1) bare dir: expected NotARepo"
    );

    // (2) mkdir .git (no HEAD) — not_a_repo
    let empty_git = dir.path().join("empty_git");
    fs::create_dir_all(empty_git.join(".git")).unwrap();
    assert_eq!(
        git_head_state(&empty_git),
        HeadState::NotARepo,
        "(2) .git dir with no HEAD: expected NotARepo"
    );

    // (3) .git file pointing at nonexistent gitdir — not_a_repo
    let gitfile = dir.path().join("gitfile_root");
    fs::create_dir_all(&gitfile).unwrap();
    fs::write(gitfile.join(".git"), "gitdir: /does/not/exist\n").unwrap();
    assert_eq!(
        git_head_state(&gitfile),
        HeadState::NotARepo,
        "(3) .git file pointing at nonexistent path: expected NotARepo"
    );

    // (4) .git/HEAD = garbage — unresolved
    let garbage = dir.path().join("garbage_head");
    let garbage_git = garbage.join(".git");
    fs::create_dir_all(&garbage_git).unwrap();
    fs::write(garbage_git.join("HEAD"), "this is not a valid HEAD\n").unwrap();
    assert_eq!(
        git_head_state(&garbage),
        HeadState::Unresolved,
        "(4) garbage HEAD: expected Unresolved"
    );

    // (5) real linked worktree — resolved
    let primary5 = dir.path().join("primary5");
    let worktree5 = dir.path().join("wt5");
    fs::create_dir_all(&primary5).unwrap();
    let gt5 = create_real_git_repo(&primary5, &[("init", &[("a.rs", "fn a(){}\n")])]);
    create_real_git_worktree(&primary5, &worktree5, "b1");

    assert_eq!(
        git_head_state(&worktree5),
        HeadState::Resolved(gt5.clone()),
        "(5) linked worktree: expected Resolved"
    );

    // (6) subdirectory of a repo — resolved (OD-3)
    let sub = primary5.join("sub");
    fs::create_dir_all(&sub).unwrap();
    // sub has no .git of its own → adopt primary5's HEAD.
    assert_eq!(
        git_head_state(&sub),
        HeadState::Resolved(gt5),
        "(6) subdirectory of repo: expected Resolved via ancestor walk"
    );
}

/// AC29 / S29 — All 17 AD-413-* markers are present in their documented source files.
///
/// Each marker is anchored to the file the plan names for it:
/// - AD-413-1..7  in gitdir.rs (git-plumbing module extracted from staleness.rs)
/// - AD-413-9     in temporal_state.rs (warn_if_temporal_unverifiable advisory)
/// - AD-413-10, AD-413-11  in gitdir.rs (module doc and fn doc)
/// - AD-413-8, AD-413-13  in mod.rs  (AD-413-8: error-message format; AD-413-13: provenance)
/// - AD-413-12  in walk.rs
/// - AD-413-14  in staleness.rs or gitdir.rs
/// - AD-413-15  in staleness.rs, gitdir.rs, or hooks.rs
/// - AD-413-16  in staleness.rs or temporal_build.rs
/// - AD-413-17  in temporal_build.rs
///
/// The test asserts that the exact ID string appears in AT LEAST ONE of the files
/// the plan lists for it.  A missing marker means the design decision anchor has
/// drifted and must be restored before merge.
#[test]
fn test_ac_413_ad_series_comments_present() {
    let staleness_src = include_str!("staleness.rs");
    let gitdir_src = include_str!("gitdir.rs");
    let temporal_state_src = include_str!("temporal_state.rs");
    let mod_src = include_str!("mod.rs");
    let walk_src = include_str!("walk.rs");
    let hooks_src = include_str!("hooks.rs");
    let temporal_src = include_str!("temporal.rs");
    let temporal_build_src = include_str!("temporal_build.rs");

    // AD-413-1..7 are in gitdir.rs (the git-plumbing module extracted from
    // staleness.rs during #413).  AD-413-9 is in temporal_state.rs.
    // AD-413-8 is in mod.rs (error-message format spec — AC18(a)/AC33(c)).
    for n in [1u8, 2, 3, 4, 5, 6, 7, 9] {
        let marker = format!("AD-413-{n}");
        assert!(
            staleness_src.contains(&marker)
                || gitdir_src.contains(&marker)
                || temporal_state_src.contains(&marker),
            "AD-413-{n} must be present in staleness.rs, gitdir.rs, or temporal_state.rs"
        );
    }
    assert!(mod_src.contains("AD-413-8"), "AD-413-8 must be in mod.rs");

    // AD-413-10 and AD-413-11 are in gitdir.rs (module doc and fn doc).
    assert!(
        gitdir_src.contains("AD-413-10"),
        "AD-413-10 must be in gitdir.rs"
    );
    assert!(
        gitdir_src.contains("AD-413-11"),
        "AD-413-11 must be in gitdir.rs"
    );

    // AD-413-12 is in walk.rs
    assert!(
        walk_src.contains("AD-413-12"),
        "AD-413-12 must be in walk.rs"
    );

    // AD-413-13 is in mod.rs
    assert!(mod_src.contains("AD-413-13"), "AD-413-13 must be in mod.rs");

    // AD-413-14 is in staleness.rs
    assert!(
        staleness_src.contains("AD-413-14"),
        "AD-413-14 must be in staleness.rs"
    );

    // AD-413-15 is in staleness.rs and hooks.rs
    assert!(
        staleness_src.contains("AD-413-15") || hooks_src.contains("AD-413-15"),
        "AD-413-15 must be in staleness.rs or hooks.rs"
    );

    // AD-413-16 is in staleness.rs and temporal_build.rs
    assert!(
        staleness_src.contains("AD-413-16") || temporal_build_src.contains("AD-413-16"),
        "AD-413-16 must be in staleness.rs or temporal_build.rs"
    );

    // AD-413-17 is in temporal_build.rs
    assert!(
        temporal_build_src.contains("AD-413-17"),
        "AD-413-17 must be in temporal_build.rs"
    );

    // AC29: build_stats_json must carry the AD-413-13 provenance sentence explaining that
    // `git_head` is the manifest's stored HEAD ("HEAD-at-last-build") while `git_head_state`
    // is live, so the pair can legitimately diverge.
    // The assertion checks for the distinguishing clause, not just the AD marker, so it
    // cannot be vacuously satisfied by an AD-413-13 occurrence elsewhere in the file.
    assert!(
        mod_src.contains("HEAD-at-last-build"),
        "AC29: build_stats_json rustdoc must contain 'HEAD-at-last-build' \
         (the AD-413-13 provenance sentence explaining git_head vs git_head_state divergence)"
    );

    // Belt-and-suspenders: every file also must not have any bare #NEW markers
    // (ADR-004: no phantom tickets; real numbers only).
    for (name, src) in &[
        ("staleness.rs", staleness_src),
        ("gitdir.rs", gitdir_src),
        ("temporal_state.rs", temporal_state_src),
        ("mod.rs", mod_src),
        ("walk.rs", walk_src),
        ("hooks.rs", hooks_src),
        ("temporal.rs", temporal_src),
        ("temporal_build.rs", temporal_build_src),
    ] {
        assert!(
            !src.contains("#NEW"),
            "ADR-004: {name} must not contain #NEW placeholder tickets"
        );
    }
}

/// AC17 supplement / S17 (6) — `resolve_repo_toplevel` is live for subdirectory roots.
///
/// A directory without its own `.git` that sits inside a real git repo returns
/// `HeadState::Resolved` — the ancestor walk adopts the nearest enclosing repo.
#[test]
fn test_git_head_state_resolves_subdirectory_root() {
    let dir = tempdir().unwrap();
    let primary = dir.path().join("primary");
    fs::create_dir_all(&primary).unwrap();

    let gt = create_real_git_repo(&primary, &[("init", &[("src/lib.rs", "fn f(){}\n")])]);

    // src/ exists but has no .git — must resolve via ancestor walk.
    let sub = primary.join("src");
    assert!(!sub.join(".git").exists(), "precondition: src/ has no .git");

    let state = git_head_state(&sub);
    assert_eq!(
        state,
        HeadState::Resolved(gt),
        "subdirectory root must resolve to the enclosing repo's HEAD"
    );
}

/// AC10 / S10 — An escape-derived poisoned stored HEAD with an UNRESOLVABLE live HEAD
/// is inert: verdict is `Current` on every call and `index.skfiles` mtime is stable.
///
/// The criterion's scenario:
/// - Stored HEAD = "2222…40" (the escape-derived SHA from AC9's fixture)
/// - Live HEAD = None (`read_git_head` returns None because the ref escapes the git dir)
/// - Expected: `(Some(stored), None)` arm → `Current` (not HeadChanged, not NoStoredHead)
/// - Expected: `git_head_state` == Unresolved (HEAD exists but does not resolve)
/// - Expected: mtime of `index.skfiles` unchanged across two consecutive `auto_refresh_if_stale` calls
///
/// Discriminating: pre-fix, live HEAD resolved to the escape SHA → HeadChanged would fire and
/// (a) would assert `git_head_state` == "resolved", not "unresolved".
/// On a *wrong fix* that forces a rebuild whenever live HEAD is None, the mtime assertions fail.
///
/// Note: the ORIGINAL test (deleted here) tested a linked worktree with a RESOLVABLE live HEAD
/// and asserted `HeadChanged` fires — that scenario belongs to AC11 (which is kept separately).
/// AC10 is specifically about "already-poisoned stored HEAD + unresolvable live HEAD is INERT".
#[test]
fn test_git_head_state_poisoned_stored_head_is_inert_no_rebuild_loop() {
    // Reuse the AC9 fixture topology: a repo whose HEAD = "ref: refs/../../../outside-sha"
    // so the escape guard rejects it and read_git_head returns None (live HEAD unresolvable).
    let parent = tempdir().unwrap();
    let project_root = parent.path().join("repo");
    let cache_dir = parent.path().join("cache");
    fs::create_dir_all(&project_root).unwrap();
    fs::create_dir_all(&cache_dir).unwrap();

    // Build a real git repo with a.rs committed, then build a real lexical+AST index so
    // the manifest reflects the on-disk tree before switching HEAD to the escape ref.
    // Using write_manifest_with_head+stubs would produce an empty manifest, causing
    // scan_working_tree to see a.rs as "added" → dirty → WorkingTreeChanged, masking
    // the no-rebuild-loop contract this test asserts (same root cause as AC16c Fix 1).
    create_real_git_repo(&project_root, &[("init", &[("a.rs", "fn a(){}\n")])]);

    let escape_sha = "2".repeat(40);
    let escape_target = parent.path().join("outside-sha");
    fs::write(&escape_target, format!("{escape_sha}\n")).unwrap();
    let ref_path = "refs/../../../outside-sha";

    // Build the index while HEAD is still valid so the manifest records a.rs.
    build_index_in(&project_root, &cache_dir);

    // Patch the manifest's git_head to the poisoned escape-derived SHA.
    // This simulates the state S10 reuses: S9 wrote 2222… into the manifest when the
    // escape guard first fired; we reproduce that stored value without disturbing the
    // file entries that make the working-tree scan come back clean.
    let poisoned = escape_sha.clone();
    {
        use crate::cmd::search::manifest::FileManifest;
        let mut manifest = FileManifest::load(project_root.clone(), cache_dir.clone())
            .expect("manifest must load after build_index_in");
        manifest.set_git_head(Some(poisoned.clone()));
        manifest.save().unwrap();
    }

    // Now switch HEAD to the escape ref to make read_git_head return None.
    let git_dir = project_root.join(".git");

    // MANDATORY PRECONDITION: the escape target is reachable (pre-fix would resolve it).
    assert!(
        git_dir.join(ref_path).exists(),
        "AC10 precondition: escape target must be reachable so the guard fires post-fix"
    );
    fs::write(git_dir.join("HEAD"), format!("ref: {ref_path}\n")).unwrap();

    // Post-fix: read_git_head must return None (escape guard rejects the ref path).
    assert!(
        read_git_head(&project_root).is_none(),
        "AC10 precondition: live HEAD must be None after the escape guard"
    );
    // Post-fix: git_head_state must be Unresolved (HEAD file exists, but ref escapes).
    assert_eq!(
        git_head_state(&project_root),
        HeadState::Unresolved,
        "AC10(a): git_head_state must be Unresolved when HEAD exists but ref escapes"
    );

    // AC10(b): verdict must be Current (not HeadChanged, not NoStoredHead).
    // The `(Some(stored), None)` arm in check_staleness returns current_or_working_tree.
    // With a real index that reflects the on-disk tree, the working-tree scan is clean,
    // so current_or_working_tree returns Current — not WorkingTreeChanged.
    let (verdict1, _) = check_staleness(&cache_dir, &project_root);
    assert!(
        matches!(verdict1, StalenessCheck::Current),
        "AC10(b): (Some(stored), None) arm must be Current — got {verdict1:?}"
    );

    // AC10(c): run auto_refresh_if_stale twice and capture index.skfiles mtime.
    // The working-tree is clean (manifest reflects the on-disk tree), so both calls
    // return early without rebuilding; the second run must be mtime-stable.
    auto_refresh_if_stale(
        &project_root,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    let mtime1 = fs::metadata(cache_dir.join("index.skfiles"))
        .map(|m| m.modified().unwrap())
        .ok();
    auto_refresh_if_stale(
        &project_root,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    let mtime2 = fs::metadata(cache_dir.join("index.skfiles"))
        .map(|m| m.modified().unwrap())
        .ok();
    assert_eq!(
        mtime1, mtime2,
        "AC10(c): index.skfiles mtime must be unchanged on the second run (no rebuild loop)"
    );

    // AC10(d): the planted poisoned SHA must still be the stored HEAD after both refreshes.
    // AC10(c) already proved no rebuild fired (mtime stable).  If a rebuild had fired,
    // `auto_refresh_if_stale` would write the live HEAD (None here) into the manifest,
    // changing the stored value away from `poisoned`.  Asserting equality here is the
    // strong form: it FAILS if a rebuild mistakenly ran, regardless of what value was written.
    use crate::cmd::search::manifest::FileManifest;
    let manifest = FileManifest::load(project_root.clone(), cache_dir.clone())
        .expect("AC10(d): manifest must load after two inert auto_refresh_if_stale calls");
    assert_eq!(
        manifest.stored_git_head(),
        Some(poisoned.as_str()),
        "AC10(d): stored HEAD must still equal the planted poisoned SHA after inert refresh — \
         a rebuild would have overwritten it",
    );
}

// ============================================================================
// #413 / AD-413-16 — the persisted repository anchor (AC16(d), AC24, AC32, AC33(f))
// ============================================================================

/// AC24 / AC32 guard-ordering — a root that owns its own `.git` is `NotAdopted`,
/// and that verdict is reached BEFORE any `temporal.db` read.
///
/// Discriminating: a `git_toplevel` row deliberately planted with a bogus value
/// must NOT change the answer.  An implementation that read the DB before
/// checking `resolve_repo_toplevel` would return `Differs` here and would also
/// pay a SQLite open on every temporal query for every pre-existing user.
#[test]
fn test_temporal_anchor_state_not_adopted_for_root_owning_dot_git() {
    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&cache_dir).unwrap();

    let head = create_real_git_repo(&repo, &[("init", &[("a.rs", "fn a(){}\n")])]);
    // Build real temporal data so a DB exists to be (incorrectly) read.
    build_temporal_for_test(&repo, &cache_dir, &head);
    assert!(
        cache_dir.join("temporal.db").exists(),
        "precondition: temporal.db must exist so the gate ordering is observable"
    );
    // Plant a bogus anchor: a gate-2-first implementation would report Differs.
    super::plant_meta_raw(
        &cache_dir.join("temporal.db"),
        rskim_search::META_GIT_TOPLEVEL,
        "/definitely/not/this/repo",
    );

    assert_eq!(
        super::temporal_anchor_state(&cache_dir, &repo),
        super::AnchorState::NotAdopted,
        "a root with its own .git must be NotAdopted regardless of any planted anchor row"
    );
}

/// AD-413-16 — `Absent` / `Agrees` / `Differs` for an ADOPTED (subdirectory) root.
///
/// Absent is the adopt-and-record case ("built before this key existed") and must
/// never be a refusal; `Agrees` is the steady state; `Differs` is the refusal.
#[test]
fn test_temporal_anchor_state_absent_agrees_differs_for_subdir_root() {
    let dir = tempdir().unwrap();
    let outer = dir.path().join("outer");
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    fs::create_dir_all(outer.join("sub")).unwrap();

    let head = create_real_git_repo(&outer, &[("init", &[("sub/s.rs", "fn s(){}\n")])]);
    let sub = outer.join("sub");
    assert!(!sub.join(".git").exists(), "precondition: sub has no .git");

    // No temporal.db yet → Absent (gate 2).
    assert_eq!(
        super::temporal_anchor_state(&cache_dir, &sub),
        super::AnchorState::Absent,
        "no temporal.db must be Absent (adopt-and-record), never a refusal"
    );

    // Build → the anchor is recorded and agrees.
    build_temporal_for_test(&sub, &cache_dir, &head);
    assert_eq!(
        super::temporal_anchor_state(&cache_dir, &sub),
        super::AnchorState::Agrees,
        "after a build for an adopted root the recorded anchor must agree"
    );

    // Delete the row → Absent again (never a refusal).
    {
        let conn = rusqlite::Connection::open(cache_dir.join("temporal.db")).unwrap();
        conn.execute(
            "DELETE FROM meta WHERE key = ?1",
            rusqlite::params![rskim_search::META_GIT_TOPLEVEL],
        )
        .unwrap();
    }
    assert_eq!(
        super::temporal_anchor_state(&cache_dir, &sub),
        super::AnchorState::Absent,
        "a deleted anchor row must be Absent (adopt-and-record), never a refusal"
    );

    // Plant a foreign toplevel → Differs.
    super::plant_meta_raw(
        &cache_dir.join("temporal.db"),
        rskim_search::META_GIT_TOPLEVEL,
        "/some/other/repo",
    );
    assert!(
        matches!(
            super::temporal_anchor_state(&cache_dir, &sub),
            super::AnchorState::Differs { ref recorded, .. }
                if recorded == &std::path::PathBuf::from("/some/other/repo")
        ),
        "a foreign recorded toplevel must be Differs and carry the recorded value"
    );
}

/// **PF-017 regression** — a PLAIN LEXICAL QUERY interleaved between two
/// repositories must NOT retarget `meta.git_toplevel`.
///
/// This is the exact hole PF-017 names: a changed enclosing repository also
/// changes the adopted HEAD, so `check_staleness` reports `HeadChanged`,
/// `auto_refresh_if_stale` rebuilds, and — without the `allow_reanchor` gate —
/// `record_temporal_anchor` would overwrite the anchor on a query that never
/// asked for temporal data, making the AD-413-16 refusal unreachable forever.
///
/// Sequence: build under repo A → make repo B the nearest enclosing repo →
/// one plain query (`allow_reanchor = false`) → anchor and `temporal.db` MUST be
/// byte-unchanged → then an explicit build arm (`allow_reanchor = true`) DOES
/// re-anchor.
///
/// Discriminating: remove the `allow_reanchor` gate from
/// `try_rebuild_temporal_nonfatal` and the mid-test `Differs` assertion fails
/// because the plain query already rewrote the anchor to repo B.
#[test]
fn test_pf017_plain_query_does_not_retarget_anchor_across_repos() {
    let dir = tempdir().unwrap();
    let outer = dir.path().join("outer");
    let mid = outer.join("mid");
    let sub = mid.join("sub");
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&sub).unwrap();
    fs::create_dir_all(&cache_dir).unwrap();

    // Repo A at `outer`, with the search root two levels down.
    create_real_git_repo(&outer, &[("init", &[("mid/sub/s.rs", "fn s(){}\n")])]);
    let outer_canon = outer.canonicalize().unwrap();

    // Explicit build arm: records the anchor for the adopted root.
    auto_refresh_if_stale(
        &sub,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Allow,
        None,
    )
    .unwrap();
    let db_path = cache_dir.join("temporal.db");
    assert!(
        db_path.exists(),
        "precondition: an adopted subdirectory root must build temporal.db (OD-3/AD-413-14)"
    );
    assert_eq!(
        super::temporal_anchor_state(&cache_dir, &sub),
        super::AnchorState::Agrees,
        "precondition: the anchor must agree immediately after the build"
    );

    // Repo B appears at `mid`, so `sub`'s NEAREST enclosing repo changes.
    // It must have a commit, otherwise HEAD is unborn and the anchor would be
    // preserved for the wrong reason (head = None short-circuit), not by the gate.
    create_real_git_repo(&mid, &[("init", &[("m.rs", "fn m(){}\n")])]);
    let mid_canon = mid.canonicalize().unwrap();
    assert_ne!(outer_canon, mid_canon, "precondition: the two repos differ");
    assert!(
        matches!(
            super::temporal_anchor_state(&cache_dir, &sub),
            super::AnchorState::Differs { ref recorded, ref live }
                if recorded == &outer_canon && live == &mid_canon
        ),
        "precondition: the live toplevel must now differ from the recorded one; got {:?}",
        super::temporal_anchor_state(&cache_dir, &sub)
    );

    let len_before = fs::metadata(&db_path).unwrap().len();

    // THE PF-017 CASE: one plain lexical query (allow_reanchor = false).
    // After AD-413-14-OD, `check_staleness` for an adopted root returns
    // `current_or_working_tree` (working-tree scan) rather than `HeadChanged` even
    // though the HEAD SHA changed — the files under `sub` are unchanged so the scan
    // returns `Current`.  The `Current` self-heal path then calls
    // `try_rebuild_temporal_nonfatal(..., Refuse)`, which detects `AnchorState::Differs`
    // and suppresses the temporal rebuild.  The PF-017 guarantee (anchor unchanged on
    // a plain query) therefore holds through the self-heal arm rather than the
    // post-rebuild arm, but the discriminating property is identical: removing the
    // `allow_reanchor` gate from `try_rebuild_temporal_nonfatal` causes the self-heal
    // to retarget the anchor to repo B, and the mid-test `Differs` assertion fails.
    auto_refresh_if_stale(
        &sub,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    assert!(
        matches!(
            super::temporal_anchor_state(&cache_dir, &sub),
            super::AnchorState::Differs { ref recorded, .. }
                if recorded == &outer_canon
        ),
        "PF-017: a plain lexical query must NOT retarget meta.git_toplevel; got {:?}",
        super::temporal_anchor_state(&cache_dir, &sub)
    );
    assert_eq!(
        fs::metadata(&db_path).unwrap().len(),
        len_before,
        "PF-017: temporal.db must be byte-length-unchanged after a refused plain query"
    );

    // A second plain query must be equally inert (no loop — AC16(d)).
    auto_refresh_if_stale(
        &sub,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();
    assert!(
        matches!(
            super::temporal_anchor_state(&cache_dir, &sub),
            super::AnchorState::Differs { ref recorded, .. }
                if recorded == &outer_canon
        ),
        "PF-017: repeated plain queries must stay inert (no rebuild loop)"
    );

    // The documented escape hatch: an explicit build arm DOES re-anchor.
    auto_refresh_if_stale(
        &sub,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Allow,
        None,
    )
    .unwrap();
    assert_eq!(
        super::temporal_anchor_state(&cache_dir, &sub),
        super::AnchorState::Agrees,
        "an explicit build arm (--build/--rebuild/--update) must re-anchor to the live toplevel"
    );
}

/// **AD-413-14-OD regression** — an adopted subdirectory root must NOT return
/// `HeadChanged` when the enclosing repository advances due to an UNRELATED commit
/// (one that touches no files under the subtree).
///
/// Pre-fix behaviour: `read_git_head(subdir)` resolved the ENCLOSING repository's
/// HEAD (via the ancestor walk added in #413), so any commit anywhere in the repo
/// bumped `current` away from the manifest's `stored` HEAD, triggering a full
/// lexical+temporal rebuild even though nothing under the subtree changed.
/// Fix (AD-413-14-OD): when `resolve_git_dir(root).is_none()` (adopted root),
/// `check_staleness` falls through to `current_or_working_tree` regardless of the
/// SHA divergence, so invalidation is scoped to real file changes under the subtree.
///
/// Discriminating: remove the `is_adopted_root` branch from `check_staleness` and
/// this test fails because `check_staleness` returns `HeadChanged`.
#[test]
fn test_adopted_subdir_unrelated_commit_does_not_return_head_changed() {
    use std::process::Command;

    let dir = tempdir().unwrap();
    let repo = dir.path().join("repo");
    let sub = repo.join("subdir");
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&sub).unwrap();
    fs::create_dir_all(&cache_dir).unwrap();

    // Repo with one commit that writes a file under the search root (subdir).
    create_real_git_repo(&repo, &[("init", &[("subdir/s.rs", "fn s() {}\n")])]);

    // Build the index for the subdirectory root.
    auto_refresh_if_stale(
        &sub,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Allow,
        None,
    )
    .unwrap();

    // Verify the manifest captured a HEAD SHA.
    let (staleness_pre, _) = check_staleness(&cache_dir, &sub);
    assert!(
        matches!(staleness_pre, StalenessCheck::Current),
        "precondition: index must be Current immediately after build; got {staleness_pre:?}"
    );

    // Make a commit that touches ONLY a file OUTSIDE the search root.
    // This advances the repo HEAD but leaves subdir/ files unchanged.
    fs::write(repo.join("outside.rs"), "fn outside() {}\n").expect("write outside.rs");
    let add_out = Command::new("git")
        .args(["add", "outside.rs"])
        .current_dir(&repo)
        .output()
        .expect("git add (spawn)");
    assert!(
        add_out.status.success(),
        "git add: {}",
        String::from_utf8_lossy(&add_out.stderr)
    );
    let commit_out = Command::new("git")
        .args(["commit", "-m", "unrelated: outside subdir"])
        .current_dir(&repo)
        .output()
        .expect("git commit (spawn)");
    assert!(
        commit_out.status.success(),
        "git commit: {}",
        String::from_utf8_lossy(&commit_out.stderr)
    );

    // The repo HEAD has advanced, but the subdir files are unchanged.
    let new_head = read_git_head(&sub).expect("HEAD must resolve after unrelated commit");
    // (The stored HEAD in the manifest is the old SHA from the initial build.)

    // check_staleness must NOT return HeadChanged for an adopted root.
    // It must instead use current_or_working_tree, which finds no changes and
    // returns Current (no files under subdir changed).
    let (staleness_post, _) = check_staleness(&cache_dir, &sub);
    assert!(
        !matches!(staleness_post, StalenessCheck::HeadChanged { .. }),
        "AD-413-14-OD REGRESSION: adopted subdir root returned HeadChanged on an \
         unrelated commit (new HEAD={new_head}); got {staleness_post:?}",
    );
    assert!(
        matches!(staleness_post, StalenessCheck::Current),
        "adopted subdir root with unchanged files must be Current after unrelated commit; \
         got {staleness_post:?}",
    );
}

/// AC17 supplement / AD-413-7 — a `HEAD` file that EXISTS but cannot be decoded is
/// `Unresolved`, not `NotARepo`.
///
/// The three-state enum exists so that "not a git repo" and "git repo whose HEAD I
/// could not resolve" stay different facts (avoids PF-016).  An `Err` from the HEAD
/// read has two causes — the file is absent (F10: `mkdir .git` ⇒ `NotARepo`) or the
/// file is present and unreadable (fs error / non-UTF-8 ⇒ `Unresolved`, the cause
/// `HeadState::Unresolved`'s own doc names).  Collapsing both into `NotARepo` emits
/// the "run 'skim search' on a git repo" lie about a directory that plainly is one.
///
/// Discriminating: with the `is_file()` split removed this returns `NotARepo` and the
/// assertion fails.  Uses non-UTF-8 bytes rather than a permission bit so the test is
/// portable and does not silently pass when the suite runs as root.
#[test]
fn test_git_head_state_unreadable_head_file_is_unresolved_not_not_a_repo() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("bad_utf8_head");
    let git_dir = root.join(".git");
    fs::create_dir_all(&git_dir).unwrap();
    // Invalid UTF-8: a lone continuation byte. `read_to_string` fails; the file exists.
    fs::write(git_dir.join("HEAD"), [0xff, 0xfe, 0x80]).unwrap();
    assert!(
        git_dir.join("HEAD").is_file(),
        "precondition: HEAD must exist as a file"
    );
    assert!(
        fs::read_to_string(git_dir.join("HEAD")).is_err(),
        "precondition: HEAD must be undecodable so the Err arm is exercised"
    );

    assert_eq!(
        git_head_state(&root),
        HeadState::Unresolved,
        "a present-but-unreadable HEAD is a repo with an unresolvable HEAD, not a non-repo"
    );

    // Control (F10): the SAME gitdir with NO HEAD file stays NotARepo.
    fs::remove_file(git_dir.join("HEAD")).unwrap();
    assert_eq!(
        git_head_state(&root),
        HeadState::NotARepo,
        "F10: a gitdir with no HEAD file must stay NotARepo (mkdir .git must not lie)"
    );
}

// ============================================================================
// AC33 — resolve_repo_toplevel unit tests (OD-3 / AD-413-14)
// ============================================================================

/// AC33 unit — `resolve_repo_toplevel` adopts the nearest enclosing repository
/// for a subdirectory root that has no `.git` of its own.
///
/// This is the OD-3 / AD-413-14 behaviour: `--root <subdir>` resolves the
/// enclosing repo's HEAD instead of reporting `NotARepo`.
///
/// Discriminating: if the ancestor walk is removed, `resolve_repo_toplevel`
/// returns `None` for `sub/` and this assertion fails.
#[test]
fn test_resolve_repo_toplevel_adopts_nearest_enclosing_repo() {
    let dir = tempdir().unwrap();
    let outer = dir.path().join("outer");
    let sub = outer.join("sub");
    fs::create_dir_all(&sub).unwrap();

    create_real_git_repo(&outer, &[("init", &[("sub/s.rs", "fn s(){}\n")])]);

    // sub/ exists but has no .git → must resolve to outer/.
    assert!(!sub.join(".git").exists(), "precondition: sub has no .git");
    let top = super::resolve_repo_toplevel(&sub);
    assert!(
        top.is_some(),
        "AC33: resolve_repo_toplevel must return Some for a subdir of a git repo"
    );
    let outer_canon = outer.canonicalize().unwrap();
    assert_eq!(
        top.unwrap(),
        outer_canon,
        "AC33: resolve_repo_toplevel must return the enclosing repo root"
    );
}

/// AC33 unit — `resolve_repo_toplevel` returns `None` for a root that owns its own `.git`.
///
/// This is the AC17 / AC32 invariant: a root with its own `.git` entry is NEVER
/// re-pointed to an ancestor repo.  Gate 1 of `temporal_anchor_state` relies on
/// this returning `None` (→ `NotAdopted`) before any DB read is attempted.
///
/// Discriminating: if the early `project_root.join(".git").exists()` guard is
/// removed, an inner repo sitting inside an outer repo would be re-pointed to
/// the outer one, producing wrong anchor state (Differs) and wrong temporal data.
#[test]
fn test_resolve_repo_toplevel_not_reached_when_root_has_dot_git() {
    let dir = tempdir().unwrap();
    let outer = dir.path().join("outer");
    let inner = outer.join("inner");
    fs::create_dir_all(&inner).unwrap();

    // Both repos must exist so the ancestor walk would find `outer` if unchecked.
    create_real_git_repo(&outer, &[("init", &[("inner/i.rs", "fn i(){}\n")])]);
    create_real_git_repo(&inner, &[("init", &[("i2.rs", "fn i2(){}\n")])]);

    // inner/ owns .git → must NOT be re-pointed to outer/.
    assert!(
        inner.join(".git").exists(),
        "precondition: inner has its own .git"
    );
    let top = super::resolve_repo_toplevel(&inner);
    assert!(
        top.is_none(),
        "AC33: resolve_repo_toplevel must return None for a root that owns .git; got {top:?}"
    );
}

// ============================================================================
// AC22 — Frozen manifest: git_head advances after the first auto_refresh
// ============================================================================

/// AC22 — A frozen manifest (stored HEAD = `None`) paired with valid lexical/AST
/// indexes reports `{git_head: null, git_head_state: "resolved"}` on the first
/// `--stats --json` call and `{git_head: "<GT>", git_head_state: "resolved"}`
/// on the second (after one `auto_refresh_if_stale`).
///
/// "Frozen" means the index was built before #413 landed (or in any situation
/// where the manifest stores no HEAD): the first `stats_json` snapshot shows the
/// null/resolved pair that AD-413-13 documents as a legal transient state.
/// After one query, `auto_refresh_if_stale` sees `NoStoredHead`, rebuilds the
/// lexical index, and stores the live SHA in the manifest — the second snapshot
/// shows the expected resolved value.
///
/// Fixture: a real **linked worktree** (plan S22), exercising the `git_head` /
/// `git_head_state` provenance divergence that #413 introduces.  A plain repo's
/// HEAD already resolved before #413, so using a plain repo here would carry no
/// discriminating power for the worktree-specific resolution path.
///
/// Discriminating:
/// - Without the `git_head_state` key (#413), the second assertion cannot
///   distinguish "resolved and recorded" from "resolved but still null".
/// - An implementation that never wrote the HEAD to the manifest would keep
///   `git_head` null permanently; the second assertion catches it.
#[test]
fn test_ac22_frozen_manifest_git_head_advances_after_refresh() {
    let (dir, _primary, root, gt) = worktree_fixture("b1");
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // Build a real index first so cache_dir contains valid index files
    // (NgramIndexReader::open requires a full header — a 6-byte stub is
    // insufficient; write_lexical_index_stub cannot satisfy build_stats_json).
    auto_refresh_if_stale(
        &root,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    // Now freeze the manifest: overwrite stored HEAD = None to simulate a
    // worktree whose manifest was written before the HEAD-recording fix.
    write_manifest_with_head(&root, &cache_dir, None);

    // First stats call: stored HEAD is None, live HEAD resolves → frozen state.
    let stats1 = super::super::stats_json_for_test(&cache_dir, &root)
        .expect("stats_json_for_test must succeed");
    assert!(
        stats1["git_head"].is_null(),
        "AC22: frozen manifest must report null git_head; got {:?}",
        stats1["git_head"]
    );
    assert_eq!(
        stats1["git_head_state"].as_str().unwrap_or(""),
        "resolved",
        "AC22: live HEAD resolves so git_head_state must be 'resolved'; got {:?}",
        stats1["git_head_state"]
    );

    // One auto_refresh_if_stale to advance the stored HEAD.
    auto_refresh_if_stale(
        &root,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    // Second stats call: stored HEAD now equals the live SHA.
    let stats2 = super::super::stats_json_for_test(&cache_dir, &root)
        .expect("stats_json_for_test must succeed");
    assert_eq!(
        stats2["git_head"].as_str().unwrap_or(""),
        gt.as_str(),
        "AC22: after refresh git_head must equal the live SHA; got {:?}",
        stats2["git_head"]
    );
    assert_eq!(
        stats2["git_head_state"].as_str().unwrap_or(""),
        "resolved",
        "AC22: git_head_state must still be 'resolved'; got {:?}",
        stats2["git_head_state"]
    );
}

// ============================================================================
// AC23 — warn_if_temporal_unverifiable call-site coverage (source inspection)
// ============================================================================

/// AC23 — `staleness::warn_if_temporal_unverifiable` is wired into every search
/// arm that can serve temporal data.
///
/// The criterion lists 11 CLI arms: `--hot`, `--cold`, `--risky`,
/// `--blast-radius`, `fn --hot` (text+temporal), `--ast --hot` (AST+temporal),
/// `--update`, `--build`, `--rebuild`, `--stats`, `--stats --json`.  All 11
/// route through one of the call sites in `mod.rs`.  This test pins the count
/// so a regression that drops a call site is caught without requiring 11 binary
/// invocations.
///
/// Discriminating: deleting any direct call site changes the exact direct-call count
/// (currently 5) and fails the assertion; adding a spurious call site without a new
/// temporal arm also fails (exact `==`, not `>=`).  Adding a new temporal arm without
/// a call site keeps both counts unchanged — THAT case requires the CLI acceptance run
/// in S23.  Comment references to the function name are excluded by using call-syntax
/// patterns (name + opening paren) so a masked-deletion via comment cannot pass.
#[test]
fn test_ac23_warn_if_temporal_unverifiable_call_site_count() {
    let mod_src = include_str!("mod.rs");
    // Use call-syntax patterns (function name + opening paren) rather than bare name
    // references so that comment mentions of the name are excluded.  A bare-name `>=`
    // bound would allow masking a deleted call site by adding a comment reference;
    // exact `==` on a call-syntax pattern closes that gap.
    //
    // `warn_if_temporal_unverifiable(` does NOT match the `_at` variant because
    // `warn_if_temporal_unverifiable_at(` has `_at` between `unverifiable` and `(`.
    let direct_call_count = mod_src.matches("warn_if_temporal_unverifiable(").count();
    // Direct calls via the bare function (mod.rs is the sole consumer):
    //   run()                     — text+AST compound arm (--ast)
    //   run_build()               — --build / --rebuild arm
    //   run_update()              — --update arm
    //   run_stats()               — --stats / --stats --json arm (reliability fix:
    //                               HEAD resolved once here instead of via _at wrapper,
    //                               so the same HeadState can be forwarded to
    //                               build_stats_json without a second resolution)
    //   run_query()               — plain-text query arm
    //   run_temporal_standalone() — --hot / --cold / --risky / --blast-radius arm
    assert_eq!(
        direct_call_count, 6,
        "AC23: mod.rs must contain exactly 6 direct warn_if_temporal_unverifiable(...) \
         call sites; found {direct_call_count} — a temporal arm lost its advisory call \
         or a new arm was added without wiring (update expected value if arms change)"
    );
    let at_call_count = mod_src.matches("warn_if_temporal_unverifiable_at(").count();
    // The _at wrapper is no longer used in mod.rs: run_stats now resolves HeadState
    // once and forwards it to both warn_if_temporal_unverifiable and build_stats_json
    // (reliability fix — eliminates the duplicate git_head_state call).
    assert_eq!(
        at_call_count, 0,
        "AC23: mod.rs must contain exactly 0 warn_if_temporal_unverifiable_at(...) \
         call sites; found {at_call_count} — the _at wrapper was re-introduced in mod.rs \
         (it leads to a second git_head_state call when build_stats_json also reads HEAD)"
    );
}

// ============================================================================
// AC24 — Guard ordering: advisory early-returns before any SQLite open
//         when HEAD resolves (HEAD-resolves path, C5)
// ============================================================================

/// AC24 — `warn_if_temporal_unverifiable` returns immediately (zero SQLite opens)
/// when the live HEAD state is not `Unresolved`.
///
/// The guard `if !matches!(head, HeadState::Unresolved) { return; }` is the first
/// statement in the function.  We verify it fires before any DB access by:
///   1. Creating a real `temporal.db` with `META_GIT_HEAD` set — so guard 2
///      (`!exists()`) would also pass if guard 1 were absent — then
///   2. Asserting that `TEMPORAL_META_READ_COUNT` (the in-process counter
///      incremented inside `read_temporal_meta` after the `exists()` check) does
///      NOT move across the two calls.
///
/// Discriminating: removing guard 1 causes `read_temporal_meta` to be called,
/// which increments `TEMPORAL_META_READ_COUNT` by 2 (one per sub-case), failing
/// the assertion.  The DB-as-directory approach previously used would not catch
/// this because `open_with_flags` swallows the error via `.ok()?`.
///
/// Sub-cases:
///   C5-R: `HeadState::Resolved("sha")` — the common healthy-repo case.
///   C5-N: `HeadState::NotARepo` — non-git directory.
#[test]
fn test_ac24_advisory_early_return_before_sqlite_on_resolved_and_not_a_repo() {
    let dir = tempdir().unwrap();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // Build a real temporal.db so guard 2 (`!temporal.db.exists()`) passes.
    // Without guard 1, `read_temporal_meta` would reach the SQLite open and
    // increment TEMPORAL_META_READ_COUNT.
    let db_path = cache_dir.join("temporal.db");
    {
        rskim_search::TemporalDb::open(&db_path).unwrap();
    }
    super::plant_meta_raw(&db_path, rskim_search::META_GIT_HEAD, &"a".repeat(40));

    // Snapshot the counter before the guarded calls.
    let before = TEMPORAL_META_READ_COUNT.load(std::sync::atomic::Ordering::SeqCst);

    // C5-R: resolved HEAD — guard 1 must short-circuit before any DB open.
    let sha = "a".repeat(40);
    super::warn_if_temporal_unverifiable(&cache_dir, &HeadState::Resolved(sha));

    // C5-N: not-a-repo — guard 1 must also short-circuit.
    super::warn_if_temporal_unverifiable(&cache_dir, &HeadState::NotARepo);

    // Assert zero DB opens: the counter must not have moved across either call.
    let after = TEMPORAL_META_READ_COUNT.load(std::sync::atomic::Ordering::SeqCst);
    let delta = after.wrapping_sub(before);
    assert_eq!(
        delta, 0,
        "AC24: guard 1 (!matches!(head, Unresolved)) must short-circuit before any \
         SQLite open for HeadState::Resolved and HeadState::NotARepo; \
         TEMPORAL_META_READ_COUNT delta = {delta} — guard is missing or bypassed",
    );
}

// ============================================================================
// AC16(a) — No rebuild loop on a healthy linked worktree:
//            temporal.db and index.skfiles mtimes unchanged on second call
// ============================================================================

/// AC16(a) — Two consecutive identical `auto_refresh_if_stale` calls on a healthy
/// linked worktree must leave both `temporal.db` and `index.skfiles` mtimes
/// unchanged on the second call (no rebuild loop).
///
/// This path is newly reachable because of #413's worktree HEAD resolution; the
/// no-loop guarantee must be asserted explicitly for the linked-worktree case.
/// The existing `test_bug_b_no_rebuild_loop_when_temporal_is_current` covers a
/// plain repo; this test covers the newly introduced worktree path.
///
/// Discriminating:
///   - A regression that rebuilds the lexical index on every call would advance
///     `index.skfiles` mtime, failing the manifest assertion.
///   - A regression that rebuilds `temporal.db` on every call would advance its
///     mtime, failing the temporal assertion.
///   - A regression that returns an error on the second call would fail `.unwrap()`.
#[test]
fn test_ac16a_healthy_worktree_no_rebuild_loop() {
    let (dir, _primary, worktree, _gt) = worktree_fixture("b1");
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // First call: build the complete index (lexical + temporal) on the healthy
    // linked worktree.
    auto_refresh_if_stale(
        &worktree,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    let temporal_db_path = cache_dir.join("temporal.db");
    let manifest_path = cache_dir.join("index.skfiles");
    assert!(
        temporal_db_path.exists(),
        "AC16(a) precondition: temporal.db must exist after first build"
    );
    assert!(
        manifest_path.exists(),
        "AC16(a) precondition: index.skfiles must exist after first build"
    );

    let temporal_mtime_before = fs::metadata(&temporal_db_path).unwrap().modified().unwrap();
    let manifest_mtime_before = fs::metadata(&manifest_path).unwrap().modified().unwrap();

    // Small delay so that any rewrite on the second call would produce a
    // measurably later mtime.
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Second call: both temporal and lexical indexes are Current.
    // Neither temporal.db nor index.skfiles must be rewritten.
    auto_refresh_if_stale(
        &worktree,
        &cache_dir,
        &TEST_ANALYTICS,
        ReanchorPolicy::Refuse,
        None,
    )
    .unwrap();

    let temporal_mtime_after = fs::metadata(&temporal_db_path).unwrap().modified().unwrap();
    let manifest_mtime_after = fs::metadata(&manifest_path).unwrap().modified().unwrap();

    assert_eq!(
        temporal_mtime_before, temporal_mtime_after,
        "AC16(a): temporal.db mtime must be unchanged on the second call \
         (no rebuild loop on healthy linked worktree)"
    );
    assert_eq!(
        manifest_mtime_before, manifest_mtime_after,
        "AC16(a): index.skfiles mtime must be unchanged on the second call \
         (no rebuild loop on healthy linked worktree)"
    );
}

// ============================================================================
// AC16(c) — Advisory fires on unresolvable HEAD + temporal.db present;
//            temporal.db is NOT rebuilt (no loop)
// ============================================================================

/// AC16(c) — After a successful index build, if the repo HEAD becomes
/// unresolvable (e.g. `git symbolic-ref HEAD refs/heads/gone`), two consecutive
/// `check_staleness` calls must both return `Current` (no rebuild loop) and
/// `warn_if_temporal_unverifiable` must fire without error.
///
/// This exercises the AC24 advisory gate on the NEGATIVE/advisory side: HEAD is
/// `Unresolved`, `temporal.db` exists, and `META_GIT_HEAD` is recorded —
/// so `warn_if_temporal_unverifiable` DOES proceed past the first two guards and
/// reads the `meta.git_head` row.  The advisory is printed to stderr; `temporal.db`
/// is not modified.
///
/// Discriminating:
///   - A wrong fix that rebuilds on Unresolved live HEAD would change the
///     `temporal.db` length and the mtime-equality assertion fails.
///   - A wrong fix that treats Unresolved as NotARepo would suppress the advisory;
///     but since the advisory goes to stderr (not testable here without subprocess),
///     this unit test focuses on the no-rebuild contract.
#[test]
fn test_ac16c_unresolvable_head_no_rebuild_loop_temporal_stable() {
    let dir = tempdir().unwrap();
    let root = dir.path().join("repo");
    let git_dir = root.join(".git");
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(git_dir.join("refs/heads")).unwrap();
    fs::create_dir_all(&cache_dir).unwrap();
    fs::write(root.join("a.rs"), "fn a(){}\n").unwrap();

    // Build a valid git repo with a commit so `temporal.db` has a recorded HEAD.
    let recorded_sha = create_real_git_repo(&root, &[("init", &[("a.rs", "fn a(){}\n")])]);
    build_temporal_for_test(&root, &cache_dir, &recorded_sha);
    assert!(
        cache_dir.join("temporal.db").exists(),
        "precondition: temporal.db must exist after the build"
    );

    // Build a real lexical+AST index so the manifest has actual file entries.
    // Using write_manifest_with_head+stubs would produce an empty manifest,
    // causing scan_working_tree to see a.rs as "added" → dirty → WorkingTreeChanged,
    // masking the no-rebuild-loop contract this test asserts (#413 Fix 1).
    build_index_in(&root, &cache_dir);

    // Make HEAD unresolvable: point to a non-existent ref.
    // We overwrite the HEAD file so the symbolic-ref points somewhere that has no file.
    fs::write(git_dir.join("HEAD"), "ref: refs/heads/gone\n").unwrap();
    assert!(
        read_git_head(&root).is_none(),
        "AC16(c) precondition: live HEAD must be None after the ref is broken"
    );
    assert_eq!(
        git_head_state(&root),
        HeadState::Unresolved,
        "AC16(c) precondition: git_head_state must be Unresolved"
    );

    // First staleness check: (Some(stored), None) arm → Current (no rebuild loop).
    let (verdict1, _) = check_staleness(&cache_dir, &root);
    assert!(
        matches!(verdict1, StalenessCheck::Current),
        "AC16(c): first check_staleness must be Current when live HEAD is Unresolved; \
         got {verdict1:?}"
    );

    // AC24 advisory path: HEAD is Unresolved + temporal.db + recorded git_head →
    // warn_if_temporal_unverifiable reads the meta row and prints to stderr.
    // We verify it completes without panic (the advisory output is on stderr).
    let db_len_before = fs::metadata(cache_dir.join("temporal.db")).unwrap().len();
    super::warn_if_temporal_unverifiable(&cache_dir, &HeadState::Unresolved);
    let db_len_after = fs::metadata(cache_dir.join("temporal.db")).unwrap().len();
    assert_eq!(
        db_len_before, db_len_after,
        "AC16(c): temporal.db must not be modified by warn_if_temporal_unverifiable"
    );

    // Second staleness check: still Current (no rebuild triggered).
    let (verdict2, _) = check_staleness(&cache_dir, &root);
    assert!(
        matches!(verdict2, StalenessCheck::Current),
        "AC16(c): second check_staleness must also be Current (no rebuild loop); \
         got {verdict2:?}"
    );
}

/// AC-12 clause 2 (NEGATIVE, #414): `check_staleness` must **not** report the
/// lexical index stale when `index.skidx` has a future format version
/// (`FORMAT_VERSION + 1`).
///
/// `check_staleness` computes `lexical_stale` as
/// `Ok(v) => v < LEXICAL_INDEX_FORMAT_VERSION`.  For `v = FORMAT_VERSION + 1`
/// the comparison is false, so the lexical dimension does NOT trigger a rebuild.
/// This is the second of the three AC-12 clauses (the first — `Ok(future_version)`
/// from the probe — is guarded by `t7_integrity_probe_future_version_returns_ok_without_size_check`
/// in `reader_tests.rs`; the third — bytes/mtimes unchanged — is also guarded
/// there).
///
/// Discriminating: if the `v < LEXICAL_INDEX_FORMAT_VERSION` comparison were
/// accidentally inverted (`v > …`) or replaced with `v != …`, this test would
/// fail because `StalenessCheck::NoStoredHead` would be returned instead of
/// `Current`.  The `.skpost` truncation mirrors the AC-12 discriminator: a
/// size-check regression that reaches the `.skpost` probe would return
/// `NoStoredHead` (corrupted → rebuild), not `Current`.
#[test]
fn t12_ac12_future_version_check_staleness_not_stale() {
    use rskim_search::LEXICAL_INDEX_FORMAT_VERSION;

    let dir = tempdir().unwrap();
    let cache_dir = dir.path().to_path_buf();
    let sha = "1234abcd1234abcd1234abcd1234abcd1234abcd";
    create_fake_git_repo(dir.path(), &format!("{sha}\n"));

    write_manifest_with_head(dir.path(), &cache_dir, Some(sha));
    // Write a stub with a FUTURE lexical version (FORMAT_VERSION + 1).
    // The version is strictly above LEXICAL_INDEX_FORMAT_VERSION, so the
    // `v < LEXICAL_INDEX_FORMAT_VERSION` guard in check_staleness must be false.
    let future_version: u16 = LEXICAL_INDEX_FORMAT_VERSION + 1;
    let mut header = [0u8; 62];
    header[0..4].copy_from_slice(b"SKIX");
    header[4..6].copy_from_slice(&future_version.to_le_bytes());
    fs::write(cache_dir.join("index.skidx"), header).unwrap();
    // AC-12 discriminator: truncate .skpost so a size-check regression returns
    // IndexCorrupted (→ rebuild) rather than silently passing.
    fs::write(cache_dir.join("index.skpost"), b"").unwrap();
    write_ast_index_stub(&cache_dir);

    let (result, _manifest) = check_staleness(&cache_dir, dir.path());

    // AC-12 clause 2: a future-version lexical index must NOT trigger a rebuild.
    // The probe returns Ok(future_version); Ok(v) => v < FORMAT_VERSION is false
    // for v > FORMAT_VERSION, so lexical_stale is false.
    assert!(
        matches!(result, StalenessCheck::Current),
        "AC-12 clause 2: check_staleness must return Current for a future-version \
         lexical index (v={future_version} > FORMAT_VERSION); got {result:?}"
    );
}

// ============================================================================
// #407 — TEMPORAL_DATA_VERSION 1→2 staleness tests (AC-10, AC-12 data side)
// ============================================================================

/// T-13 (#407 AC-10): a `temporal.db` carrying `data_version = "1"` with a
/// matching `git_head` MUST make `temporal_db_is_stale` return `true`.
///
/// AD-408-4 (Check 2): the gate uses `stored < current`.  After the #407 bump
/// `TEMPORAL_DATA_VERSION == 2`, so any DB with `data_version = "1"` (written
/// by a pre-#407 binary with first-parent-only walk) is stale.
///
/// Discriminating: if `temporal_db_is_stale` returned `false` for
/// `data_version = "1"`, pre-#407 DBs would never be self-healed and hotspot/risk
/// scores would remain undercounted (~3×) on branch-heavy repositories.
#[test]
fn test_pre_407_db_is_stale() {
    let dir = tempdir().unwrap();
    let head = "cafe0001cafe0001cafe0001cafe0001cafe0001";

    // Build a pre-#407 DB: correct schema (v2), HEAD recorded, data_version = "1".
    plant_db_at_data_version(dir.path(), head, "1");

    assert!(
        temporal_db_is_stale(dir.path(), head, None),
        "T-13 (AC-10): data_version=\"1\" with matching HEAD must be stale — \
         the #407 bump to TEMPORAL_DATA_VERSION=2 makes every pre-#407 DB stale \
         so the full-DAG self-heal fires (AD-408-4 Check 2: stored < current)"
    );
}

/// T-14 (#407 AC-10): after exactly one sync (the self-heal rebuild),
/// `temporal_db_is_stale` MUST return `false` and `get_meta(META_DATA_VERSION)`
/// MUST equal `Some("2")`.
///
/// AC-10 contract: the self-heal is one-shot.  `TemporalDb::sync` is the only
/// version-attesting write path (AD-408-3); it writes `TEMPORAL_DATA_VERSION.to_string()`
/// unconditionally alongside `git_head`.  One `sync` call is sufficient to
/// advance `data_version` from "1" to "2".
///
/// See AD-407-5 for the ONE case where self-heal is NOT one-shot (build-backoff
/// sentinel present for the current HEAD+shallow pair — AC-13).
#[test]
fn test_407_self_heal_is_one_shot() {
    let dir = tempdir().unwrap();
    let head = "cafe0002cafe0002cafe0002cafe0002cafe0002";

    // Set up a pre-#407 DB: correct schema (v2), HEAD recorded, data_version = "1".
    let db_path = plant_db_at_data_version(dir.path(), head, "1");

    // Verify the pre-condition: stale.
    assert!(
        temporal_db_is_stale(dir.path(), head, None),
        "T-14 pre-condition: data_version=\"1\" must be stale before self-heal"
    );

    // The self-heal: one sync call writes data_version = "2".
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    db.sync(&[], &[], &[], head, false).unwrap();
    drop(db);

    // After exactly one rebuild, not stale.
    assert!(
        !temporal_db_is_stale(dir.path(), head, None),
        "T-14 (AC-10): after one sync (self-heal), temporal_db_is_stale must return false"
    );

    // And the meta row carries "2".
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    let stored = db.get_meta(rskim_search::META_DATA_VERSION).unwrap();
    assert_eq!(
        stored,
        Some("2".to_string()),
        "T-14 (AC-10): after one sync, get_meta(META_DATA_VERSION) must be Some(\"2\")"
    );
}

/// AC-12 (downgrade-safety guard): a `temporal.db` carrying `data_version = "3"`
/// (written by a hypothetical newer binary) MUST NOT be flagged stale by this build.
///
/// AD-408-4: the comparison is `stored < current` (not `stored != current`), so
/// a DB written by a NEWER binary (data_version > TEMPORAL_DATA_VERSION) is not
/// spuriously rebuilt by an older post-fix binary. This preserves downgrade safety:
/// rolling back from a future skim binary to this one does not trigger an endless
/// self-heal loop.
///
/// Discriminating: a `stored != current` gate (wrong) would fire for data_version "3"
/// with TEMPORAL_DATA_VERSION == 2, causing a spurious rebuild.  The `<` gate
/// (correct) returns false and leaves the DB untouched.
#[test]
fn test_temporal_data_version_3_not_stale_after_407_bump() {
    let dir = tempdir().unwrap();
    let head = "cafe0003cafe0003cafe0003cafe0003cafe0003";

    // Build a DB with data_version = "3" (future version — downgrade-safety guard).
    plant_db_at_data_version(dir.path(), head, "3");

    assert!(
        !temporal_db_is_stale(dir.path(), head, None),
        "AC-12: data_version=\"3\" (newer binary) MUST NOT be flagged stale by \
         TEMPORAL_DATA_VERSION=2 build — downgrade-safety guard (AD-408-4: stored < current)"
    );
}

/// Library-layer health check: a `temporal.db` that starts stale
/// (`data_version = "1"`) and is healed by one `sync` call MUST NOT enter
/// a corrupt or schema-mismatch state.
///
/// This test verifies internal DB state only — that `temporal_db_is_stale`
/// returns false, that `TemporalDb::open` succeeds, and that `schema_version`
/// is still 2.  These are preconditions for the CLI-level AC-18 guarantee,
/// not the guarantee itself.
///
/// AC-18 as written in the plan is an observable-output criterion: no
/// `degraded` key in query `--json`, and `--stats --json` reporting
/// `temporal_state: "ready"`.  Those end-to-end assertions are exercised by
/// `test_ac18_stale_data_version_heals_no_degraded_on_next_query` in
/// `crates/rskim/tests/cli_temporal_first_parent.rs`, which drives the full
/// CLI stack.  This unit test verifies the underlying library layer that
/// makes the CLI guarantee possible.
#[test]
fn test_stale_data_version_no_degraded_state_after_heal() {
    let dir = tempdir().unwrap();
    let head = "cafe0004cafe0004cafe0004cafe0004cafe0004";

    // Pre-heal state: data_version = "1" (stale), but HEAD matches.
    let db_path = plant_db_at_data_version(dir.path(), head, "1");
    assert!(
        temporal_db_is_stale(dir.path(), head, None),
        "pre-condition: data_version=\"1\" must be flagged stale"
    );

    // Self-heal: one sync advances data_version to "2".
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    db.sync(&[], &[], &[], head, false).unwrap();
    drop(db);

    // Post-heal: staleness flag is clear (DB is no longer a self-heal candidate).
    assert!(
        !temporal_db_is_stale(dir.path(), head, None),
        "after self-heal sync, temporal_db_is_stale must be false"
    );

    // Post-heal: DB opens without error (no DatabaseCorrupt / UnsupportedSchemaVersion).
    let db_result = rskim_search::TemporalDb::open(&db_path);
    assert!(
        db_result.is_ok(),
        "post-heal DB must open without error; got: {:?}",
        db_result.unwrap_err()
    );

    // Post-heal: schema version is still 2 (no spurious migration was triggered).
    let schema = db_result.unwrap().schema_version().unwrap();
    assert_eq!(
        schema, 2,
        "post-heal schema_version must remain 2 — self-heal must not trigger a schema migration"
    );
}

/// AC-14 (PF-017 preserved): when `temporal.db` carries a stale `data_version`
/// AND the stored `git_toplevel` differs from the current enclosing repository,
/// a **query** (`ReanchorPolicy::Refuse`) MUST NOT modify `temporal.db` — bytes
/// and file length unchanged.
///
/// Only an explicit build arm (`ReanchorPolicy::Allow`, i.e. `--rebuild`) may
/// re-anchor.  This test verifies the anchor-mismatch guard is NOT weakened by
/// the data-version staleness check — both conditions are present, and the
/// refuse policy must win.
///
/// PF-017: the guard lives in `try_rebuild_temporal_nonfatal`, which calls
/// `temporal_anchor_state` and returns early when the result is
/// `AnchorState::Differs` + `ReanchorPolicy::Refuse`.  Only
/// `--root <subdirectory>` roots (those that don't own their own `.git`) reach
/// this guard; a root with `.git` gets `AnchorState::NotAdopted` and the
/// anchor logic is skipped entirely.
///
/// To trigger `AnchorState::Differs` the test sets up:
/// - `parent/.git/HEAD` — a minimal enclosing git repo so that
///   `resolve_repo_toplevel(parent/sub)` succeeds and returns `parent`.
/// - `parent/sub/` — a subdirectory without `.git` (the `--root` being tested).
/// - `temporal.db` anchored to `"other_repo"` (a different path) and with
///   `data_version = "1"` (stale).
///
/// After `try_rebuild_temporal_nonfatal` with `Refuse`, `temporal.db` must be
/// byte-for-byte unchanged and both `git_toplevel` and `data_version` must
/// retain their planted values.
#[test]
fn test_ac14_stale_data_version_anchor_mismatch_no_rebuild() {
    let dir = tempdir().unwrap();

    // Build a minimal enclosing git repo so resolve_repo_toplevel(sub) returns parent.
    let parent = dir.path().join("parent_repo");
    let git_dir = parent.join(".git");
    fs::create_dir_all(&git_dir).unwrap();
    // A readable HEAD file is required by resolve_repo_toplevel (F10).
    fs::write(git_dir.join("HEAD"), b"ref: refs/heads/main").unwrap();

    // sub/ has no .git of its own — it's the "--root <subdirectory>" case.
    let sub = parent.join("sub");
    fs::create_dir_all(&sub).unwrap();

    // cache_dir is separate from both parent and sub.
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // Build temporal.db anchored to "other_repo" (a different toplevel) and
    // with data_version = "1" (stale — pre-#407).
    let head_a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa0";
    let db_path = plant_db_at_data_version(&cache_dir, head_a, "1");
    super::plant_meta_raw(&db_path, rskim_search::META_GIT_TOPLEVEL, "/other_repo");

    // Capture the full byte contents before the attempted query-path rebuild.
    // A same-length-but-different-content rewrite (e.g., a SQLite WAL checkpoint)
    // would not be caught by a length-only check; compare actual bytes (AC-14).
    let bytes_before = fs::read(&db_path).unwrap();

    // Verify that temporal_anchor_state sees Differs (not NotAdopted or Absent)
    // for the subdirectory root — this is the precondition for AC-14.
    let anchor = super::temporal_anchor_state(&cache_dir, &sub);
    assert!(
        matches!(anchor, super::AnchorState::Differs { .. }),
        "AC-14 precondition: temporal_anchor_state must be Differs for sub, got {anchor:?}"
    );

    // Call try_rebuild_temporal_nonfatal (the query-path orchestrator that owns
    // the PF-017 guard) with Refuse policy.  The guard must fire and return
    // before touching temporal.db.
    let fake_head = super::HeadState::Resolved(head_a.to_string());
    super::try_rebuild_temporal_nonfatal(
        &sub,
        &cache_dir,
        &fake_head,
        "test-ac14",
        super::ReanchorPolicy::Refuse,
    );

    // temporal.db must be byte-for-byte unchanged (AC-14 file-bytes guard).
    // Compare full contents rather than just file length: a SQLite WAL checkpoint
    // could produce an identically-sized but content-modified file, which a
    // length-only check would miss.
    let bytes_after = fs::read(&db_path).unwrap();
    assert_eq!(
        bytes_after, bytes_before,
        "AC-14 (PF-017): temporal.db must be byte-for-byte unchanged after a \
         refused query (anchor mismatch + stale data_version — refuse policy wins)"
    );

    // git_toplevel must still point at other_repo (not re-anchored to parent).
    let db = rskim_search::TemporalDb::open(&db_path).unwrap();
    let stored_toplevel = db
        .get_meta(rskim_search::META_GIT_TOPLEVEL)
        .unwrap()
        .unwrap_or_default();
    assert_eq!(
        stored_toplevel, "/other_repo",
        "AC-14 (PF-017): git_toplevel must remain /other_repo after a refused query"
    );

    // data_version must still be "1" (no rebuild happened).
    let stored_version = db
        .get_meta(rskim_search::META_DATA_VERSION)
        .unwrap()
        .unwrap_or_default();
    assert_eq!(
        stored_version, "1",
        "AC-14: data_version must still be \"1\" — no rebuild happened on the \
         refused query (anchor mismatch blocked before data-version self-heal)"
    );
}
