//! Transformation module - AST to transformed source
//!
//! ARCHITECTURE: This module operates on tree-sitter Trees.
//! Each mode has its own transformation strategy.
//! JSON, YAML, and TOML are handled separately without tree-sitter (serde-based).

pub(crate) mod json;
pub(crate) mod minimal;
pub(crate) mod pseudo;
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
        Mode::Pseudo => pseudo::transform_pseudo_with_spans(source, tree, language, config),
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

/// Transform AST and return text, NodeSpan metadata, AND source line map.
///
/// ARCHITECTURE: Extended version of `transform_tree` that additionally returns
/// a source line map when `config.line_numbers` is true. The source line map
/// maps each output line index (0-based) to its 1-indexed source line number.
/// Value `0` indicates an omission/truncation marker (no line number annotation).
///
/// When `config.line_numbers` is false, returns `None` for the source line map
/// (avoids unnecessary computation).
///
/// # Design Decision (AC-18)
/// Line number computation is done inside the core library (rskim-core) so that
/// the CLI layer can simply apply `format_with_line_numbers` without understanding
/// each mode's internal structure. This keeps the CLI layer thin while the core
/// library owns the mode-specific knowledge.
pub(crate) fn transform_tree_with_line_map(
    source: &str,
    tree: &Tree,
    language: Language,
    config: &TransformConfig,
) -> Result<(String, Option<Vec<usize>>)> {
    if !config.line_numbers {
        let text = transform_tree(source, tree, language, config)?;
        return Ok((text, None));
    }

    // For modes that support source line maps, compute them alongside the transform.
    let (text, spans, line_map) = match config.mode {
        Mode::Structure => {
            structure::transform_structure_with_spans_and_line_map(source, tree, language, config)?
        }
        Mode::Signatures => {
            signatures::transform_signatures_with_spans_and_line_map(source, tree, language)?
        }
        Mode::Types => {
            types::transform_types_with_spans_and_line_map(source, tree, language)?
        }
        Mode::Full => {
            // Full mode: identity map
            let text = source.to_string();
            let line_count = text.lines().count();
            let spans = vec![NodeSpan::new(0..line_count, "source_file")];
            let line_map: Vec<usize> = (1..=line_count).collect();
            (text, spans, line_map)
        }
        Mode::Minimal => {
            // Minimal mode: identity map over output (minimal keeps most source lines)
            let text = minimal::transform_minimal(source, tree, language, config)?;
            let line_count = text.lines().count();
            let spans = vec![NodeSpan::new(0..line_count, "source_file")];
            // For minimal mode, compute the line map by text matching
            let line_map = compute_line_map_by_text_matching(source, &text);
            (text, spans, line_map)
        }
        Mode::Pseudo => {
            // Pseudo mode: compute line map by text matching after transform
            let (text, spans) =
                pseudo::transform_pseudo_with_spans(source, tree, language, config)?;
            let line_map = compute_line_map_by_text_matching(source, &text);
            (text, spans, line_map)
        }
    };

    // Apply max_lines truncation (adjusting the line map)
    let (final_text, final_line_map) = if let Some(max_lines) = config.max_lines {
        let truncated_text = truncate::truncate_to_lines(&text, &spans, language, max_lines)?;
        // After truncation, the output has a subset of lines plus omission markers.
        // Rebuild the line map: match output lines back to pre-truncation line map.
        let final_line_map = reconcile_line_map_after_truncation(&text, &truncated_text, &line_map);
        (truncated_text, final_line_map)
    } else {
        (text, line_map)
    };

    Ok((final_text, Some(final_line_map)))
}

/// Compute a source line map by matching output lines to source lines (text scan).
///
/// ARCHITECTURE: Used for Minimal and Pseudo modes where removed ranges leave
/// verbatim sections of source in the output. Each output line is matched to
/// the first unmatched source line with identical content.
///
/// This is a best-effort heuristic: if identical lines appear multiple times,
/// the first unmatched occurrence is used. In practice this is correct for
/// minimal/pseudo modes because lines are processed in source order.
pub(crate) fn compute_line_map_by_text_matching(source: &str, output: &str) -> Vec<usize> {
    let source_lines: Vec<&str> = source.lines().collect();
    let output_lines: Vec<&str> = output.lines().collect();

    // Track current position in source to maintain order
    let mut source_pos = 0usize;
    let mut result = Vec::with_capacity(output_lines.len());

    for output_line in &output_lines {
        // Search for this output line in source, starting from current position
        let mut found = false;
        for (offset, source_line) in source_lines[source_pos..].iter().enumerate() {
            if *source_line == *output_line {
                let source_line_num = source_pos + offset + 1; // 1-indexed
                result.push(source_line_num);
                source_pos = source_pos + offset + 1;
                found = true;
                break;
            }
        }
        if !found {
            // Line not found in remaining source (could be an omission marker)
            result.push(0);
        }
    }

    result
}

