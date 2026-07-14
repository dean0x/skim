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
    // AD-395-1: minified_metric requires BOTH signals:
    //   (1) content.len() >= MINIFY_MIN_BYTES (= 64 KiB)
    //   (2) first 8 KiB probe has newline_count <= 1
    // A single-line .js file with 70 000 bytes (> 64 KiB) satisfies both.
    // Pre-fix used 10 000 bytes — bumped here because 10 KB fails the size
    // gate and is now INDEXED (the ticket repro fix; R1 mitigation).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    fs::create_dir_all(root.join(".git")).unwrap();

    // Write a genuine minified JS file: one very long line, >= 64 KiB, no newlines.
    let content = "x".repeat(70_000);
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
            super::super::types::SkipReason::Minified { path, .. }
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
    let (entries, _) = walk_metadata(&root, 50_000, None).unwrap();
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
    let (entries, _) = walk_metadata(&root, 3, None).unwrap();
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

    let (entries, _) = walk_metadata(&root, 50_000, None).unwrap();
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
    let (entries, _) = walk_metadata(&root, 50_000, None).unwrap();

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
    let (entries, _) = walk_metadata(&root, 50_000, None).unwrap();

    assert_eq!(
        entries.len(),
        3,
        "all 3 flat source files must be collected"
    );

    // For a flat corpus, byte order and PathBuf order are identical.
    let keys: Vec<String> = entries
        .iter()
        .map(|e| normalize_rel_path(&e.rel_path))
        .collect();
    assert_eq!(keys[0], "src/a.rs");
    assert_eq!(keys[1], "src/b.rs");
    assert_eq!(keys[2], "src/c.rs");
}

/// AC-10 / ADR-003: pin the pure-string contract of `normalize_rel_path`.
/// The helper only folds `\\`→`/`; it performs NO filesystem-style canonicalization
/// (no leading-`./` strip, no `..` collapse). This test asserts that contract
/// directly on two literal inputs (`foo` and `./foo`) — it does NOT build an index
/// or exercise the commit-boundary guard.
///
/// Why it matters for #373: every FileId-ordering consumer (walk sort key, the
/// manifest `BTreeMap<String>` key, the lexical cache lookup) routes through this
/// one helper, so its byte-for-byte output IS the FileId↔path contract. The
/// related collision/abort behavior (two entries → one BTreeMap key →
/// manifest_count != file_count → abort) lives in `index.rs::run` and is covered
/// by the `test_adr006_*` build-level tests, not here.
#[test]
fn test_normalize_rel_path_collision_two_forms_of_same_path() {
    use std::path::Path;

    // `foo` is already the canonical manifest key form.
    let key_a = normalize_rel_path(Path::new("foo"));
    // `./foo` is NOT collapsed to `foo`: `normalize_rel_path` only folds `\\`→`/`,
    // it does not strip a leading `./` (that is `temporal::normalize_blast_radius_path`'s
    // job). So the two forms produce DISTINCT keys here — the helper does pure
    // string work with no filesystem normalization (AC-10 / ADR-003).
    let key_b = normalize_rel_path(Path::new("./foo"));

    assert_eq!(key_a, "foo", "normalize_rel_path('foo') must be 'foo'");
    // The commit-boundary guard in `index.rs::run` (manifest_count != file_count)
    // is what catches two walk entries collapsing to one BTreeMap key; on a real
    // filesystem `foo` and `./foo` cannot coexist as distinct walk entries, so
    // this unit test only pins the helper's pure-string contract, not the guard.
    assert_eq!(
        key_b, "./foo",
        "normalize_rel_path('./foo') must be './foo' (no leading-dot strip — \
         that is temporal::normalize_blast_radius_path's domain, not this helper's)"
    );
}

// ============================================================================
// AC1 — minified_metric predicate + two-signal gate (#395)
// ============================================================================

