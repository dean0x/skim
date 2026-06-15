//! Byte-range surgery for text-payload mutation (AC9b, AC10).
//!
//! This module implements targeted splice-mutation: given the raw JSON bytes of a
//! parsed Anthropic body, it locates the exact byte span of a leaf text value and
//! replaces only those bytes, leaving every other byte literally unchanged.
//!
//! # Invariant
//!
//! Every byte at an index not in `[span_start, span_end)` is byte-identical in the
//! output to the input. Only the replaced span — the JSON-quoted string that encodes
//! the old text payload — is substituted with the JSON-quoted new text. This satisfies
//! AC9(b) (surrounding bytes byte-identical) and AC10 (only differences within replaced
//! spans).
//!
//! # Navigation strategy
//!
//! The `LeafRef` encodes a structural path into the Anthropic body JSON tree
//! (message index, block index, leaf index). We walk the raw JSON bytes using a
//! minimal streaming scanner that tracks object keys and array indices to reach the
//! target field, then records the byte span of the JSON string value at that position.
//!
//! The scanner is conservative: it understands only the Anthropic schema structure
//! needed for navigation — it does not attempt to parse arbitrary JSON, and it returns
//! `Err(NotFound)` rather than guessing if the structure is unexpected.

use crate::model::anthropic::LeafRef;
use crate::{LlmError, Result};

/// The byte range `[start, end)` of a JSON string value in a raw bytes buffer.
///
/// Both `start` and `end` are byte indices; `start` points to the opening `"` and
/// `end` points one past the closing `"` of the JSON-quoted string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StringSpan {
    /// Index of the opening `"` of the JSON string.
    pub start: usize,
    /// One past the closing `"` of the JSON string.
    pub end: usize,
}

/// Splice-replace the text at `span` in `raw` with `new_text` (JSON-quoted).
///
/// Returns a new `Vec<u8>` where the bytes at `[span.start, span.end)` are
/// replaced by the JSON-encoded form of `new_text`, and all other bytes are
/// byte-identical to `raw`.
///
/// # Errors
///
/// Returns `Err` if `serde_json::to_string` fails (OOM). In practice this is
/// unreachable for a `&str` argument.
pub fn splice_replace(raw: &[u8], span: StringSpan, new_text: &str) -> Result<Vec<u8>> {
    // JSON-quote the replacement text.  serde_json::to_string always produces
    // valid UTF-8 JSON; the only fallible scenario is an OOM — which is surfaced
    // as a Json error rather than a panic.
    let quoted = serde_json::to_string(new_text)?;
    let new_bytes = quoted.as_bytes();

    let mut out = Vec::with_capacity(raw.len() - (span.end - span.start) + new_bytes.len());
    out.extend_from_slice(&raw[..span.start]);
    out.extend_from_slice(new_bytes);
    out.extend_from_slice(&raw[span.end..]);
    Ok(out)
}

/// Locate the byte span of the text payload named by `leaf` inside `raw`.
///
/// Returns `Err(LlmError::BlockNotFound)` if the structural path does not exist
/// in the raw bytes (this should not happen if the body was parsed from `raw`,
/// but is returned defensively).
pub fn find_leaf_span(raw: &[u8], leaf: &LeafRef) -> Result<StringSpan> {
    // Validate UTF-8 — the raw bytes should always be valid UTF-8 JSON, but we
    // check defensively before indexing into the text with char boundaries.
    let text = std::str::from_utf8(raw).map_err(|e| LlmError::InvalidUtf8(e.to_string()))?;

    let mut scanner = Scanner::new(text);

    // Step 1: navigate into the top-level object and find "messages"
    scanner.enter_object()?;
    scanner.seek_key("messages")?;

    // Step 2: navigate to messages[msg_idx]
    let (msg_idx, rest_path) = leaf_path_head(leaf);
    scanner.enter_array()?;
    for _ in 0..msg_idx {
        scanner.skip_value()?;
        scanner.skip_comma();
    }

    // Step 3: enter the message object and navigate to "content"
    scanner.enter_object()?;
    scanner.seek_key("content")?;

    match rest_path {
        RestPath::MessageString => {
            // The content IS the string payload
            scanner.find_string_span()
        }

        RestPath::TextBlock { blk_idx } => {
            // content is an array; navigate to blocks[blk_idx], then "text"
            scanner.enter_array()?;
            for _ in 0..blk_idx {
                scanner.skip_value()?;
                scanner.skip_comma();
            }
            scanner.enter_object()?;
            scanner.seek_key("text")?;
            scanner.find_string_span()
        }

        RestPath::ToolResultString { blk_idx } => {
            // content[blk_idx].content (string)
            scanner.enter_array()?;
            for _ in 0..blk_idx {
                scanner.skip_value()?;
                scanner.skip_comma();
            }
            scanner.enter_object()?;
            scanner.seek_key("content")?;
            scanner.find_string_span()
        }

        RestPath::ToolResultLeaf { blk_idx, leaf_idx } => {
            // content[blk_idx].content[leaf_idx].text
            scanner.enter_array()?;
            for _ in 0..blk_idx {
                scanner.skip_value()?;
                scanner.skip_comma();
            }
            scanner.enter_object()?;
            scanner.seek_key("content")?;
            scanner.enter_array()?;
            for _ in 0..leaf_idx {
                scanner.skip_value()?;
                scanner.skip_comma();
            }
            scanner.enter_object()?;
            scanner.seek_key("text")?;
            scanner.find_string_span()
        }
    }
}

