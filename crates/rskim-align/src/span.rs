//! Top-level field span location via borrowed-RawValue pointer arithmetic.
//!
//! # Purpose
//!
//! Locates the exact byte span of each top-level field **value** in the raw JSON
//! input string. The spans are later used by `canonical_emit` to copy field values
//! verbatim into the restructured envelope without re-serializing them.
//!
//! # Mechanism
//!
//! Uses `serde_json::Deserializer::from_str` + `DeserializeSeed` + a streaming
//! `MapAccess` visitor that calls `next_value::<&'de RawValue>()`. The
//! `serde_json` implementation of zero-copy deserialization for `&'de RawValue`
//! guarantees that the returned reference points into the original `input` buffer,
//! making the pointer arithmetic `val_ptr - src_ptr` valid. Both pointers lie in
//! the same allocation; the bound check `val_ptr < src_start || val_ptr + len >
//! src_end` catches any unexpected allocation (should be unreachable).
//!
//! # AD-CA-11
//!
//! Duplicate top-level keys — including but not limited to `tools`, `system`, and
//! `functions` — cause `locate_top_level_spans` to return `None`. The caller in
//! `lib.rs` maps `None` to a whole-request passthrough (fail-open).

use serde::de::{DeserializeSeed, MapAccess, Visitor};
use serde_json::value::RawValue;
use std::collections::{HashMap, HashSet};
use std::fmt;

/// Byte span of a top-level field value in the original input string.
///
/// The span is half-open: `input[start..start + len]` yields the raw JSON text
/// of the value. The key string and the `:` separator are NOT included.
///
/// All spans produced by [`locate_top_level_spans`] satisfy:
/// - `start + len <= input.len()`
/// - The slice `&input[start..start + len]` is valid UTF-8 (subset of a valid JSON string)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Byte offset within the original input where the value starts.
    pub start: usize,
    /// Byte length of the value.
    pub len: usize,
}

impl Span {
    /// Extract the raw text of this span from the input.
    ///
    /// Returns `None` if the span is out of bounds (defensive guard; in practice,
    /// spans produced by `locate_top_level_spans` are always in-bounds).
    pub fn extract<'a>(&self, input: &'a str) -> Option<&'a str> {
        let end = self.start.checked_add(self.len)?;
        input.get(self.start..end)
    }
}

/// Locate byte spans of all top-level field values in a JSON object string.
///
/// Returns `None` when:
/// - `input` is not a valid JSON object.
/// - Any top-level key appears more than once (**AD-CA-11** fail-open on duplicate keys).
///
/// On success, returns a `HashMap<field_name, Span>` covering every unique key in
/// the object. The spans point into `input` (zero-copy; no byte duplication).
///
/// # Pointer arithmetic guarantee
///
/// `serde_json::from_str` zero-copy-deserializes `&'de RawValue` to a reference
/// into the original input allocation. The arithmetic `val_ptr - src_ptr` is
/// therefore well-defined for all values in a valid JSON object parsed from `input`.
pub fn locate_top_level_spans(input: &str) -> Option<HashMap<String, Span>> {
    let mut de = serde_json::Deserializer::from_str(input);
    SpanLocator { src: input }.deserialize(&mut de).ok()
}

// ============================================================================
// DeserializeSeed implementation
// ============================================================================

struct SpanLocator<'src> {
    src: &'src str,
}

impl<'de, 'src: 'de> DeserializeSeed<'de> for SpanLocator<'src> {
    type Value = HashMap<String, Span>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(SpanVisitor { src: self.src })
    }
}

struct SpanVisitor<'src> {
    src: &'src str,
}

