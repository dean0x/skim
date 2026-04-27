//! Line number formatting for `--line-numbers` / `-n` flag.
//!
//! ARCHITECTURE: Line number formatting is applied at the CLI layer, AFTER the
//! guardrail check and BEFORE cache write. The core library (`rskim-core`) remains
//! pure — it computes source line maps but does NOT apply formatting.
//!
//! # Format
//!
//! Each output line is formatted as `{source_line}\t{content}` (tab-separated,
//! no fixed-width padding). Lines whose source line map value is `0` (omission
//! markers, truncation markers) are emitted without a line number prefix.
//!
//! # Design Decision (AC-18)
//!
//! Tab-separated format (`{line}\t{content}`) was chosen over space-padded format
//! (`{line:4} {content}`) because:
//! - LLMs consume the line number token once and move on; padding wastes tokens.
//! - Tab-separated is easy to parse programmatically (`split('\t', 2)`).
//! - No fixed-width means line 1 and line 1000 take the same format.
//! - Omission/truncation markers have NO prefix — agents can identify gaps naturally.

/// Format transformed output with source line number annotations.
///
/// # Arguments
/// * `text` - The transformed output text (possibly with trailing newline)
/// * `source_line_map` - One entry per output line. Value is the 1-indexed source
///   line number, or `0` for omission/truncation markers (no prefix emitted).
///
/// # Returns
/// Formatted text where each line is prefixed with `{source_line}\t`, except
/// lines with a map value of `0` which are emitted verbatim. Trailing newline
/// is preserved if the input has one.
///
/// # Panics
/// Does not panic. If `source_line_map` is shorter than the number of output
/// lines, remaining lines are treated as having source line 0 (no prefix).
pub(crate) fn format_with_line_numbers(text: &str, source_line_map: &[usize]) -> String {
    if text.is_empty() {
        return String::new();
    }

    let trailing_newline = text.ends_with('\n');
    let lines: Vec<&str> = text.lines().collect();

    if lines.is_empty() {
        return if trailing_newline {
            "\n".to_string()
        } else {
            String::new()
        };
    }

    let mut result = String::with_capacity(text.len() + lines.len() * 4);

    for (i, line) in lines.iter().enumerate() {
        let source_line = source_line_map.get(i).copied().unwrap_or(0);
        if source_line == 0 {
            // Omission/truncation marker — emit without prefix
            result.push_str(line);
        } else {
            // Normal content line — emit with "{source_line}\t" prefix
            result.push_str(&source_line.to_string());
            result.push('\t');
            result.push_str(line);
        }
        result.push('\n');
    }

    // If the input did not end with a newline, remove the trailing newline we added
    if !trailing_newline && result.ends_with('\n') {
        result.pop();
    }

    result
}

/// Build an identity source line map for full-mode output.
///
/// Full mode is a passthrough — output line N corresponds to source line N.
/// Returns a vector of `[1, 2, 3, ..., n]` where `n` is the number of output lines.
pub(crate) fn identity_line_map(output: &str) -> Vec<usize> {
    let n = output.lines().count();
    (1..=n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_empty_input() {
        let result = format_with_line_numbers("", &[]);
        assert_eq!(result, "");
    }

    #[test]
    fn test_format_single_line_with_trailing_newline() {
        let result = format_with_line_numbers("type A = string;\n", &[3]);
        assert_eq!(result, "3\ttype A = string;\n");
    }

    #[test]
    fn test_format_single_line_without_trailing_newline() {
        let result = format_with_line_numbers("type A = string;", &[1]);
        assert_eq!(result, "1\ttype A = string;");
    }

    #[test]
    fn test_format_multiple_lines() {
        let text = "type A = string;\ntype B = number;\n";
        let map = vec![2, 5];
        let result = format_with_line_numbers(text, &map);
        assert_eq!(result, "2\ttype A = string;\n5\ttype B = number;\n");
    }

    #[test]
    fn test_format_omission_marker_has_no_prefix() {
        // Map value 0 means omission marker — no prefix
        let text = "type A = string;\n// ...\ntype B = number;\n";
        let map = vec![1, 0, 5]; // middle line is omission marker
        let result = format_with_line_numbers(text, &map);
        assert_eq!(result, "1\ttype A = string;\n// ...\n5\ttype B = number;\n");
    }

    #[test]
    fn test_format_short_map_uses_zero_for_remaining() {
        // If map is shorter than lines, remaining lines get 0 (no prefix)
        let text = "line 1\nline 2\nline 3\n";
        let map = vec![1]; // only one entry
        let result = format_with_line_numbers(text, &map);
        assert_eq!(result, "1\tline 1\nline 2\nline 3\n");
    }

    #[test]
    fn test_format_tab_separated_no_fixed_width() {
        // Line 10 should be "10\t..." not " 10\t..."
        let text = "const x = 1;\n";
        let map = vec![10];
        let result = format_with_line_numbers(text, &map);
        assert_eq!(result, "10\tconst x = 1;\n");
        assert!(!result.starts_with(' '), "Should not have leading space");
    }

    #[test]
    fn test_identity_line_map() {
        let output = "line 1\nline 2\nline 3\n";
        let map = identity_line_map(output);
        assert_eq!(map, vec![1, 2, 3]);
    }

    #[test]
    fn test_identity_line_map_empty() {
        let map = identity_line_map("");
        assert_eq!(map, Vec::<usize>::new());
    }

    #[test]
    fn test_identity_line_map_single_line() {
        let map = identity_line_map("hello\n");
        assert_eq!(map, vec![1]);
    }
}
