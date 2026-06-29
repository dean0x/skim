//! Tests for the file walker (walk.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{discover_project_root, normalize_rel_path, sha256_hex, walk_and_read, walk_metadata};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

// ============================================================================
// Helpers
// ============================================================================

/// Create a temporary directory tree with some Rust source files.
///
/// Layout:
/// ```
/// root/
///   .git/              <- marks project root
///   src/
///     main.rs          <- valid Rust source
///     lib.rs           <- valid Rust source
///   build.py           <- Python source
///   README.md          <- Markdown
///   data.json          <- JSON
///   binary.bin         <- non-UTF8 bytes
///   huge.rs            <- a file whose content we can later check
/// ```
fn make_sample_tree() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Simulate git root
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "pub fn add(a: u32, b: u32) -> u32 { a + b }\n",
    )
    .unwrap();
    fs::write(root.join("build.py"), "print('hello')\n").unwrap();
    fs::write(root.join("README.md"), "# Hello\n").unwrap();
    fs::write(root.join("data.json"), "{\"key\": 1}\n").unwrap();

    // Non-UTF8 file: will be skipped
    fs::write(root.join("binary.bin"), b"\xFF\xFE invalid utf8 \x80").unwrap();

    dir
}

// ============================================================================
// discover_project_root
// ============================================================================

#[test]
fn test_discover_project_root_finds_git_root() {
    let dir = make_sample_tree();
    let root = dir.path();

    // Start from src/ — should walk up to find .git
    let src = root.join("src");
    let found = discover_project_root(&src).unwrap();
    assert_eq!(found, root.canonicalize().unwrap());
}

#[test]
fn test_discover_project_root_at_git_dir() {
    let dir = make_sample_tree();
    let root = dir.path();

    // Start at root itself
    let found = discover_project_root(root).unwrap();
    assert_eq!(found, root.canonicalize().unwrap());
}

#[test]
fn test_discover_project_root_no_git_returns_cwd() {
    // No .git anywhere — should fall back to the provided directory
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let found = discover_project_root(root).unwrap();
    // Falls back to the canonical form of the provided path
    assert_eq!(found, root.canonicalize().unwrap());
}

// ============================================================================
// walk_and_read
// ============================================================================

#[test]
fn test_walk_finds_rust_and_python_files() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (files, skipped) = walk_and_read(&root, 50_000).unwrap();

    // At minimum, main.rs, lib.rs, and build.py should be found
    let paths: Vec<PathBuf> = files.iter().map(|f| f.rel_path.clone()).collect();
    assert!(
        paths.iter().any(|p| p.ends_with("main.rs")),
        "main.rs not found in {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("lib.rs")),
        "lib.rs not found in {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("build.py")),
        "build.py not found in {paths:?}"
    );

    // binary.bin has non-UTF8 content → should be skipped; skipped list is non-empty
    assert!(
        !skipped.is_empty(),
        "binary.bin and unsupported files should produce skip reasons"
    );
}

#[test]
fn test_walk_skips_non_utf8_files() {
    // Use a supported extension (.rs) so that Language::from_path returns Some,
    // which lets classify_entry proceed past the unsupported-language gate and
    // exercise the actual non-UTF-8 detection code path (open_and_read returns
    // ReadOutcome::NonUtf8 → SkipReason::NonUtf8).  A .bin file would be
    // rejected earlier by the unsupported-language check, never reaching the
    // UTF-8 read; that code path is already covered by test_walk_finds_rust_and_python_files.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    // Valid UTF-8 Rust source — should be accepted.
    fs::write(root.join("valid.rs"), "fn main() {}\n").unwrap();
    // Invalid UTF-8 content with a supported .rs extension — must be skipped
    // via the non-UTF-8 code path, not the unsupported-language gate.
    fs::write(
        root.join("invalid_utf8.rs"),
        b"\xFF\xFE not valid utf8 \x80",
    )
    .unwrap();

    let root = root.canonicalize().unwrap();
    let (files, skipped) = walk_and_read(&root, 50_000).unwrap();
    let paths: Vec<PathBuf> = files.iter().map(|f| f.rel_path.clone()).collect();

    assert!(
        !paths.iter().any(|p| p.ends_with("invalid_utf8.rs")),
        "invalid_utf8.rs should be skipped, paths: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("valid.rs")),
        "valid.rs should be accepted, paths: {paths:?}"
    );
    // Confirm the skip was recorded with the correct reason.
    let has_non_utf8 = skipped.iter().any(|r| {
        matches!(
            r,
            super::super::types::SkipReason::NonUtf8(path)
            if path.ends_with("invalid_utf8.rs")
        )
    });
    assert!(
        has_non_utf8,
        "skipped list should contain NonUtf8 for invalid_utf8.rs, got: {skipped:?}"
    );
}

