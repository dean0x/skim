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
                if b == b'\n' { Some(i + 1) } else { None }
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
/// `trim_and_normalize` has two blank-line rules that reduce output lines:
///
/// 1. **Leading blanks dropped** — blank lines before the first non-blank line
///    are silently discarded. `trim_and_normalize` calls `result.push_str("")`
///    for each such line, but `result` remains empty (an empty push is a no-op),
///    so the blank never appears in the output.
/// 2. **3+ consecutive blanks capped to 2** — blank runs longer than 2 are
///    truncated. The third (and any subsequent) blank in a run is skipped via
///    `continue`.
///
/// Both rules must be mirrored here so the line map stays in sync with the
/// output text that `trim_and_normalize` produces.
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
            // Rule 1: mirror trim_and_normalize's leading-blank drop.
            // trim_and_normalize calls result.push_str("") for every blank line,
            // but since `result` stays empty until the first non-blank content is
            // pushed, those blanks never appear in the output. Skip the map entry
            // for the same leading blank lines.
            if result.is_empty() {
                continue;
            }
            // Rule 2: cap consecutive blanks at 2.
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
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // acceptable in tests
mod tests {
    use super::*;
    use crate::transform::minimal::trim_and_normalize;

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

    #[test]
    fn test_normalize_line_map_leading_blank_dropped() {
        // Regression test for #476 / Fix 5: trim_and_normalize discards blank lines
        // before the first non-blank content (an empty push to an empty result is a
        // no-op). normalize_line_map_blanks must mirror that behaviour.
        // pre_normalized_text: blank(line 1), blank(line 2), "code"(line 3)
        let text = "\n\ncode\n";
        // Three entries in the intermediate map: src lines 1, 2, 3
        let line_map = vec![1, 2, 3];
        let result = normalize_line_map_blanks(text, line_map);
        // Only "code" (src line 3) appears in the output.
        assert_eq!(
            result,
            vec![3],
            "leading blank entries must be dropped to match trim_and_normalize output"
        );
    }

    #[test]
    fn test_normalize_line_map_single_leading_blank_dropped() {
        // K=1 leading blank: the single blank must be dropped.
        let text = "\nfoo\nbar\n";
        let line_map = vec![1, 2, 3];
        let result = normalize_line_map_blanks(text, line_map);
        assert_eq!(result, vec![2, 3]);
    }

    #[test]
    fn test_normalize_line_map_five_leading_blanks_all_dropped() {
        // K=5 leading blanks.  Before the leading-blank fix the 3+ consecutive
        // rule would keep 2 entries (shift of 2), producing vec![24, 25, 26].
        // After the fix all 5 leading blanks are dropped, shift is 0 not 2.
        let text = "\n\n\n\n\nimport os\n";
        // Pre-normalized line map: 5 blank lines (src 21-25) + import (src 26)
        let line_map = vec![21, 22, 23, 24, 25, 26];
        let result = normalize_line_map_blanks(text, line_map);
        assert_eq!(
            result,
            vec![26],
            "all 5 leading blanks must be dropped (shift is 0, not 2)"
        );
    }

