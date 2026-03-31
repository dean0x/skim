//! Verify From impls, ? operator compatibility, and error trait chain.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::error::Error;
use std::io;

use rskim_search::SearchError;

fn assert_send<T: Send>() {}

#[test]
fn test_io_error_converts() {
    let io_err = io::Error::new(io::ErrorKind::NotFound, "missing");
    let search_err: SearchError = io_err.into();
    assert!(matches!(search_err, SearchError::Io(_)));
}

#[test]
fn test_skim_error_converts() {
    let core_err = rskim_core::SkimError::ParseError("bad parse".to_string());
    let search_err: SearchError = core_err.into();
    assert!(matches!(search_err, SearchError::CoreError(_)));
}

#[test]
fn test_question_mark_io_error() {
    fn inner() -> rskim_search::Result<()> {
        let _file = std::fs::File::open("/nonexistent/path/that/does/not/exist")?;
        Ok(())
    }
    let result = inner();
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), SearchError::Io(_)));
}

#[test]
fn test_question_mark_skim_error() {
    fn inner() -> rskim_search::Result<()> {
        let err: rskim_core::SkimError = rskim_core::SkimError::ParseError("fail".to_string());
        Err(err)?;
        Ok(())
    }
    let result = inner();
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), SearchError::CoreError(_)));
}

#[test]
fn test_search_error_non_exhaustive_match() {
    let err = SearchError::IndexError("test".to_string());
    // Wildcard arm required by #[non_exhaustive]
    match err {
        SearchError::Io(_) => panic!("wrong variant"),
        SearchError::IndexError(_) => {} // expected
        SearchError::InvalidQuery(_) => panic!("wrong variant"),
        SearchError::CoreError(_) => panic!("wrong variant"),
        SearchError::SerializationError(_) => panic!("wrong variant"),
        _ => {} // non_exhaustive wildcard — must compile
    }
}

#[test]
fn test_search_error_is_send() {
    assert_send::<SearchError>();
}

#[test]
fn test_search_error_io_source_chain() {
    let io_err = io::Error::new(io::ErrorKind::BrokenPipe, "pipe broke");
    let search_err: SearchError = io_err.into();
    let source = search_err.source();
    assert!(source.is_some());
    assert!(source
        .expect("source should exist")
        .downcast_ref::<io::Error>()
        .is_some());
}

#[test]
fn test_search_error_core_source_chain() {
    let core_err = rskim_core::SkimError::ParseError("bad".to_string());
    let search_err: SearchError = core_err.into();
    let source = search_err.source();
    assert!(source.is_some());
    assert!(source
        .expect("source should exist")
        .downcast_ref::<rskim_core::SkimError>()
        .is_some());
}