/// AC1 (predicate, two-signal, AD-395-1): minified_metric returns None for the
/// 1.4 KB ticket repro (fails size gate), Some for a >= 64 KiB single-line
/// string (both signals), and None for a >= 64 KiB multi-line string (fails the
/// single-line signal). Also asserts MINIFY_MIN_BYTES == 8 * MINIFY_PROBE_BYTES.
#[test]
fn test_minified_metric_two_signal_gate() {
    use super::super::walk::{MINIFY_MIN_BYTES, minified_metric};

    // Compile-time guard: must equal 8 × 8192 = 65_536.
    assert_eq!(
        MINIFY_MIN_BYTES,
        8 * 8192,
        "MINIFY_MIN_BYTES must equal 8 * MINIFY_PROBE_BYTES (= 65_536)"
    );

    // (a) Ticket repro: "A"*700 + " NEEDLE_LONGLINE " + "B"*700 ≈ 1.4 KB.
    // Fails signal 1 (size gate) → None.
    let repro: String = "A".repeat(700) + " NEEDLE_LONGLINE " + &"B".repeat(700);
    assert!(
        repro.len() < MINIFY_MIN_BYTES,
        "repro must be < MINIFY_MIN_BYTES for this test to be meaningful"
    );
    assert!(
        minified_metric(&repro).is_none(),
        "1.4 KB single-line string must return None (fails size gate)"
    );

    // (b) 70 KB single-line: both signals → Some(avg > 500).
    let big_single_line: String = "x".repeat(70_000);
    let metric = minified_metric(&big_single_line);
    assert!(
        metric.is_some(),
        "70 KB single-line string must return Some(avg)"
    );
    assert!(
        metric.unwrap() > 500,
        "avg_line_bytes must exceed 500, got {:?}",
        metric
    );

    // (c) 65_535 bytes single-line: one byte below size gate → None.
    let just_below: String = "x".repeat(65_535);
    assert!(
        minified_metric(&just_below).is_none(),
        "65_535-byte string must return None (fails size gate by 1 byte)"
    );

    // (d) 65_537 bytes single-line: one byte above size gate → Some.
    let just_above: String = "x".repeat(65_537);
    assert!(
        minified_metric(&just_above).is_some(),
        "65_537-byte string must return Some (passes both signals)"
    );

    // (e) >= 64 KiB but multi-line probe (newline_count > 1 in first 8 KiB):
    // a 120 KB file made of 200 lines × 600 bytes each has ~13 newlines in the
    // first 8 KiB probe — fails the single-line signal → None (indexed).
    let multiline: String = ("y".repeat(600) + "\n").repeat(200);
    assert!(
        multiline.len() >= MINIFY_MIN_BYTES,
        "multiline fixture must be >= MINIFY_MIN_BYTES"
    );
    assert!(
        minified_metric(&multiline).is_none(),
        ">= 64 KiB multi-line string must return None (fails single-line signal — INDEXED)"
    );

    // (f) >= 64 KiB with EXACTLY ONE newline in the 8 KiB probe: newline_count == 1
    // satisfies the `<= 1` boundary and must still be classified as minified.
    // This pins the `<= 1` semantic: a regression narrowing the check to `== 0`
    // (reading "single-line" as "zero newlines") would make this case return None
    // and would be caught here.
    let mut one_newline_in_probe = "z".repeat(4_000);
    one_newline_in_probe.push('\n'); // exactly one newline inside the 8 KiB probe
    one_newline_in_probe.push_str(&"z".repeat(70_000));
    assert!(
        one_newline_in_probe.len() >= MINIFY_MIN_BYTES,
        "one-newline fixture must be >= MINIFY_MIN_BYTES"
    );
    assert!(
        minified_metric(&one_newline_in_probe).is_some(),
        ">= 64 KiB file with exactly one newline in the probe must return Some \
         (newline_count == 1 satisfies the <= 1 boundary — still minified)"
    );
}

/// OD-395-4: persist_discriminant must return None for non-deterministic skip
/// reasons so they are never durably excluded from the manifest skip-set.
///
/// If a future refactor caused ReadError to return Some(_), a transiently-
/// unreadable file would be persisted and durably excluded until its mtime/size
/// changed — silently reintroducing the drop bug.  This test guards that path.
#[test]
fn test_persist_discriminant_non_deterministic_returns_none() {
    use super::super::types::SkipReason;
    use std::path::PathBuf;

    // ReadError is transient — must NOT be persisted.
    assert!(
        SkipReason::ReadError {
            path: PathBuf::from("a.rs"),
            error: "io error".to_string(),
        }
        .persist_discriminant()
        .is_none(),
        "ReadError::persist_discriminant must return None (OD-395-4)"
    );

    // UnsupportedLanguage depends on configured language support — must NOT be persisted.
    assert!(
        SkipReason::UnsupportedLanguage(PathBuf::from("a.xyz"))
            .persist_discriminant()
            .is_none(),
        "UnsupportedLanguage::persist_discriminant must return None (OD-395-4)"
    );

    // CapReached is a sampling artifact, not a content property — must NOT be persisted.
    assert!(
        SkipReason::CapReached.persist_discriminant().is_none(),
        "CapReached::persist_discriminant must return None"
    );

    // Positive: deterministic reasons MUST be persisted.
    assert!(
        SkipReason::Minified {
            path: PathBuf::from("bundle.js"),
            avg_line_bytes: 1_200,
        }
        .persist_discriminant()
        .is_some(),
        "Minified::persist_discriminant must return Some"
    );
    assert!(
        SkipReason::NonUtf8(PathBuf::from("icon.bin"))
            .persist_discriminant()
            .is_some(),
        "NonUtf8::persist_discriminant must return Some"
    );
    assert!(
        SkipReason::TooLarge {
            path: PathBuf::from("dump.log"),
            size: 200_000_000,
        }
        .persist_discriminant()
        .is_some(),
        "TooLarge::persist_discriminant must return Some"
    );
}

