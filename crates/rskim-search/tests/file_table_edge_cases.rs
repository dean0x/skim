//! FileTable normalization and registration edge cases.

use std::path::Path;

use rskim_search::{FileId, FileTable};

// ============================================================================
// Lookup edge cases
// ============================================================================

#[test]
fn test_empty_table_lookup_returns_none() {
    let table = FileTable::new();
    assert!(table.lookup(FileId::new(0)).is_none());
}

#[test]
fn test_lookup_unregistered_id() {
    let mut table = FileTable::new();
    let _id = table.register(Path::new("src/main.rs"));
    assert!(table.lookup(FileId::new(99)).is_none());
}

// ============================================================================
// Path edge cases
// ============================================================================

#[test]
fn test_register_empty_path() {
    let mut table = FileTable::new();
    let id = table.register(Path::new(""));
    // Empty path registers (documents behavior)
    assert_eq!(table.len(), 1);
    assert!(table.lookup(id).is_some());
}

#[test]
fn test_register_dot_only() {
    let mut table = FileTable::new();
    let id = table.register(Path::new("."));
    // CurDir component stripped — normalizes to empty path
    assert!(table.lookup(id).is_some());
}

#[test]
fn test_register_unicode_path() {
    let mut table = FileTable::new();
    let id = table.register(Path::new("src/日本語/файл.rs"));
    assert_eq!(table.len(), 1);
    assert_eq!(
        table.lookup(id),
        Some(Path::new("src/日本語/файл.rs"))
    );
}

#[test]
fn test_register_spaces_in_path() {
    let mut table = FileTable::new();
    let id = table.register(Path::new("my project/src/main.rs"));
    assert_eq!(
        table.lookup(id),
        Some(Path::new("my project/src/main.rs"))
    );
}

// ============================================================================
// Normalization
// ============================================================================

#[test]
fn test_normalize_multiple_parent_dirs() {
    let mut table = FileTable::new();
    let id = table.register(Path::new("a/b/c/../../d"));
    // a/b/c/../../d → a/d
    assert_eq!(table.lookup(id), Some(Path::new("a/d")));
}

#[test]
fn test_normalize_parent_dir_at_start() {
    let mut table = FileTable::new();
    let id = table.register(Path::new("../foo"));
    // Nothing to pop → stays as "../foo"
    assert_eq!(table.lookup(id), Some(Path::new("../foo")));
}

#[test]
fn test_normalize_absolute_path() {
    let mut table = FileTable::new();
    let id = table.register(Path::new("/usr/local/bin"));
    assert_eq!(table.lookup(id), Some(Path::new("/usr/local/bin")));
}

#[test]
fn test_normalize_absolute_with_parent() {
    let mut table = FileTable::new();
    let id = table.register(Path::new("/a/../b"));
    assert_eq!(table.lookup(id), Some(Path::new("/b")));
}

#[test]
fn test_normalize_complex_chain() {
    let mut table = FileTable::new();
    let id = table.register(Path::new("a/./b/../c/./d/../e"));
    // a/./b/../c/./d/../e → a/c/e
    assert_eq!(table.lookup(id), Some(Path::new("a/c/e")));
}

#[test]
fn test_idempotent_parent_dir_variants() {
    let mut table = FileTable::new();
    let id1 = table.register(Path::new("src/../src/main.rs"));
    let id2 = table.register(Path::new("src/main.rs"));
    assert_eq!(id1, id2);
    assert_eq!(table.len(), 1);
}

#[test]
fn test_normalize_trailing_slash() {
    let mut table = FileTable::new();
    let id1 = table.register(Path::new("src/"));
    let id2 = table.register(Path::new("src"));
    // Trailing slash is stripped by Path::components()
    assert_eq!(id1, id2);
    assert_eq!(table.len(), 1);
}

/// `/a/../../b` — the second `..` has only `RootDir` behind it (not a Normal
/// component), so normalize cannot pop it and instead keeps `..` in the output.
/// The result is `/../b`, which is logically above the filesystem root.
///
/// This is a documented limitation of the I/O-free normalizer: it does not
/// clamp absolute paths at root. Callers providing paths that traverse above
/// root receive a path that still contains a `..` segment.
#[test]
fn test_normalize_absolute_over_root() {
    let mut table = FileTable::new();
    let id = table.register(Path::new("/a/../../b"));
    // Second `..` cannot pop RootDir — stays in output as `/../b`.
    assert_eq!(table.lookup(id), Some(Path::new("/../b")));
}

// ============================================================================
// Registration semantics
// ============================================================================

#[test]
fn test_register_many_files() {
    let mut table = FileTable::new();
    for i in 0..1000 {
        let path = format!("src/file_{i}.rs");
        let id = table.register(Path::new(&path));
        assert_eq!(id.as_u64(), i as u64);
    }
    assert_eq!(table.len(), 1000);

    // All lookupable
    for i in 0..1000 {
        let path = format!("src/file_{i}.rs");
        assert_eq!(
            table.lookup(FileId::new(i as u64)),
            Some(Path::new(&path)),
        );
    }
}

#[test]
fn test_len_tracks_unique_paths() {
    let mut table = FileTable::new();
    for _ in 0..5 {
        table.register(Path::new("src/main.rs"));
    }
    assert_eq!(table.len(), 1);
}

#[test]
fn test_is_empty_on_fresh_table() {
    assert!(FileTable::new().is_empty());
}

#[test]
fn test_register_order_deterministic() {
    let mut table = FileTable::new();
    let id0 = table.register(Path::new("a.rs"));
    let id1 = table.register(Path::new("b.rs"));
    let id2 = table.register(Path::new("c.rs"));

    assert_eq!(id0.as_u64(), 0);
    assert_eq!(id1.as_u64(), 1);
    assert_eq!(id2.as_u64(), 2);
}

#[test]
fn test_default_is_empty() {
    assert!(FileTable::default().is_empty());
}
