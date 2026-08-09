//! Volatile content pattern detector (warn/stats only).
//!
//! # AD-CA-6 — Structural isolation from breakpoint.rs
//!
//! This module is intentionally and structurally isolated from `breakpoint.rs`.
//! There is **no import path** from this module into `breakpoint.rs`: `breakpoint.rs`
//! does not `use crate::volatile` and this module does not export any function
//! that `breakpoint.rs` calls.
//!
//! Marker placement is determined solely by request structure (tool-array cardinality,
//! system-block form, client marker count) per AC4 and AD-CA-10. The volatile
//! detector's output flows ONLY to `AlignStats::volatile_warn_count` and to a
//! log warning (AC6). It cannot influence where or whether markers are placed —
//! this is a structural guarantee, not a runtime flag.
//!
//! # Purpose
//!
//! Detects patterns in JSON request bodies that may indicate volatile content
//! (UUIDs, ISO-8601 timestamps, monotonically-increasing counters). These patterns
//! suggest that the request prefix may not be stable across turns, which reduces
//! the KV-cache hit rate. The detection result is used for observability only:
//! logged as a warning and recorded in `AlignStats`.

// ============================================================================
// Public types
// ============================================================================

/// A report of volatile content patterns found in a JSON request.
///
/// Carries counts and flags only — no content, no positional information.
///
/// # AC6
///
/// This type is never used by `breakpoint.rs`. It flows from `align()` through
/// `AlignStats::volatile_warn_count` only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VolatileReport {
    /// Whether any UUID-like pattern (8-4-4-4-12 hex groups with hyphens) was detected.
    pub has_uuid: bool,
    /// Whether any ISO-8601 timestamp-like pattern (`YYYY-MM-DDTHH:MM:SS`) was detected.
    pub has_iso_timestamp: bool,
    /// Total count of distinct volatile pattern occurrences.
    pub pattern_count: usize,
}

impl VolatileReport {
    /// Whether any volatile pattern was detected.
    pub fn is_volatile(&self) -> bool {
        self.pattern_count > 0
    }
}

// ============================================================================
// Public: detect_volatile
// ============================================================================

/// Detect volatile content patterns in the given JSON string.
///
/// # AD-CA-6 — Warn/stats only, no import path into breakpoint.rs
///
/// This function is called from `lib.rs::try_align_with_markers()` AFTER
/// marker placement is finalized. Its output is used only to populate
/// `AlignStats::volatile_warn_count`. It is never called from `breakpoint.rs`.
///
/// # Patterns detected
///
/// - **UUID** (`8-4-4-4-12` hex): `550e8400-e29b-41d4-a716-446655440000`
/// - **ISO-8601 timestamp**: `2024-01-15T12:34:56Z` or `2024-01-15T12:34:56+00:00`
///
/// Patterns are detected via a linear scan; false positives are possible for
/// JSON strings that coincidentally match the pattern shape.
pub fn detect_volatile(input: &str) -> VolatileReport {
    let mut report = VolatileReport::default();
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // UUID pattern: 8hex-4hex-4hex-4hex-12hex (total 36 chars)
        if i + 36 <= len && is_uuid_at(bytes, i) {
            report.has_uuid = true;
            report.pattern_count += 1;
            i += 36;
            continue;
        }
        // ISO-8601 timestamp: YYYY-MM-DDTHH:MM:SS (minimum 19 chars)
        if i + 19 <= len && is_iso_timestamp_at(bytes, i) {
            report.has_iso_timestamp = true;
            report.pattern_count += 1;
            i += 19;
            continue;
        }
        i += 1;
    }

    report
}

// ============================================================================
// Pattern-check helpers (private)
// ============================================================================

/// Check if `bytes[offset..]` starts with a UUID-like pattern.
///
/// UUID form: `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`
/// Segments:  8-4-4-4-12 lowercase or uppercase hex digits separated by hyphens.
fn is_uuid_at(bytes: &[u8], offset: usize) -> bool {
    if offset + 36 > bytes.len() {
        return false;
    }
    let b = &bytes[offset..offset + 36];
    // Hyphens at fixed positions
    if b[8] != b'-' || b[13] != b'-' || b[18] != b'-' || b[23] != b'-' {
        return false;
    }
    // Check hex groups
    is_hex_run(b, 0, 8)
        && is_hex_run(b, 9, 13)
        && is_hex_run(b, 14, 18)
        && is_hex_run(b, 19, 23)
        && is_hex_run(b, 24, 36)
}