/// Reconcile source line map after AST-aware truncation.
///
/// After `truncate_to_lines`, the output may have omission markers inserted
/// and some lines may be reordered or dropped. This function builds the final
/// line map by matching each truncated output line back to the pre-truncation
/// line map via text comparison.
///
/// Lines in the truncated output that match lines in the pre-truncation output
/// get their source line from the pre-truncation map. Omission markers (not in
/// the pre-truncation output) get source line 0.
pub(crate) fn reconcile_line_map_after_truncation(
    pre_trunc_text: &str,
    truncated_text: &str,
    pre_trunc_line_map: &[usize],
) -> Vec<usize> {
    let pre_lines: Vec<&str> = pre_trunc_text.lines().collect();
    let trunc_lines: Vec<&str> = truncated_text.lines().collect();

    // Build a lookup: line content -> list of (line_index, source_line_num) in pre-truncation
    // Use a simple scan approach since truncation is rare (most files aren't truncated).
    let mut result = Vec::with_capacity(trunc_lines.len());
    let mut pre_used = vec![false; pre_lines.len()];

    for trunc_line in &trunc_lines {
        // Find the first unused matching line in pre-truncation output
        let mut found = false;
        for (idx, pre_line) in pre_lines.iter().enumerate() {
            if !pre_used[idx] && pre_line == trunc_line {
                let source_line = pre_trunc_line_map.get(idx).copied().unwrap_or(0);
                result.push(source_line);
                pre_used[idx] = true;
                found = true;
                break;
            }
        }
        if !found {
            // Omission marker or line not in pre-truncation output
            result.push(0);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // compute_line_map_by_text_matching
    // ========================================================================

    #[test]
    fn test_text_matching_identity() {
        let source = "line 1\nline 2\nline 3\n";
        let output = "line 1\nline 2\nline 3\n";
        let map = compute_line_map_by_text_matching(source, output);
        assert_eq!(map, vec![1, 2, 3]);
    }

    #[test]
    fn test_text_matching_skipped_lines() {
        // Output has lines 1 and 3 from source (line 2 was removed)
        let source = "aaa\nbbb\nccc\n";
        let output = "aaa\nccc\n";
        let map = compute_line_map_by_text_matching(source, output);
        assert_eq!(map, vec![1, 3]);
    }

    #[test]
    fn test_text_matching_unmatched_line() {
        // Output has a line not in source (e.g., omission marker)
        let source = "aaa\nbbb\n";
        let output = "aaa\n// ...\nbbb\n";
        let map = compute_line_map_by_text_matching(source, output);
        assert_eq!(map, vec![1, 0, 2]);
    }

    #[test]
    fn test_text_matching_empty() {
        let map = compute_line_map_by_text_matching("", "");
        assert!(map.is_empty());
    }

    #[test]
    fn test_text_matching_duplicate_lines() {
        // Source has duplicates; should match in order
        let source = "x\nx\nx\n";
        let output = "x\nx\n";
        let map = compute_line_map_by_text_matching(source, output);
        assert_eq!(map, vec![1, 2]);
    }

    // ========================================================================
    // reconcile_line_map_after_truncation
    // ========================================================================

    #[test]
    fn test_reconcile_identity() {
        // No truncation happened
        let pre = "aaa\nbbb\nccc\n";
        let trunc = "aaa\nbbb\nccc\n";
        let pre_map = vec![1, 5, 10];
        let result = reconcile_line_map_after_truncation(pre, trunc, &pre_map);
        assert_eq!(result, vec![1, 5, 10]);
    }

    #[test]
    fn test_reconcile_with_dropped_line() {
        let pre = "aaa\nbbb\nccc\n";
        let trunc = "aaa\nccc\n";
        let pre_map = vec![1, 5, 10];
        let result = reconcile_line_map_after_truncation(pre, trunc, &pre_map);
        assert_eq!(result, vec![1, 10]);
    }

    #[test]
    fn test_reconcile_with_omission_marker() {
        let pre = "aaa\nbbb\nccc\n";
        let trunc = "aaa\n/* ... */\nccc\n";
        let pre_map = vec![1, 5, 10];
        let result = reconcile_line_map_after_truncation(pre, trunc, &pre_map);
        // "aaa" -> 1, "/* ... */" not in pre -> 0, "ccc" -> 10
        assert_eq!(result, vec![1, 0, 10]);
    }

    #[test]
    fn test_reconcile_empty() {
        let result = reconcile_line_map_after_truncation("", "", &[]);
        assert!(result.is_empty());
    }
}
