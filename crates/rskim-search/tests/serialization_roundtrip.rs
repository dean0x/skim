//! JSON serialize/deserialize roundtrips for all Serialize+Deserialize types.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use rskim_search::{
    FileId, FileTable, IndexStats, LineRange, MatchSpan, SearchField, SearchResult,
};

fn json_roundtrip<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T) -> T {
    let json = serde_json::to_string(value).expect("serialize failed");
    serde_json::from_str(&json).expect("deserialize failed")
}

// ============================================================================
// FileId
// ============================================================================

#[test]
fn test_file_id_json_roundtrip() {
    let id = FileId::new(42);
    let rt = json_roundtrip(&id);
    assert_eq!(rt.as_u64(), 42);
}

#[test]
fn test_file_id_max_json_roundtrip() {
    let id = FileId::new(u64::MAX);
    let rt = json_roundtrip(&id);
    assert_eq!(rt.as_u64(), u64::MAX);
}

// ============================================================================
// SearchField
// ============================================================================

#[test]
fn test_search_field_all_variants_roundtrip() {
    let fields = [
        SearchField::TypeDefinition,
        SearchField::FunctionSignature,
        SearchField::SymbolName,
        SearchField::ImportExport,
        SearchField::FunctionBody,
        SearchField::Comment,
        SearchField::StringLiteral,
    ];
    for field in &fields {
        let rt = json_roundtrip(field);
        assert_eq!(&rt, field);
    }
}

#[test]
fn test_search_field_json_format_pinned() {
    let cases = [
        (SearchField::TypeDefinition, "\"TypeDefinition\""),
        (SearchField::FunctionSignature, "\"FunctionSignature\""),
        (SearchField::SymbolName, "\"SymbolName\""),
        (SearchField::ImportExport, "\"ImportExport\""),
        (SearchField::FunctionBody, "\"FunctionBody\""),
        (SearchField::Comment, "\"Comment\""),
        (SearchField::StringLiteral, "\"StringLiteral\""),
    ];
    for (field, expected) in &cases {
        let json = serde_json::to_string(field).expect("serialize failed");
        assert_eq!(&json, expected, "format mismatch for {field:?}");
    }
}

#[test]
fn test_search_field_unknown_variant_fails() {
    let result = serde_json::from_str::<SearchField>("\"UnknownField\"");
    assert!(result.is_err());
}

// ============================================================================
// MatchSpan
// ============================================================================

#[test]
fn test_match_span_json_roundtrip() {
    let span = MatchSpan::new(10, 20);
    let rt = json_roundtrip(&span);
    assert_eq!(rt.start, 10);
    assert_eq!(rt.end, 20);
}

// ============================================================================
// LineRange
// ============================================================================

#[test]
fn test_line_range_json_roundtrip() {
    let range = LineRange::new(1, 42);
    let rt = json_roundtrip(&range);
    assert_eq!(rt.start, 1);
    assert_eq!(rt.end, 42);
}

// ============================================================================
// SearchResult
// ============================================================================

#[test]
fn test_search_result_json_roundtrip() {
    let result = SearchResult {
        file_path: PathBuf::from("src/main.rs"),
        line_range: LineRange::new(10, 20),
        score: 2.75,
        matched_field: SearchField::FunctionSignature,
        snippet: "fn main() {".to_string(),
        match_positions: vec![MatchSpan::new(3, 7)],
    };
    let rt = json_roundtrip(&result);
    assert_eq!(rt.file_path, PathBuf::from("src/main.rs"));
    assert_eq!(rt.line_range.start, 10);
    assert_eq!(rt.line_range.end, 20);
    assert!((rt.score - 2.75).abs() < f32::EPSILON);
    assert_eq!(rt.matched_field, SearchField::FunctionSignature);
    assert_eq!(rt.snippet, "fn main() {");
    assert_eq!(rt.match_positions.len(), 1);
    assert_eq!(rt.match_positions[0].start, 3);
}

#[test]
fn test_search_result_empty_fields_roundtrip() {
    let result = SearchResult {
        file_path: PathBuf::from("x.rs"),
        line_range: LineRange::new(1, 1),
        score: 0.0,
        matched_field: SearchField::Comment,
        snippet: String::new(),
        match_positions: vec![],
    };
    let rt = json_roundtrip(&result);
    assert!(rt.snippet.is_empty());
    assert!(rt.match_positions.is_empty());
}

// ============================================================================
// IndexStats
// ============================================================================

#[test]
fn test_index_stats_json_roundtrip() {
    let stats = IndexStats {
        file_count: 1234,
        total_ngrams: 567_890,
        index_size_bytes: 1_048_576,
        last_updated: 1_711_900_000,
        format_version: 1,
    };
    let rt = json_roundtrip(&stats);
    assert_eq!(rt.file_count, 1234);
    assert_eq!(rt.total_ngrams, 567_890);
    assert_eq!(rt.index_size_bytes, 1_048_576);
    assert_eq!(rt.last_updated, 1_711_900_000);
    assert_eq!(rt.format_version, 1);
}

#[test]
fn test_index_stats_max_values_roundtrip() {
    let stats = IndexStats {
        file_count: u64::MAX,
        total_ngrams: u64::MAX,
        index_size_bytes: u64::MAX,
        last_updated: u64::MAX,
        format_version: u32::MAX,
    };
    let rt = json_roundtrip(&stats);
    assert_eq!(rt.file_count, u64::MAX);
    assert_eq!(rt.format_version, u32::MAX);
}

// ============================================================================
// FileTable (custom serde)
// ============================================================================