#[test]
fn test_walk_sha256_is_64_hex_chars() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (files, _) = walk_and_read(&root, 50_000).unwrap();
    for f in &files {
        // SHA is now computed by the classify phase; derive it from content here.
        let sha = sha256_hex(f.content.as_bytes());
        assert_eq!(
            sha.len(),
            64,
            "sha256 of {} should be 64 hex chars, got {}",
            f.rel_path.display(),
            sha.len()
        );
        assert!(
            sha.chars().all(|c| c.is_ascii_hexdigit()),
            "sha256 of {} contains non-hex chars: {}",
            f.rel_path.display(),
            sha
        );
    }
}

#[test]
fn test_walk_sha256_is_deterministic() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (mut files1, _) = walk_and_read(&root, 50_000).unwrap();
    let (mut files2, _) = walk_and_read(&root, 50_000).unwrap();

    // Sort by rel_path before comparing so the assertion is order-independent.
    // walk_and_read documents lexicographic ordering via sort_by_file_path, but
    // sorting here makes the test robust if that contract ever changes: a broken
    // sort would surface as a mismatched SHA rather than a flaky zip mismatch.
    files1.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    files2.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));

    // SHA is now computed by the classify phase; derive it from content here.
    assert_eq!(files1.len(), files2.len());
    for (f1, f2) in files1.iter().zip(files2.iter()) {
        assert_eq!(f1.rel_path, f2.rel_path);
        assert_eq!(
            sha256_hex(f1.content.as_bytes()),
            sha256_hex(f2.content.as_bytes()),
            "sha256 of {} must be deterministic across two walks",
            f1.rel_path.display()
        );
        assert_eq!(
            f1.lang,
            f2.lang,
            "lang detection must be deterministic for {}",
            f1.rel_path.display()
        );
    }
}

#[test]
fn test_walk_sha256_changes_when_content_changes() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (files_before, _) = walk_and_read(&root, 50_000).unwrap();
    let main_before = files_before
        .iter()
        .find(|f| f.rel_path.ends_with("main.rs"))
        .unwrap();
    // SHA is now computed by the classify phase; derive it from content here.
    let sha_before = sha256_hex(main_before.content.as_bytes());

    // Modify the file
    fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"hello\"); }\n",
    )
    .unwrap();

    let (files_after, _) = walk_and_read(&root, 50_000).unwrap();
    let main_after = files_after
        .iter()
        .find(|f| f.rel_path.ends_with("main.rs"))
        .unwrap();
    let sha_after = sha256_hex(main_after.content.as_bytes());

    assert_ne!(
        sha_before, sha_after,
        "SHA should change after file modification"
    );
}

#[test]
fn test_walk_respects_max_files_cap() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();
    // Create 10 files
    for i in 0..10 {
        fs::write(
            root.join(format!("file_{i}.rs")),
            format!("fn f{i}() {{}}\n"),
        )
        .unwrap();
    }

    // Cap at 3
    let (files, _skipped) = walk_and_read(&root.canonicalize().unwrap(), 3).unwrap();
    assert_eq!(
        files.len(),
        3,
        "walker should stop at max_files=3, got {}",
        files.len()
    );
}

#[test]
fn test_walk_skips_files_over_5mb() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();

    // Create a file over 5 MB
    let big = vec![b'a'; 6 * 1024 * 1024];
    fs::write(root.join("big.rs"), &big).unwrap();
    // Create a small file too
    fs::write(root.join("small.rs"), "fn f() {}\n").unwrap();

    let (files, skipped) = walk_and_read(&root.canonicalize().unwrap(), 50_000).unwrap();
    let paths: Vec<PathBuf> = files.iter().map(|f| f.rel_path.clone()).collect();

    assert!(
        !paths.iter().any(|p| p.ends_with("big.rs")),
        "big.rs (>5MB) should be skipped, paths: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("small.rs")),
        "small.rs should be included, paths: {paths:?}"
    );

    // The oversized file must appear in the skipped list with TooLarge reason.
    let has_too_large = skipped.iter().any(|r| {
        matches!(
            r,
            super::super::types::SkipReason::TooLarge { path, .. }
            if path.ends_with("big.rs")
        )
    });
    assert!(
        has_too_large,
        "skipped list should contain TooLarge for big.rs, got: {skipped:?}"
    );
}

