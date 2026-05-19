//! Lightweight byte-range scanners for JSON, YAML, and TOML.
//!
//! All functions are **infallible**: they accept `&str` and return
//! `Vec<(Range<usize>, SearchField)>` without `Result`. Invalid or
//! malformed input degrades gracefully — the output is always a valid,
//! contiguous, sorted range list.
//!
//! # JSON scanner
//!
//! Byte-by-byte state machine tracking brace/bracket depth and
//! key-vs-value position. String values are scanned with
//! [`scan_json_string`] which handles `\"`, `\\`, and `\uXXXX` escapes.
//!
//! # YAML scanner
//!
//! Line-by-line scanner. Only block-style YAML is classified; flow-style
//! falls to Other.
//!
//! Documented scope limitations (inline TODO comments):
//! - Flow-style `{key: value}` → Other
//! - Multi-line scalars `|`, `>` → continuation lines fall to Other
//! - Anchors `&name` / aliases `*name` → fall to Other
//! - Complex keys `? key\n: value` → fall to Other
//!
//! # TOML scanner
//!
//! Line-by-line scanner handling `[section]` headers, `[[array]]` headers,
//! `key = value` lines, `# comments`, dotted keys, and multi-line strings.

use std::ops::Range;

use crate::SearchField;

use super::fill_gaps_and_merge;

// ============================================================================
// JSON scanner
// ============================================================================

/// Classify byte ranges in a JSON source string.
///
/// Field mapping:
/// - Depth-0 key whose value is `{` or `[` → [`SearchField::TypeDefinition`]
/// - Other keys → [`SearchField::SymbolName`]
/// - String values → [`SearchField::StringLiteral`]
/// - Everything else (numbers, booleans, null, structural chars) → [`SearchField::Other`]
///
/// The output satisfies all contiguity invariants via [`fill_gaps_and_merge`].
pub(crate) fn classify_json(source: &str) -> Vec<(Range<usize>, SearchField)> {
    if source.is_empty() {
        return Vec::new();
    }

    let bytes = source.as_bytes();
    let len = bytes.len();

    let mut ranges: Vec<(Range<usize>, SearchField)> = Vec::new();

    // Maximum nesting depth tracked on `in_key_stack`. Beyond this depth we
    // still parse correctly (brace_depth keeps counting) but stop pushing new
    // entries to avoid unbounded heap growth on pathologically deep input.
    const MAX_JSON_DEPTH: usize = 1024;

    // Stack-based depth tracking:
    // - brace_depth: count of currently open `{` objects
    // - bracket_depth: count of currently open `[` arrays
    // - in_key: true when we expect to read a key (vs. a value) at current depth
    //   * starts true at object open `{`
    //   * flips to false after reading the key's `:` separator
    //   * flips back to true after `,` or `{`
    let mut brace_depth: usize = 0;
    let mut bracket_depth: usize = 0;
    let mut in_key_stack: Vec<bool> = Vec::new(); // one entry per `{` opened (up to MAX_JSON_DEPTH)
    let mut i = 0;

    while i < len {
        let b = bytes[i];
        match b {
            b'{' => {
                brace_depth += 1;
                // Start of an object: next token expected is a key (or `}`).
                // Only track state up to MAX_JSON_DEPTH to bound heap usage.
                if brace_depth <= MAX_JSON_DEPTH {
                    in_key_stack.push(true);
                }
                i += 1;
            }
            b'}' => {
                brace_depth = brace_depth.saturating_sub(1);
                in_key_stack.pop();
                i += 1;
            }
            b'[' => {
                bracket_depth += 1;
                i += 1;
            }
            b']' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                i += 1;
            }
            b':' => {
                // Separator between key and value: flip to "expecting value".
                if let Some(top) = in_key_stack.last_mut() {
                    *top = false;
                }
                i += 1;
            }
            b',' => {
                // After a value: flip back to "expecting key" if we're in an object.
                if let Some(top) = in_key_stack.last_mut() {
                    *top = true;
                }
                i += 1;
            }
            b'"' => {
                // String token: scan to the end of the string.
                let str_start = i;
                let str_end = scan_json_string(bytes, i);

                let in_key = in_key_stack.last().copied().unwrap_or(false);
                let in_object = !in_key_stack.is_empty();
                // bracket_depth > 0 means we're inside an array.
                let inside_array_at_root = bracket_depth > 0 && brace_depth <= bracket_depth;

                if in_object && in_key {
                    // Key string: determine whether this should be TypeDefinition or SymbolName.
                    // TypeDefinition: depth-0 key (brace_depth == 1) AND value is object/array.
                    // We determine this by looking ahead past whitespace after the colon.
                    // The current position is the key's opening quote, so we need to check
                    // what follows the closing quote + colon + whitespace.
                    let field = if brace_depth == 1 && !inside_array_at_root {
                        // Look ahead: skip past this string, whitespace, colon, whitespace
                        // to see if the next char is `{` or `[`.
                        let after_key = str_end;
                        let mut j = after_key;
                        // Skip whitespace
                        while j < len
                            && (bytes[j] == b' '
                                || bytes[j] == b'\t'
                                || bytes[j] == b'\n'
                                || bytes[j] == b'\r')
                        {
                            j += 1;
                        }
                        // Skip colon
                        if j < len && bytes[j] == b':' {
                            j += 1;
                        }
                        // Skip whitespace
                        while j < len
                            && (bytes[j] == b' '
                                || bytes[j] == b'\t'
                                || bytes[j] == b'\n'
                                || bytes[j] == b'\r')
                        {
                            j += 1;
                        }
                        // Check if next char is `{` or `[`
                        if j < len && (bytes[j] == b'{' || bytes[j] == b'[') {
                            SearchField::TypeDefinition
                        } else {
                            SearchField::SymbolName
                        }
                    } else {
                        SearchField::SymbolName
                    };
                    ranges.push((str_start..str_end, field));
                } else {
                    // Value string → StringLiteral.
                    ranges.push((str_start..str_end, SearchField::StringLiteral));
                }

                i = str_end;
            }
            _ => {
                i += 1;
            }
        }
    }

    fill_gaps_and_merge(ranges, len)
}