/// Decompose a `LeafRef` into (message_index, rest_path).
fn leaf_path_head(leaf: &LeafRef) -> (usize, RestPath) {
    match leaf {
        LeafRef::MessageString { msg_idx } => (*msg_idx, RestPath::MessageString),
        LeafRef::TextBlock { msg_idx, blk_idx } => {
            (*msg_idx, RestPath::TextBlock { blk_idx: *blk_idx })
        }
        LeafRef::ToolResultString { msg_idx, blk_idx } => {
            (*msg_idx, RestPath::ToolResultString { blk_idx: *blk_idx })
        }
        LeafRef::ToolResultLeaf {
            msg_idx,
            blk_idx,
            leaf_idx,
        } => (
            *msg_idx,
            RestPath::ToolResultLeaf {
                blk_idx: *blk_idx,
                leaf_idx: *leaf_idx,
            },
        ),
    }
}

/// The portion of a `LeafRef` path below the message-level content field.
#[derive(Debug)]
enum RestPath {
    MessageString,
    TextBlock { blk_idx: usize },
    ToolResultString { blk_idx: usize },
    ToolResultLeaf { blk_idx: usize, leaf_idx: usize },
}

// ---------------------------------------------------------------------------
// Minimal JSON scanner
// ---------------------------------------------------------------------------

