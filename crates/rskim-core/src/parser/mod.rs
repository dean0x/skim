//! Parser module - tree-sitter wrapper
//!
//! The `Parser` type in `types.rs` wraps tree-sitter and selects the language.
//! This module owns the language helpers and input limits.

pub(crate) mod grammar_limits;
pub mod language;

// Tests moved to validate actual Parser struct (not deleted duplicate)
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // Unwrapping/expect is acceptable in tests
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
            (Language::C, "int test() { return 0; }"),
            (Language::Cpp, "int test() { return 0; }"),
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