/// AC1 negative: a small long-line file that was previously dropped is now indexed.
#[test]
fn test_small_long_line_file_is_indexed() {
    use super::super::walk::walk_and_read;
    use std::fs;
    use std::path::PathBuf;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join(".git")).unwrap();

    // Write the ticket repro: single long line, ~1.4 KB, a Python file.
    let content: String = "A".repeat(700) + " NEEDLE_LONGLINE " + &"B".repeat(700);
    fs::write(root.join("longline.py"), &content).unwrap();

    let root_canonical = root.canonicalize().unwrap();
    let (files, skipped) = walk_and_read(&root_canonical, 50_000).unwrap();
    let paths: Vec<PathBuf> = files.iter().map(|f| f.rel_path.clone()).collect();

    // POSITIVE: the small long-line file must be indexed (pre-fix: was skipped).
    assert!(
        paths.iter().any(|p| p.ends_with("longline.py")),
        "small 1.4 KB long-line file must be INDEXED, paths: {paths:?}"
    );

    // NEGATIVE: it must NOT appear in the skip list.
    let was_skipped = skipped.iter().any(|r| {
        matches!(
            r,
            super::super::types::SkipReason::Minified { path, .. }
            if path.ends_with("longline.py")
        )
    });
    assert!(
        !was_skipped,
        "small long-line file must NOT be in the minified skip list"
    );
}

// ============================================================================
// #402 — git-tracked union (AC6, AC9, AC11, AC13 unit)
// ============================================================================

/// AC6 (#402) — Determinism: two consecutive `walk_metadata` calls on a repo
/// with a tracked-but-.gitignored file return byte-identical `rel_path` sequences.
///
/// PF-012: selection is an order-invariant SET function of the complete
/// candidate set (walked ∪ tracked). Two consecutive calls MUST return the
/// same membership AND the same order (sort by normalize_rel_path, ascending).
#[test]
fn test_ac6_402_walk_metadata_determinism_with_union() {
    let dir = super::super::tests::make_tracked_ignored_repo();
    let root = dir.path().canonicalize().unwrap();

    let (entries1, _) = walk_metadata(&root, 50_000, None).unwrap();
    let (entries2, _) = walk_metadata(&root, 50_000, None).unwrap();

    let keys1: Vec<String> = entries1
        .iter()
        .map(|e| normalize_rel_path(&e.rel_path))
        .collect();
    let keys2: Vec<String> = entries2
        .iter()
        .map(|e| normalize_rel_path(&e.rel_path))
        .collect();

    assert_eq!(
        keys1, keys2,
        "AC6: two consecutive walk_metadata calls must return byte-identical rel_path sequences \
         (PF-012 / AD-402-2)"
    );

    // The tracked-but-gitignored file must appear in both results.
    assert!(
        keys1.iter().any(|k| k == "secretdoc.md"),
        "AC6: secretdoc.md (tracked-but-gitignored) must appear in walk_metadata output; \
         keys: {keys1:?}"
    );
}

/// AC9 (#402) — `.git/**` is never unioned: the gix index never enumerates
/// `.git/` internal files, so the union cannot introduce .git/ paths.
///
/// Extends the pre-existing `test_walk_skips_git_directory` invariant: the
/// union must not break that guarantee.
#[test]
fn test_ac9_402_walk_metadata_never_unions_dot_git() {
    let dir = super::super::tests::make_tracked_ignored_repo();
    let root = dir.path().canonicalize().unwrap();

    let (entries, _) = walk_metadata(&root, 50_000, None).unwrap();

    for e in &entries {
        let key = normalize_rel_path(&e.rel_path);
        assert!(
            !key.starts_with(".git/"),
            "AC9: no .git/ entry must appear in walk_metadata output after union; \
             found: {key}"
        );
    }
}

/// AC11 (#402) — Symlink/gitlink tracked entries are Transparent (skipped).
///
/// `classify_tracked_path` uses `symlink_metadata` (not `metadata`), so a
/// tracked symlink returns `Transparent` via the `!is_file()` guard.
/// Gitlink entries (submodule COMMIT mode) are pre-filtered in
/// `list_tracked_files`, so they never reach `classify_tracked_path`.
#[cfg(unix)]
#[test]
fn test_ac11_402_classify_tracked_path_symlink_transparent() {
    use std::os::unix::fs::symlink;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();

    // A real file (should be Accept).
    let real_rs = root.join("a.rs");
    fs::write(&real_rs, "fn a() {}\n").unwrap();

    // A symlink to the real file (should be Transparent — not a regular file).
    let link_rs = root.join("link.rs");
    symlink(&real_rs, &link_rs).unwrap();

    // Classify the real file via classify_tracked_path → must be Accept.
    let outcome_real = super::classify_tracked_path(&real_rs, &root);
    assert!(
        matches!(outcome_real, super::ClassifyOutcome::Accept(_)),
        "AC11: regular file must be Accept via classify_tracked_path"
    );

    // Classify the symlink → must be Transparent (not a regular file).
    let outcome_link = super::classify_tracked_path(&link_rs, &root);
    assert!(
        matches!(outcome_link, super::ClassifyOutcome::Transparent),
        "AC11: symlink must be Transparent via classify_tracked_path (not followed)"
    );
}

