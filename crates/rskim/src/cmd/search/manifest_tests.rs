//! Tests for the manifest sidecar (manifest.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::ops::Range;

use super::{FileManifest, ManifestEntry, decode_field_map, encode_field_map};
use rskim_search::SearchField;

// ============================================================================
// Helpers
// ============================================================================

fn sample_field_map() -> Vec<(Range<usize>, SearchField)> {
    vec![
        (0..10, SearchField::FunctionSignature),
        (10..30, SearchField::FunctionBody),
        (30..50, SearchField::Comment),
    ]
}

fn sample_entry(path: &str, sha256: &str) -> ManifestEntry {
    ManifestEntry {
        path: path.to_string(),
        sha256: sha256.to_string(),
        lang: "rust".to_string(),
        field_map: encode_field_map(&sample_field_map()),
        mtime: None,
        size: None,
    }
}

// ============================================================================
// FileManifest::new / empty
// ============================================================================

#[test]
fn test_manifest_empty_has_no_entries() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = FileManifest::new(dir.path().to_path_buf(), dir.path().to_path_buf());
    assert!(manifest.lookup("any_file.rs").is_none());
}

// ============================================================================
// save / load roundtrip
// ============================================================================

#[test]
fn test_manifest_roundtrip_single_entry() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().to_path_buf();

    let entry = sample_entry(
        "src/main.rs",
        "aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111aaaa1111",
    );
    let mut manifest = FileManifest::new(root.clone(), cache_dir.clone());
    manifest.insert(entry.clone());
    manifest.save().unwrap();

    let loaded = FileManifest::load(root, cache_dir).unwrap();
    let found = loaded.lookup("src/main.rs").unwrap();
    assert_eq!(found.sha256, entry.sha256);
    assert_eq!(found.lang, "rust");
}

#[test]
fn test_manifest_roundtrip_multiple_entries() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().to_path_buf();

    let entries: Vec<ManifestEntry> = (0..5)
        .map(|i| ManifestEntry {
            path: format!("src/file_{i}.rs"),
            sha256: format!("{:0>64}", i),
            lang: "rust".to_string(),
            field_map: encode_field_map(&sample_field_map()),
            mtime: None,
            size: None,
        })
        .collect();

    let mut manifest = FileManifest::new(root.clone(), cache_dir.clone());
    for entry in &entries {
        manifest.insert(entry.clone());
    }
    manifest.save().unwrap();

    let loaded = FileManifest::load(root, cache_dir).unwrap();
    for entry in &entries {
        let found = loaded.lookup(&entry.path);
        assert!(found.is_some(), "should find {}", entry.path);
        assert_eq!(found.unwrap().sha256, entry.sha256);
    }
}

// ============================================================================
// field_map encoding roundtrip
// ============================================================================

#[test]
fn test_field_map_encoding_roundtrip() {
    let original = sample_field_map();
    let encoded = encode_field_map(&original);
    let decoded = decode_field_map(&encoded);

    assert_eq!(decoded.len(), original.len());
    for ((r1, f1), (r2, f2)) in original.iter().zip(decoded.iter()) {
        assert_eq!(r1, r2);
        assert_eq!(f1, f2);
    }
}

#[test]
fn test_field_map_unknown_discriminant_filtered() {
    // Discriminant 255 is unknown — filter_map should skip it.
    let encoded = vec![(0usize, 10usize, 0u8), (10, 20, 255u8)];
    let decoded = decode_field_map(&encoded);
    assert_eq!(decoded.len(), 1, "unknown discriminant should be filtered");
    assert_eq!(decoded[0].0, 0..10);
    assert_eq!(decoded[0].1, SearchField::TypeDefinition);
}

// ============================================================================
// lookup
// ============================================================================

#[test]
fn test_lookup_missing_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = FileManifest::new(dir.path().to_path_buf(), dir.path().to_path_buf());
    assert!(manifest.lookup("src/nonexistent.rs").is_none());
}

#[test]
fn test_lookup_present_returns_entry() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().to_path_buf();

    let entry = sample_entry(
        "src/foo.ts",
        "bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222bbbb2222",
    );
    let mut manifest = FileManifest::new(root, cache_dir);
    manifest.insert(entry.clone());

    let found = manifest.lookup("src/foo.ts").unwrap();
    assert_eq!(found.sha256, entry.sha256);
}

// ============================================================================
// load from corrupted or missing file
// ============================================================================

