//! Compile-time verification that all traits are object-safe with correct bounds.

use rskim_search::{
    FieldClassifier, FileId, FileTable, IndexStats, LayerBuilder, LineRange, MatchSpan, Result,
    SearchError, SearchField, SearchLayer, SearchQuery, SearchResult, TemporalFlags,
};

fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}
fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn test_search_layer_object_safe() {
    fn _f(_: &dyn SearchLayer) {}
}

#[test]
fn test_search_layer_boxed() {
    fn _f(_: Box<dyn SearchLayer>) {}
}

#[test]
fn test_layer_builder_object_safe() {
    fn _f(_: Box<dyn LayerBuilder>) {}
}

#[test]
fn test_field_classifier_object_safe() {
    fn _f(_: &dyn FieldClassifier) {}
}

#[test]
fn test_search_layer_send_sync() {
    assert_send_sync::<Box<dyn SearchLayer>>();
}

#[test]
fn test_layer_builder_send() {
    assert_send::<Box<dyn LayerBuilder>>();
}

#[test]
fn test_field_classifier_send_sync() {
    assert_send_sync::<Box<dyn FieldClassifier>>();
}

/// Imports every public symbol from rskim_search — catches accidental re-export removal.
#[test]
fn test_public_api_surface() {
    // If any of these types are removed from the public API, this test fails to compile.
    let _ = std::any::type_name::<FileId>();
    let _ = std::any::type_name::<FileTable>();
    let _ = std::any::type_name::<IndexStats>();
    let _ = std::any::type_name::<LineRange>();
    let _ = std::any::type_name::<MatchSpan>();
    let _ = std::any::type_name::<Result<()>>();
    let _ = std::any::type_name::<SearchError>();
    let _ = std::any::type_name::<SearchField>();
    let _ = std::any::type_name::<SearchQuery>();
    let _ = std::any::type_name::<SearchResult>();
    let _ = std::any::type_name::<TemporalFlags>();
    let _ = std::any::type_name::<dyn SearchLayer>();
    let _ = std::any::type_name::<dyn LayerBuilder>();
    let _ = std::any::type_name::<dyn FieldClassifier>();

    assert_send::<SearchError>();
    assert_sync::<SearchError>();
}