/// AC13 unit (i) (#402) — Memo cache HIT on unchanged `.git/index`: a second
/// call to `tracked_files_memoized` does NOT re-invoke the live gix read.
///
/// Discriminating: reverting the memoization (always re-parsing) makes
/// `ENUM_CALL_COUNT` reach 2 after the second call, failing this assertion.
///
/// AC13 unit (ii) — After `git add -f newsecret.md` (which rewrites
/// `.git/index`), the NEXT call is a cache MISS and enumerates the new file.
#[test]
fn test_ac13_402_memo_cache_hit_and_miss() {
    let dir = super::super::tests::make_tracked_ignored_repo();
    let root = dir.path().canonicalize().unwrap();

    // Use an isolated temp dir as the sidecar cache and pass it directly to
    // tracked_files_memoized — no SKIM_CACHE_DIR mutation needed, eliminating
    // the concurrent-setenv/getenv data-race window (#[serial] only serialises
    // against other #[serial] tests, not against parallel non-serial ones).
    // The tempdir already exists, so no create_dir_all is required.
    let cache_tmp = tempfile::tempdir().unwrap();
    let cache_dir = cache_tmp.path().to_path_buf();

    // Reset the per-thread test counter.
    super::ENUM_CALL_COUNT.with(|c| c.set(0));

    // --- AC13(i): cache HIT on unchanged .git/index ---

    // First call: cold sidecar → cache MISS → gix is called once.
    // Pass Some(&cache_dir) so tracked_files_memoized reuses the already-
    // resolved cache dir (finding: tracked-memo-redundant-resolve).
    let result1 = super::tracked_files_memoized(&root, Some(&cache_dir));
    assert!(result1.is_some(), "AC13(i): first call must return Some");
    let count1 = super::ENUM_CALL_COUNT.with(std::cell::Cell::get);
    assert_eq!(
        count1, 1,
        "AC13(i): first call must be a cache miss (gix called exactly once)"
    );

    // Second call: .git/index unchanged → sidecar hit → gix NOT called again.
    let result2 = super::tracked_files_memoized(&root, Some(&cache_dir));
    assert!(result2.is_some(), "AC13(i): second call must return Some");
    let count2 = super::ENUM_CALL_COUNT.with(std::cell::Cell::get);
    assert_eq!(
        count2, 1,
        "AC13(i): second call must be a cache HIT (ENUM_CALL_COUNT stays at 1)"
    );

    // Both calls must return the same set of paths.
    assert_eq!(
        result1.unwrap(),
        result2.unwrap(),
        "AC13(i): cache hit must return same path list as first enumeration"
    );

    // --- AC13(ii): cache MISS after git add (rewrites .git/index) ---

    // Add a new tracked-but-.gitignored file — this rewrites .git/index.
    fs::write(dir.path().join("newsecret.md"), "NEWSECRETTOKEN\n").unwrap();
    std::process::Command::new("git")
        .args(["add", "-f", "newsecret.md"])
        .current_dir(dir.path())
        .output()
        .expect("git add newsecret.md");

    // Third call: .git/index has changed → cache MISS → gix is called again.
    let result3 = super::tracked_files_memoized(&root, Some(&cache_dir));
    assert!(
        result3.is_some(),
        "AC13(ii): post-git-add call must return Some"
    );
    let count3 = super::ENUM_CALL_COUNT.with(std::cell::Cell::get);
    assert_eq!(
        count3, 2,
        "AC13(ii): post-git-add call must be a cache MISS (ENUM_CALL_COUNT increments to 2)"
    );

    // The newly-staged file must appear in the result.
    let paths3 = result3.unwrap();
    assert!(
        paths3.iter().any(|p| p.ends_with("newsecret.md")),
        "AC13(ii): newsecret.md must appear after cache miss + re-enumeration; \
         paths: {paths3:?}"
    );
}

/// resolve_git_index_path: regular repo returns .git/index, linked worktree
/// reads the gitdir from the .git file and returns <gitdir>/index.
///
/// Regression for finding: worktree-memo-bypass — in a linked worktree .git is
/// a plain file so root/.git/index never exists, causing tracked_files_memoized
/// to always fall through to a live list_tracked_files call on every query and
/// silently losing the AD-402-6 memoization bound.
#[test]
fn test_resolve_git_index_path_regular_repo() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let git_dir = root.join(".git");
    fs::create_dir_all(&git_dir).unwrap();
    // Create the index file so the path is valid.
    fs::write(git_dir.join("index"), b"").unwrap();

    let idx = super::resolve_git_index_path(root)
        .expect("resolve_git_index_path must return Some for a regular repo");
    assert_eq!(
        idx,
        root.join(".git").join("index"),
        "regular repo: git index must be at .git/index"
    );
}