#[test]
fn test_load_missing_file_returns_empty_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().to_path_buf();

    // No manifest file exists yet
    let manifest = FileManifest::load(root, cache_dir).unwrap();
    assert!(manifest.lookup("anything").is_none());
}

#[test]
fn test_load_corrupted_file_returns_empty_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().to_path_buf();

    // Write garbage to the manifest file
    fs::write(
        cache_dir.join("index.skfiles"),
        b"this is not valid jsonl\x00\xFF",
    )
    .unwrap();

    let manifest = FileManifest::load(root, cache_dir).unwrap();
    // Should return an empty manifest (invalid lines silently skipped or whole file skipped).
    // Either way, no crash.
    let _ = manifest.lookup("anything");
}

// ============================================================================
// Atomic write: partial write leaves previous manifest intact
// ============================================================================

#[test]
fn test_save_is_atomic_existing_file_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().to_path_buf();

    // Save first manifest
    let entry1 = sample_entry("a.rs", &"a".repeat(64));
    let mut m1 = FileManifest::new(root.clone(), cache_dir.clone());
    m1.insert(entry1);
    m1.save().unwrap();

    // Save second manifest (overwrites first)
    let entry2 = sample_entry("b.rs", &"b".repeat(64));
    let mut m2 = FileManifest::new(root.clone(), cache_dir.clone());
    m2.insert(entry2);
    m2.save().unwrap();

    let loaded = FileManifest::load(root, cache_dir).unwrap();
    // Only the second manifest's entries should be present.
    assert!(
        loaded.lookup("b.rs").is_some(),
        "b.rs should be in manifest"
    );
    // a.rs was in the first manifest; since m2 replaced it completely, a.rs should NOT be there.
    assert!(
        loaded.lookup("a.rs").is_none(),
        "a.rs should not be in second manifest"
    );
}

// ============================================================================
// Safety limits
// ============================================================================

/// AC-3 (#380): a binary manifest whose declared entry_count exceeds
/// `MAX_MANIFEST_ENTRIES` MUST be rejected BEFORE allocating — `load()` returns
/// `Ok(empty)` without panic and without attempting to read millions of entries.
#[test]
fn test_load_rejects_over_cap_entry_count() {
    use super::MAX_MANIFEST_ENTRIES;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();
    let path = cache_dir.join("index.skfiles");

    // Forge a valid SKFM header with a count just over the cap, no body.
    let mut buf = Vec::new();
    buf.extend_from_slice(b"SKFM");
    buf.extend_from_slice(&FileManifest::FORMAT_VERSION.to_le_bytes());
    let forged = u32::try_from(MAX_MANIFEST_ENTRIES + 1).unwrap();
    buf.extend_from_slice(&forged.to_le_bytes());
    // root string (length-prefixed) so the header block parses up to the count check.
    let root_bytes = root.to_string_lossy();
    buf.extend_from_slice(&u32::try_from(root_bytes.len()).unwrap().to_le_bytes());
    buf.extend_from_slice(root_bytes.as_bytes());
    buf.push(0u8); // git_head absent
    std::fs::write(&path, &buf).unwrap();

    let manifest = FileManifest::load(root, cache_dir).unwrap();
    assert_eq!(
        manifest.entry_count(),
        0,
        "over-cap declared entry_count must be rejected before allocation (AC-3)"
    );
}

/// AC-3 (#380), the required NEGATIVE: a forged `u32::MAX` entry_count plus an
/// oversized declared per-entry length yields `Ok(empty)` WITHOUT panic — the
/// decoder must reject before slicing/allocating.
#[test]
fn test_load_forged_count_and_length_no_panic() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();
    let path = cache_dir.join("index.skfiles");

    let mut buf = Vec::new();
    buf.extend_from_slice(b"SKFM");
    buf.extend_from_slice(&FileManifest::FORMAT_VERSION.to_le_bytes());
    buf.extend_from_slice(&u32::MAX.to_le_bytes()); // forged count
    let root_bytes = root.to_string_lossy();
    buf.extend_from_slice(&u32::try_from(root_bytes.len()).unwrap().to_le_bytes());
    buf.extend_from_slice(root_bytes.as_bytes());
    buf.push(0u8); // git_head absent
    // First entry: a path field with a forged u32::MAX length prefix and no data.
    buf.extend_from_slice(&u32::MAX.to_le_bytes());
    std::fs::write(&path, &buf).unwrap();

    // Must not panic; must reject the whole file.
    let manifest = FileManifest::load(root, cache_dir).unwrap();
    assert_eq!(
        manifest.entry_count(),
        0,
        "forged count + oversized length must yield empty without panic (AC-3 negative)"
    );
}