    #[test]
    fn test_normalize_line_map_invariant_matches_trim_and_normalize() {
        // Fix 5c invariant: for any input with at least one non-blank line, the
        // map length must equal the line count of trim_and_normalize's output.
        // This is the durable regression guard for the class of defect where the
        // text and the map diverge because of an unmirrored transformation rule.
        let cases: &[(&str, Vec<usize>)] = &[
            // K=1 leading blank
            ("\nimport os\n", vec![21, 22]),
            // K=5 leading blanks
            ("\n\n\n\n\nimport os\n", vec![21, 22, 23, 24, 25, 26]),
            // 3+ consecutive interior blanks (the pre-existing rule)
            ("a\n\n\n\nb\n", vec![1, 2, 3, 4, 5]),
            // Mixed: leading blank + interior 3+ run
            ("\na\n\n\n\nb\n", vec![1, 2, 3, 4, 5, 6]),
        ];
        for (text, line_map) in cases {
            let normalized_map = normalize_line_map_blanks(text, line_map.clone());
            let normalized_text = trim_and_normalize(text);
            assert_eq!(
                normalized_map.len(),
                normalized_text.lines().count(),
                "map length ({}) must match trim_and_normalize line count ({}) for input {:?}",
                normalized_map.len(),
                normalized_text.lines().count(),
                text,
            );
        }
        // Known divergence: all-blank input → text gets 1 line (trailing-newline
        // restore at minimal.rs:406-408) but map returns [].  Harmless: format.rs
        // degrades via `.get(i).unwrap_or(0)` and renders a blank line with no
        // prefix, which is correct output for a blank line.
        let blank_map = normalize_line_map_blanks("\n\n", vec![1, 2]);
        assert!(blank_map.is_empty(), "all-blank input returns empty map");
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

    // ========================================================================
    // Source-level complexity contract for the transform walkers
    // ========================================================================

    /// Root-descending / index-scanning `tree_sitter` node APIs, each paired with
    /// the reason it is banned from the transform walkers.
    ///
    /// Each entry is matched as a plain substring against the *production* region
    /// of the policed file with comments and string literals blanked out. The
    /// leading `.` and the trailing `(` are load-bearing:
    ///
    /// - `.child(` does not match `.child_by_field_name(` or `.child_count(`
    /// - `.named_child(` does not match `.named_children(`
    /// - `.next_sibling(` does not match `TreeCursor::goto_next_sibling(`
    /// - `.next_named_sibling(` does not match `goto_next_named_sibling(`
    ///
    /// The genuinely O(1) traversal set — `Node::walk`,
    /// `TreeCursor::goto_first_child` / `goto_next_sibling`, `Node::children`,
    /// `Node::named_children`, `Node::child_count`, `child_by_field_name` on a node
    /// already in hand — is deliberately absent and must stay absent: those are the
    /// APIs the walkers are *supposed* to use, and `blank_comments_and_strings_self_test`
    /// pins that none of them trips an entry here.
    const FORBIDDEN_NODE_APIS: &[(&str, &str)] = &[
        (
            ".parent()",
            "re-descends from the tree root; thread WalkPosition / depth instead",
        ),
        (
            ".prev_sibling(",
            "O(index-in-parent): scans the parent's child list from 0; use WalkPosition",
        ),
        (
            ".next_sibling(",
            "O(index-in-parent): scans the parent's child list from 0; use a TreeCursor",
        ),
        (
            ".prev_named_sibling(",
            "O(index) plus a parent() root descent",
        ),
        (
            ".next_named_sibling(",
            "O(index) plus a parent() root descent; this is the Theta(M^3/3) Go defect",
        ),
        (
            ".named_child(",
            "O(i) rescan from position 0; use root.named_children(&mut cursor)",
        ),
        (".child(", "O(i) rescan from position 0; use a TreeCursor"),
        (
            ".rfind(",
            "O(start) backward byte scan; use build_newline_table + binary_search",
        ),
    ];

    /// Blank every `//` line comment, `/* */` block comment (nesting-aware) and
    /// `"`-delimited string literal, replacing their bytes with ASCII spaces.
    ///
    /// Single forward pass, **byte-length preserving** and **newline preserving**,
    /// so byte offsets and line numbers in the result still address the original
    /// file. Backslash escapes inside string literals are honoured, which also
    /// makes multi-line `"… \` continuations (used heavily in this crate's assert
    /// messages) come out intact.
    ///
    /// Char literals (`'…'`) are deliberately NOT tracked. In Rust the single quote
    /// is shared with lifetimes and loop labels (`&'a str`, `'outer:`), so a naive
    /// char-literal state machine mis-parses ordinary code far more often than it
    /// helps. The only inputs this omission would corrupt are a `'"'` literal and a
    /// raw string; the contract test asserts neither policed file contains one, so
    /// the simplification stays honest rather than becoming a silent blind spot.
    ///
    /// Blanking exists because both policed files *document* the forbidden APIs in
    /// rustdoc (`WalkPosition`'s fields are specified as "equivalent to
    /// `node.parent()`…"), and because `is_doc_comment` holds `"/**"`, `"/*!"` and
    /// `"///"` as string literals — an unsanitised text scan would both false-positive
    /// on the prose and let `"/**"` open a comment that swallows real code
    /// (source-corpus PF-018: sanitize before scanning source text).
    fn blank_comments_and_strings(src: &str) -> String {
        let bytes = src.as_bytes();
        let n = bytes.len();
        let mut out = bytes.to_vec();
        let mut i = 0usize;

        // Bounded by construction: every branch advances `i` by at least 1.
        while i < n {
            if bytes[i] == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
                // Line comment — blank through to (but not including) the newline.
                while i < n && bytes[i] != b'\n' {
                    out[i] = b' ';
                    i += 1;
                }
            } else if bytes[i] == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
                // Block comment — Rust nests them, so track depth.
                let mut depth = 0usize;
                while i < n {
                    if bytes[i] == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
                        depth += 1;
                        out[i] = b' ';
                        out[i + 1] = b' ';
                        i += 2;
                        continue;
                    }
                    if bytes[i] == b'*' && i + 1 < n && bytes[i + 1] == b'/' {
                        depth = depth.saturating_sub(1);
                        out[i] = b' ';
                        out[i + 1] = b' ';
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                        continue;
                    }
                    if bytes[i] != b'\n' {
                        out[i] = b' ';
                    }
                    i += 1;
                }
            } else if bytes[i] == b'"' {
                out[i] = b' ';
                i += 1;
                while i < n {
                    if bytes[i] == b'\\' {
                        out[i] = b' ';
                        i += 1;
                        if i < n {
                            if bytes[i] != b'\n' {
                                out[i] = b' ';
                            }
                            i += 1;
                        }
                        continue;
                    }
                    if bytes[i] == b'"' {
                        out[i] = b' ';
                        i += 1;
                        break;
                    }
                    if bytes[i] != b'\n' {
                        out[i] = b' ';
                    }
                    i += 1;
                }
            } else {
                i += 1;
            }
        }

        String::from_utf8(out).expect("blanking only ever substitutes ASCII spaces")
    }

    /// A Rust char literal holding a double quote, spelled without embedding one
    /// literally so this constant does not itself become the thing it detects.
    const DOUBLE_QUOTE_CHAR_LITERAL: &str = "'\u{22}'";

    /// Does `src` open a raw string (`r"…"` / `r#"…"#`)?
    ///
    /// A bare `contains("r\"")` is useless here: any ordinary message ending in the
    /// letter `r` matches it (`"… must be a module header"`). An `r` is a raw-string
    /// prefix only when it does not continue an identifier and is followed by zero or
    /// more `#` and then a quote.
    fn contains_raw_string_opener(src: &str) -> bool {
        let bytes = src.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b != b'r' {
                continue;
            }
            if i > 0 && (bytes[i - 1] == b'_' || bytes[i - 1].is_ascii_alphanumeric()) {
                continue;
            }
            let mut j = i + 1;
            while j < bytes.len() && bytes[j] == b'#' {
                j += 1;
            }
            if bytes.get(j) == Some(&b'"') {
                return true;
            }
        }
        false
    }

    /// The production region of `name`: everything before the `#[cfg(test)]` that
    /// gates the file's `mod tests`, with comments and string literals blanked.
    ///
    /// The cut uses the **last** `\n#[cfg(test)]` in the file, not the first:
    /// `pseudo.rs` carries an earlier `#[cfg(test)]` on `transform_pseudo`, a
    /// test-only *production* helper that must stay inside the policed region.
    /// `minimal.rs` has only one marker, so the two agree there.
    fn production_region(name: &str, src: &str) -> String {
        const MARKER: &str = "\n#[cfg(test)]";

        let blanked = blank_comments_and_strings(src);
        let Some(cut) = blanked.rfind(MARKER) else {
            panic!(
                "{name}: no `{}` marker found, so the production region cannot be \
                 delimited and this contract would police either everything or nothing. \
                 A guard with no region is a vacuous guard (source-corpus PF-007 / PF-014).",
                MARKER.trim_start()
            );
        };
        let (head, tail) = blanked.split_at(cut);

        assert!(
            tail.contains("\nmod tests {"),
            "{name}: nothing after the last `#[cfg(test)]` declares `mod tests` — \
             the contract would police the wrong region (source-corpus PF-007 / PF-014: \
             a guard aimed at the wrong text asserts nothing)."
        );
        // Stronger form of the same check, and the one that actually pins `rfind`:
        // the marker we cut at must be the one *immediately* gating `mod tests`
        // (attribute lines may sit between). With `find` instead of `rfind`,
        // `pseudo.rs` cuts at `transform_pseudo` — whose tail still *contains*
        // `\nmod tests {` further down, so the containment check alone passes and
        // ~440 lines of production code silently leave the policed region.
        let gated = tail[MARKER.len()..]
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("#["));
        assert_eq!(
            gated,
            Some("mod tests {"),
            "{name}: the last `#[cfg(test)]` does not gate `mod tests` (next item is \
             {gated:?}) — the contract would police the wrong region \
             (source-corpus PF-007 / PF-014)."
        );

        head.to_string()
    }

    /// The transform walkers must never reach for a relational fact about a node by
    /// asking tree-sitter for it again.
    ///
    /// # Why a source-text guard and not an instrumented counter
    ///
    /// An operation counter cannot discriminate here. The quadratic term lives in
    /// tree-sitter's C code, not in ours: a `TSNode` carries no parent pointer, so
    /// `parent()`, `prev_sibling()`, `next_sibling()`, `next_named_sibling()` and
    /// `named_child(i)` each re-descend from the tree root and rescan the parent's
    /// child list from position 0 (source-corpus PF-020 — a `TSNode` walk is never
    /// O(1)). The defective and the fixed walker execute the **same number of Rust
    /// statements** and produce **byte-identical output**; the only thing that
    /// differs is the C-side cost per call. Any counter that could tell them apart
    /// would have to live inside the very code a regression replaces. See the
    /// measured series in `minimal.rs` (`is_go_doc_comment`,
    /// `compute_header_end_byte`) for the empirical form of that fact.
    ///
    /// So the checkable invariant is not "how many operations ran" but "which API
    /// the source calls" — a property of the text, available before anything runs.
    ///
    /// # Why comments and string literals are blanked first
    ///
    /// Both policed files document the forbidden APIs in rustdoc — `WalkPosition`'s
    /// fields are specified as "equivalent to `node.parent()`" — and `is_doc_comment`
    /// holds `"/**"`, `"/*!"` and `"///"` as string literals. Scanning raw text would
    /// therefore fail on prose that is *explaining* the ban, and `"/**"` would open a
    /// block comment that swallowed live code (source-corpus PF-018).
    ///
    /// # Why the O(1) traversal set is excluded
    ///
    /// `Node::walk`, `TreeCursor::goto_first_child` / `goto_next_sibling`,
    /// `Node::children`, `Node::named_children`, `Node::child_count` and
    /// `child_by_field_name` on a node already in hand are the APIs the walkers are
    /// *supposed* to use — a `TreeCursor` keeps an explicit ancestor stack, so each
    /// step is genuinely O(1). Banning them would leave no legal way to traverse.
    ///
    /// # What this is not
    ///
    /// This is a **lint, not a proof**. It pins the construct, not the complexity:
    /// a novel super-linear pattern that avoids all eight tokens would pass. The
    /// stronger follow-up is a type-level facade — a `WalkNode` wrapper that exposes
    /// only the O(1) set, making the forbidden construct unrepresentable rather than
    /// merely detectable. That is tracked separately.
    ///
    /// It also does not discriminate O(N) from O(N²) on its own, and neither do the
    /// per-walker artifact tests in `minimal.rs` / `pseudo.rs`: the defect was
    /// output-preserving, which is exactly why the discriminating job lands here, on
    /// the source text.
    ///
    /// Housed in `transform/mod.rs`, beside the code it polices, rather than in a
    /// standalone lint crate (source-corpus PF-017).
    ///
    /// # A note on the PF citations above
    ///
    /// Every `PF-NNN` in this module refers to the **source-corpus** pitfall
    /// numbering, which is a different sequence from the decisions-ledger numbering
    /// in `.devflow/learning/pitfalls.md` (PF-023). The two collide: the ledger's
    /// PF-020 is an unrelated entry. Read these IDs as source-corpus only.
    #[test]
    fn contract_transform_walkers_use_no_root_descending_node_apis() {
        const POLICED: &[(&str, &str)] = &[
            ("minimal.rs", include_str!("minimal.rs")),
            ("pseudo.rs", include_str!("pseudo.rs")),
        ];

        for (name, src) in POLICED {
            // `blank_comments_and_strings` does not model char literals or raw
            // strings. Assert the assumption instead of hoping for it — an
            // unmodelled double-quote char literal would open a phantom string and
            // blank out live code, turning this contract vacuous
            // (source-corpus PF-007 / PF-014).
            assert!(
                !src.contains(DOUBLE_QUOTE_CHAR_LITERAL),
                "{name}: contains a double-quote char literal, which \
                 `blank_comments_and_strings` does not model. Teach the blanker about \
                 char literals before adding one, or this contract silently stops seeing \
                 the code after it."
            );
            assert!(
                !contains_raw_string_opener(src),
                "{name}: contains a raw string literal, which \
                 `blank_comments_and_strings` does not model (no backslash escapes, \
                 `#` delimiters). Teach the blanker about raw strings before adding one."
            );

            let region = production_region(name, src);

            for (token, reason) in FORBIDDEN_NODE_APIS {
                let Some(idx) = region.find(token) else {
                    continue;
                };
                let line = region[..idx].matches('\n').count() + 1;
                panic!(
                    "COMPLEXITY CONTRACT VIOLATION\n\
                     \x20 file:   crates/rskim-core/src/transform/{name}:{line}\n\
                     \x20 token:  `{token}`\n\
                     \x20 reason: {reason}\n\
                     \n\
                     The transform walkers must obtain every relational fact about a node \
                     (parent kind, previous sibling, index in parent, line start) from the \
                     walk itself — a threaded `WalkPosition` / `depth`, a `TreeCursor`, or a \
                     precomputed per-file table — never by asking tree-sitter for it again. \
                     A `TSNode` carries no parent pointer, so each of these calls re-descends \
                     from the tree root or rescans the parent's child list from position 0 \
                     (source-corpus PF-020: a TSNode walk is never O(1)).\n\
                     \n\
                     Every historical quadratic/cubic defect in these two files used one of \
                     these calls, and NONE of them changed the output by a single byte — so \
                     no behavioural test and no operation counter can see them. This contract \
                     is the only gate that can.\n\
                     \n\
                     If the call is provably O(1) at this site, add an explicit exception \
                     here together with the measurement that proves it."
                );
            }
        }
    }

    /// `blank_comments_and_strings` must be offset-faithful, must blank what it
    /// claims to blank, must not blank live code, and must not let the legitimate
    /// O(1) traversal APIs trip a `FORBIDDEN_NODE_APIS` entry.
    ///
    /// Without this the contract test above could pass by blanking everything
    /// (source-corpus PF-007 / PF-014: reading a guard is not evidence it guards).
    #[test]
    fn blank_comments_and_strings_self_test() {
        const SRC: &str = concat!(
            "let k = node.parent();\n",
            "// a comment naming node.parent() must not count\n",
            "/* block /* nested */ still-blanked */\n",
            "let jsdoc = \"/**\";\n",
            "let after_string = 1;\n",
        );

        let blanked = blank_comments_and_strings(SRC);

        // Offset fidelity: byte offsets and line numbers still address the original.
        assert_eq!(
            blanked.len(),
            SRC.len(),
            "blanking must preserve byte length so reported line numbers stay valid"
        );
        assert_eq!(
            blanked.lines().count(),
            SRC.lines().count(),
            "blanking must preserve newlines so reported line numbers stay valid"
        );

        // A real call survives; the one inside a line comment does not.
        assert!(
            blanked.contains("node.parent();"),
            "a live `.parent()` call must survive blanking, got:\n{blanked}"
        );
        assert_eq!(
            blanked.matches(".parent()").count(),
            1,
            "exactly one `.parent()` must survive (the live one); the commented one \
             must be blanked. Got:\n{blanked}"
        );

        // Block-comment content is blanked, nesting included.
        assert!(
            !blanked.contains("nested") && !blanked.contains("still-blanked"),
            "block comment content (including nested comments) must be blanked, got:\n{blanked}"
        );

        // The string literal `"/**"` must NOT open a block comment — if it did, the
        // rest of the file would be swallowed and the contract would see nothing.
        assert!(
            blanked.contains("let after_string = 1;"),
            "the string literal \"/**\" must not open a block comment; code after it \
             must survive. Got:\n{blanked}"
        );

        // The legitimate O(1) traversal set must not trip any forbidden token.
        const LOOKALIKES: &str = concat!(
            "cursor.goto_next_sibling();\n",
            "n.child_count();\n",
            "n.children(&mut c);\n",
            "p.child_by_field_name(\"x\");\n",
            "root.named_children(&mut c);\n",
        );
        let lookalikes = blank_comments_and_strings(LOOKALIKES);
        for (token, _) in FORBIDDEN_NODE_APIS {
            assert!(
                !lookalikes.contains(token),
                "legitimate O(1) traversal must not match forbidden token `{token}`; \
                 the `.`/`(` anchoring is what keeps them apart. Got:\n{lookalikes}"
            );
        }
    }
}
