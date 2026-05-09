//! Skim Search - Code search foundation library
//!
//! # Architecture
//!
//! **IMPORTANT: This is a LIBRARY with NO I/O.**
//! - Accepts pre-parsed data, not file paths
//! - Returns Result types, not stdout writes
//! - Pure types and traits, no side effects
//!
//! CLI/binary code in `crates/rskim/src/cmd/search.rs` handles all I/O.

mod types;

pub use types::*;

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_public_api_accessible() {
        let _id = FileId(0);
        let _query = SearchQuery::new("test");
        let _field = SearchField::TypeDefinition;
        let _flags = TemporalFlags::default();
        let _stats = IndexStats {
            file_count: 0,
            total_ngrams: 0,
            index_size_bytes: 0,
            last_updated: None,
        };

        // Verify traits are in scope (compile-time check)
        fn _assert_search_layer<T: SearchLayer>() {}
        fn _assert_layer_builder<T: LayerBuilder>() {}
        fn _assert_field_classifier<T: FieldClassifier>() {}
    }
}