#[test]
fn test_load_oversized_file_returns_empty_manifest() {
    use super::MAX_MANIFEST_FILE_BYTES;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();
    let path = cache_dir.join("index.skfiles");

    // Create a sparse file that reports a size exceeding the limit without
    // allocating real disk space (seek-then-write a single byte at the end).
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_MANIFEST_FILE_BYTES + 1).unwrap();
    drop(file);

    let manifest = FileManifest::load(root, cache_dir).unwrap();
    assert!(
        manifest.lookup("anything").is_none(),
        "oversized manifest should be discarded and return empty"
    );
}

// ============================================================================
// Wrong-root detection
// ============================================================================

#[test]
fn test_wrong_root_returns_empty_manifest() {
    let dir1 = tempfile::tempdir().unwrap();
    let dir2 = tempfile::tempdir().unwrap();

    let root1 = dir1.path().canonicalize().unwrap();
    let root2 = dir2.path().canonicalize().unwrap();

    // Save a manifest for root1 in root1's cache dir
    let entry = sample_entry("src/x.rs", &"c".repeat(64));
    let mut manifest = FileManifest::new(root1.clone(), root1.clone());
    manifest.insert(entry);
    manifest.save().unwrap();

    // Load that manifest file but pass root2 as the project root
    // (simulate wrong-root detection)
    let loaded = FileManifest::load(root2, root1).unwrap();
    // The root mismatch should cause the manifest to be treated as empty
    assert!(
        loaded.lookup("src/x.rs").is_none(),
        "manifest from different root should not be used"
    );
}

// ============================================================================
// Mtime pre-screening
// ============================================================================

/// Inserting a ManifestEntry with `mtime: Some(...)`, saving, and loading
/// should preserve the value exactly.
#[test]
fn test_mtime_persisted_in_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();

    let mtime_value: u64 = 1_700_000_000;
    let entry = ManifestEntry {
        path: "src/x.rs".to_string(),
        sha256: "a".repeat(64),
        lang: "rust".to_string(),
        field_map: vec![],
        mtime: Some(mtime_value),
        size: None,
    };

    let mut manifest = FileManifest::new(root.clone(), cache_dir.clone());
    manifest.insert(entry);
    manifest.save().unwrap();

    let loaded = FileManifest::load(root, cache_dir).unwrap();
    let found = loaded.lookup("src/x.rs").unwrap();
    assert_eq!(
        found.mtime,
        Some(mtime_value),
        "mtime must survive save/load roundtrip"
    );
}

/// AC-4 (#380): a v3 JSONL manifest (the immediate predecessor, post-#373) MUST
/// be discarded on load — it lacks the SKFM binary magic, so `load()` cold-starts
/// and `version_matches()` reports a mismatch (which drives the rebuild).
#[test]
fn test_stale_v3_jsonl_manifest_triggers_cold_start() {
    use std::io::Write as _;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();
    let path = cache_dir.join("index.skfiles");

    // Write a v3 JSONL manifest (the format #373 produced). Starts with `{`,
    // never the SKFM magic.
    let mut f = std::fs::File::create(&path).unwrap();
    let header =
        serde_json::json!({"version": 3, "root": root.to_string_lossy(), "git_head": null});
    writeln!(f, "{header}").unwrap();
    let entry_json = serde_json::json!({
        "path": "src/old.rs",
        "sha256": "b".repeat(64),
        "lang": "rust",
        "field_map": []
    });
    writeln!(f, "{entry_json}").unwrap();
    drop(f);

    // load() must cold-start (no SKFM magic).
    let manifest = FileManifest::load(root.clone(), cache_dir.clone()).unwrap();
    assert!(
        manifest.lookup("src/old.rs").is_none(),
        "v3 JSONL manifest must be discarded after the binary 3→4 bump — cold start required (AC-4)"
    );
    // version_matches() must report below-current so check_staleness rebuilds.
    assert!(
        !FileManifest::version_matches(&cache_dir).unwrap(),
        "v3 JSONL manifest must NOT be accepted as current (AC-4 negative)"
    );
}