#[test]
fn test_resolve_git_index_path_linked_worktree() {
    // Simulate a linked worktree where .git is a plain file containing a gitdir
    // reference. The gitdir path may be absolute (normal) or relative (defensive
    // fallback). We test the absolute case here (by far the most common).
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    // The "real" git dir (normally somewhere like /path/to/main/.git/worktrees/wt).
    let fake_gitdir = tempfile::tempdir().unwrap();
    let fake_index = fake_gitdir.path().join("index");
    fs::write(&fake_index, b"").unwrap();

    // Write a .git file (not a directory) pointing at fake_gitdir.
    let gitdir_str = fake_gitdir.path().to_str().unwrap();
    fs::write(root.join(".git"), format!("gitdir: {gitdir_str}\n")).unwrap();

    let idx = super::resolve_git_index_path(root)
        .expect("resolve_git_index_path must return Some for a linked worktree");
    assert_eq!(
        idx,
        fake_gitdir.path().join("index"),
        "linked worktree: git index must be at <gitdir>/index, not root/.git/index"
    );
}

#[test]
fn test_resolve_git_index_path_no_git() {
    // A directory with no .git entry (not a git repo) must return None.
    let dir = tempfile::tempdir().unwrap();
    let result = super::resolve_git_index_path(dir.path());
    assert!(
        result.is_none(),
        "resolve_git_index_path must return None when .git does not exist"
    );
}

/// AC14 (#402) — Design decisions are traceable in source code.
///
/// Grep walk.rs for `AD-402-1` through `AD-402-6` each at their documented
/// call site. A missing marker means a design decision is no longer anchored
/// to the code that implements it.
#[test]
fn test_ac14_402_ad_series_comments_present() {
    let walk_src = include_str!("walk.rs");

    for marker in [
        "AD-402-1", "AD-402-2", "AD-402-3", "AD-402-4", "AD-402-5", "AD-402-6",
    ] {
        assert!(
            walk_src.contains(marker),
            "AC14: walk.rs must contain the design-decision anchor `{marker}` at its call site"
        );
    }
}

// ============================================================================
// #402 — fail-soft branch coverage (review findings)
// ============================================================================

/// parse_tracked_union_cache: corrupt/truncated sidecar returns None and
/// triggers live re-enumeration (fail-soft).
///
/// The sidecar is an optimization cache with no format version; any parse
/// failure must return None (cache miss → re-enumerate), never panic or
/// return a partial list.  AC13 covers fingerprint-mismatch invalidation;
/// this test covers the content-corruption paths that AC13 does not exercise.
#[test]
fn test_parse_tracked_union_cache_corrupt_and_truncated() {
    // parse_tracked_union_cache now accepts &[u8] (byte-lossless for non-UTF-8
    // paths — finding: sidecar-lossy-round-trip). Use byte literals throughout.
    let parse = |s: &[u8], mt: u64, ln: u64| super::parse_tracked_union_cache(s, mt, ln);

    // Empty byte slice: no header at all → None (re-enumerate).
    assert!(
        parse(b"", 100, 512).is_none(),
        "empty sidecar must return None"
    );

    // Garbled header (no space separator between mtime and len) → None.
    assert!(
        parse(b"not_a_valid_header\0path.rs\0", 100, 512).is_none(),
        "garbled header must return None"
    );

    // Second token in header is not a number → None.
    assert!(
        parse(b"100 notanumber\0path.rs\0", 100, 512).is_none(),
        "non-numeric len field must return None"
    );

    // Correct numeric header but fingerprint mismatch → None (cache miss).
    // This covers the mtime-differs direction.
    assert!(
        parse(b"999 512\0path.rs\0", 100, 512).is_none(),
        "mtime mismatch must return None"
    );
    // And the len-differs direction.
    assert!(
        parse(b"100 999\0path.rs\0", 100, 512).is_none(),
        "len mismatch must return None"
    );

    // Truncated: correct header only (no path records) → Some([]).
    // Verifies that a truncated-after-header write is handled as an empty
    // but valid cache hit (not treated as corrupt).
    assert_eq!(
        parse(b"100 512\0", 100, 512),
        Some(vec![]),
        "truncated-to-header sidecar with matching fingerprint must return Some([])"
    );

    // Valid single path: basic round-trip.
    assert_eq!(
        parse(b"100 512\0src/main.rs\0", 100, 512),
        Some(vec![std::path::PathBuf::from("src/main.rs")]),
        "valid single-entry sidecar must parse correctly"
    );

    // Valid multi-path (confirms NUL framing, not newline).
    assert_eq!(
        parse(b"100 512\0foo.rs\0bar.rs\0", 100, 512),
        Some(vec![
            std::path::PathBuf::from("foo.rs"),
            std::path::PathBuf::from("bar.rs"),
        ]),
        "valid two-entry sidecar must parse both paths (NUL framing)"
    );
}