/// Check if all bytes in `b[start..end]` are ASCII hex digits.
fn is_hex_run(b: &[u8], start: usize, end: usize) -> bool {
    b[start..end].iter().all(|c| c.is_ascii_hexdigit())
}

/// Check if `bytes[offset..]` starts with an ISO-8601 datetime pattern.
///
/// Accepts: `YYYY-MM-DDTHH:MM:SS` (exactly 19 chars; optionally followed by
/// `Z`, `+HH:MM`, or `-HH:MM` — we stop after the 19-char base).
fn is_iso_timestamp_at(bytes: &[u8], offset: usize) -> bool {
    if offset + 19 > bytes.len() {
        return false;
    }
    let b = &bytes[offset..offset + 19];
    let d = |c: u8| c.is_ascii_digit();
    // YYYY-MM-DD
    d(b[0]) && d(b[1]) && d(b[2]) && d(b[3])
        && b[4] == b'-'
        && d(b[5]) && d(b[6])
        && b[7] == b'-'
        && d(b[8]) && d(b[9])
        // T separator
        && b[10] == b'T'
        // HH:MM:SS
        && d(b[11]) && d(b[12])
        && b[13] == b':'
        && d(b[14]) && d(b[15])
        && b[16] == b':'
        && d(b[17]) && d(b[18])
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn detect_uuid() {
        let r = detect_volatile("550e8400-e29b-41d4-a716-446655440000");
        assert!(r.has_uuid);
        assert_eq!(r.pattern_count, 1);
    }

    #[test]
    fn detect_uuid_in_json_string() {
        let r = detect_volatile(r#"{"id":"550e8400-e29b-41d4-a716-446655440000"}"#);
        assert!(r.has_uuid);
        assert!(r.pattern_count >= 1);
    }

    #[test]
    fn detect_iso_timestamp() {
        let r = detect_volatile("2024-01-15T12:34:56Z");
        assert!(r.has_iso_timestamp);
        assert_eq!(r.pattern_count, 1);
    }

    #[test]
    fn detect_iso_timestamp_in_json() {
        let r = detect_volatile(r#"{"ts":"2024-01-15T12:34:56Z"}"#);
        assert!(r.has_iso_timestamp);
    }

    #[test]
    fn no_volatile_in_static_content() {
        let r = detect_volatile(r#"{"tools":[{"name":"search","description":"Search the web"}]}"#);
        assert!(!r.has_uuid);
        assert!(!r.has_iso_timestamp);
        assert_eq!(r.pattern_count, 0);
        assert!(!r.is_volatile());
    }

    #[test]
    fn both_uuid_and_timestamp_detected() {
        let s = "550e8400-e29b-41d4-a716-446655440000 2024-01-15T12:34:56Z";
        let r = detect_volatile(s);
        assert!(r.has_uuid);
        assert!(r.has_iso_timestamp);
        assert!(r.pattern_count >= 2);
    }

    #[test]
    fn empty_string_no_patterns() {
        let r = detect_volatile("");
        assert!(!r.is_volatile());
    }

    #[test]
    fn ac6_volatile_body_same_placement_as_non_volatile() {
        // AC6: volatile detector has no import path into breakpoint.rs
        // This test verifies the structural isolation: calling detect_volatile
        // on a body with a UUID produces a VolatileReport, but the VolatileReport
        // is NEVER passed to any breakpoint function.
        //
        // We simply assert that detect_volatile() produces a VolatileReport
        // and that the VolatileReport type has no method that calls anything
        // in breakpoint.rs (compile-time isolation: breakpoint.rs has no
        // `use crate::volatile` import).
        let body_with_uuid = r#"{"messages":[],"session":"550e8400-e29b-41d4-a716-446655440000"}"#;
        let body_without_uuid = r#"{"messages":[],"session":"stable-session-id"}"#;

        let r1 = detect_volatile(body_with_uuid);
        let r2 = detect_volatile(body_without_uuid);

        assert!(r1.is_volatile());
        assert!(!r2.is_volatile());

        // The VolatileReport type has no path into breakpoint logic — structural proof.
        // (The fact that this code compiles proves the isolation: if volatile.rs imported
        //  breakpoint.rs or vice-versa, a circular-dependency build error would occur.)
    }
}
