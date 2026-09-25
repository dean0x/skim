//! Output guardrail (L2-A): prevents emitting compressed output that is larger
//! than raw — unified gate post-A2/A4.
//!
//! The guard delegates all decisions to [`crate::output::fidelity::decide`]:
//!
//! - **No 256-byte floor (A4):** all payload sizes are subject to the gate.
//! - **Tie → Passthrough (A2):** the gate is strictly-smaller-in-both-bytes-
//!   and-tokens; equal lengths trigger passthrough, not pass.
//! - **Banner gated behind `SKIM_DEBUG` (ADR-011):** the `[skim:guardrail]`
//!   banner is a no-data-loss raw-fallback notice; it must not fire on the
//!   default silent path.
//!
//! `apply()` still accepts an explicit `Write` so existing tests can assert
//! the banner text without a real stderr.

use std::io::{self, Write};

use anyhow::Result;

/// Outcome of the guardrail check.
#[derive(Debug)]
pub(crate) enum GuardrailOutcome {
    /// Compressed body is strictly smaller in bytes and tokens — emit it.
    Passed { output: String },
    /// Compressed body is equal or larger — emit raw verbatim instead.
    Triggered { output: String },
}

impl GuardrailOutcome {
    /// Returns `true` if the guardrail fell back to raw (tie or expansion).
    pub(crate) fn was_triggered(&self) -> bool {
        matches!(self, GuardrailOutcome::Triggered { .. })
    }

    /// Consume the outcome and return the output string.
    pub(crate) fn into_output(self) -> String {
        match self {
            GuardrailOutcome::Passed { output } | GuardrailOutcome::Triggered { output } => output,
        }
    }
}

/// Apply the output guardrail, writing a debug banner to `writer` on trigger.
///
/// The unified rule (see [`crate::output::fidelity::decide`]): keep compressed
/// IFF strictly smaller than raw in both bytes and tokens.  Tie or expansion →
/// `Triggered { output: raw }`.
///
/// Takes ownership of both strings to avoid cloning on the fast path.
///
/// Charge-nothing shim over [`apply_with_notice`]: prices the stdout bodies
/// only. See [`crate::output::fidelity::decide`].
pub(crate) fn apply(
    raw: String,
    compressed: String,
    writer: &mut impl Write,
) -> Result<GuardrailOutcome> {
    apply_with_notice(raw, compressed, None, writer)
}

/// [`apply`], charging the stderr disclosure that the `Passed` branch will
/// emit against the compressed side (ADR-001 amendment 2026-09-24).
///
/// `notice` is the **differential** cost of choosing compressed — `None`
/// whenever the raw branch would print a byte-identical notice of its own.
/// See [`crate::output::fidelity::decide_with_notice`] for the rule.
pub(crate) fn apply_with_notice(
    raw: String,
    compressed: String,
    notice: Option<&str>,
    writer: &mut impl Write,
) -> Result<GuardrailOutcome> {
    use crate::output::fidelity::{FidelityDecision, decide_with_notice};
    match decide_with_notice(&raw, &compressed, notice) {
        FidelityDecision::Keep => Ok(GuardrailOutcome::Passed { output: compressed }),
        FidelityDecision::Passthrough => {
            writeln!(
                writer,
                "[skim:guardrail] compressed output not strictly smaller; emitting raw"
            )?;
            Ok(GuardrailOutcome::Triggered { output: raw })
        }
    }
}

/// Convenience wrapper: apply the guardrail, writing the banner only when
/// `SKIM_DEBUG=1` / `--debug` is active (ADR-011: no-data-loss banners are
/// debug-gated; loss-bearing elision markers are unconditional).
///
/// Charge-nothing: routes through [`apply`], which prices the bodies only.
pub(crate) fn apply_to_stderr(raw: String, compressed: String) -> Result<GuardrailOutcome> {
    if crate::debug::is_debug_enabled() {
        apply(raw, compressed, &mut io::stderr())
    } else {
        apply(raw, compressed, &mut io::sink())
    }
}