/// Regression guard for the sidecar-lossy-round-trip fix (AD-379-1).
///
/// `format_tracked_union_cache` encodes paths via `OsStrExt::as_bytes` on Unix
/// so that non-UTF-8 filenames survive the sidecar write → read cycle
/// byte-losslessly.  If the implementation regressed to `to_string_lossy`, each
/// `\xFF` or `\xFE` byte would become the three-byte UTF-8 sequence `\xEF\xBF\xBD`
/// (U+FFFD replacement character), making the round-trip assertion below fail.
///
/// PF-007: all paths in `test_parse_tracked_union_cache_corrupt_and_truncated`
/// are ASCII, so that test would pass even after such a regression.  This test
/// feeds a non-UTF-8 `PathBuf` through the full format→parse pipeline and
/// asserts that the raw `OsStr` bytes are preserved.
#[cfg(unix)]
#[test]
fn test_format_parse_tracked_union_cache_non_utf8_round_trip() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    // Raw bytes that are NOT valid UTF-8 (0xFF, 0xFE are illegal UTF-8 lead bytes).
    // On Linux/macOS, filenames with such bytes are legal at the OS level.
    let raw_bytes: &[u8] = b"\xFF\xFEnon_utf8_file.rs";
    let non_utf8_path = PathBuf::from(OsStr::from_bytes(raw_bytes));

    // Serialize via format_tracked_union_cache — must use OsStrExt::as_bytes.
    let formatted = super::format_tracked_union_cache(1_000_000, 4096, &[non_utf8_path]);

    // Deserialize via parse_tracked_union_cache — must use OsStr::from_bytes.
    let parsed = super::parse_tracked_union_cache(&formatted, 1_000_000, 4096)
        .expect("format→parse round-trip must succeed for a non-UTF-8 path");

    assert_eq!(parsed.len(), 1, "round-trip must produce exactly one path");

    // The critical assertion: raw OsStr bytes must be bit-identical after the
    // sidecar round-trip.  A regression to to_string_lossy would encode \xFF as
    // \xEF\xBF\xBD (U+FFFD in UTF-8), making this assertion fail and proving that
    // AD-379-1 build↔scan set-identity would be broken for non-UTF-8 filenames
    // (they would be silently dropped from the union on any subsequent query that
    // hit the sidecar cache).
    assert_eq!(
        parsed[0].as_os_str().as_bytes(),
        raw_bytes,
        "non-UTF-8 path bytes must survive format→parse byte-losslessly \
         (AD-379-1 / sidecar-lossy-round-trip fix); a regression to \
         to_string_lossy replaces \\xFF/\\xFE with U+FFFD here"
    );
}

/// list_tracked_files: SKIP_WORKTREE entries are excluded from the result.
///
/// `git update-index --skip-worktree <file>` marks a committed file as
/// intentionally absent from the work tree (sparse checkout etc.).
/// `list_tracked_files` must not return it — there is nothing to read at
/// that path on disk (AD-402-5: skip-worktree entries not materialized).
#[test]
fn test_list_tracked_files_skip_worktree_excluded() {
    use std::path::Path;
    use std::process::Command as StdCmd;

    let dir = tempfile::tempdir().unwrap();
    let root_path = dir.path();

    let git = |args: &[&str]| {
        StdCmd::new("git")
            .args(args)
            .current_dir(root_path)
            .output()
            .unwrap()
    };

    git(&["init"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "T"]);

    fs::write(root_path.join("visible.rs"), "fn v() {}\n").unwrap();
    fs::write(root_path.join("sparse.rs"), "fn s() {}\n").unwrap();
    git(&["add", "visible.rs", "sparse.rs"]);
    git(&["commit", "-m", "init"]);

    // Mark sparse.rs as SKIP_WORKTREE (simulates sparse-checkout intent).
    git(&["update-index", "--skip-worktree", "sparse.rs"]);

    let root = root_path.canonicalize().unwrap();
    let tracked = super::list_tracked_files(&root)
        .expect("list_tracked_files must succeed on a valid git repo");

    assert!(
        tracked.iter().any(|p| p == Path::new("visible.rs")),
        "visible.rs must appear in list_tracked_files; tracked: {tracked:?}"
    );
    assert!(
        !tracked.iter().any(|p| p == Path::new("sparse.rs")),
        "SKIP_WORKTREE entry 'sparse.rs' must be excluded (AD-402-5); tracked: {tracked:?}"
    );
}

