//! Caller-registered, component-specific invariant extensions.
//!
//! Beyond the eight core invariants, downstream tickets may register their own
//! per-component invariant checks with the harness. This module provides the
//! registration surface.
//!
//! # Extension invariants vs. core invariants
//!
//! Core invariants (1–8) are checked by the harness automatically for every
//! registered component. Extension invariants are opt-in, registered by a
//! specific component for a specific check it cares about.
//!
//! # Motivating example: marker-immutability (#308)
//!
//! The #308 marker-layer consumer injects `cache_control` markers. Once a
//! marker is injected, it must remain byte-identical in future passes
//! (marker-immutability invariant). This is not one of the eight core invariants
//! but can be registered as an extension:
//!
//! ```rust
//! use rskim_contract::extension::{InvariantCheck, ExtensionRegistry};
//!
//! let mut registry = ExtensionRegistry::new();
//!
//! registry.register(
//!     "marker-immutability",
//!     Box::new(|input: &[u8], output: &[u8]| {
//!         // If input contains markers, they must appear unchanged in output.
//!         let marker = b"\"cache_control\"";
//!         if input.windows(marker.len()).any(|w| w == marker) {
//!             output.windows(marker.len()).any(|w| w == marker)
//!         } else {
//!             true // No markers in input → invariant vacuously satisfied.
//!         }
//!     }),
//! );
//!
//! let input = b"{\"cache_control\":{\"type\":\"ephemeral\"}}";
//! let output = b"{\"cache_control\":{\"type\":\"ephemeral\"}}";
//! assert!(registry.check_all("marker-immutability", input, output));
//! ```

/// A single named invariant check: takes input and output bytes, returns `true`
/// if the invariant holds for this (input, output) pair.
///
/// Returning `false` means the component violated the registered invariant.
pub type InvariantCheck = dyn Fn(&[u8], &[u8]) -> bool + Send + Sync;

/// Result of running extension invariant checks.
#[derive(Debug, Clone)]
pub struct ExtensionCheckResult {
    /// The invariant name that was checked.
    pub invariant_name: String,
    /// `true` if the invariant held, `false` if it was violated.
    pub passed: bool,
}

/// Registry of caller-registered extension invariant checks.
///
/// The harness accepts a registry and runs all registered checks against each
/// (input, output) pair during the conformance run.
///
/// # Registration order
///
/// Checks are run in registration order. All registered checks are evaluated;
/// a failing check does not stop subsequent checks.
pub struct ExtensionRegistry {
    entries: Vec<(String, Box<InvariantCheck>)>,
}

impl ExtensionRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Register a named invariant check.
    ///
    /// The `name` is used in harness reports to identify which invariant failed.
    /// Names do not need to be unique, but uniqueness is recommended.
    pub fn register(&mut self, name: impl Into<String>, check: Box<InvariantCheck>) {
        self.entries.push((name.into(), check));
    }

    /// Run all checks registered under `invariant_name` against `(input, output)`.
    ///
    /// Returns `true` iff all matching checks pass. Returns `true` if no checks
    /// are registered under that name (vacuously true).
    pub fn check_all(&self, invariant_name: &str, input: &[u8], output: &[u8]) -> bool {
        self.entries
            .iter()
            .filter(|(name, _)| name == invariant_name)
            .all(|(_, check)| check(input, output))
    }

    /// Run all registered checks against `(input, output)`.
    ///
    /// Returns a result per registered entry.
    pub fn run_all(&self, input: &[u8], output: &[u8]) -> Vec<ExtensionCheckResult> {
        self.entries
            .iter()
            .map(|(name, check)| ExtensionCheckResult {
                invariant_name: name.clone(),
                passed: check(input, output),
            })
            .collect()
    }

    /// Returns the number of registered checks.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` if no checks are registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for ExtensionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Built-in extension checks for downstream consumers
// ============================================================================

