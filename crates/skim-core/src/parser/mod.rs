//! Parser module - tree-sitter wrapper
//!
//! ARCHITECTURE: This is the ONLY module that imports tree-sitter.
//! All AST operations happen here or in transform module.
//!
//! Design: Parser instance is bound to a specific language.

pub mod language;

use crate::{Language, Result, SkimError};
use tree_sitter::{Parser, Tree};

/// Parse source code with tree-sitter
///
/// ARCHITECTURE: This function is internal to the library.
/// Public API uses `Parser` struct from types.rs.
///
/// # Implementation
///
/// 1. Create tree-sitter Parser
/// 2. Set language grammar based on Language enum
/// 3. Parse source → Tree
/// 4. Handle parse errors gracefully (tree-sitter returns Option)
pub(crate) fn parse_source(
    source: &str,
    language: Language,
) -> Result<Tree> {
    let mut parser = Parser::new();

    // Set language grammar
    let ts_language = match language {
        Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
        Language::JavaScript => tree_sitter_javascript::LANGUAGE,
        Language::Python => tree_sitter_python::LANGUAGE,
        Language::Rust => tree_sitter_rust::LANGUAGE,
        Language::Go => tree_sitter_go::LANGUAGE,
        Language::Java => tree_sitter_java::LANGUAGE,
    };

    parser
        .set_language(&ts_language.into())
        .map_err(|e| SkimError::ParseError(format!("Failed to set language: {}", e)))?;

    // Parse source
    parser
        .parse(source, None)
        .ok_or_else(|| SkimError::ParseError("tree-sitter returned None (parse failed)".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_typescript() {
        let source = "function test() {}";
        let result = parse_source(source, Language::TypeScript);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parse_all_languages() {
        let test_cases = vec![
            (Language::TypeScript, "function test() {}"),
            (Language::JavaScript, "function test() {}"),
            (Language::Python, "def test():\n    pass"),
            (Language::Rust, "fn test() {}"),
            (Language::Go, "func test() {}"),
            (Language::Java, "class Test { void test() {} }"),
        ];

        for (language, source) in test_cases {
            let result = parse_source(source, language);
            assert!(result.is_ok(), "Failed to parse {:?}", language);
        }
    }

    #[test]
    fn test_parse_invalid_syntax() {
        let source = "function {{{{{ this is broken";
        let result = parse_source(source, Language::TypeScript);
        // tree-sitter is error-tolerant, so this should still return a tree
        // but with error nodes
        assert!(result.is_ok());
    }
}
