//! Parser module - tree-sitter wrapper
//!
//! ARCHITECTURE: This is the ONLY module that imports tree-sitter.
//! All AST operations happen here or in transform module.
//!
//! Design: Parser instance is bound to a specific language.

pub mod language;

use crate::{Language, Result, SkimError};

/// Parse source code with tree-sitter
///
/// ARCHITECTURE: This function is internal to the library.
/// Public API uses `Parser` struct from types.rs.
///
/// # Implementation Notes (Week 1)
///
/// 1. Create tree-sitter Parser
/// 2. Set language grammar
/// 3. Parse source → Tree
/// 4. Handle parse errors gracefully
pub(crate) fn parse_source(
    source: &str,
    language: Language,
) -> Result<tree_sitter::Tree> {
    todo!("Week 1: Implement tree-sitter parsing")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore] // Remove #[ignore] when implemented
    fn test_parse_typescript() {
        let source = "function test() {}";
        let result = parse_source(source, Language::TypeScript);
        assert!(result.is_ok());
    }
}
