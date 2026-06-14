//! Zone boundary: live zone editing API and hot-zone splice.
//!
//! # Zone boundary definition
//!
//! The conversation array is divided into two zones:
//!
//! - **Hot zone** — everything up to and including the last assistant message.
//!   These bytes form the cached prefix; they MUST be re-emitted from the
//!   original buffer by splice (invariant 3). Re-serialisation is forbidden
//!   because it risks changing bytes (e.g., key ordering, number formatting,
//!   whitespace) which would bust the prompt cache.
//!
//! - **Live zone** — the trailing run of turns after the last assistant message.
//!   These may be edited by waivered components within their narrowed rules.
//!
//! # Live-zone edit API
//!
//! The edit API is deliberately narrow:
//! - `per_slot_edit` — replace one slot in the live zone with new bytes.
//!   Caller is responsible for ensuring `new_bytes.len() <= original_bytes.len()`
//!   (the guardrail enforces this at the `guarded_transform` level).
//! - No delete/insert/reorder surface exists on this type. Those operations
//!   are forbidden by invariant 4 and are type-level impossible.
//!
//! # Hot-zone splice (invariant 3)
//!
//! `splice_hot_zone` slices the original buffer at the byte range of the hot
//! zone. The offset arithmetic uses `checked_add` / saturating arithmetic so
//! an out-of-range structural offset fails open (returns `None`, caller emits
//! passthrough) rather than panicking.

use crate::request::StructuralView;

/// A per-slot live-zone edit: replace the bytes of one turn slot.
///
/// The slot index is relative to the messages array, not the live zone.
/// The caller is responsible for ensuring:
/// 1. `slot_index` is within the live zone (≥ `zone.live_zone_range().start`)
/// 2. `new_bytes.len() <= original_slot_bytes.len()` (invariant 2)
///
/// These invariants are enforced by the harness; the type does not check them
/// at construction time.
#[derive(Debug, Clone)]
pub struct SlotEdit {
    /// Zero-based index in the messages array.
    pub slot_index: usize,
    /// Replacement bytes for this slot.
    pub new_bytes: Vec<u8>,
}

impl SlotEdit {
    /// Construct a new slot edit.
    pub fn new(slot_index: usize, new_bytes: Vec<u8>) -> Self {
        Self {
            slot_index,
            new_bytes,
        }
    }
}

/// Byte range within a buffer (start inclusive, end exclusive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    /// Start offset (inclusive).
    pub start: usize,
    /// End offset (exclusive).
    pub end: usize,
}

