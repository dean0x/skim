//! Boundary values and edge cases for all public types.

use std::collections::HashMap;
use std::path::PathBuf;

use rskim_search::{
    FileId, FileTable, IndexStats, LineRange, MatchSpan, SearchField, SearchQuery, SearchResult,
    TemporalFlags,
};

// ============================================================================
// FileId
// ============================================================================

#[test]
fn test_file_id_zero() {
    let id = FileId::new(0);
    assert_eq!(id.as_u64(), 0);
}

#[test]
fn test_file_id_max() {
    let id = FileId::new(u64::MAX);
    assert_eq!(id.as_u64(), u64::MAX);
}

#[test]
fn test_file_id_equality_and_hash() {
    let a = FileId::new(42);
    let b = FileId::new(42);
    assert_eq!(a, b);

    // Usable as HashMap key
    let mut map = HashMap::new();
    map.insert(a, "value");
    assert_eq!(map.get(&b), Some(&"value"));
}

// ============================================================================
// MatchSpan
// ============================================================================

#[test]
fn test_match_span_u32_max() {
    let span = MatchSpan::new(u32::MAX, u32::MAX);
    assert_eq!(span.len(), 0);
    assert!(span.is_empty());
}

#[test]
fn test_match_span_max_start_zero_end() {
    let span = MatchSpan::new(u32::MAX, 0);
    assert_eq!(span.len(), 0); // saturating_sub
    assert!(span.is_empty());
}

#[test]
fn test_match_span_full_range() {
    let span = MatchSpan::new(0, u32::MAX);
    assert_eq!(span.len(), u32::MAX);
    assert!(!span.is_empty());
}

#[test]
fn test_match_span_direct_construction() {
    // Public fields — no validation, inverted spans construct fine
    let span = MatchSpan { start: 20, end: 10 };
    assert_eq!(span.start, 20);
    assert_eq!(span.end, 10);
    assert_eq!(span.len(), 0); // saturating_sub handles inversion
}

// ============================================================================
// LineRange
// ============================================================================

#[test]
fn test_line_range_zero_zero() {
    // Documents: LineRange::new(0,0) constructs (no enforcement of 1-indexing)
    let r = LineRange::new(0, 0);
    assert_eq!(r.start, 0);
    assert_eq!(r.end, 0);
}

#[test]
fn test_line_range_inverted() {
    // Documents: inverted range constructs (no validation)
    let r = LineRange::new(10, 5);
    assert_eq!(r.start, 10);
    assert_eq!(r.end, 5);
}

#[test]
fn test_line_range_direct_construction() {
    // Public fields bypass new()
    let r = LineRange { start: 0, end: 0 };
    assert_eq!(r.start, 0);
    assert_eq!(r.end, 0);
}

// ============================================================================
// SearchQuery
// ============================================================================

#[test]
fn test_search_query_limit_zero() {
    let q = SearchQuery::new().with_limit(0);
    assert_eq!(q.limit, 0);
}

#[test]
fn test_search_query_empty_text() {
    let q = SearchQuery::text("");
    // Empty string sets Some(""), NOT None
    assert_eq!(q.text_query, Some(String::new()));
}

#[test]
fn test_search_query_both_text_and_ast() {
    let q = SearchQuery::text("foo").with_ast_pattern("fn _()");
    assert_eq!(q.text_query, Some("foo".to_string()));
    assert_eq!(q.ast_pattern, Some("fn _()".to_string()));
}

#[test]
fn test_search_query_default_trait() {
    let a = SearchQuery::default();
    let b = SearchQuery::new();
    assert_eq!(a.text_query, b.text_query);
    assert_eq!(a.limit, b.limit);
    assert_eq!(a.offset, b.offset);
    assert_eq!(a.ast_pattern, b.ast_pattern);
}

#[test]
fn test_search_query_usize_max_limit() {
    let q = SearchQuery::new().with_limit(usize::MAX);
    assert_eq!(q.limit, usize::MAX);
}

#[test]
fn test_search_query_with_text_overwrites() {
    let q = SearchQuery::text("a").with_text("b");
    assert_eq!(q.text_query, Some("b".to_string()));
}

// ============================================================================
// TemporalFlags
// ============================================================================

#[test]
fn test_temporal_flags_contradictory() {
    // hot+cold both true — valid by design (combinable)
    let flags = TemporalFlags {
        hot: true,
        cold: true,
        ..Default::default()
    };
    assert!(flags.hot);
    assert!(flags.cold);
}

#[test]
fn test_temporal_flags_all_true() {
    let flags = TemporalFlags {
        blast_radius: true,
        hot: true,
        cold: true,
        risky: true,
    };
    assert!(flags.blast_radius);
    assert!(flags.hot);
    assert!(flags.cold);
    assert!(flags.risky);
}

// ============================================================================
// SearchField
// ============================================================================

#[test]
fn test_search_field_all_boosts_positive() {
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
        assert!(
            field.default_boost() > 0.0,
            "{field:?} boost should be positive"
        );
    }
}

#[test]
fn test_search_field_boost_ordering() {
    assert!(
        SearchField::TypeDefinition.default_boost()
            > SearchField::FunctionSignature.default_boost()
    );
    assert!(
        SearchField::FunctionSignature.default_boost() > SearchField::SymbolName.default_boost()
    );
    assert!(SearchField::SymbolName.default_boost() > SearchField::ImportExport.default_boost());
    assert!(SearchField::ImportExport.default_boost() > SearchField::FunctionBody.default_boost());
    assert!(SearchField::FunctionBody.default_boost() > SearchField::Comment.default_boost());
    assert!(SearchField::Comment.default_boost() > SearchField::StringLiteral.default_boost());
}

// ============================================================================
// IndexStats
// ============================================================================

#[test]
fn test_index_stats_all_zeros() {
    let stats = IndexStats {
        file_count: 0,
        total_ngrams: 0,
        index_size_bytes: 0,
        last_updated: 0,
        format_version: 0,
    };
    assert_eq!(stats.file_count, 0);
    assert_eq!(stats.format_version, 0);
}

// ============================================================================
// FileTable
// ============================================================================

#[test]
fn test_file_table_default_trait() {
    let a = FileTable::default();
    let b = FileTable::new();
    assert!(a.is_empty());
    assert!(b.is_empty());
    assert_eq!(a.len(), b.len());
}

// ============================================================================
// SearchResult
// ============================================================================

#[test]
fn test_search_result_nan_score_constructible() {
    // Documents: NaN score is valid, consumers must handle
    let result = SearchResult {
        file_path: PathBuf::from("test.rs"),
        line_range: LineRange::new(1, 1),
        score: f32::NAN,
        matched_field: SearchField::FunctionBody,
        snippet: String::new(),
        match_positions: vec![],
    };
    assert!(result.score.is_nan());
}

#[test]
fn test_search_result_negative_score() {
    // Documents: no validation on score
    let result = SearchResult {
        file_path: PathBuf::from("test.rs"),
        line_range: LineRange::new(1, 1),
        score: -1.0,
        matched_field: SearchField::FunctionBody,
        snippet: String::new(),
        match_positions: vec![],
    };
    assert_eq!(result.score, -1.0);
}