#[test]
fn test_file_table_json_roundtrip() {
    let mut table = FileTable::new();
    let id_a = table.register(Path::new("src/main.rs"));
    let id_b = table.register(Path::new("src/lib.rs"));
    let id_c = table.register(Path::new("tests/test.rs"));

    let rt = json_roundtrip(&table);
    assert_eq!(rt.len(), 3);
    assert_eq!(rt.lookup(id_a), Some(Path::new("src/main.rs")));
    assert_eq!(rt.lookup(id_b), Some(Path::new("src/lib.rs")));
    assert_eq!(rt.lookup(id_c), Some(Path::new("tests/test.rs")));
}

#[test]
fn test_file_table_empty_json_roundtrip() {
    let table = FileTable::new();
    let rt = json_roundtrip(&table);
    assert!(rt.is_empty());
    assert_eq!(rt.len(), 0);
}

#[test]
fn test_file_table_roundtrip_preserves_lookups() {
    let mut table = FileTable::new();
    table.register(Path::new("a.rs"));
    table.register(Path::new("b.rs"));

    let json = serde_json::to_string(&table).expect("serialize failed");
    let mut rt: FileTable = serde_json::from_str(&json).expect("deserialize failed");

    // Lookups from deserialized state work
    assert_eq!(rt.lookup(FileId::new(0)), Some(Path::new("a.rs")));
    assert_eq!(rt.lookup(FileId::new(1)), Some(Path::new("b.rs")));

    // register() still works after roundtrip — new file gets next id
    let new_id = rt.register(Path::new("c.rs"));
    assert_eq!(new_id.as_u64(), 2);
    assert_eq!(rt.len(), 3);

    // Re-registering existing path is still idempotent
    let existing = rt.register(Path::new("a.rs"));
    assert_eq!(existing.as_u64(), 0);
    assert_eq!(rt.len(), 3);
}

#[test]
fn test_file_table_serialized_is_just_paths() {
    let mut table = FileTable::new();
    table.register(Path::new("src/main.rs"));
    table.register(Path::new("src/lib.rs"));

    let json = serde_json::to_string(&table).expect("serialize failed");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse failed");

    // Should be a plain array of strings (no HashMap, no ids)
    assert!(parsed.is_array(), "serialized form should be an array");
    let arr = parsed.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0], "src/main.rs");
    assert_eq!(arr[1], "src/lib.rs");
}

// ============================================================================
// FileTable deserialization idempotency with unnormalized paths
// ============================================================================

/// Roundtrip a FileTable that was registered with unnormalized paths (e.g., "./src/main.rs").
///
/// The serialized form stores the normalized path. After deserialization, the ids
/// map must also be keyed on the normalized path so that a subsequent register()
/// call with the same unnormalized input returns the existing FileId rather than
/// creating a duplicate entry.
#[test]
fn test_file_table_unnormalized_paths_idempotent_after_roundtrip() {
    let mut table = FileTable::new();
    let id_a = table.register(Path::new("./src/main.rs"));
    let id_b = table.register(Path::new("./src/lib.rs"));
    let id_c = table.register(Path::new("./tests/test.rs"));

    // Paths normalize away the leading `./`
    assert_eq!(table.lookup(id_a), Some(Path::new("src/main.rs")));
    assert_eq!(table.len(), 3);

    let json = serde_json::to_string(&table).expect("serialize failed");
    let mut rt: FileTable = serde_json::from_str(&json).expect("deserialize failed");

    assert_eq!(rt.len(), 3);
    assert_eq!(rt.lookup(id_a), Some(Path::new("src/main.rs")));
    assert_eq!(rt.lookup(id_b), Some(Path::new("src/lib.rs")));
    assert_eq!(rt.lookup(id_c), Some(Path::new("tests/test.rs")));

    // Registering the same unnormalized paths must return the existing IDs — no duplicates.
    let re_a = rt.register(Path::new("./src/main.rs"));
    let re_b = rt.register(Path::new("src/lib.rs")); // already-normalized form
    let re_c = rt.register(Path::new("./tests/test.rs"));
    assert_eq!(re_a, id_a, "unnormalized re-register must return existing id");
    assert_eq!(re_b, id_b, "normalized re-register must return existing id");
    assert_eq!(re_c, id_c, "unnormalized re-register must return existing id");
    assert_eq!(rt.len(), 3, "no duplicate entries after re-registering unnormalized paths");
}

/// Serialized form of a FileTable registered with unnormalized paths must store
/// the normalized paths, not the raw input.
#[test]
fn test_file_table_serialized_form_uses_normalized_paths() {
    let mut table = FileTable::new();
    table.register(Path::new("./src/main.rs"));
    table.register(Path::new("a/b/../c.rs"));

    let json = serde_json::to_string(&table).expect("serialize failed");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("parse failed");

    let arr = parsed.as_array().expect("serialized form must be an array");
    assert_eq!(arr[0], "src/main.rs", "leading ./ must be stripped");
    assert_eq!(arr[1], "a/c.rs", ".. must be collapsed");
}

// ============================================================================
// SearchError Display
// ============================================================================

#[test]
fn test_search_error_display_io() {
    let err = rskim_search::SearchError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "not found",
    ));
    let display = format!("{err}");
    assert!(display.contains("IO error"));
}

#[test]
fn test_search_error_display_index() {
    let err = rskim_search::SearchError::IndexError("corrupt".to_string());
    assert_eq!(format!("{err}"), "Index error: corrupt");
}

#[test]
fn test_search_error_display_query() {
    let err = rskim_search::SearchError::InvalidQuery("bad syntax".to_string());
    assert_eq!(format!("{err}"), "Invalid query: bad syntax");
}

#[test]
fn test_search_error_display_serialization() {
    let err = rskim_search::SearchError::SerializationError("failed".to_string());
    assert_eq!(format!("{err}"), "Serialization error: failed");
}