impl ByteRange {
    /// Returns the byte length of this range.
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Returns `true` if the range has zero length.
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

/// Splice the hot zone from the original buffer.
///
/// Returns a slice of `original` covering `range`, or `None` if the range is
/// out of bounds. Out-of-bounds → caller MUST emit passthrough (fail-open).
///
/// # Safety properties (PF-004)
///
/// `range.end` is capped to `original.len()` via `.min()` before slicing.
/// If `range.start > capped_end` (inverted or wholly-out-of-bounds range),
/// `None` is returned — the slice `&original[start..end]` is never attempted
/// with an invalid range, so no panic can occur.
pub fn splice_hot_zone(original: &[u8], range: ByteRange) -> Option<&[u8]> {
    let start = range.start;
    // Saturating end: if range.end > original.len(), cap to original.len().
    let end = range.end.min(original.len());
    if start > end {
        return None;
    }
    Some(&original[start..end])
}

/// Locate the byte range of the hot zone in the serialised JSON.
///
/// # Status: stub — always returns `None` until #302 provides the typed offset model.
///
/// The intended algorithm: find the `"messages"` key in `source` and derive byte
/// ranges for elements 0..=`last_assistant_index`. The required per-element byte
/// offsets are not available at this layer (the structural view stores cloned
/// `serde_json::Value` objects, not raw buffer offsets). Full offset extraction
/// is a per-consumer responsibility (#302).
///
/// Returning `None` causes the caller to emit passthrough (fail-open), which is
/// correct and safe — the hot zone is preserved verbatim by passing all bytes
/// through unchanged.
///
/// Returns `None` unconditionally (stub). Once the full implementation lands in
/// #302, will return `None` when any of these conditions hold:
///
/// - The structural view has no assistant turns (hot zone is empty)
/// - The `"messages"` key cannot be located at byte level
/// - Arithmetic overflows (PF-004 checked/saturating ops)
pub fn locate_hot_zone_range(_source: &[u8], _view: &StructuralView) -> Option<ByteRange> {
    // Stub: always returns None until #302 provides the typed offset model.
    // Callers fall back to passthrough, which is safe and correct at this layer.
    // Precise byte-offset extraction is a per-consumer responsibility (#302).
    None
}

/// Apply a set of live-zone slot edits to produce an output buffer.
///
/// # Status: stub — always returns `None` until #302 provides the typed offset model.
///
/// The intended algorithm once #302 lands:
/// 1. Reconstruct the messages array from the structural view's turns.
/// 2. For each turn in the live zone, apply any matching `SlotEdit`.
/// 3. Re-emit hot-zone bytes from the original buffer via `splice_hot_zone`
///    (using byte offsets provided by the #302 typed model).
///
/// Returns `None` unconditionally (stub). Will return `None` when any invariant
/// would be violated:
/// - A slot edit is outside the live zone
/// - A slot edit's bytes exceed the original slot bytes (inflation)
/// - Offset arithmetic overflows
///
/// The caller emits passthrough on `None`, which is correct — no edits applied.
pub fn apply_live_zone_edits(
    _original: &[u8],
    _view: &StructuralView,
    _edits: &[SlotEdit],
) -> Option<Vec<u8>> {
    // Full live-zone edit assembly requires the typed model from #302.
    // At this layer we return None (→ passthrough) for any attempt to
    // apply edits, ensuring the invariant is maintained until the full
    // implementation lands.
    None
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn splice_hot_zone_within_bounds() {
        let buf = b"abcdefghij";
        let range = ByteRange { start: 2, end: 6 };
        let slice = splice_hot_zone(buf, range).expect("must succeed");
        assert_eq!(slice, b"cdef");
    }

    #[test]
    fn splice_hot_zone_full_range() {
        let buf = b"hello";
        let range = ByteRange { start: 0, end: 5 };
        let slice = splice_hot_zone(buf, range).expect("must succeed");
        assert_eq!(slice, b"hello");
    }

    #[test]
    fn splice_hot_zone_end_beyond_buf_is_capped() {
        let buf = b"short";
        // end (100) > buf.len() (5) — end is capped to 5, start (3) < 5 → Some(&buf[3..5]).
        let range = ByteRange { start: 3, end: 100 };
        let result = splice_hot_zone(buf, range);
        assert_eq!(result, Some(b"rt".as_ref()));
    }

    #[test]
    fn splice_hot_zone_inverted_range_returns_none() {
        let buf = b"hello";
        // start > end → None after capping.
        let range = ByteRange {
            start: 10, // beyond buf.len() (5), so start > capped_end (3)
            end: 3,
        };
        // end (3) < buf.len() (5) → stays 3; start (10) > end (3) → None
        let result = splice_hot_zone(buf, range);
        assert!(result.is_none());
    }

    #[test]
    fn splice_hot_zone_empty_range() {
        let buf = b"hello";
        let range = ByteRange { start: 2, end: 2 };
        let slice = splice_hot_zone(buf, range).expect("empty range is valid");
        assert_eq!(slice, b"");
    }

    #[test]
    fn byte_range_len_and_empty() {
        let range = ByteRange { start: 3, end: 8 };
        assert_eq!(range.len(), 5);
        assert!(!range.is_empty());

        let empty = ByteRange { start: 5, end: 5 };
        assert_eq!(empty.len(), 0);
        assert!(empty.is_empty());
    }

    #[test]
    fn slot_edit_construction() {
        let edit = SlotEdit::new(2, b"replacement".to_vec());
        assert_eq!(edit.slot_index, 2);
        assert_eq!(edit.new_bytes, b"replacement");
    }
}