/// AC-4 (#380): an even-older v2 JSONL manifest is likewise rejected (no magic).
#[test]
fn test_stale_v2_jsonl_manifest_triggers_cold_start() {
    use std::io::Write as _;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();
    let path = cache_dir.join("index.skfiles");

    let mut f = std::fs::File::create(&path).unwrap();
    let header = serde_json::json!({"version": 2, "root": root.to_string_lossy()});
    writeln!(f, "{header}").unwrap();
    drop(f);

    let manifest = FileManifest::load(root.clone(), cache_dir.clone()).unwrap();
    assert!(
        manifest.lookup("anything").is_none(),
        "v2 JSONL manifest must be discarded — cold start required (AC-4)"
    );
    assert!(
        !FileManifest::version_matches(&cache_dir).unwrap(),
        "v2 JSONL manifest must NOT be accepted as current (AC-4 negative)"
    );
}

// ============================================================================
// git_head extensions
// ============================================================================

/// Roundtrip: set_git_head → save → load → stored_git_head returns same value.
#[test]
fn test_git_head_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();

    let sha = "abc1234def5678901234567890123456789012345".to_string();
    let mut manifest = FileManifest::new(root.clone(), cache_dir.clone());
    manifest.set_git_head(Some(sha.clone()));
    manifest.save().unwrap();

    let loaded = FileManifest::load(root, cache_dir).unwrap();
    assert_eq!(
        loaded.stored_git_head(),
        Some(sha.as_str()),
        "git_head must survive save/load roundtrip"
    );
}

/// A binary manifest written with `git_head: None` (presence byte 0) round-trips
/// to `stored_git_head() == None`.
#[test]
fn test_git_head_none_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();

    let mut manifest = FileManifest::new(root.clone(), cache_dir.clone());
    manifest.set_git_head(None);
    manifest.insert(sample_entry("src/x.rs", &"a".repeat(64)));
    manifest.save().unwrap();

    let loaded = FileManifest::load(root, cache_dir).unwrap();
    assert_eq!(
        loaded.stored_git_head(),
        None,
        "manifest saved with git_head None must load back as None"
    );
    assert!(loaded.lookup("src/x.rs").is_some(), "entry must survive");
}

/// sorted_paths returns entry paths in alphabetical order.
#[test]
fn test_sorted_paths_invariant() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().to_path_buf();

    let paths = ["src/z.rs", "src/a.rs", "src/m.rs", "README.md"];
    let mut manifest = FileManifest::new(root, cache_dir);
    for p in &paths {
        manifest.insert(ManifestEntry {
            path: p.to_string(),
            sha256: "a".repeat(64),
            lang: "rust".to_string(),
            field_map: vec![],
            mtime: None,
            size: None,
        });
    }

    let sorted = manifest.sorted_paths();
    assert_eq!(sorted.len(), paths.len());
    for i in 1..sorted.len() {
        assert!(
            sorted[i - 1] <= sorted[i],
            "paths should be sorted: {} > {}",
            sorted[i - 1],
            sorted[i]
        );
    }
    // Verify specific order
    assert_eq!(sorted[0], "README.md");
    assert_eq!(sorted[1], "src/a.rs");
    assert_eq!(sorted[2], "src/m.rs");
    assert_eq!(sorted[3], "src/z.rs");
}

/// entry_count returns the number of entries.
#[test]
fn test_entry_count() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().to_path_buf();

    let mut manifest = FileManifest::new(root, cache_dir);
    assert_eq!(manifest.entry_count(), 0);

    manifest.insert(sample_entry("a.rs", &"a".repeat(64)));
    assert_eq!(manifest.entry_count(), 1);

    manifest.insert(sample_entry("b.rs", &"b".repeat(64)));
    assert_eq!(manifest.entry_count(), 2);
}

/// sorted_paths on empty manifest returns empty vec.
#[test]
fn test_sorted_paths_empty() {
    let dir = tempfile::tempdir().unwrap();
    let manifest = FileManifest::new(dir.path().to_path_buf(), dir.path().to_path_buf());
    assert!(manifest.sorted_paths().is_empty());
}

// ============================================================================
// Binary format (#380)
// ============================================================================