/// Create a marker-immutability invariant check (for #308 demonstration / AC16).
///
/// Returns a check that verifies: if `input` contains a `cache_control` marker,
/// the `output` must also contain it byte-identical.
///
/// This demonstrates the extension mechanism; the production #308 implementation
/// will register a more precise check.
pub fn marker_immutability_check() -> Box<InvariantCheck> {
    Box::new(|input: &[u8], output: &[u8]| {
        let marker = b"\"cache_control\"";
        let input_has_marker = input.windows(marker.len()).any(|w| w == marker.as_ref());
        if !input_has_marker {
            return true; // Vacuously satisfied: no markers in input.
        }
        output.windows(marker.len()).any(|w| w == marker.as_ref())
    })
}

// ============================================================================
// ext:lossless-content — per-engine oracle (#427 Pass 3)
// ============================================================================

/// Budget for [`crate::canonical::value_equivalent_raw`] in the
/// `lossless-content` extension check.
///
/// 10× the production `DEFAULT_WORK_BUDGET` (200 000 nodes). The harness runs
/// offline against a small static corpus, so a generous budget is safe and
/// ensures even large adversarial JSON fixtures are fully compared.
const LOSSLESS_CONTENT_BUDGET: usize = 2_000_000;

/// Create the `lossless-content` extension invariant check.
///
/// # Semantics
///
/// The check passes iff:
/// - `output == input` (byte-identical), **or**
/// - For each CHANGED `"text"` content block in the Anthropic request body:
///   - **JSON block** (trimmed text starts with `{` or `[` and parses as valid
///     JSON): the change is value-equivalent with raw number token preservation,
///     verified via [`crate::canonical::value_equivalent_raw`] with a generous
///     budget.
///   - **LOG block** (≥50% of non-blank lines match log-level patterns): the
///     change satisfies the log oracle — every distinct input line appears
///     verbatim as a substring in the output, **and** the output header's total
///     line count equals the non-blank input line count.
///   - **All other block classes**: byte-identical required (no oracle ⇒ no
///     exemption).
///
/// # Implementation: whole-body granularity + per-block class detection
///
/// The harness passes raw request bytes. Block segmentation navigates the
/// Anthropic schema: `messages[].content[].text` for `"type": "text"` blocks.
/// Each corpus fixture follows the one-class convention (JSON or LOG per
/// fixture; see `corpus::ANTHROPIC_JSON_NUMBER_TOKENS` and
/// `corpus::ANTHROPIC_LOG_DEDUP`) so per-block class detection is reliable.
///
/// # Falsifiability (PF-007)
///
/// `LossyEngineContract` in `harness::self_test` stubs a JSON number to `0`,
/// which is NOT value-equivalent to `1e10` by raw bytes. It must fail ONLY
/// this invariant. Isolation is asserted by
/// `assert_lossy_engine_fails_lossless_content_only`.
///
/// # ADR-007 / #427
///
/// Registered as `"lossless-content"` → reported as `"ext:lossless-content"` in
/// `ConformanceReport`.
pub fn lossless_content_check() -> Box<InvariantCheck> {
    Box::new(|input: &[u8], output: &[u8]| check_lossless_content(input, output))
}

fn check_lossless_content(input: &[u8], output: &[u8]) -> bool {
    // Byte-identical: trivially passes.
    if input == output {
        return true;
    }
    // Non-UTF-8 changed body: no oracle available.
    let Ok(input_str) = std::str::from_utf8(input) else {
        return false;
    };
    let Ok(output_str) = std::str::from_utf8(output) else {
        return false;
    };
    // Parse both as JSON to navigate the Anthropic message schema.
    let Ok(input_val) = serde_json::from_str::<serde_json::Value>(input_str) else {
        return false;
    };
    let Ok(output_val) = serde_json::from_str::<serde_json::Value>(output_str) else {
        return false;
    };

    let input_texts = extract_content_texts(&input_val);
    let output_texts = extract_content_texts(&output_val);

    // Block count must be identical (no messages/blocks added or removed).
    if input_texts.len() != output_texts.len() {
        return false;
    }

    for (i_text, o_text) in input_texts.iter().zip(output_texts.iter()) {
        if i_text == o_text {
            continue; // Block unchanged.
        }
        if !check_text_block_lossless(i_text, o_text) {
            return false;
        }
    }
    true
}