/// Scan a JSON string starting at `pos` (the opening `"`).
///
/// Returns the byte index **after** the closing `"`. Handles:
/// - `\"` escaped quotes
/// - `\\` escaped backslashes
/// - `\uXXXX` unicode escapes
/// - End-of-input (unterminated string) → returns `bytes.len()`
fn scan_json_string(bytes: &[u8], pos: usize) -> usize {
    debug_assert!(pos < bytes.len() && bytes[pos] == b'"');
    let len = bytes.len();
    let mut i = pos + 1; // skip opening `"`
    while i < len {
        match bytes[i] {
            b'"' => {
                return i + 1; // past closing `"`
            }
            b'\\' => {
                // Skip the escape character and the escaped character.
                i += 1;
                if i < len {
                    if bytes[i] == b'u' {
                        // \uXXXX: skip 4 hex digits (if present).
                        i += 1;
                        let end = (i + 4).min(len);
                        i = end;
                    } else {
                        i += 1;
                    }
                }
            }
            _ => {
                i += 1;
            }
        }
    }
    // Unterminated string.
    len
}

// ============================================================================
// YAML scanner
// ============================================================================

/// Classify byte ranges in a YAML source string (block-style only).
///
/// Field mapping:
/// - Indent-0 keys → [`SearchField::TypeDefinition`]
/// - Indent-1+ keys → [`SearchField::SymbolName`]
/// - Quoted string values after `:` → [`SearchField::StringLiteral`]
/// - `#` comments → [`SearchField::Comment`]
/// - Multi-document `---` and `...` markers → Other
/// - List items `- item` → Other (content is value, not key)
///
/// # Scope limitations
///
/// - Flow-style `{key: value}` → falls to Other
///   (TODO: YAML spec coverage — flow-style mapping classification not implemented)
/// - Multi-line scalars `|`, `>` → continuation lines fall to Other
///   (TODO: YAML spec coverage — literal/folded block scalar continuation)
/// - Anchors `&name` / aliases `*name` → fall to Other
///   (TODO: YAML spec coverage — anchor/alias classification)
/// - Complex keys `? key\n: value` → fall to Other
///   (TODO: YAML spec coverage — complex key classification)
pub(crate) fn classify_yaml(source: &str) -> Vec<(Range<usize>, SearchField)> {
    if source.is_empty() {
        return Vec::new();
    }

    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut ranges: Vec<(Range<usize>, SearchField)> = Vec::new();

    let mut line_start = 0usize;

    while line_start < len {
        // Find end of line.
        let line_end = bytes[line_start..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| line_start + p + 1) // include the newline
            .unwrap_or(len);

        let line = &bytes[line_start..line_end];

        // Calculate indent (leading spaces).
        let indent = line.iter().take_while(|&&b| b == b' ').count();
        let rest_start = line_start + indent;
        let rest = &bytes[rest_start..line_end];

        // Skip blank lines.
        if rest.is_empty() || rest[0] == b'\n' || rest[0] == b'\r' {
            line_start = line_end;
            continue;
        }

        // --- Full-line comment (# at start of content).
        if rest[0] == b'#' {
            // Classify the entire comment from `#` to end of line.
            ranges.push((rest_start..line_end, SearchField::Comment));
            line_start = line_end;
            continue;
        }

        // --- Multi-document markers: `---` or `...`
        if rest.starts_with(b"---") || rest.starts_with(b"...") {
            // Leave as Other (gap fill).
            line_start = line_end;
            continue;
        }

        // --- List items: `- ` or `- ` then key/value on same line
        // List item lines can have an inline key: `  - name: foo`
        // We strip the `- ` prefix and re-classify the remainder.
        let (eff_indent, eff_rest_start, eff_rest) =
            if rest.starts_with(b"- ") || rest == b"-\n" || rest == b"-\r\n" || rest == b"-" {
                let list_item_offset = if rest.len() >= 2 && rest[1] == b' ' {
                    2
                } else {
                    1
                };
                let new_rest_start = rest_start + list_item_offset;
                let new_rest = &bytes[new_rest_start..line_end];
                // List items get an effective indent of indent + 1 (nested under the list key).
                (indent + 1, new_rest_start, new_rest)
            } else {
                (indent, rest_start, rest)
            };

        // --- Key detection: look for `: ` or `:\n` or `:\r\n` or end-of-line-colon.
        // We need to find the key text and the colon position.
        if let Some(colon_rel) = find_yaml_key_colon(eff_rest) {
            let key_start = eff_rest_start;
            let key_end = eff_rest_start + colon_rel;

            // Key field based on effective indent.
            let key_field = if eff_indent == 0 {
                SearchField::TypeDefinition
            } else {
                SearchField::SymbolName
            };

            // Trim trailing whitespace from key.
            let key_text = &bytes[key_start..key_end];
            let trimmed_end = key_end
                - key_text
                    .iter()
                    .rev()
                    .take_while(|&&b| b == b' ' || b == b'\t')
                    .count();
            if trimmed_end > key_start {
                ranges.push((key_start..trimmed_end, key_field));
            }

            // Look for a value on the same line after the colon.
            let value_start_rel = colon_rel + 1; // skip `:`
            if value_start_rel < eff_rest.len() {
                let value_bytes = &eff_rest[value_start_rel..];
                // Skip leading space after colon.
                let space_skip = value_bytes
                    .iter()
                    .take_while(|&&b| b == b' ' || b == b'\t')
                    .count();
                let actual_val_start = eff_rest_start + value_start_rel + space_skip;

                if actual_val_start < line_end {
                    let first_val_byte = bytes[actual_val_start];
                    if first_val_byte == b'"' || first_val_byte == b'\'' {
                        // Quoted string value → StringLiteral.
                        // Trim the trailing newline so the '\n' byte is not boosted
                        // with StringLiteral weight, which would skew BM25F scores.
                        // Inline comment detection is not implemented: values like
                        // "http://x.com # not a comment" would cause false positives.
                        // TODO: YAML spec coverage — inline comment classification.
                        let mut str_end = line_end;
                        if str_end > actual_val_start && bytes[str_end - 1] == b'\n' {
                            str_end -= 1;
                        }
                        if str_end > actual_val_start {
                            ranges.push((actual_val_start..str_end, SearchField::StringLiteral));
                        }
                    }
                    // Unquoted values (scalars, flow indicators) → Other (gap fill).
                }
            }
        }

        line_start = line_end;
    }

    fill_gaps_and_merge(ranges, len)
}

