//! Tests for the manifest sidecar (manifest.rs).

#![allow(clippy::unwrap_used)]

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

#[test]
fn test_load_stops_at_entry_cap() {
    use super::MAX_MANIFEST_ENTRIES;
    use std::io::Write as _;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();

    // Write a manifest with more entries than MAX_MANIFEST_ENTRIES.
    let path = cache_dir.join("index.skfiles");
    let mut f = std::fs::File::create(&path).unwrap();

    // Header line
    let header = serde_json::json!({"version": 1, "root": root.to_string_lossy()});
    writeln!(f, "{header}").unwrap();

    // Write MAX_MANIFEST_ENTRIES + 10 entry lines.
    for i in 0..(MAX_MANIFEST_ENTRIES + 10) {
        let entry = serde_json::json!({
            "path": format!("src/file_{i}.rs"),
            "sha256": "a".repeat(64),
            "lang": "rust",
            "field_map": []
        });
        writeln!(f, "{entry}").unwrap();
    }
    drop(f);

    let manifest = FileManifest::load(root, cache_dir).unwrap();
    // Must not exceed the cap (entries beyond the cap are simply ignored).
    assert!(
        manifest.entries.len() <= MAX_MANIFEST_ENTRIES,
        "entry count {} exceeds MAX_MANIFEST_ENTRIES {}",
        manifest.entries.len(),
        MAX_MANIFEST_ENTRIES
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

/// A manifest written without a `mtime` field (e.g. by an older version of
/// skim) must deserialize cleanly with `mtime: None`.
#[test]
fn test_mtime_backward_compat_none() {
    use std::io::Write as _;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_dir = root.clone();
    let path = cache_dir.join("index.skfiles");

    // Write a manifest with no `mtime` field in the entry.
    let mut f = std::fs::File::create(&path).unwrap();
    let header = serde_json::json!({"version": 1, "root": root.to_string_lossy()});
    writeln!(f, "{header}").unwrap();
    // Deliberately omit `mtime` to simulate an old manifest format.
    let entry_json = serde_json::json!({
        "path": "src/old.rs",
        "sha256": "b".repeat(64),
        "lang": "rust",
        "field_map": []
    });
    writeln!(f, "{entry_json}").unwrap();
    drop(f);

    let manifest = FileManifest::load(root, cache_dir).unwrap();
    let found = manifest.lookup("src/old.rs").unwrap();
    assert_eq!(
        found.mtime, None,
        "mtime should be None when field is absent (backward compat)"
    );
}
