//! Transformation module - AST to transformed source
//!
//! ARCHITECTURE: This module operates on tree-sitter Trees.
//! Each mode has its own transformation strategy.
//! JSON, YAML, and TOML are handled separately without tree-sitter (serde-based).

pub(crate) mod json;
pub(crate) mod minimal;
pub(crate) mod signatures;
pub(crate) mod structure;
pub(crate) mod toml;
pub(crate) mod truncate;
pub(crate) mod types;
pub(crate) mod utils;
pub(crate) mod yaml;

use crate::{Language, Mode, Result, TransformConfig};
use tree_sitter::Tree;
use truncate::NodeSpan;

/// Internal result from mode-specific transforms that includes span metadata
///
/// ARCHITECTURE: Each transform mode returns its output text along with NodeSpan
/// metadata describing which output lines correspond to which AST node kinds.
/// This metadata is consumed by the truncation engine when --max-lines is set.
type TransformOutput = (String, Vec<NodeSpan>);

/// Transform AST based on configuration
///
/// ARCHITECTURE: Dispatcher function that routes to mode-specific transformers.
/// When max_lines is set, applies AST-aware truncation as a post-processing step.
///
/// Pipeline:
/// 1. Route to mode-specific transformer -> (text, spans)
/// 2. If max_lines set, apply truncation using spans
/// 3. Return final text
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
    let (text, spans) = transform_tree_with_spans(source, tree, language, config)?;

    // Apply truncation if max_lines is set
    if let Some(max_lines) = config.max_lines {
        truncate::truncate_to_lines(&text, &spans, language, max_lines)
    } else {
        Ok(text)
    }
}

/// Transform AST and return both text and NodeSpan metadata
///
/// Internal function that dispatches to mode-specific transformers and collects
/// span metadata for truncation.
fn transform_tree_with_spans(
    source: &str,
    tree: &Tree,
    language: Language,
    config: &TransformConfig,
) -> Result<TransformOutput> {
    match config.mode {
        Mode::Structure => {
            structure::transform_structure_with_spans(source, tree, language, config)
        }
        Mode::Signatures => {
            signatures::transform_signatures_with_spans(source, tree, language, config)
        }
        Mode::Types => types::transform_types_with_spans(source, tree, language, config),
        // ARCHITECTURE: Full and Minimal produce a single "source_file" span
        // inline (no _with_spans variant needed since there is no AST ranking).
        Mode::Full => {
            let text = source.to_string();
            let line_count = text.lines().count();
            let spans = vec![NodeSpan::new(0..line_count, "source_file")];
            Ok((text, spans))
        }
        Mode::Minimal => {
            let text = minimal::transform_minimal(source, tree, language, config)?;
            let line_count = text.lines().count();
            let spans = vec![NodeSpan::new(0..line_count, "source_file")];
            Ok((text, spans))
        }
    }
}

#[cfg(test)]
mod tests {
    // NOTE: Tests require parser implementation
    // These are schema validation only
}