/// AC-1 (#380): a field_map whose END byte-offset exceeds `u16::MAX` (65535) MUST
/// survive save()→load() with every field byte-identical. The old u16 key path
/// (#355 Part B legacy) truncated such offsets; the binary v4 encoder uses u32.
#[test]
fn test_field_map_large_offset_roundtrip_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();

    // End offset 70000 > u16::MAX (65535).
    let big_field_map = vec![
        (
            0usize,
            100usize,
            SearchField::FunctionSignature.discriminant(),
        ),
        (100, 70_000, SearchField::FunctionBody.discriminant()),
    ];
    let entry = ManifestEntry {
        path: "src/huge.rs".to_string(),
        sha256: "f".repeat(64),
        lang: "rust".to_string(),
        field_map: big_field_map.clone(),
        mtime: Some(1_700_000_123),
        size: Some(80_000),
    };

    let mut manifest = FileManifest::new(root.clone(), cache_dir.clone());
    manifest.insert(entry.clone());
    manifest.save().unwrap();

    let loaded = FileManifest::load(root, cache_dir).unwrap();
    let found = loaded.lookup("src/huge.rs").unwrap();
    assert_eq!(
        found, &entry,
        "every field (incl. the >u16::MAX offset) must round-trip byte-identical (AC-1)"
    );
    assert_eq!(
        found.field_map, big_field_map,
        "the 70000 end offset must NOT be truncated (AC-1)"
    );
}

/// AC-1 NEGATIVE (#380): the on-disk `index.skfiles` MUST NOT contain ASCII JSON
/// array encoding after save() — i.e. no literal `],[` substring. This is the
/// discriminating check that the encoding is binary, not JSONL.
#[test]
fn test_saved_manifest_is_not_jsonl() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();

    let mut manifest = FileManifest::new(root.clone(), cache_dir.clone());
    manifest.insert(sample_entry("src/a.rs", &"a".repeat(64)));
    manifest.insert(sample_entry("src/b.rs", &"b".repeat(64)));
    manifest.save().unwrap();

    let bytes = fs::read(cache_dir.join("index.skfiles")).unwrap();
    // Must start with the SKFM magic.
    assert_eq!(
        &bytes[0..4],
        b"SKFM",
        "binary manifest must start with SKFM magic"
    );
    // Must NOT contain the JSON-array delimiter the JSONL field_map produced.
    let needle = b"],[";
    assert!(
        !bytes.windows(needle.len()).any(|w| w == needle),
        "binary manifest must not contain JSON-array encoding `],[` (AC-1 negative)"
    );
}

/// AC-2 (#380): the binary body MUST begin with the 4-byte magic, then version,
/// then a u32 entry count.
#[test]
fn test_binary_header_layout() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();

    let mut manifest = FileManifest::new(root.clone(), cache_dir.clone());
    manifest.insert(sample_entry("a.rs", &"a".repeat(64)));
    manifest.insert(sample_entry("b.rs", &"b".repeat(64)));
    manifest.save().unwrap();

    let bytes = fs::read(cache_dir.join("index.skfiles")).unwrap();
    assert_eq!(&bytes[0..4], b"SKFM");
    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    assert_eq!(version, FileManifest::FORMAT_VERSION);
    assert_eq!(version, 4, "FORMAT_VERSION must be 4 (AC-2)");
    let count = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    assert_eq!(
        count, 2,
        "entry count must be encoded as u32 in the header (AC-2)"
    );
}

/// AC-2 (#380): load() returns Ok(empty) when the magic is absent.
#[test]
fn test_load_absent_magic_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();

    // Plausible length but wrong magic.
    fs::write(
        cache_dir.join("index.skfiles"),
        b"XXXX\x04\x00\x00\x00\x00\x00\x00\x00",
    )
    .unwrap();
    let manifest = FileManifest::load(root, cache_dir).unwrap();
    assert_eq!(manifest.entry_count(), 0, "absent magic → Ok(empty) (AC-2)");
}

/// AC-2 (#380): load() returns Ok(empty) when the version != 4 (e.g. a future v5
/// binary, or a forged version int).
#[test]
fn test_load_wrong_version_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();

    let mut buf = Vec::new();
    buf.extend_from_slice(b"SKFM");
    buf.extend_from_slice(&99u32.to_le_bytes()); // wrong version
    buf.extend_from_slice(&0u32.to_le_bytes()); // count
    let root_bytes = root.to_string_lossy();
    buf.extend_from_slice(&u32::try_from(root_bytes.len()).unwrap().to_le_bytes());
    buf.extend_from_slice(root_bytes.as_bytes());
    buf.push(0u8);
    fs::write(cache_dir.join("index.skfiles"), &buf).unwrap();

    let manifest = FileManifest::load(root, cache_dir).unwrap();
    assert_eq!(manifest.entry_count(), 0, "version != 4 → Ok(empty) (AC-2)");
}

