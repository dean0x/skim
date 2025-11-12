//! Parser module - tree-sitter wrapper
//!
//! ARCHITECTURE: This is the ONLY module that imports tree-sitter.
//! All AST operations happen here or in transform module.
//!
//! Design: Parser instance is bound to a specific language.
//!
//! NOTE: Parser struct is defined in types.rs for better module organization.
//! This module only contains language-specific helper functions.

pub mod language;

// Tests moved to validate actual Parser struct (not deleted duplicate)
#[cfg(test)]
#[allow(clippy::unwrap_used)] // Unwrapping is acceptable in tests
mod tests {
    use crate::{Language, Parser};

    #[test]
    fn test_parser_typescript() {
        let source = "function test() {}";
        let mut parser = Parser::new(Language::TypeScript).unwrap();
        let result = parser.parse(source);
        assert!(result.is_ok());
    }

    #[test]
    fn test_parser_all_languages() {
        let test_cases = vec![
            (Language::TypeScript, "function test() {}"),
            (Language::JavaScript, "function test() {}"),
            (Language::Python, "def test():\n    pass"),
            (Language::Rust, "fn test() {}"),
            (Language::Go, "func test() {}"),
            (Language::Java, "class Test { void test() {} }"),
        ];

        for (language, source) in test_cases {
            let mut parser = Parser::new(language).unwrap();
            let result = parser.parse(source);
            assert!(result.is_ok(), "Failed to parse {:?}", language);
        }
    }

    #[test]
    fn test_parser_invalid_syntax() {
        let source = "function {{{{{ this is broken";
        let mut parser = Parser::new(Language::TypeScript).unwrap();
        let result = parser.parse(source);
        // tree-sitter is error-tolerant, so this should still return a tree
        // but with error nodes
        assert!(result.is_ok());
    }
}