/// Find the position of the `:` in a YAML key on a line's content bytes.
///
/// Returns `Some(offset)` where `offset` is the byte offset of `:` within
/// `line_content`. Returns `None` if no YAML key colon is found.
///
/// A YAML key colon is `:` followed by space, tab, newline, `\r`, or end of
/// content. This avoids false positives on URLs (`http://example.com`).
fn find_yaml_key_colon(line_content: &[u8]) -> Option<usize> {
    for (i, &b) in line_content.iter().enumerate() {
        if b == b':' {
            let next = line_content.get(i + 1).copied();
            match next {
                None | Some(b' ') | Some(b'\t') | Some(b'\n') | Some(b'\r') => {
                    return Some(i);
                }
                _ => {}
            }
        }
    }
    None
}

// ============================================================================
// TOML scanner
// ============================================================================

/// Classify byte ranges in a TOML source string.
///
/// Field mapping:
/// - `[section]` headers → [`SearchField::TypeDefinition`]
/// - `[[array]]` headers → [`SearchField::TypeDefinition`]
/// - Keys (including dotted keys like `a.b.c`) → [`SearchField::SymbolName`]
/// - Quoted string values (`"..."`, `'...'`) → [`SearchField::StringLiteral`]
/// - `# comments` → [`SearchField::Comment`]
/// - Everything else → [`SearchField::Other`]
///
/// Multi-line strings (`"""..."""`, `'''...'''`) are scanned to their closing
/// delimiter and the entire span is classified as [`SearchField::StringLiteral`].
pub(crate) fn classify_toml(source: &str) -> Vec<(Range<usize>, SearchField)> {
    if source.is_empty() {
        return Vec::new();
    }

    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut ranges: Vec<(Range<usize>, SearchField)> = Vec::new();

    let mut i = 0usize;

    while i < len {
        // Skip leading whitespace on the current line (spaces/tabs only).
        while i < len && (bytes[i] == b' ' || bytes[i] == b'\t') {
            i += 1;
        }

        if i >= len {
            break;
        }

        // Find end of line.
        let eol = bytes[i..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|p| i + p)
            .unwrap_or(len);

        match bytes[i] {
            b'\n' | b'\r' => {
                // Blank line — advance.
                i += 1;
                continue;
            }
            b'#' => {
                // Full-line comment.
                ranges.push((i..eol, SearchField::Comment));
                i = (eol + 1).min(len);
                continue;
            }
            b'[' => {
                // Section header: `[section]` or `[[array]]`.
                let header_start = i;
                let header_end = eol;
                // Find the closing `]` (or `]]`).
                if let Some(close) = bytes[i..eol].iter().rposition(|&b| b == b']') {
                    let close_abs = i + close + 1;
                    ranges.push((header_start..close_abs, SearchField::TypeDefinition));
                } else {
                    // Malformed — skip to EOL.
                    ranges.push((header_start..header_end, SearchField::TypeDefinition));
                }
                i = (eol + 1).min(len);
                continue;
            }
            _ => {
                // Possibly a key = value line.
                // Find the `=` sign (but not inside a string).
                if let Some(eq_rel) = find_toml_eq_sign(&bytes[i..eol]) {
                    let eq_abs = i + eq_rel;
                    // Key: from i to eq_abs (trimming trailing whitespace).
                    let key_end = eq_abs;
                    let key_text = &bytes[i..key_end];
                    let trimmed_key_end = key_end
                        - key_text
                            .iter()
                            .rev()
                            .take_while(|&&b| b == b' ' || b == b'\t')
                            .count();
                    if trimmed_key_end > i {
                        ranges.push((i..trimmed_key_end, SearchField::SymbolName));
                    }

                    // Value: from eq_abs+1 to eol.
                    let val_region_start = eq_abs + 1;
                    if val_region_start < eol {
                        let val_bytes = &bytes[val_region_start..eol];
                        // Skip leading whitespace.
                        let ws_skip = val_bytes
                            .iter()
                            .take_while(|&&b| b == b' ' || b == b'\t')
                            .count();
                        let val_start = val_region_start + ws_skip;

                        if val_start < eol {
                            classify_toml_value(bytes, val_start, eol, len, &mut ranges, &mut i);
                        }
                    }
                }
                // Advance to end of line (classify_toml_value may have advanced `i` for multi-line strings).
                if i <= eol {
                    i = (eol + 1).min(len);
                }
            }
        }
    }

    fill_gaps_and_merge(ranges, len)
}