/// AC-2 / AC-5 (#380): load() returns Ok(empty) when the body is truncated below
/// the fixed header.
#[test]
fn test_load_truncated_header_returns_empty() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();

    fs::write(cache_dir.join("index.skfiles"), b"SKFM\x04").unwrap(); // 5 bytes, < 12
    let manifest = FileManifest::load(root, cache_dir).unwrap();
    assert_eq!(manifest.entry_count(), 0, "truncated header → Ok(empty)");
}

/// AC-5 (#380), FALSIFIABLE: a valid header declaring count=3 but a body holding
/// only ~1.5 entries MUST reject the WHOLE file — `entry_count()` is 0, never a
/// partially-recovered manifest shorter than written (preserves the FileId↔path
/// alignment invariant).
#[test]
fn test_load_truncated_body_rejects_whole_file() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();

    // Build a valid 3-entry manifest, then truncate the on-disk bytes mid-second-entry.
    let mut manifest = FileManifest::new(root.clone(), cache_dir.clone());
    for name in ["a.rs", "b.rs", "c.rs"] {
        manifest.insert(sample_entry(name, &"a".repeat(64)));
    }
    manifest.save().unwrap();

    let path = cache_dir.join("index.skfiles");
    let full = fs::read(&path).unwrap();
    // Confirm the header really declared 3 entries.
    assert_eq!(u32::from_le_bytes(full[8..12].try_into().unwrap()), 3);
    // Truncate to a point well past the header but before all 3 entries decode —
    // ~60% of the file lands inside the body.
    let cut = full.len() * 6 / 10;
    fs::write(&path, &full[..cut]).unwrap();

    let loaded = FileManifest::load(root, cache_dir).unwrap();
    assert_eq!(
        loaded.entry_count(),
        0,
        "truncated body (count=3, <3 entries present) must reject WHOLE file → entry_count()==0 (AC-5)"
    );
}

/// AC-2 / AC-3 (#380): a forged over-cap count combined with an under-length body
/// never panics and yields empty (complements the in-module forged-length test).
#[test]
fn test_load_over_max_file_bytes_returns_empty() {
    use super::MAX_MANIFEST_FILE_BYTES;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();
    let path = cache_dir.join("index.skfiles");

    // Sparse file over the byte cap — must be rejected before reading into RAM.
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_MANIFEST_FILE_BYTES + 1).unwrap();
    drop(file);

    let manifest = FileManifest::load(root, cache_dir).unwrap();
    assert_eq!(
        manifest.entry_count(),
        0,
        "manifest over MAX_MANIFEST_FILE_BYTES must be discarded (AC-3)"
    );
}

/// AC-11 / regression (#380): the full set of entry fields (path, sha, lang,
/// field_map, mtime, size, git_head) round-trips byte-identical for several
/// entries — the query path reads exactly the same entry data it did under JSONL.
#[test]
fn test_full_entry_set_roundtrip_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();

    let entries = vec![
        ManifestEntry {
            path: "README.md".to_string(),
            sha256: "1".repeat(64),
            lang: "markdown".to_string(),
            field_map: vec![],
            mtime: Some(10),
            size: Some(20),
        },
        ManifestEntry {
            path: "src/a.rs".to_string(),
            sha256: "2".repeat(64),
            lang: "rust".to_string(),
            field_map: encode_field_map(&sample_field_map()),
            mtime: None,
            size: Some(4096),
        },
        ManifestEntry {
            path: "src/z.ts".to_string(),
            sha256: "3".repeat(64),
            lang: "typescript".to_string(),
            field_map: vec![(0, 1, 0u8), (1, 100_000, 1u8)],
            mtime: Some(99),
            size: None,
        },
    ];

    let mut manifest = FileManifest::new(root.clone(), cache_dir.clone());
    manifest.set_git_head(Some("c".repeat(40)));
    for e in &entries {
        manifest.insert(e.clone());
    }
    manifest.save().unwrap();

    let loaded = FileManifest::load(root, cache_dir).unwrap();
    assert_eq!(loaded.stored_git_head(), Some("c".repeat(40).as_str()));
    for e in &entries {
        assert_eq!(
            loaded.lookup(&e.path),
            Some(e),
            "entry {} must round-trip",
            e.path
        );
    }
    // Sorted order (FileId↔path contract) preserved.
    assert_eq!(
        loaded.sorted_paths(),
        vec!["README.md", "src/a.rs", "src/z.ts"]
    );
}