/// A forward-only, minimal JSON scanner over a `&str`.
///
/// The scanner tracks a cursor position and provides navigation primitives
/// sufficient to locate a specific string value in the Anthropic body tree.
/// It does not build an AST — it only advances the cursor and records spans.
///
/// Error on unexpected structure returns `LlmError::BlockNotFound` (the
/// structural path the caller asked for does not exist in the raw bytes).
///
/// # Recursion depth bound
///
/// `skip_value` is mutually recursive with `skip_object`/`skip_array` (each
/// calls the other for nested structures). The recursion depth is bounded
/// implicitly by [`crate::MAX_DEPTH`] = 64: every `ParsedBody` is constructed
/// only via [`crate::parse`] / [`crate::parse_with_provider`], both of which
/// call `validate` → `check_depth` first.  Any body that passes `check_depth`
/// has nesting ≤ 64, so the recursion depth here is also ≤ 64.
///
/// **Callers MUST ensure the bytes passed to `find_leaf_span` have already
/// cleared `check_depth`.** Passing un-validated bytes would allow unbounded
/// recursion.  The `splice_replace` function JSON-quotes the replacement text
/// before storing it in `raw_bytes`, so a post-mutation body cannot introduce
/// structural nesting that exceeds the original depth.
struct Scanner<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Scanner<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    /// Advance past ASCII whitespace.
    fn skip_ws(&mut self) {
        while self.pos < self.src.len() {
            match self.src.as_bytes()[self.pos] {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    /// Peek the next non-whitespace byte without consuming it.
    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.src.as_bytes().get(self.pos).copied()
    }

    /// Consume the next non-whitespace byte, returning an error if it doesn't
    /// match `expected`.
    fn consume_byte(&mut self, expected: u8) -> Result<()> {
        self.skip_ws();
        let byte = self.src.as_bytes().get(self.pos).copied().ok_or_else(|| {
            LlmError::BlockNotFound(format!(
                "unexpected end of input (expected {:?})",
                expected as char
            ))
        })?;
        if byte != expected {
            return Err(LlmError::BlockNotFound(format!(
                "expected {:?} at byte {}, found {:?}",
                expected as char, self.pos, byte as char
            )));
        }
        self.pos += 1;
        Ok(())
    }

    /// Enter a JSON object: consume the opening `{`.
    fn enter_object(&mut self) -> Result<()> {
        self.consume_byte(b'{')
    }

    /// Enter a JSON array: consume the opening `[`.
    fn enter_array(&mut self) -> Result<()> {
        self.consume_byte(b'[')
    }

    /// Skip optional comma and whitespace between elements.
    ///
    /// Does not return an error — a missing comma in an array/object is tolerated
    /// here since we are navigating by index, not validating structure.
    fn skip_comma(&mut self) {
        self.skip_ws();
        if self.src.as_bytes().get(self.pos).copied() == Some(b',') {
            self.pos += 1;
        }
    }

    /// Advance past a complete JSON string, returning its byte span `[start, end)`.
    ///
    /// `start` is the index of the opening `"`, `end` is one past the closing `"`.
    fn scan_string(&mut self) -> Result<StringSpan> {
        self.skip_ws();
        let start = self.pos;
        self.consume_byte(b'"')?;
        let bytes = self.src.as_bytes();
        let mut i = self.pos;
        loop {
            if i >= bytes.len() {
                return Err(LlmError::BlockNotFound(
                    "unterminated JSON string".to_string(),
                ));
            }
            match bytes[i] {
                b'\\' => {
                    // Escape sequence: skip both the backslash and the next byte.
                    // For \uXXXX the loop will handle the 4 hex digits naturally
                    // since they are all ASCII and none are `"` or `\`.
                    //
                    // Guard: if the backslash is the last byte before EOF, `i + 2`
                    // would exceed `bytes.len()`.  The loop's `i >= bytes.len()`
                    // check at the top would still catch it (no out-of-bounds read),
                    // but we advance conservatively to keep the scanner robust
                    // against un-validated input (defense-in-depth per PF-004).
                    if i + 1 < bytes.len() {
                        i += 2;
                    } else {
                        // Trailing lone backslash — unterminated string
                        return Err(LlmError::BlockNotFound(
                            "unterminated JSON string (trailing backslash)".to_string(),
                        ));
                    }
                }
                b'"' => {
                    i += 1;
                    break;
                }
                _ => {
                    i += 1;
                }
            }
        }
        self.pos = i;
        Ok(StringSpan { start, end: i })
    }

    /// Advance past a complete JSON number (integer or float).
    fn skip_number(&mut self) -> Result<()> {
        self.skip_ws();
        let bytes = self.src.as_bytes();
        let start = self.pos;
        // Optional leading '-'
        if bytes.get(self.pos).copied() == Some(b'-') {
            self.pos += 1;
        }
        // Digits, '.', 'e', 'E', '+', '-'
        while self.pos < bytes.len() {
            match bytes[self.pos] {
                b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-' => self.pos += 1,
                _ => break,
            }
        }
        if self.pos == start {
            return Err(LlmError::BlockNotFound("expected number".to_string()));
        }
        Ok(())
    }

    /// Skip a `true` / `false` / `null` literal.
    fn skip_literal(&mut self, lit: &[u8]) -> Result<()> {
        let end = self.pos + lit.len();
        if self.src.as_bytes().get(self.pos..end) == Some(lit) {
            self.pos = end;
            Ok(())
        } else {
            Err(LlmError::BlockNotFound(format!(
                "expected literal {:?} at byte {}",
                std::str::from_utf8(lit).unwrap_or("?"),
                self.pos
            )))
        }
    }

    /// Skip a complete JSON value of any type.
    ///
    /// Mutually recursive with `skip_object`/`skip_array`. Recursion depth is
    /// bounded by [`crate::MAX_DEPTH`] via the upstream `check_depth` gate —
    /// see [`Scanner`] for the invariant.
    fn skip_value(&mut self) -> Result<()> {
        match self
            .peek()
            .ok_or_else(|| LlmError::BlockNotFound("unexpected end of input".to_string()))?
        {
            b'"' => {
                self.scan_string()?;
            }
            b'{' => self.skip_object()?,
            b'[' => self.skip_array()?,
            b't' => {
                self.skip_literal(b"true")?;
            }
            b'f' => {
                self.skip_literal(b"false")?;
            }
            b'n' => {
                self.skip_literal(b"null")?;
            }
            b'-' | b'0'..=b'9' => self.skip_number()?,
            b => {
                return Err(LlmError::BlockNotFound(format!(
                    "unexpected byte {:?} at position {}",
                    b as char, self.pos
                )));
            }
        }
        Ok(())
    }

    /// Skip a complete JSON object (including nested objects and arrays).
    fn skip_object(&mut self) -> Result<()> {
        self.consume_byte(b'{')?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(());
                }
                Some(b'"') => {
                    self.scan_string()?; // key
                    self.skip_ws();
                    self.consume_byte(b':')?;
                    self.skip_value()?; // value
                    self.skip_ws();
                    match self.peek() {
                        Some(b',') => {
                            self.pos += 1;
                        }
                        Some(b'}') => {}
                        _ => {}
                    }
                }
                _ => {
                    return Err(LlmError::BlockNotFound(format!(
                        "unexpected byte in object at {}",
                        self.pos
                    )));
                }
            }
        }
    }

    /// Skip a complete JSON array.
    fn skip_array(&mut self) -> Result<()> {
        self.consume_byte(b'[')?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b']') => {
                    self.pos += 1;
                    return Ok(());
                }
                _ => {
                    self.skip_value()?;
                    self.skip_ws();
                    match self.peek() {
                        Some(b',') => {
                            self.pos += 1;
                        }
                        Some(b']') => {}
                        _ => {}
                    }
                }
            }
        }
    }

    /// Seek to the value of a specific key within the current object level.
    ///
    /// The scanner must have already consumed the `{` via `enter_object()`.
    /// After this call, the cursor is positioned at the start of the value for `key`.
    fn seek_key(&mut self, key: &str) -> Result<()> {
        loop {
            self.skip_ws();
            // Check for empty object or end
            match self.peek() {
                None | Some(b'}') => {
                    return Err(LlmError::BlockNotFound(format!(
                        "key {:?} not found in object",
                        key
                    )));
                }
                _ => {}
            }
            // Read the key string
            let key_span = self.scan_string()?;
            let found_key = &self.src[key_span.start + 1..key_span.end - 1];
            self.skip_ws();
            self.consume_byte(b':')?;

            if found_key == key {
                // Found! Cursor is now before the value.
                return Ok(());
            } else {
                // Skip the value and any trailing comma
                self.skip_value()?;
                self.skip_ws();
                match self.peek() {
                    Some(b',') => {
                        self.pos += 1;
                    }
                    Some(b'}') | None => {
                        return Err(LlmError::BlockNotFound(format!(
                            "key {:?} not found in object",
                            key
                        )));
                    }
                    _ => {}
                }
            }
        }
    }

    /// Return the byte span of the current string value (at the current cursor position).
    ///
    /// This is distinct from `scan_string()` in that it is used for the final
    /// target value — we want its span, not to skip past it.
    fn find_string_span(&mut self) -> Result<StringSpan> {
        self.skip_ws();
        let next = self.src.as_bytes().get(self.pos).copied();
        if next != Some(b'"') {
            return Err(LlmError::BlockNotFound(format!(
                "expected string value at byte {}, found {:?}",
                self.pos,
                next.map(|b| b as char)
            )));
        }
        self.scan_string()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::model::anthropic::LeafRef;

    #[test]
    fn splice_simple_string() {
        let raw =
            br#"{"model":"m","messages":[{"role":"user","content":"Hello"}],"max_tokens":100}"#;
        let leaf = LeafRef::MessageString { msg_idx: 0 };
        let span = find_leaf_span(raw, &leaf).expect("find span failed");
        // "Hello" starts at the opening quote
        let s = &raw[span.start..span.end];
        assert_eq!(s, b"\"Hello\"", "span should cover the quoted string");

        let result = splice_replace(raw, span, "World").expect("splice failed");
        let result_str = std::str::from_utf8(&result).unwrap();
        assert_eq!(
            result_str,
            r#"{"model":"m","messages":[{"role":"user","content":"World"}],"max_tokens":100}"#,
        );
    }

    #[test]
    fn splice_text_block() {
        let raw = br#"{"model":"m","messages":[{"role":"user","content":[{"type":"text","text":"OLD"}]}],"max_tokens":100}"#;
        let leaf = LeafRef::TextBlock {
            msg_idx: 0,
            blk_idx: 0,
        };
        let span = find_leaf_span(raw, &leaf).expect("find span failed");
        let result = splice_replace(raw, span, "NEW").expect("splice failed");
        let result_str = std::str::from_utf8(&result).unwrap();
        assert_eq!(
            result_str,
            r#"{"model":"m","messages":[{"role":"user","content":[{"type":"text","text":"NEW"}]}],"max_tokens":100}"#,
        );
    }

    #[test]
    fn splice_preserves_envelope() {
        // Mutating a content block must not touch the envelope (model, max_tokens, etc.)
        let raw = br#"{"model":"claude-3-5-sonnet-20241022","messages":[{"role":"user","content":"Greet"}],"max_tokens":1e3}"#;
        let leaf = LeafRef::MessageString { msg_idx: 0 };
        let span = find_leaf_span(raw, &leaf).expect("find span");
        let result = splice_replace(raw, span, "Bye").expect("splice failed");
        let result_str = std::str::from_utf8(&result).unwrap();
        // The envelope token 1e3 must be byte-identical — NOT rewritten to 1000.0
        assert!(
            result_str.contains("1e3"),
            "envelope token 1e3 must be preserved: {result_str}"
        );
        assert!(
            result_str.contains("claude-3-5-sonnet-20241022"),
            "model field must be preserved"
        );
    }

    #[test]
    fn splice_tool_result_string() {
        let raw = br#"{"model":"m","messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"call_001","content":"ORIGINAL"}]}],"max_tokens":100}"#;
        let leaf = LeafRef::ToolResultString {
            msg_idx: 0,
            blk_idx: 0,
        };
        let span = find_leaf_span(raw, &leaf).expect("find span");
        let result = splice_replace(raw, span, "REPLACED").expect("splice failed");
        let result_str = std::str::from_utf8(&result).unwrap();
        assert!(result_str.contains("\"REPLACED\""), "payload replaced");
        // tool_use_id must be byte-identical
        assert!(result_str.contains("\"call_001\""), "tool_use_id preserved");
    }

    #[test]
    fn splice_preserves_non_canonical_number_in_sibling_field() {
        // Mutation of a text payload must not touch a non-canonical number in a sibling
        // field — this is the exact failure the Gate-2 audit proved.
        let raw = br#"{"model":"m","messages":[{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"OLD"},{"type":"text","text":"sibling"}]}],"max_tokens":1e3}"#;
        let leaf = LeafRef::ToolResultString {
            msg_idx: 0,
            blk_idx: 0,
        };
        let span = find_leaf_span(raw, &leaf).expect("find span");
        let result = splice_replace(raw, span, "NEW").expect("splice failed");
        let result_str = std::str::from_utf8(&result).unwrap();
        assert!(
            result_str.contains("1e3"),
            "max_tokens 1e3 must survive mutation"
        );
        assert!(
            result_str.contains("\"sibling\""),
            "sibling text must be preserved"
        );
        // Byte-identical check: all bytes outside [span.start, span.end) are identical
        let original = std::str::from_utf8(raw).unwrap();
        let prefix = &original[..span.start];
        let suffix = &original[span.end..];
        assert!(
            result_str.starts_with(prefix),
            "prefix must be byte-identical"
        );
        assert!(
            result_str.ends_with(suffix),
            "suffix must be byte-identical"
        );
    }

    #[test]
    fn seek_key_handles_escaped_key_correctly() {
        // A key containing a backslash escape must not confuse the key scanner
        let raw = br#"{"model":"m","messages":[{"role":"user","content":"Hi"}],"meta\\key":"v","max_tokens":1}"#;
        let leaf = LeafRef::MessageString { msg_idx: 0 };
        let span = find_leaf_span(raw, &leaf).expect("find span");
        let result = splice_replace(raw, span, "Bye").expect("splice failed");
        assert!(std::str::from_utf8(&result).unwrap().contains("\"Bye\""));
    }
}
