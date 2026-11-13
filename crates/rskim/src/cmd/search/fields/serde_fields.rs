//! Lightweight byte-range scanners for JSON, YAML, and TOML.
//!
//! All functions are **infallible**: they accept `&str` and return
//! `Vec<(Range<usize>, SearchField)>` without `Result`. Invalid or
//! malformed input degrades gracefully — the output is always a valid,
//! contiguous, sorted range list.
//!
//! Each scanner marks the comments and the quoted strings of its format and
//! leaves every other byte (keys, headers, numbers, structure) to
//! [`SearchField::Other`]: the role rules read the comment and string fields
//! only. A key is never a string.
//!
//! # JSON scanner
//!
//! Byte-by-byte state machine tracking the stack of open objects and arrays
//! and the key-vs-value position in each object. String values are scanned
//! with [`scan_json_string`] which handles `\"`, `\\`, and `\uXXXX` escapes.
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

use super::super::field::SearchField;

use super::fill_gaps_and_merge;

// ============================================================================
// JSON scanner
// ============================================================================

/// One open JSON container on the scanner stack.
enum JsonFrame {
    /// An open `{`. `expect_key` is true when the next string is a key.
    Object { expect_key: bool },
    /// An open `[`. Every string in it is a value.
    Array,
}

/// Classify byte ranges in a JSON source string.
///
/// Field mapping:
/// - String values (in an object or in an array) → [`SearchField::StringLiteral`]
/// - Everything else (keys, numbers, booleans, null, structural chars) → [`SearchField::Other`]
///
/// The output satisfies all contiguity invariants via [`fill_gaps_and_merge`].
pub(crate) fn classify_json(source: &str) -> Vec<(Range<usize>, SearchField)> {
    if source.is_empty() {
        return Vec::new();
    }

    let bytes = source.as_bytes();
    let len = bytes.len();

    let mut ranges: Vec<(Range<usize>, SearchField)> = Vec::new();

    // Maximum nesting depth tracked on `frames`. Beyond this depth we still
    // parse correctly (`open_depth` keeps counting) but stop pushing new
    // entries to avoid unbounded heap growth on pathologically deep input.
    const MAX_JSON_DEPTH: usize = 1024;

    // Stack-based depth tracking:
    // - open_depth: count of currently open `{` and `[` together
    // - frames: one entry per open container (up to MAX_JSON_DEPTH), innermost
    //   last. An object frame says whether we expect to read a key (vs. a
    //   value) at its position:
    //   * starts true at object open `{`
    //   * flips to false after reading the key's `:` separator
    //   * flips back to true after `,`
    //   An array frame has no key position, so `,` and `:` leave it alone.
    let mut open_depth: usize = 0;
    let mut frames: Vec<JsonFrame> = Vec::new();
    let mut i = 0;

    while i < len {
        let b = bytes[i];
        match b {
            b'{' => {
                open_depth += 1;
                // Start of an object: next token expected is a key (or `}`).
                // Only track state up to MAX_JSON_DEPTH to bound heap usage.
                if open_depth <= MAX_JSON_DEPTH {
                    frames.push(JsonFrame::Object { expect_key: true });
                }
                i += 1;
            }
            b'[' => {
                open_depth += 1;
                if open_depth <= MAX_JSON_DEPTH {
                    frames.push(JsonFrame::Array);
                }
                i += 1;
            }
            b'}' | b']' => {
                // Only pop if we pushed: containers opened beyond
                // MAX_JSON_DEPTH do not push, so closing ones must not pop.
                // Check before saturating_sub so the comparison uses the
                // pre-decrement depth.
                if open_depth <= MAX_JSON_DEPTH {
                    frames.pop();
                }
                open_depth = open_depth.saturating_sub(1);
                i += 1;
            }
            b':' => {
                // Separator between key and value: flip to "expecting value".
                if let Some(JsonFrame::Object { expect_key }) = frames.last_mut() {
                    *expect_key = false;
                }
                i += 1;
            }
            b',' => {
                // After a value: flip back to "expecting key" if the
                // innermost container is an object.
                if let Some(JsonFrame::Object { expect_key }) = frames.last_mut() {
                    *expect_key = true;
                }
                i += 1;
            }
            b'"' => {
                // String token: scan to the end of the string. A key is not
                // a string value, so only a value is marked.
                let str_end = scan_json_string(bytes, i);
                let is_key = matches!(frames.last(), Some(JsonFrame::Object { expect_key: true }));
                if !is_key {
                    ranges.push((i..str_end, SearchField::StringLiteral));
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
/// - Quoted string values after `:` → [`SearchField::StringLiteral`]
/// - `#` comments → [`SearchField::Comment`]
/// - Keys → Other
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
        // We strip the `- ` prefix and look at the remainder.
        let (eff_rest_start, eff_rest) = strip_list_prefix(bytes, rest_start, rest, line_end);

        // --- Key detection: look for `: ` or `:\n` or `:\r\n` or end-of-line-colon.
        // The key is not marked; the colon tells where its value starts.
        if let Some(colon_rel) = find_yaml_key_colon(eff_rest) {
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
                        // Trim trailing \n and \r so line-ending bytes are not part of
                        // the string field.
                        // Inline comment detection is not implemented: values like
                        // "http://x.com # not a comment" would cause false positives.
                        // TODO: YAML spec coverage — inline comment classification.
                        let mut str_end = line_end;
                        if str_end > actual_val_start && bytes[str_end - 1] == b'\n' {
                            str_end -= 1;
                        }
                        if str_end > actual_val_start && bytes[str_end - 1] == b'\r' {
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

/// Strip the YAML list-item prefix (`- ` or `-`) from a line's content bytes.
///
/// Returns `(effective_rest_start, effective_rest)`:
/// - `effective_rest_start`: absolute byte index of the content after the prefix
/// - `effective_rest`: byte slice of the remaining line content
///
/// If the line does not start with a list prefix, the inputs are returned unchanged.
fn strip_list_prefix<'a>(
    bytes: &'a [u8],
    rest_start: usize,
    rest: &'a [u8],
    line_end: usize,
) -> (usize, &'a [u8]) {
    if rest.starts_with(b"- ") || rest == b"-\n" || rest == b"-\r\n" || rest == b"-" {
        let list_item_offset = if rest.starts_with(b"- ") { 2 } else { 1 };
        let new_rest_start = rest_start + list_item_offset;
        (new_rest_start, &bytes[new_rest_start..line_end])
    } else {
        (rest_start, rest)
    }
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
/// - Quoted string values (`"..."`, `'...'`) → [`SearchField::StringLiteral`]
/// - `# comments` → [`SearchField::Comment`]
/// - `[section]` and `[[array]]` headers, keys (including dotted keys like
///   `a.b.c`) and everything else → [`SearchField::Other`]
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
                // Section header: `[section]` or `[[array]]`. It holds no
                // string or comment, so the line is skipped.
                i = (eol + 1).min(len);
                continue;
            }
            _ => {
                // Possibly a key = value line.
                // Find the `=` sign (but not inside a string).
                if let Some(eq_rel) = find_toml_eq_sign(&bytes[i..eol]) {
                    let eq_abs = i + eq_rel;
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
                let end = scan_single_quote_toml(bytes, val_start, b'"', eol);
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
                let end = scan_single_quote_toml(bytes, val_start, b'\'', eol);
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
    // Look for ` #` pattern (space + hash) between `from` and `eol`. A range
    // that does not lie in the source holds no comment.
    let Some(region) = bytes.get(from..eol) else {
        return;
    };
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
/// `eol` is the byte offset of the line end (the `\n`, or the source length on
/// the last line). The scan never goes past it: a backslash before the line
/// end escapes nothing, because a basic string has no line continuation.
///
/// Returns the byte offset after the closing delimiter, or `eol` when the
/// string is unterminated.
fn scan_single_quote_toml(bytes: &[u8], start: usize, delim: u8, eol: usize) -> usize {
    debug_assert!(start < eol && eol <= bytes.len() && bytes[start] == delim);
    let mut i = start + 1;
    while i < eol {
        let b = bytes[i];
        if b == delim {
            return i + 1;
        }
        if b == b'\\' && delim == b'"' {
            // Skip escaped character.
            i += 2;
            continue;
        }
        i += 1;
    }
    eol
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
