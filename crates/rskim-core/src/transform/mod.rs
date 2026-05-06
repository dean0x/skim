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
        Mode::Signatures => signatures::transform_signatures_with_spans_and_line_map(
            source, tree, language, config,
        )?,
        Mode::Types => {
            types::transform_types_with_spans_and_line_map(source, tree, language, config)?
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
            // Pseudo mode: compute line map from byte-level removal ranges.
            // Text matching fails here because pseudo mode modifies lines (e.g.,
            // `def f(a: int) -> int:` → `def f(a):`) so the output line is not
            // verbatim in the source.
            pseudo::transform_pseudo_with_spans_and_line_map(source, tree, language, config)?
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

/// Compute byte offsets of line starts for a UTF-8 string's raw bytes.
///
/// Returns a `Vec` where `result[i]` is the byte offset of the first byte of
/// line `i + 1` (1-indexed). The first entry is always `0`. Each subsequent
/// entry is the byte immediately after a `'\n'`.
///
/// Newlines are always single-byte ASCII, so iterating over raw bytes is both
/// correct and avoids unnecessary UTF-8 decoding overhead.
pub(crate) fn compute_line_starts(bytes: &[u8]) -> Vec<usize> {
    std::iter::once(0)
        .chain(bytes.iter().enumerate().filter_map(
            |(i, &b)| {
                if b == b'\n' {
                    Some(i + 1)
                } else {
                    None
                }
            },
        ))
        .collect()
}

/// Compute a source line map by matching output lines to source lines (text scan).
///
/// ARCHITECTURE: Used for Minimal mode where removed ranges leave verbatim
/// sections of source in the output. Each output line is matched to the first
/// unmatched source line with identical content.
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
                source_pos += offset + 1;
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

/// Compute a source line map from sorted byte ranges removed from source.
///
/// ARCHITECTURE: Used by pseudo mode where removed ranges can *partially modify*
/// a line (e.g., `def f(a: int) -> int:` → `def f(a):`). Text matching cannot
/// find such lines in source because their content differs.
///
/// This function walks the source bytes, skipping removed ranges, and for each
/// newline that appears in the resulting output, records which source line we
/// were on when the output line started. The first byte contributed to an output
/// line determines its source line number (1-indexed). Source lines that are
/// removed entirely produce no output lines.
///
/// The ranges must be sorted (ascending by start byte) and non-overlapping.
pub(crate) fn compute_line_map_from_removed_ranges(
    source: &str,
    ranges: &[(usize, usize)],
) -> Vec<usize> {
    let source_bytes = source.as_bytes();
    let total_bytes = source.len();

    // Precompute byte offsets of line starts for O(log n) line-number lookup.
    // line_starts[i] = byte offset of the first byte of line (i+1).
    // This replaces the previous dense Vec<usize> (one entry per source byte,
    // 8 bytes/byte on 64-bit) with a much smaller Vec sized by line count.
    let line_starts: Vec<usize> = compute_line_starts(source_bytes);

    // Returns the 1-indexed source line number for byte position `pos`.
    let byte_to_line = |pos: usize| -> usize {
        match line_starts.binary_search(&pos) {
            Ok(idx) => idx + 1,
            Err(idx) => idx.max(1), // idx is the number of line starts strictly before pos
        }
    };

    let mut result: Vec<usize> = Vec::new();
    // Source line number for the current (not-yet-emitted) output line.
    // None = no bytes have been contributed to the current output line yet.
    let mut current_output_source_line: Option<usize> = None;

    let mut range_idx = 0usize;
    let mut pos = 0usize;

    while pos < total_bytes {
        // Advance past any removed range that covers the current position.
        while range_idx < ranges.len() && pos >= ranges[range_idx].0 {
            let range_end = ranges[range_idx].1;
            range_idx += 1;
            if range_end > pos {
                pos = range_end;
            }
        }
        if pos >= total_bytes {
            break;
        }

        let byte = source_bytes[pos];
        let src_line = byte_to_line(pos);

        // Record the source line for this output line on the first byte.
        if current_output_source_line.is_none() {
            current_output_source_line = Some(src_line);
        }

        if byte == b'\n' {
            // A newline in the output ends the current output line.
            result.push(current_output_source_line.unwrap_or(src_line));
            current_output_source_line = None;
        }

        pos += 1;
    }

    // Handle a final line with no trailing newline.
    if let Some(src_line) = current_output_source_line {
        result.push(src_line);
    }

    result
}

/// Normalize a line map to match `trim_and_normalize`'s blank-line dropping.
///
/// `trim_and_normalize` drops output lines when there are 3+ consecutive blank
/// lines (keeping at most 2). The line map computed from byte ranges has one
/// entry per line in the intermediate output (after `remove_ranges` and
/// `collapse_whitespace`, before `trim_and_normalize`). This function replays
/// the same blank-line dropping logic over the line map to keep it in sync.
///
/// `pre_normalized_text` is the intermediate text (after `collapse_whitespace`,
/// before `trim_and_normalize`). `line_map` has the same length as the number
/// of lines in `pre_normalized_text`. Returns a filtered line map that matches
/// the final post-normalized output.
pub(crate) fn normalize_line_map_blanks(
    pre_normalized_text: &str,
    line_map: Vec<usize>,
) -> Vec<usize> {
    let mut result = Vec::with_capacity(line_map.len());
    let mut consecutive_blanks: usize = 0;

    for (line, &src_line) in pre_normalized_text.lines().zip(line_map.iter()) {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            consecutive_blanks += 1;
            if consecutive_blanks > 2 {
                // trim_and_normalize drops this line — skip it in the map too.
                continue;
            }
        } else {
            consecutive_blanks = 0;
        }
        result.push(src_line);
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
///
/// # Monotonic matching
///
/// Truncation preserves document order: the truncated output is always a
/// subsequence of the pre-truncation output (with optional omission markers
/// inserted). Therefore each matched position must be >= the previous matched
/// position. Monotonic matching prevents duplicate lines (e.g. multiple `}`
/// closings at different source positions) from being mapped to their first
/// occurrence rather than the correct occurrence in the tail.
pub(crate) fn reconcile_line_map_after_truncation(
    pre_trunc_text: &str,
    truncated_text: &str,
    pre_trunc_line_map: &[usize],
) -> Vec<usize> {
    let pre_lines: Vec<&str> = pre_trunc_text.lines().collect();
    let trunc_lines: Vec<&str> = truncated_text.lines().collect();

    // Use a monotonic cursor: each new match must start at or after the
    // previous match position. This exploits document-order preservation.
    let mut result = Vec::with_capacity(trunc_lines.len());
    let mut cursor = 0usize; // next search starts here

    for trunc_line in &trunc_lines {
        // Find the first matching line at or after cursor.
        // Using .position() on the tail slice avoids the range-loop and
        // mut-range-bound lints while keeping monotonic semantics.
        let tail = &pre_lines[cursor..];
        if let Some(offset) = tail.iter().position(|pre| pre == trunc_line) {
            let abs_idx = cursor + offset;
            let source_line = pre_trunc_line_map.get(abs_idx).copied().unwrap_or(0);
            result.push(source_line);
            cursor = abs_idx + 1; // next search must be strictly after this match
        } else {
            // Omission marker or line not in remaining pre-truncation output
            result.push(0);
            // cursor does NOT advance: the next content line still searches
            // from the same position (markers don't consume pre-trunc lines)
        }
    }

    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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

    #[test]
    fn test_reconcile_duplicate_lines_tail_bias() {
        // Pre-truncation text has `}` at positions 3, 5, and 7 (pre_map values 3, 5, 7).
        // After --last-lines style truncation the tail keeps lines from position 5 onward.
        // Monotonic matching must map the trailing `}` to position 5/7, not to 3.
        let pre = "a\nb\n}\nc\n}\nd\n}\n";
        // pre_map: a=1, b=2, }=3, c=4, }=5, d=6, }=7
        let pre_map = vec![1, 2, 3, 4, 5, 6, 7];
        // Simulated --last-lines output: marker + last 3 content lines (c, }, d, } → 4 content)
        // Use last 4 lines (c, }, d, }) for simplicity
        let trunc = "/* ... */\nc\n}\nd\n}\n";
        let result = reconcile_line_map_after_truncation(pre, trunc, &pre_map);
        // "/* ... */" not found → 0
        // "c" → 4 (cursor advances past 4)
        // "}" → 5 (first `}` at or after cursor=4 is index 4, pre_map[4]=5)
        // "d" → 6
        // "}" → 7 (next `}` at or after cursor=5 is index 6, pre_map[6]=7)
        assert_eq!(result, vec![0, 4, 5, 6, 7]);
    }

    #[test]
    fn test_reconcile_omission_marker_does_not_advance_cursor() {
        // An omission marker (not in pre text) should leave the cursor unchanged so
        // the following content line still finds its correct pre-truncation position.
        let pre = "x\ny\nz\n";
        let pre_map = vec![10, 20, 30];
        // Simulated: marker inserted before y (marker is not in pre)
        let trunc = "/* ... */\ny\nz\n";
        let result = reconcile_line_map_after_truncation(pre, trunc, &pre_map);
        // "/* ... */" not found → 0, cursor stays at 0
        // "y" found at index 1 → 20, cursor advances to 2
        // "z" found at index 2 → 30
        assert_eq!(result, vec![0, 20, 30]);
    }

    // ========================================================================
    // compute_line_map_from_removed_ranges
    // ========================================================================

    #[test]
    fn test_from_ranges_identity_no_ranges() {
        // No ranges removed: each output line maps to its source line.
        let source = "aaa\nbbb\nccc\n";
        let map = compute_line_map_from_removed_ranges(source, &[]);
        assert_eq!(map, vec![1, 2, 3]);
    }

    #[test]
    fn test_from_ranges_whole_line_removed() {
        // Remove the middle line entirely (including its newline).
        // source: "aaa\nbbb\nccc\n"
        // ranges: remove bytes 4..8 ("bbb\n")
        let source = "aaa\nbbb\nccc\n";
        let ranges = [(4, 8)]; // removes "bbb\n"
        let map = compute_line_map_from_removed_ranges(source, &ranges);
        // Output: "aaa\nccc\n" → lines [aaa, ccc]
        // "aaa" starts at source line 1; "ccc" starts at source line 3
        assert_eq!(map, vec![1, 3]);
    }

    #[test]
    fn test_from_ranges_inline_range_removed() {
        // Remove only part of a line (inline modification).
        // source: "def foo(a: int):\n    pass\n"
        // Remove ": int" (bytes 9..15) from the first line → "def foo(a):\n"
        let source = "def foo(a: int):\n    pass\n";
        let colon_int = source.find(": int").unwrap();
        let ranges = [(colon_int, colon_int + ": int".len())];
        let map = compute_line_map_from_removed_ranges(source, &ranges);
        // Output: "def foo(a):\n    pass\n" → lines [def foo(a):, "    pass"]
        // Both output lines originate from their respective source lines.
        assert_eq!(map, vec![1, 2]);
    }

    #[test]
    fn test_from_ranges_modified_def_line_maps_to_correct_source_line() {
        // Regression test for the pseudo-mode bug: a `def` line whose type
        // annotations are stripped still maps to its original source line.
        //
        // source (4 lines):
        //   1: def foo(a: int) -> str:\n
        //   2:     return str(a)\n
        //   3: def bar(b: str) -> int:\n
        //   4:     return len(b)\n
        let source = "def foo(a: int) -> str:\n    return str(a)\ndef bar(b: str) -> int:\n    return len(b)\n";
        //
        // Simulate removing `: int` (bytes 9..14) and ` -> str` (bytes 14..22)
        // from the first def line, and `: str` (bytes ?) and ` -> int` from the
        // third def line. Rather than computing exact byte offsets, use a simple
        // helper: remove ranges [9..14] and [14..22] from line 1.
        //
        // For this test we just verify that the first output line maps to source
        // line 1, even after inline removal (which text-matching would fail).
        // `: int` starts after "def foo(a"
        let a_end = 9usize; // byte after 'a'
        let colon_int_end = a_end + ": int".len(); // 14
                                                   // " -> str" starts at 14; ranges remove ": int" and " -> str", producing "def foo(a):"
        let arrow_end = colon_int_end + " -> str".len(); // 21
        let ranges = [(a_end, colon_int_end), (colon_int_end, arrow_end)];
        let map = compute_line_map_from_removed_ranges(source, &ranges);
        // First output line ("def foo(a):...") must map to source line 1.
        assert_eq!(
            map[0], 1,
            "Modified def line must map to source line 1, not 0. Got map: {:?}",
            map
        );
        // Body lines on source lines 2 and 4 must also be correct.
        assert_eq!(
            map[1], 2,
            "return str(a) must map to source line 2. Got map: {:?}",
            map
        );
    }

    #[test]
    fn test_from_ranges_empty_source() {
        let map = compute_line_map_from_removed_ranges("", &[]);
        assert!(map.is_empty());
    }

    #[test]
    fn test_from_ranges_no_trailing_newline() {
        // Source without trailing newline: last line still gets an entry.
        let source = "aaa\nbbb";
        let map = compute_line_map_from_removed_ranges(source, &[]);
        assert_eq!(map, vec![1, 2]);
    }

    // ========================================================================
    // normalize_line_map_blanks
    // ========================================================================

    #[test]
    fn test_normalize_line_map_no_excess_blanks() {
        // Fast path: counts already match (no 3+ blank runs).
        let text = "a\n\nb\n";
        let line_map = vec![1, 2, 3];
        let result = normalize_line_map_blanks(text, line_map.clone());
        assert_eq!(result, line_map);
    }

    #[test]
    fn test_normalize_line_map_drops_third_blank() {
        // Three consecutive blank lines: the third and beyond should be dropped.
        // pre_normalized_text has 3 blank lines in a row.
        let text = "a\n\n\n\nb\n";
        // line_map before normalization: a=1, blank=2, blank=3, blank=4, b=5
        let line_map = vec![1, 2, 3, 4, 5];
        let result = normalize_line_map_blanks(text, line_map);
        // trim_and_normalize keeps at most 2 consecutive blanks:
        // a(keep) blank(keep,1) blank(keep,2) blank(DROP,3) b(keep)
        // → [1, 2, 3, 5] (source lines for kept lines)
        assert_eq!(result, vec![1, 2, 3, 5]);
    }

    #[test]
    fn test_normalize_line_map_empty() {
        let result = normalize_line_map_blanks("", vec![]);
        assert!(result.is_empty());
    }

    // ========================================================================
    // byte_to_line boundary: newline byte sits on the line it terminates
    // ========================================================================

    /// When a removed range includes only the newline byte between two lines,
    /// the two lines are joined in the output. Verify the output line maps to
    /// source line 1 (the line whose bytes appear first in the output).
    ///
    /// source = "ab\ncd\n" (bytes: a=0, b=1, \n=2, c=3, d=4, \n=5)
    /// Remove bytes 2..3 (the first newline) → output = "abcd\n"
    /// The only output line starts with bytes from source line 1, so the map
    /// must be [1].  This exercises binary_search hitting Err(1) for pos=2
    /// (the newline byte itself), which must return source line 1, not 2.
    #[test]
    fn test_from_ranges_newline_byte_boundary() {
        let source = "ab\ncd\n";
        // Remove bytes 2..3 (the '\n' at the end of "ab")
        let ranges = [(2usize, 3usize)];
        let map = compute_line_map_from_removed_ranges(source, &ranges);
        // Output: "abcd\n" — one line whose first byte ('a') is on source line 1.
        assert_eq!(
            map,
            vec![1],
            "Joining two lines by removing only the newline must map output line to source line 1, got {:?}",
            map
        );
    }
}