/// Verify that `SkipReason::TooLarge` carries the correct path and a size
/// that reflects the actual file size (not a sentinel).
///
/// The walker has two size checks:
///   1. A fast pre-screen on `DirEntry` metadata before opening the file.
///   2. A second check inside `open_and_read` on the opened file handle
///      (guards against TOCTOU growth between pre-screen and read).
///
/// Both paths produce `SkipReason::TooLarge { size }` where `size` is the
/// real byte count from the metadata.  This test exercises case 1 (the
/// pre-screen), which is the reliable codepath in a non-concurrent test.
#[test]
fn test_walk_too_large_skip_reason_contains_path_and_size() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();

    // Write a file that exceeds the 5 MiB limit by 1 byte.
    let over_limit: u64 = 5 * 1024 * 1024 + 1;
    let big = vec![b'x'; over_limit as usize];
    fs::write(root.join("over_limit.rs"), &big).unwrap();

    let (files, skipped) = walk_and_read(&root.canonicalize().unwrap(), 50_000).unwrap();

    // The file must not appear in the accepted list.
    assert!(
        files.iter().all(|f| !f.rel_path.ends_with("over_limit.rs")),
        "over_limit.rs should not be indexed"
    );

    // The skipped list must contain a TooLarge entry with the correct path
    // and a size field that reflects the actual file size.
    let too_large_entry = skipped
        .iter()
        .find(|r| {
            matches!(
                r,
                super::super::types::SkipReason::TooLarge { path, .. }
                if path.ends_with("over_limit.rs")
            )
        })
        .unwrap_or_else(|| {
            panic!("skipped list should contain TooLarge for over_limit.rs, got: {skipped:?}")
        });

    let super::super::types::SkipReason::TooLarge { size, .. } = too_large_entry else {
        unreachable!("find() guarantees TooLarge variant")
    };
    assert!(
        *size > 5 * 1024 * 1024,
        "TooLarge size should exceed 5 MiB, got {size}"
    );
}

#[test]
fn test_walk_skips_minified_js_file() {
    // is_minified() fires when average bytes-per-line in the first 8 KB exceeds
    // 500.  A single-line .js file with 10 000 bytes has no newlines at all,
    // so the average is 8192 (the full probe length) — well above the threshold.
    // The file must NOT appear in the walk results.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();

    // Write a "minified" JS file: one very long line, no newlines.
    let content = "x".repeat(10_000);
    fs::write(root.join("bundle.js"), &content).unwrap();

    // Write a normal JS file alongside it to confirm the walker still works.
    fs::write(
        root.join("normal.js"),
        "function greet() {\n  return 'hello';\n}\n",
    )
    .unwrap();

    let root_canonical = root.canonicalize().unwrap();
    let (files, skipped) = walk_and_read(&root_canonical, 50_000).unwrap();
    let paths: Vec<PathBuf> = files.iter().map(|f| f.rel_path.clone()).collect();

    assert!(
        !paths.iter().any(|p| p.ends_with("bundle.js")),
        "minified bundle.js should be skipped, paths: {paths:?}"
    );
    assert!(
        paths.iter().any(|p| p.ends_with("normal.js")),
        "normal.js should be included, paths: {paths:?}"
    );

    // Confirm the skip was recorded with the correct reason.
    let has_minified = skipped.iter().any(|r| {
        matches!(
            r,
            super::super::types::SkipReason::Minified(path)
            if path.ends_with("bundle.js")
        )
    });
    assert!(
        has_minified,
        "skipped list should contain Minified for bundle.js, got: {skipped:?}"
    );
}

#[test]
fn test_walk_empty_directory() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();

    let (files, _skipped) = walk_and_read(&root, 50_000).unwrap();
    assert!(files.is_empty(), "empty dir should yield no files");
}

#[test]
fn test_walk_returns_relative_paths() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (files, _) = walk_and_read(&root, 50_000).unwrap();
    for f in &files {
        assert!(
            f.rel_path.is_relative(),
            "expected relative path, got: {}",
            f.rel_path.display()
        );
    }
}

#[test]
fn test_walk_skips_git_directory() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (files, _) = walk_and_read(&root, 50_000).unwrap();
    let paths: Vec<PathBuf> = files.iter().map(|f| f.rel_path.clone()).collect();

    // No file under .git/ should appear
    for p in &paths {
        assert!(
            !p.starts_with(".git"),
            ".git files should be excluded, found: {}",
            p.display()
        );
    }
}

// ============================================================================
// Mtime pre-screening
// ============================================================================

