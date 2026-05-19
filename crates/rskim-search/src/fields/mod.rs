//! Format-specific byte-range classifiers for non-tree-sitter formats and Markdown.
//!
//! # Module layout
//!
//! - [`serde_fields`] — Lightweight scanners for JSON, YAML, and TOML. All are
//!   **infallible** (return `Vec`, not `Result`) and use only `std`.
//! - [`markdown`] — Tree-sitter-based classifier for Markdown headings, code
//!   blocks, and prose. Returns `Result` but catches parser errors via fallback.
//!
//! # Shared helper
//!
//! [`fill_gaps_and_merge`] is the shared post-processing step for the serde
//! scanners: it sorts their non-overlapping, non-contiguous ranges, fills gaps
//! with `SearchField::Other`, and coalesces adjacent same-field ranges.
//!
//! Markdown uses [`crate::lexical::classifier::build_field_ranges`] directly
//! (the tree-sitter "innermost wins" algorithm) because it may receive
//! overlapping parent/child node ranges.

use std::ops::Range;

use crate::SearchField;

pub(crate) mod markdown;
pub(crate) mod serde_fields;

#[cfg(test)]
#[path = "fields_tests.rs"]
mod tests;

/// Fill gaps between classified ranges with [`SearchField::Other`] and merge
/// adjacent same-field ranges.
///
/// This is the shared post-processing step for serde scanners (JSON, YAML,
/// TOML). Their byte-range outputs are:
/// - Non-overlapping (each scanner never emits overlapping ranges)
/// - Non-contiguous (gaps between classified regions exist and must be filled)
///
/// The function:
/// 1. Sorts `ranges` by start position.
/// 2. Inserts `Other` ranges for every uncovered byte span.
/// 3. Calls [`crate::lexical::classifier::merge_adjacent`] to coalesce
///    adjacent ranges that share the same field.
///
/// # Preconditions
/// - `ranges` must be non-overlapping (caller responsibility).
/// - All `Range` values must be within `0..source_len`.
///
/// # Output invariants
/// - Sorted ascending by `range.start`.
/// - Non-overlapping.
/// - Contiguous: covers every byte `0..source_len`.
/// - For `source_len == 0`, returns an empty `Vec`.
pub(crate) fn fill_gaps_and_merge(
    mut ranges: Vec<(Range<usize>, SearchField)>,
    source_len: usize,
) -> Vec<(Range<usize>, SearchField)> {
    if source_len == 0 {
        return Vec::new();
    }

    // Sort by start position so we can do a single linear gap-fill pass.
    ranges.sort_unstable_by_key(|(r, _)| r.start);

    let mut result: Vec<(Range<usize>, SearchField)> = Vec::with_capacity(ranges.len() * 2 + 1);
    let mut cursor = 0usize;

    for (range, field) in ranges {
        // Clamp to source bounds (safety against rounding errors in scanners).
        let start = range.start.min(source_len);
        let end = range.end.min(source_len);
        if start >= end {
            continue;
        }
        if cursor < start {
            // Gap before this range → fill with Other.
            result.push((cursor..start, SearchField::Other));
        }
        result.push((start..end, field));
        cursor = end;
    }

    // Trailing gap → fill with Other.
    if cursor < source_len {
        result.push((cursor..source_len, SearchField::Other));
    }

    crate::lexical::classifier::merge_adjacent(&mut result);
    result
}