/// list_tracked_files: gitlink (submodule) entries are excluded from the result.
///
/// A git submodule appears in the parent's index as a Mode::COMMIT entry
/// (mode 0o160000).  `list_tracked_files` must skip these — they are directory
/// pointers in the work tree, not indexable source files (AD-402-5).
///
/// Two local git repos are used so `git submodule add` works without a remote.
/// `protocol.file.allow=always` is set to permit file-URI submodules in
/// git >= 2.38.1 where file-protocol is restricted by default.
#[test]
fn test_list_tracked_files_gitlink_excluded() {
    use std::path::Path;
    use std::process::Command as StdCmd;

    // Helper closure: run git in a specific dir.
    let git = |args: &[&str], cwd: &std::path::Path| {
        let out = StdCmd::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        out
    };

    // Build a minimal "remote" repo that the submodule will point to.
    let remote_dir = tempfile::tempdir().unwrap();
    let remote = remote_dir.path();
    git(&["init"], remote);
    git(&["config", "user.email", "t@t"], remote);
    git(&["config", "user.name", "T"], remote);
    fs::write(remote.join("dummy.rs"), "fn x() {}\n").unwrap();
    git(&["add", "dummy.rs"], remote);
    git(&["commit", "-m", "init"], remote);

    // Build the parent repo that will contain the submodule.
    let parent_dir = tempfile::tempdir().unwrap();
    let parent = parent_dir.path();
    git(&["init"], parent);
    git(&["config", "user.email", "t@t"], parent);
    git(&["config", "user.name", "T"], parent);
    fs::write(parent.join("real.rs"), "fn main() {}\n").unwrap();
    git(&["add", "real.rs"], parent);
    git(&["commit", "-m", "init"], parent);

    // Add a submodule from the local remote (file-protocol, needs allow=always).
    let remote_path = remote.to_str().unwrap();
    let add_out = git(
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            remote_path,
            "sub",
        ],
        parent,
    );

    // If `git submodule add` is unavailable or fails (restrictive CI env),
    // skip rather than fail — this test covers defensive filtering, not the
    // submodule feature itself.
    if !add_out.status.success() {
        eprintln!(
            "skipping test_list_tracked_files_gitlink_excluded: \
             git submodule add failed ({}); stderr: {}",
            add_out.status,
            String::from_utf8_lossy(&add_out.stderr)
        );
        return;
    }

    git(
        &[
            "-c",
            "protocol.file.allow=always",
            "commit",
            "-m",
            "add submodule",
        ],
        parent,
    );

    let root = parent.canonicalize().unwrap();
    let tracked = super::list_tracked_files(&root)
        .expect("list_tracked_files must succeed on a valid git repo");

    // `sub` is a gitlink (COMMIT mode) — must NOT appear as a path entry.
    assert!(
        !tracked.iter().any(|p| p == Path::new("sub")),
        "gitlink 'sub' must be excluded (Mode::COMMIT filter, AD-402-5); tracked: {tracked:?}"
    );
    // The real source file must still be present.
    assert!(
        tracked.iter().any(|p| p == Path::new("real.rs")),
        "real.rs must appear in list_tracked_files; tracked: {tracked:?}"
    );
}

/// merge_tracked_union must not append a duplicate SkipReason for a tracked
/// file that the ignore-walk already classified as Skip (e.g. TooLarge).
///
/// Without the skip-dedup fix, the union re-classifies the file (one extra
/// symlink_metadata syscall) and appends a second TooLarge entry.  With the
/// fix, the `yielded` set is pre-seeded from the skip list so the union
/// dedup short-circuits before calling classify_tracked_path.
///
/// The indexed entry set is unchanged either way (bounded by MAX_SKIP_REASONS,
/// never an Accept); this test covers the cosmetic / syscall-waste aspect.
#[test]
fn test_merge_tracked_union_no_duplicate_skip_for_already_skipped() {
    use super::super::types::SkipReason;
    use std::process::Command as StdCmd;

    let dir = tempfile::tempdir().unwrap();
    let root_path = dir.path();

    let git = |args: &[&str]| {
        StdCmd::new("git")
            .args(args)
            .current_dir(root_path)
            .output()
            .unwrap()
    };

    git(&["init"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "T"]);

    fs::write(root_path.join("real.rs"), "fn main() {}\n").unwrap();

    // Write a file large enough to trigger TooLarge (> 5 MiB).
    let big = vec![b'a'; 6 * 1024 * 1024];
    fs::write(root_path.join("huge.rs"), &big).unwrap();

    git(&["add", "real.rs", "huge.rs"]);
    git(&["commit", "-m", "init"]);

    let root = root_path.canonicalize().unwrap();

    // walk_metadata runs the ignore-walk (classifies huge.rs as TooLarge →
    // goes into skips) and then calls merge_tracked_union (which sees huge.rs
    // in the tracked list but should dedup against the skip list and NOT
    // re-classify it).
    let (_entries, skips) = walk_metadata(&root, 50_000, None).unwrap();

    let huge_skip_count = skips
        .iter()
        .filter(|r| matches!(r, SkipReason::TooLarge { path, .. } if path.ends_with("huge.rs")))
        .count();

    assert_eq!(
        huge_skip_count, 1,
        "huge.rs must appear in the skip list exactly once \
         (no duplicate from union re-classification); all skips: {skips:?}"
    );
}