/// Extract decoded text from all `"type": "text"` content blocks in an Anthropic
/// request body.
///
/// Navigates `messages[].content` in both string-form and array-of-blocks-form.
/// Non-text block types (thinking, image, tool_use, tool_result) are skipped.
fn extract_content_texts(val: &serde_json::Value) -> Vec<String> {
    let mut texts = Vec::new();
    let Some(messages) = val.get("messages").and_then(|m| m.as_array()) else {
        return texts;
    };
    for msg in messages {
        extract_message_texts(msg, &mut texts);
    }
    texts
}

fn extract_message_texts(msg: &serde_json::Value, texts: &mut Vec<String>) {
    match msg.get("content") {
        Some(serde_json::Value::String(s)) => texts.push(s.clone()),
        Some(serde_json::Value::Array(blocks)) => {
            for block in blocks {
                if block
                    .get("type")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t == "text")
                    && let Some(text) = block.get("text").and_then(|t| t.as_str())
                {
                    texts.push(text.to_owned());
                }
            }
        }
        _ => {}
    }
}

/// Check a single changed text block for losslessness.
///
/// - **JSON class**: verified via `value_equivalent_raw`.
/// - **LOG class**: verified via the log oracle.
/// - **Other class**: requires byte-identical; change already detected by caller.
fn check_text_block_lossless(input_text: &str, output_text: &str) -> bool {
    let trimmed = input_text.trim();

    // JSON class: starts with `{` or `[` and parses as valid JSON.
    if (trimmed.starts_with('{') || trimmed.starts_with('['))
        && serde_json::from_str::<serde::de::IgnoredAny>(trimmed).is_ok()
    {
        let mut budget = LOSSLESS_CONTENT_BUDGET;
        return matches!(
            crate::canonical::value_equivalent_raw(trimmed, output_text.trim(), &mut budget),
            Some(true)
        );
    }

    // LOG class: ≥50% of non-blank lines match log-level patterns.
    if is_log_text(input_text) {
        return check_log_oracle(input_text, output_text);
    }

    // Other class: change detected by caller → fail (no oracle).
    false
}

/// Returns `true` if ≥50% of non-blank lines in `text` carry a log-level prefix.
///
/// Mirrors the heuristic in `rskim_llm::classify::try_classify_log` to detect
/// the same block class without introducing an rskim-llm dependency.
fn is_log_text(text: &str) -> bool {
    let (total, matching) = text.lines().filter(|l| !l.trim().is_empty()).fold(
        (0usize, 0usize),
        |(total, matching), line| {
            (
                total + 1,
                matching + usize::from(line_has_log_level_prefix(line)),
            )
        },
    );
    total > 0 && matching * 2 >= total
}

/// Returns `true` if `line` starts with a known log-level prefix.
///
/// Recognizes bare and colon/space/tab-separated prefixes (e.g. `ERROR:`,
/// `WARN `, `INFO\t`). Uses ASCII uppercase comparison to avoid allocation.
///
/// Also handles lines where a log-level prefix is preceded by an ISO-8601 'T'-
/// style timestamp (e.g. `"2024-01-01T10:00:00Z ERROR: msg"`): the timestamp
/// portion (up to the first space, provided it starts with 4 digits + '-') is
/// stripped before the level check.
fn line_has_log_level_prefix(line: &str) -> bool {
    const LEVELS: &[&str] = &[
        "ERROR", "WARN", "WARNING", "INFO", "DEBUG", "TRACE", "FATAL", "CRITICAL",
    ];

    // Strip a leading ISO-8601-ish timestamp prefix (YYYY-MM-... up to first space)
    // so that timestamped log lines such as "2024-01-01T10:00:00Z ERROR: msg"
    // are detected as log lines by `is_log_text`.
    let effective_line = if let Some(space_pos) = line.find(' ') {
        let prefix = &line[..space_pos];
        let pb = prefix.as_bytes();
        // Heuristic: starts with 4 digits + '-' = date-like prefix.
        if pb.len() >= 10 && pb[..4].iter().all(|b| b.is_ascii_digit()) && pb.get(4) == Some(&b'-')
        {
            &line[space_pos + 1..]
        } else {
            line
        }
    } else {
        line
    };

    let lb = effective_line.as_bytes();
    LEVELS.iter().any(|&level| {
        let lev = level.as_bytes();
        if lb.len() < lev.len() {
            return false;
        }
        if !lb[..lev.len()].eq_ignore_ascii_case(lev) {
            return false;
        }
        let rest = &lb[lev.len()..];
        rest.is_empty() || rest[0] == b':' || rest[0] == b' ' || rest[0] == b'\t'
    })
}