/// Classify a TOML value starting at `val_start` within the source bytes.
///
/// Handles:
/// - Triple-quoted strings (`"""..."""`, `'''...'''`) — multi-line, scan to closing delimiter.
/// - Single-quoted strings (`"..."`, `'...'`) — single-line.
/// - Inline comments (`# ...`) after the value.
/// - Non-string values (numbers, booleans, etc.) — left as gaps (Other).
fn classify_toml_value(
    bytes: &[u8],
    val_start: usize,
    eol: usize,
    len: usize,
    ranges: &mut Vec<(Range<usize>, SearchField)>,
    i: &mut usize,
) {
    if val_start >= len {
        return;
    }

    let b = bytes[val_start];

    match b {
        b'"' => {
            // Check for triple-quote.
            if bytes.get(val_start + 1) == Some(&b'"') && bytes.get(val_start + 2) == Some(&b'"') {
                // Multi-line basic string.
                let end = scan_triple_quote(bytes, val_start, b'"', len);
                ranges.push((val_start..end, SearchField::StringLiteral));
                *i = end;
            } else {
                // Single-line basic string.
                let end = scan_single_quote_toml(bytes, val_start, b'"', len);
                ranges.push((val_start..end, SearchField::StringLiteral));
                // Look for inline comment.
                classify_toml_inline_comment(bytes, end, eol, ranges);
            }
        }
        b'\'' => {
            // Check for triple-quote (literal string).
            if bytes.get(val_start + 1) == Some(&b'\'') && bytes.get(val_start + 2) == Some(&b'\'')
            {
                let end = scan_triple_quote(bytes, val_start, b'\'', len);
                ranges.push((val_start..end, SearchField::StringLiteral));
                *i = end;
            } else {
                // Single-line literal string.
                let end = scan_single_quote_toml(bytes, val_start, b'\'', len);
                ranges.push((val_start..end, SearchField::StringLiteral));
                classify_toml_inline_comment(bytes, end, eol, ranges);
            }
        }
        b'#' => {
            // Value is actually a comment (rare: `key = # comment`).
            ranges.push((val_start..eol, SearchField::Comment));
        }
        _ => {
            // Non-string value (number, boolean, datetime, inline table, array).
            // Look for an inline comment.
            classify_toml_inline_comment(bytes, val_start, eol, ranges);
        }
    }
}