/// merge_tracked_union sort+truncate branch fires when walked ∪ tracked > max_files.
///
/// AC6 (#402) only verifies determinism at max_files=50_000, so the `entries.sort`
/// + `entries.truncate(max_files)` branch in `merge_tracked_union` is never
/// exercised with union entries actually present AND the cap actually binding.
///
/// PF-012: selection is "the max_files smallest keys over the complete candidate
/// set" (an order-invariant SET function), not "the first max_files visited".
/// This test verifies the post-union truncation keeps the SMALLEST-keyed entries
/// and drops the LARGEST-keyed ones, even when those larger entries were produced
/// by the ignore-walk and the smaller ones came via the tracked union.
///
/// Setup:
///   - `.gitignore` hides `a_secret.rs` and `b_secret.rs`; both are force-added
///     so they appear in the git index → tracked union adds them.
///   - Walk finds: `c_visible.rs`, `d_visible.rs`, `e_visible.rs` (3 entries).
///   - max_files = 3: walk fills the cap; union adds 2 more → 5 total.
///   - sort+truncate(3) → [a_secret.rs, b_secret.rs, c_visible.rs].
///   - `d_visible.rs` and `e_visible.rs` (the two largest keys) are dropped.
#[test]
fn test_merge_tracked_union_truncates_to_smallest_k() {
    use std::process::Command as StdCmd;

    let dir = tempfile::tempdir().unwrap();
    let root_path = dir.path();

    let git = |args: &[&str]| {
        StdCmd::new("git")
            .args(args)
            .current_dir(root_path)
            .output()
            .unwrap()
    };

    git(&["init"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "T"]);

    // .gitignore hides a_secret.rs and b_secret.rs from the ignore-walk, so
    // those files can only enter via the tracked union path.
    fs::write(root_path.join(".gitignore"), "a_secret.rs\nb_secret.rs\n").unwrap();
    fs::write(root_path.join("a_secret.rs"), "fn a() {}\n").unwrap();
    fs::write(root_path.join("b_secret.rs"), "fn b() {}\n").unwrap();
    // These three files are NOT gitignored; the ignore-walk finds all of them.
    fs::write(root_path.join("c_visible.rs"), "fn c() {}\n").unwrap();
    fs::write(root_path.join("d_visible.rs"), "fn d() {}\n").unwrap();
    fs::write(root_path.join("e_visible.rs"), "fn e() {}\n").unwrap();

    // Force-add the gitignored files so they appear in the git index.
    git(&[
        "add",
        "-f",
        ".gitignore",
        "a_secret.rs",
        "b_secret.rs",
        "c_visible.rs",
        "d_visible.rs",
        "e_visible.rs",
    ]);
    git(&["commit", "-m", "init"]);

    let root = root_path.canonicalize().unwrap();

    // max_files = 3.  Walk produces [c_visible.rs, d_visible.rs, e_visible.rs]
    // (the only non-gitignored source files, 3 total — exactly filling the cap).
    // Tracked union adds a_secret.rs and b_secret.rs → 5 total.
    // merge_tracked_union: sort_by_cached_key → truncate(3)
    //   → [a_secret.rs, b_secret.rs, c_visible.rs].
    // d_visible.rs and e_visible.rs (the two largest normalize_rel_path keys)
    // must be dropped.
    let (entries, _) = walk_metadata(&root, 3, None).unwrap();

    let keys: Vec<String> = entries
        .iter()
        .map(|e| normalize_rel_path(&e.rel_path))
        .collect();

    assert_eq!(
        keys.len(),
        3,
        "after union+truncation to max_files=3 the result must contain exactly 3 entries; \
         got: {keys:?}"
    );

    // The three smallest normalize_rel_path keys must be retained.
    assert!(
        keys.iter().any(|k| k == "a_secret.rs"),
        "a_secret.rs (union entry, smallest key 'a') must be retained; keys: {keys:?}"
    );
    assert!(
        keys.iter().any(|k| k == "b_secret.rs"),
        "b_secret.rs (union entry, second-smallest key 'b') must be retained; keys: {keys:?}"
    );
    assert!(
        keys.iter().any(|k| k == "c_visible.rs"),
        "c_visible.rs (third-smallest key 'c') must be retained; keys: {keys:?}"
    );

    // The two largest-keyed entries must be absent — dropped by truncation.
    // These were produced by the ignore-walk; the union entries displaced them.
    assert!(
        !keys.iter().any(|k| k == "d_visible.rs"),
        "d_visible.rs must be dropped by truncation (4th-smallest key); keys: {keys:?}"
    );
    assert!(
        !keys.iter().any(|k| k == "e_visible.rs"),
        "e_visible.rs must be dropped by truncation (largest key); keys: {keys:?}"
    );

    // Entries must be sorted ascending by normalize_rel_path (AD-402-2 / PF-012).
    let mut sorted_keys = keys.clone();
    sorted_keys.sort();
    assert_eq!(
        keys, sorted_keys,
        "walk_metadata must return entries sorted ascending by normalize_rel_path \
         after union+truncation (AD-402-2 / PF-012)"
    );
}