/// Log oracle for the `lossless-content` invariant.
///
/// Verifies:
/// 1. Every distinct non-blank input line (after stripping an ISO-8601 timestamp
///    prefix) appears verbatim (as a substring) in the output, proving no log
///    message was silently dropped.
/// 2. The output header reports a total equal to the non-blank input line count,
///    proving no line was silently excluded from the count.
/// 3. (Conditional) If the input has ISO-8601 timestamp prefixes AND the output
///    contains a `..` range annotation, both the min and max timestamps extracted
///    from the input must appear as substrings in the output.
///
/// Header format from `LogResult::render`:
/// `"N lines → M unique (K duplicates removed)"` where `N` is the first token.
///
/// # Why substring containment for check 1
///
/// The Lossless log engine reformats entries as ` LEVEL: message (×N, [ts..ts])`.
/// After stripping the timestamp prefix, the verbatim message text
/// `"ERROR: connection refused"` IS a substring of
/// `" ERROR: connection refused (×3, [2024-01-01T10:00:00Z..2024-01-01T10:10:00Z])"`,
/// so no content was lost.
///
/// # Timestamp extremes (check 3 — ADR-007 Pass 5)
///
/// The Lossless engine annotates duplicate groups with `[ts_min..ts_max]`. The
/// oracle verifies that both extremes appear verbatim in the output, proving the
/// metadata was captured and not silently dropped. The check is conditional: it
/// only applies when the input has ≥2 distinct ISO-8601 timestamp prefixes AND
/// the output contains the `..` separator (i.e., a range annotation is present).
fn check_log_oracle(input_text: &str, output_text: &str) -> bool {
    let mut non_blank_count = 0usize;
    let mut seen = std::collections::HashSet::<String>::new();

    // Collect ISO-8601 timestamp prefixes for check 3.
    // Heuristic: a line starting with 4 ASCII digits followed by '-' carries a timestamp.
    let mut timestamps: Vec<String> = Vec::new();

    // Check 1: every distinct non-blank input line appears as a substring in output.
    for line in input_text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        non_blank_count += 1;

        // Collect timestamp prefix for check 3 (before stripping it for check 1).
        let bytes = trimmed.as_bytes();
        if bytes.len() >= 10
            && bytes[..4].iter().all(|&b| b.is_ascii_digit())
            && bytes.get(4) == Some(&b'-')
            && bytes.get(7) == Some(&b'-')
        {
            let ts_end = trimmed.find(' ').unwrap_or(trimmed.len().min(30));
            timestamps.push(trimmed[..ts_end].to_owned());
        }

        // Strip leading timestamp prefix (if present) to get the message text.
        let message = if let Some(space_pos) = trimmed.find(' ') {
            let prefix = &trimmed[..space_pos];
            let prefix_bytes = prefix.as_bytes();
            if prefix_bytes.len() >= 10
                && prefix_bytes[..4].iter().all(|&b| b.is_ascii_digit())
                && prefix_bytes.get(4) == Some(&b'-')
            {
                trimmed[space_pos + 1..].trim()
            } else {
                trimmed
            }
        } else {
            trimmed
        };

        if seen.insert(message.to_owned()) && !output_text.contains(message) {
            return false;
        }
    }

    // Check 2: parse the header's total and compare to non_blank_count.
    // Header: "N lines → M unique (K duplicates removed)"
    let header_ok = if let Some(reported_total) = output_text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().next())
        .and_then(|n| n.parse::<usize>().ok())
    {
        reported_total == non_blank_count
    } else {
        // No parseable header: the output is not a log compression result.
        return false;
    };

    if !header_ok {
        return false;
    }

    // Check 3 (conditional): when input has ≥2 timestamps AND output has a range
    // annotation (`..`), verify that both the min and max timestamps appear in output.
    if timestamps.len() >= 2 && output_text.contains("..") {
        let (Some(min_ts), Some(max_ts)) = (timestamps.iter().min(), timestamps.iter().max())
        else {
            // Non-empty iterator always yields Some; unreachable in practice.
            return true;
        };
        if !output_text.contains(min_ts.as_str()) || !output_text.contains(max_ts.as_str()) {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_empty_check_all_returns_true() {
        let registry = ExtensionRegistry::new();
        // No checks registered → vacuously true.
        assert!(registry.check_all("any-invariant", b"input", b"output"));
    }

    #[test]
    fn registry_check_all_passing() {
        let mut registry = ExtensionRegistry::new();
        registry.register("always-pass", Box::new(|_input, _output| true));
        assert!(registry.check_all("always-pass", b"input", b"output"));
    }

    #[test]
    fn registry_check_all_failing() {
        let mut registry = ExtensionRegistry::new();
        registry.register("always-fail", Box::new(|_input, _output| false));
        assert!(!registry.check_all("always-fail", b"input", b"output"));
    }

    #[test]
    fn registry_check_all_filters_by_name() {
        let mut registry = ExtensionRegistry::new();
        registry.register("pass", Box::new(|_, _| true));
        registry.register("fail", Box::new(|_, _| false));
        // Only "pass" checks run.
        assert!(registry.check_all("pass", b"x", b"y"));
        // Only "fail" checks run.
        assert!(!registry.check_all("fail", b"x", b"y"));
    }

    #[test]
    fn registry_run_all_returns_all_results() {
        let mut registry = ExtensionRegistry::new();
        registry.register("check-a", Box::new(|_, _| true));
        registry.register("check-b", Box::new(|_, _| false));
        let results = registry.run_all(b"in", b"out");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].invariant_name, "check-a");
        assert!(results[0].passed);
        assert_eq!(results[1].invariant_name, "check-b");
        assert!(!results[1].passed);
    }

    #[test]
    fn registry_len_and_is_empty() {
        let mut registry = ExtensionRegistry::new();
        assert!(registry.is_empty());
        registry.register("x", Box::new(|_, _| true));
        assert_eq!(registry.len(), 1);
        assert!(!registry.is_empty());
    }

    // ========================================================================
    // Marker-immutability extension check (AC16)
    // ========================================================================

    #[test]
    fn marker_immutability_input_has_marker_output_has_it() {
        let check = marker_immutability_check();
        let input = b"{\"cache_control\":{\"type\":\"ephemeral\"}}";
        let output = b"{\"cache_control\":{\"type\":\"ephemeral\"}}";
        assert!(check(input, output));
    }

    #[test]
    fn marker_immutability_input_has_marker_output_drops_it() {
        let check = marker_immutability_check();
        let input = b"{\"cache_control\":{\"type\":\"ephemeral\"}}";
        let output = b"{}"; // marker dropped
        assert!(!check(input, output));
    }

    #[test]
    fn marker_immutability_no_marker_in_input_vacuously_true() {
        let check = marker_immutability_check();
        let input = b"{\"model\":\"gpt-4o\"}";
        let output = b"{\"model\":\"gpt-4o-mini\"}"; // modified, but no marker
        assert!(check(input, output));
    }

    #[test]
    fn registry_with_marker_immutability() {
        let mut registry = ExtensionRegistry::new();
        registry.register("marker-immutability", marker_immutability_check());

        // Reference impl: input with marker, output preserves it → pass.
        let input = b"x\"cache_control\"y";
        let output = b"x\"cache_control\"y";
        assert!(registry.check_all("marker-immutability", input, output));

        // Marker-dropping impl: input with marker, output drops it → fail.
        let bad_output = b"xy";
        assert!(!registry.check_all("marker-immutability", input, bad_output));
    }
}