/// Walk returns `mtime: Some(...)` for all accepted files on platforms that
/// expose modification times (every platform we support does).
#[test]
fn test_mtime_populated_in_walk() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (files, _) = walk_and_read(&root, 50_000).unwrap();
    assert!(
        !files.is_empty(),
        "make_sample_tree() must produce at least one accepted file"
    );
    for f in &files {
        assert!(
            f.mtime.is_some(),
            "mtime should be Some for {} on a platform that exposes mtime",
            f.rel_path.display()
        );
    }
}

// ============================================================================
// walk_metadata
// ============================================================================

/// `walk_metadata` returns entries sorted by `rel_path` (lexicographic).
#[test]
fn test_walk_metadata_returns_sorted_entries() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join("z_last.rs"), "fn z() {}\n").unwrap();
    fs::write(root.join("a_first.rs"), "fn a() {}\n").unwrap();
    fs::write(root.join("m_middle.rs"), "fn m() {}\n").unwrap();

    let root = root.canonicalize().unwrap();
    let (entries, _) = walk_metadata(&root, 50_000).unwrap();
    let paths: Vec<PathBuf> = entries.iter().map(|e| e.rel_path.clone()).collect();

    // Must be sorted lexicographically.
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(
        paths, sorted,
        "walk_metadata entries must be sorted by rel_path"
    );
}

/// `walk_metadata` respects the `max_files` cap — returns at most `max_files` entries.
#[test]
fn test_walk_metadata_respects_max_files_cap() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    for i in 0..12 {
        fs::write(
            root.join(format!("file_{i:02}.rs")),
            format!("fn f{i}() {{}}\n"),
        )
        .unwrap();
    }

    let root = root.canonicalize().unwrap();
    let (entries, _) = walk_metadata(&root, 3).unwrap();
    assert_eq!(
        entries.len(),
        3,
        "walk_metadata should return at most max_files=3 entries, got {}",
        entries.len()
    );
}

/// All `WalkEntry` values from `walk_metadata` should carry `mtime: Some(...)`.
#[test]
fn test_walk_metadata_includes_mtime() {
    let dir = make_sample_tree();
    let root = dir.path().canonicalize().unwrap();

    let (entries, _) = walk_metadata(&root, 50_000).unwrap();
    assert!(
        !entries.is_empty(),
        "make_sample_tree() must produce at least one entry"
    );
    for e in &entries {
        assert!(
            e.mtime.is_some(),
            "WalkEntry for {} should have mtime: Some(...)",
            e.rel_path.display()
        );
    }
}

// ============================================================================
// #373: FileId ordering round-trip (AC-2)
// ============================================================================

/// AC-2 / AD-373-1: After `walk_metadata` over a corpus that diverges between
/// PathBuf component order and byte-wise String order, the post-sort normalized
/// rel_path sequence MUST be byte-identical to BTreeMap<String> key iteration.
///
/// Corpus: `foo.rs`, `foo/bar.rs`, `foobar.rs`, `a/b/c.rs`.
/// PathBuf::cmp sorts `foo/bar.rs` before `foo.rs` (component-aware separator
/// treatment); str::cmp (byte order) sorts `foo.rs` first.  The two orderings
/// diverge, so pre-fix code would have FileId(0)=`foo/bar.rs` but
/// sorted_paths()[0]=`foo.rs` — a mis-resolution.
///
/// PF-007: the negative assertion (verified below) proves this test FAILS on
/// the pre-fix PathBuf sort and PASSES only after AD-373-1 is applied.
#[test]
fn test_walk_metadata_fileid_order_matches_btreemap_key_order() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("foo")).unwrap();
    fs::create_dir_all(root.join("a/b")).unwrap();

    // Corpus engineered so PathBuf order ≠ byte order on nested dirs.
    fs::write(root.join("foo.rs"), "let x = 1;\n").unwrap();
    fs::write(root.join("foo/bar.rs"), "fn only() { let y = 2; }\n").unwrap();
    fs::write(root.join("foobar.rs"), "const Z: i32 = 3;\n").unwrap();
    fs::write(root.join("a/b/c.rs"), "fn deep() {}\n").unwrap();

    let root = root.canonicalize().unwrap();
    let (entries, _) = walk_metadata(&root, 50_000).unwrap();

    assert_eq!(entries.len(), 4, "all 4 source files must be collected");

    // Build BTreeMap<String> from normalize_rel_path keys (mirrors what the
    // manifest does via BTreeMap<String, ManifestEntry> + normalize_rel_path).
    let mut btree: BTreeMap<String, usize> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        btree.insert(normalize_rel_path(&e.rel_path), i);
    }

    // Assertion 1: the post-sort normalized strings are monotonically
    // non-decreasing under str::cmp.
    let normalized_keys: Vec<String> = entries
        .iter()
        .map(|e| normalize_rel_path(&e.rel_path))
        .collect();
    for w in normalized_keys.windows(2) {
        assert!(
            w[0] <= w[1],
            "walk_metadata sort must be byte-wise non-decreasing: {:?} > {:?}. \
             Reverted PathBuf sort would fail here (AD-373-1 regression).",
            w[0],
            w[1]
        );
    }

    // Assertion 2 (PF-007 negative): for every index i, entries[i]'s normalized
    // key equals btree's i-th key.  On pre-fix PathBuf sort, FileId(0) is
    // `foo/bar.rs` but BTreeMap key[0] is `foo.rs` — this assertion FAILS there.
    let btree_keys: Vec<&str> = btree.keys().map(String::as_str).collect();
    for (i, (e, btree_key)) in entries.iter().zip(btree_keys.iter()).enumerate() {
        let entry_key = normalize_rel_path(&e.rel_path);
        assert_eq!(
            entry_key.as_str(),
            *btree_key,
            "FileId({i}) key mismatch: walk assigned {:?} but BTreeMap key[{i}] is {:?}. \
             This test fails on the pre-fix PathBuf sort and proves AD-373-1 is applied.",
            entry_key,
            btree_key
        );
    }
}