/// Find an inline `# comment` after a value region and classify it.
fn classify_toml_inline_comment(
    bytes: &[u8],
    from: usize,
    eol: usize,
    ranges: &mut Vec<(Range<usize>, SearchField)>,
) {
    // Look for ` #` pattern (space + hash) between `from` and `eol`.
    let region = &bytes[from..eol];
    for (j, &b) in region.iter().enumerate() {
        if b == b'#' {
            let hash_abs = from + j;
            // Make sure it's preceded by whitespace (not inside a string or URL).
            let preceded_by_ws =
                hash_abs == 0 || bytes[hash_abs - 1] == b' ' || bytes[hash_abs - 1] == b'\t';
            if preceded_by_ws {
                ranges.push((hash_abs..eol, SearchField::Comment));
                return;
            }
        }
    }
}

/// Find the `=` sign in a TOML key-value line content (not inside a string).
///
/// Returns the byte offset of `=` within `content`, or `None` if not found.
fn find_toml_eq_sign(content: &[u8]) -> Option<usize> {
    let mut in_str = false;
    let mut str_char = b'"';
    let mut i = 0;
    while i < content.len() {
        let b = content[i];
        if in_str {
            // Handle backslash escape inside double-quoted strings only.
            // Single-quoted TOML literal strings treat backslash as literal.
            if b == b'\\' && str_char == b'"' {
                // Skip the escaped character entirely.
                i += 2;
                continue;
            }
            if b == str_char {
                in_str = false;
            }
        } else {
            match b {
                b'"' | b'\'' => {
                    in_str = true;
                    str_char = b;
                }
                b'=' => return Some(i),
                b'#' => return None, // comment before `=` → not a key-value line
                _ => {}
            }
        }
        i += 1;
    }
    None
}

/// Scan a single-line TOML string (basic `"..."` or literal `'...'`).
///
/// Returns the byte offset after the closing delimiter.
fn scan_single_quote_toml(bytes: &[u8], start: usize, delim: u8, len: usize) -> usize {
    debug_assert!(start < len && bytes[start] == delim);
    let mut i = start + 1;
    while i < len {
        let b = bytes[i];
        if b == delim {
            return i + 1;
        }
        if b == b'\\' && delim == b'"' {
            // Skip escaped character.
            i += 2;
            continue;
        }
        if b == b'\n' {
            // Unterminated single-line string.
            return i;
        }
        i += 1;
    }
    len
}

/// Scan a multi-line TOML triple-quoted string to its closing `"""` or `'''`.
///
/// `start` is the position of the first `"` or `'` of the opening triple.
/// Returns the byte offset after the closing triple delimiter.
fn scan_triple_quote(bytes: &[u8], start: usize, delim: u8, len: usize) -> usize {
    debug_assert!(
        bytes.get(start) == Some(&delim)
            && bytes.get(start + 1) == Some(&delim)
            && bytes.get(start + 2) == Some(&delim)
    );
    let mut i = start + 3; // skip opening triple
    while i + 2 < len {
        if bytes[i] == delim && bytes[i + 1] == delim && bytes[i + 2] == delim {
            return i + 3; // past closing triple
        }
        // Handle escape in basic strings.
        if delim == b'"' && bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        i += 1;
    }
    len
}