/// [`apply_to_stderr`], charging the differential disclosure cost.
pub(crate) fn apply_to_stderr_with_notice(
    raw: String,
    compressed: String,
    notice: Option<&str>,
) -> Result<GuardrailOutcome> {
    if crate::debug::is_debug_enabled() {
        apply_with_notice(raw, compressed, notice, &mut io::stderr())
    } else {
        apply_with_notice(raw, compressed, notice, &mut io::sink())
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_when_compressed_strictly_shorter() {
        let raw = "function hello() { return 'world'; }".to_string();
        let compressed = "function hello()".to_string();
        let mut buf = Vec::new();
        let outcome = apply(raw, compressed.clone(), &mut buf).unwrap();
        assert!(!outcome.was_triggered());
        assert_eq!(outcome.into_output(), compressed);
        assert!(buf.is_empty(), "banner must not fire on Passed path");
    }

    /// A2: tie (equal trimmed length) → Triggered (Passthrough).
    /// Pre-A2 the gate used `<=` (tie → Passed); post-A2 it uses strict-less.
    #[test]
    fn a2_tie_triggers_passthrough() {
        let raw = "hello world".to_string();
        let compressed = "world hello".to_string(); // same byte length
        let mut buf = Vec::new();
        let outcome = apply(raw.clone(), compressed, &mut buf).unwrap();
        assert!(
            outcome.was_triggered(),
            "A2: byte tie must trigger (strictly-smaller rule)"
        );
        assert_eq!(outcome.into_output(), raw);
    }

    /// A4: no 256-byte floor — tiny payloads are NOT exempt.
    /// Pre-A4: a 1-byte raw with a 100-byte compressed form skipped the gate
    /// (Tier-0 exemption) and returned Passed.  Post-A4: Triggered fires.
    #[test]
    fn a4_no_floor_tiny_raw_compressed_larger_triggers() {
        let raw = "x".to_string();
        let compressed =
            "this is a much longer string that has many more tokens than the raw input".to_string();
        let mut buf = Vec::new();
        let outcome = apply(raw.clone(), compressed, &mut buf).unwrap();
        assert!(
            outcome.was_triggered(),
            "A4: tiny raw must NOT skip the gate — compressed larger → Triggered"
        );
        assert_eq!(outcome.into_output(), raw);
    }

    #[test]
    fn triggered_when_compressed_larger_bytes_and_tokens() {
        let raw = "x".repeat(300);
        let compressed_content = "this is a much longer string with many more tokens ".repeat(20);
        let mut buf = Vec::new();
        let outcome = apply(raw.clone(), compressed_content, &mut buf).unwrap();
        assert!(outcome.was_triggered());
        assert_eq!(outcome.into_output(), raw);
        let warning = String::from_utf8(buf).unwrap();
        assert!(
            warning.contains("[skim:guardrail]"),
            "expected guardrail banner, got: {warning}"
        );
    }

    /// A4: even when compressed > raw with tiny raw, gate applies (floor gone).
    #[test]
    fn a4_tiny_raw_compressed_larger_triggers() {
        let raw = "abcdefghij".to_string();
        let compressed = "a b c d e f g h i j k".to_string(); // more bytes
        let mut buf = Vec::new();
        let outcome = apply(raw.clone(), compressed, &mut buf).unwrap();
        // compressed is byte-larger → Triggered (floor removal means no exemption)
        assert!(
            outcome.was_triggered(),
            "A4: tiny raw with larger compressed must trigger"
        );
        assert_eq!(outcome.into_output(), raw);
    }

    /// ADR-001 amendment 2026-09-24: a compressed view that cannot pay for its
    /// own stderr disclosure falls back to raw — and says so on the debug
    /// banner, which is what distinguishes "the guard decided" from "the
    /// transform silently produced raw".
    #[test]
    fn notice_wider_than_saving_triggers_with_banner() {
        let raw = "alpha beta gamma delta epsilon zeta eta theta".to_string(); // 44 B
        let compressed = "alpha beta gamma delta".to_string(); // 22 B saving

        // Body-only arithmetic keeps the 22-byte saving.
        let mut uncharged = Vec::new();
        let passed = apply(raw.clone(), compressed.clone(), &mut uncharged).unwrap();
        assert!(!passed.was_triggered(), "uncharged: 22-byte saving is kept");
        assert!(uncharged.is_empty(), "banner must not fire on Passed path");

        // The production `pseudo` direct marker is 90 bytes — four times the
        // saving. Charging it turns the same view into a net loss.
        let notice = "x".repeat(90);
        let mut charged = Vec::new();
        let outcome =
            apply_with_notice(raw.clone(), compressed, Some(&notice), &mut charged).unwrap();
        assert!(
            outcome.was_triggered(),
            "a view that cannot pay for its own disclosure must serve raw"
        );
        assert_eq!(
            outcome.into_output(),
            raw,
            "the raw bytes must be served verbatim"
        );
        let banner = String::from_utf8(charged).unwrap();
        assert!(
            banner.contains("[skim:guardrail]"),
            "the fallback must be attributable to the guard, not silent; got: {banner}"
        );
    }

    /// Empty raw + empty compressed: both trim to ""; comp_t.len() >= raw_t.len()
    /// (0 >= 0) → Triggered (emit raw = empty string).
    #[test]
    fn empty_inputs_trigger() {
        let mut buf = Vec::new();
        let outcome = apply(String::new(), String::new(), &mut buf).unwrap();
        // tie (0 == 0) → Triggered; output is the raw empty string
        assert!(outcome.was_triggered(), "empty tie must trigger");
        assert_eq!(outcome.into_output(), "");
    }
}