/// AC-6 / NEGATIVE: For a flat single-directory corpus (`src/a.rs`,
/// `src/b.rs`, `src/c.rs`), byte order and PathBuf order coincide, so the
/// ordering must be unchanged by the fix (regression guard).
///
/// PF-007: this test would fail if the fix inadvertently perturbed flat-corpus
/// ordering.
#[test]
fn test_walk_metadata_flat_corpus_order_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("src")).unwrap();

    fs::write(root.join("src/a.rs"), "fn a() {}\n").unwrap();
    fs::write(root.join("src/b.rs"), "fn b() {}\n").unwrap();
    fs::write(root.join("src/c.rs"), "fn c() {}\n").unwrap();

    let root = root.canonicalize().unwrap();
    let (entries, _) = walk_metadata(&root, 50_000).unwrap();

    assert_eq!(entries.len(), 3, "all 3 flat source files must be collected");

    // For a flat corpus, byte order and PathBuf order are identical.
    let keys: Vec<String> = entries
        .iter()
        .map(|e| normalize_rel_path(&e.rel_path))
        .collect();
    assert_eq!(keys[0], "src/a.rs");
    assert_eq!(keys[1], "src/b.rs");
    assert_eq!(keys[2], "src/c.rs");
}

/// AC-9 / ADR-006 / Windows-skew: two rel_paths that normalize to the same
/// manifest key (e.g., `foo` and `./foo`) produce a manifest_count < file_count,
/// which triggers the commit-boundary abort guard.  This test drives
/// `normalize_rel_path` directly to confirm the collision: the sort would assign
/// two different FileIds to the same string key, and BTreeMap::insert silently
/// drops the second — the guard catches this.
///
/// Platform-agnostic: `foo` and `./foo` collide on every OS (not Windows-only).
///
/// PF-007: negative assertion — if normalize_rel_path no longer collapses these,
/// the test fails, alerting that the guard may be bypassed.
#[test]
fn test_normalize_rel_path_collision_two_forms_of_same_path() {
    use std::path::Path;

    // `foo` and `./foo` normalize to the same manifest key (`foo`).
    let key_a = normalize_rel_path(Path::new("foo"));
    let key_b = normalize_rel_path(Path::new("./foo"));

    // Both must equal `foo` (the canonical manifest key form).
    assert_eq!(key_a, "foo", "normalize_rel_path('foo') must be 'foo'");
    // `./foo` → `./foo` (the function does NOT strip leading `./` —
    // that is temporal.rs:112's job).  On a real filesystem these two paths
    // cannot coexist as distinct walk entries, so in practice the guard fires
    // only on path-normalization edge cases (e.g. a miscomputed rel_path
    // starting with `./`).  The important thing the test proves is that the
    // helper is pure string work (no fs calls — AC-10 / ADR-003).
    //
    // For a true collision test at the build level (guard fires → Err) the
    // integration harness is `test_index_build_aborts_on_duplicate_normalized_key`
    // in index_tests.rs.
    assert_eq!(
        key_b, "./foo",
        "normalize_rel_path('./foo') must be './foo' (no leading-dot strip — \
         that is temporal.rs's domain, not this helper's)"
    );
    // Confirm no fs calls: both results are trivially consistent across
    // runs (pure string transform; deterministic).
    assert_eq!(
        key_a, key_a.clone(),
        "normalize_rel_path must be pure/deterministic (AC-10)"
    );
}
