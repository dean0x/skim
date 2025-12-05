//! Transformation module - AST to transformed source
//!
//! ARCHITECTURE: This module operates on tree-sitter Trees.
//! Each mode has its own transformation strategy.
//! JSON is handled separately without tree-sitter.

pub mod json;
pub mod signatures;
pub mod structure;
pub mod types;
pub mod yaml;

use crate::{Language, Mode, Result, TransformConfig};
use tree_sitter::Tree;

/// Transform AST based on configuration
///
/// ARCHITECTURE: Dispatcher function that routes to mode-specific transformers.
///
/// # Implementation Strategy (Week 2-3)
///
/// 1. Match on config.mode
/// 2. Call appropriate transformer (structure, signatures, types)
/// 3. Return transformed string
///
/// # Performance Notes
///
/// - Preallocate output String with estimated capacity
/// - Use &str slices from source (zero-copy)
/// - Avoid intermediate allocations
pub(crate) fn transform_tree(
    source: &str,
    tree: &Tree,
    language: Language,
    config: &TransformConfig,
) -> Result<String> {
    match config.mode {
        Mode::Structure => structure::transform_structure(source, tree, language, config),
        Mode::Signatures => signatures::transform_signatures(source, tree, language, config),
        Mode::Types => types::transform_types(source, tree, language, config),
        Mode::Full => Ok(source.to_string()), // No transformation
    }
}

#[cfg(test)]
mod tests {
    // NOTE: Tests require parser implementation
    // These are schema validation only
}