impl<'de, 'src: 'de> Visitor<'de> for SpanVisitor<'src> {
    type Value = HashMap<String, Span>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a JSON object")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut result: HashMap<String, Span> = HashMap::new();
        let src_start = self.src.as_ptr() as usize;
        let src_end = src_start.saturating_add(self.src.len());

        while let Some(key) = map.next_key::<String>()? {
            // AD-CA-11: duplicate top-level key → fail-open
            if !seen.insert(key.clone()) {
                return Err(serde::de::Error::custom("duplicate top-level key"));
            }
            let raw: &'de RawValue = map.next_value()?;
            let val_ptr = raw.get().as_ptr() as usize;
            let val_len = raw.get().len();

            // Validate that the pointer lies within the source buffer.
            // Unreachable for valid serde_json zero-copy, but guards against
            // unexpected behaviour → return error → locate_top_level_spans returns None
            // → whole-request passthrough (fail-open).
            let val_end = val_ptr.saturating_add(val_len);
            if val_ptr < src_start || val_end > src_end {
                return Err(serde::de::Error::custom(
                    "RawValue pointer outside source buffer",
                ));
            }

            let offset = val_ptr - src_start;
            result.insert(key, Span { start: offset, len: val_len });
        }
        Ok(result)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn basic_span_location() {
        let input = r#"{"model":"claude-3","max_tokens":100,"messages":[]}"#;
        let spans = locate_top_level_spans(input).expect("must locate spans");

        let model_span = spans["model"];
        assert_eq!(model_span.extract(input).unwrap(), "\"claude-3\"");

        let tokens_span = spans["max_tokens"];
        assert_eq!(tokens_span.extract(input).unwrap(), "100");

        let msgs_span = spans["messages"];
        assert_eq!(msgs_span.extract(input).unwrap(), "[]");
    }

    #[test]
    fn nested_value_span() {
        let input = r#"{"tools":[{"name":"foo","input_schema":{"type":"object"}}]}"#;
        let spans = locate_top_level_spans(input).expect("must locate spans");

        let tools_span = spans["tools"];
        assert_eq!(
            tools_span.extract(input).unwrap(),
            r#"[{"name":"foo","input_schema":{"type":"object"}}]"#,
        );
    }

    #[test]
    fn span_byte_offset_arithmetic() {
        // Verify that start + len == source byte position
        let input = r#"{"a":"hello","b":42}"#;
        let spans = locate_top_level_spans(input).expect("must locate spans");

        let a_span = spans["a"];
        let a_raw = a_span.extract(input).unwrap();
        assert_eq!(a_raw, "\"hello\"");
        // Verify the offset lies within the input
        assert!(a_span.start < input.len());
        assert!(a_span.start + a_span.len <= input.len());
    }

    #[test]
    fn duplicate_key_returns_none_ad_ca_11() {
        // AD-CA-11: duplicate tools key → None → whole-request fail-open
        let input = r#"{"tools":[],"tools":[]}"#;
        assert!(
            locate_top_level_spans(input).is_none(),
            "AD-CA-11: duplicate top-level key must return None"
        );
    }

    #[test]
    fn duplicate_system_key_returns_none_ad_ca_11() {
        let input = r#"{"system":"a","system":"b"}"#;
        assert!(locate_top_level_spans(input).is_none());
    }

    #[test]
    fn malformed_json_returns_none() {
        assert!(locate_top_level_spans("{not valid json").is_none());
    }

    #[test]
    fn empty_object_returns_empty_map() {
        let spans = locate_top_level_spans("{}").expect("empty object must succeed");
        assert!(spans.is_empty());
    }

    #[test]
    fn whitespace_in_object() {
        // Keys with surrounding whitespace should still be located correctly
        let input = "{ \"model\" : \"gpt-4o\" , \"messages\" : [] }";
        let spans = locate_top_level_spans(input).expect("whitespace is valid JSON");
        let model_span = spans["model"];
        assert_eq!(model_span.extract(input).unwrap(), "\"gpt-4o\"");
    }

    #[test]
    fn number_token_preserved_verbatim() {
        // Span must preserve the raw token bytes (1e3, not 1000)
        let input = r#"{"count":1e3}"#;
        let spans = locate_top_level_spans(input).expect("must locate spans");
        assert_eq!(spans["count"].extract(input).unwrap(), "1e3");
    }
}
