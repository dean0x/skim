//! Three-tier parse degradation output (#41)
//!
//! Provides ParseResult enum (Full/Degraded/Passthrough), output cleaning
//! (ANSI stripping, token-aware truncation, and filter transparency headers.

// Infrastructure module — canonical, tee, and several items here (OutputMode,
// PassthroughTruncator, FilterTransparencyHeader) have consumers arriving in
// Phase B tickets; suppress dead_code until those callers land.
#![allow(dead_code)]

pub(crate) mod canonical;
pub(crate) mod fidelity;
pub(crate) mod guardrail;
pub(crate) mod tee;

use std::io::{self, Write};

use crate::tokens;

// ============================================================================
// ParseResult<T> — three-tier parse degradation
// ============================================================================

/// Result of parsing external process output through three degradation tiers.
///
/// - `Full`: clean parse, no issues
/// - `Degraded`: partially parsed with warning markers
/// - `Passthrough`: unparseable, returned as-is (always `String`)
/// - `RawPassthrough`: payload-less signal — execution.rs serves `CommandOutput::stdout`
///   byte-faithfully without cloning it into the parse result
#[derive(Debug, Clone)]
pub(crate) enum ParseResult<T> {
    Full(T),
    Degraded(T, Vec<String>),
    /// Always `String` regardless of `T` — content could not be parsed.
    Passthrough(String),
    /// Payload-less passthrough signal: execution.rs serves `CommandOutput::stdout`
    /// byte-faithfully without cloning it into the parse result.
    ///
    /// Used by pure-passthrough handlers (grep, rg) where the parse function would
    /// otherwise clone the entire stdout into the enum payload only to have it
    /// re-read immediately by `serialize_output`.  The caller
    /// (`run_parsed_command_with_exit`) detects this variant and emits from
    /// `output.stdout` directly — no content round-trip through the enum.
    ///
    /// `content()` returns `""` for this variant; callers that need the
    /// original bytes must read from `CommandOutput::stdout` directly.
    /// `to_json_envelope()` is unreachable for this variant (execution.rs handles
    /// it before reaching `serialize_output`).
    RawPassthrough,
}

impl<T> ParseResult<T> {
    /// Returns `true` if this is a `Full` result.
    pub(crate) fn is_full(&self) -> bool {
        matches!(self, ParseResult::Full(_))
    }

    /// Returns `true` if this is a `Degraded` result.
    pub(crate) fn is_degraded(&self) -> bool {
        matches!(self, ParseResult::Degraded(_, _))
    }

    /// Returns `true` if this is a `Passthrough` or `RawPassthrough` result.
    pub(crate) fn is_passthrough(&self) -> bool {
        matches!(
            self,
            ParseResult::Passthrough(_) | ParseResult::RawPassthrough
        )
    }

    /// Returns the tier name as a static string.
    pub(crate) fn tier_name(&self) -> &'static str {
        match self {
            ParseResult::Full(_) => "full",
            ParseResult::Degraded(_, _) => "degraded",
            ParseResult::Passthrough(_) | ParseResult::RawPassthrough => "passthrough",
        }
    }
}

impl<T: AsRef<str>> ParseResult<T> {
    /// Read access to inner content for all tiers.
    ///
    /// Returns `""` for [`ParseResult::RawPassthrough`]; callers must read from
    /// `CommandOutput::stdout` directly for that variant.
    pub(crate) fn content(&self) -> &str {
        match self {
            ParseResult::Full(inner) | ParseResult::Degraded(inner, _) => inner.as_ref(),
            ParseResult::Passthrough(s) => s.as_str(),
            ParseResult::RawPassthrough => "",
        }
    }

    /// Write degradation markers to the given writer.
    ///
    /// - `Full`: writes nothing
    /// - `Degraded`: writes each marker as `[skim:warning]` line (only when debug enabled)
    /// - `Passthrough`: writes a `[skim:notice]` line (only when debug enabled)
    ///
    /// When debug mode is disabled (the default), this is a no-op for all tiers.
    /// Enable with `--debug` CLI flag or `SKIM_DEBUG=1` env var.
    pub(crate) fn emit_markers(&self, writer: &mut impl Write) -> io::Result<()> {
        if !crate::debug::is_debug_enabled() {
            return Ok(());
        }
        match self {
            ParseResult::Full(_) => Ok(()),
            ParseResult::Degraded(_, markers) => {
                for marker in markers {
                    writeln!(writer, "[skim:warning] {marker}")?;
                }
                Ok(())
            }
            ParseResult::Passthrough(_) | ParseResult::RawPassthrough => {
                writeln!(
                    writer,
                    "[skim:notice] output passed through without parsing"
                )
            }
        }
    }
}

impl<T: serde::Serialize> ParseResult<T> {
    /// Serialize the parse result as a JSON string suitable for `--json` output.
    ///
    /// - `Full(result)` -> direct JSON serialization (no envelope -- preserves existing behavior)
    /// - `Degraded(result, warnings)` -> `{"tier":"degraded","warnings":[...],"result":{...}}`
    /// - `Passthrough(raw)` -> `{"tier":"passthrough","raw":"..."}`
    pub(crate) fn to_json_envelope(&self) -> serde_json::Result<String> {
        match self {
            ParseResult::Full(inner) => serde_json::to_string(inner),
            ParseResult::Degraded(inner, warnings) => {
                let val = serde_json::json!({
                    "tier": "degraded",
                    "warnings": warnings,
                    "result": inner,
                });
                serde_json::to_string(&val)
            }
            ParseResult::Passthrough(raw) => {
                let val = serde_json::json!({
                    "tier": "passthrough",
                    "raw": raw,
                });
                serde_json::to_string(&val)
            }
            ParseResult::RawPassthrough => {
                // RawPassthrough carries no payload; execution.rs handles this variant
                // directly from CommandOutput::stdout before reaching to_json_envelope().
                // This arm is unreachable in correct usage.
                unreachable!(
                    "RawPassthrough has no payload for JSON serialization; \
                     execution.rs must handle it before calling to_json_envelope()"
                )
            }
        }
    }
}

impl<T: Into<String>> ParseResult<T> {
    /// Consuming access to inner content as `String`.
    ///
    /// Returns `String::new()` for [`ParseResult::RawPassthrough`]; callers must
    /// read from `CommandOutput::stdout` directly for that variant.
    pub(crate) fn into_content(self) -> String {
        match self {
            ParseResult::Full(inner) | ParseResult::Degraded(inner, _) => inner.into(),
            ParseResult::Passthrough(s) => s,
            ParseResult::RawPassthrough => String::new(),
        }
    }
}

// ============================================================================
// OutputMode
// ============================================================================

/// Controls how much cleaning is applied to passthrough output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputMode {
    /// Full cleaning pipeline: ANSI strip + progress collapse + deduplication.
    Compact,
    /// Minimal cleaning: ANSI strip only (preserves progress lines and duplicates).
    Verbose,
}

impl OutputMode {
    /// Derive output mode from a process exit code.
    ///
    /// - `0` → `Compact` (success, clean output)
    /// - non-zero → `Verbose` (failure, preserve everything for debugging)
    pub(crate) fn from_exit_code(code: i32) -> Self {
        if code == 0 {
            OutputMode::Compact
        } else {
            OutputMode::Verbose
        }
    }

    /// Derive output mode from an optional exit code.
    ///
    /// - `Some(code)` → delegates to `from_exit_code`
    /// - `None` (signal kill, no exit code) → `Verbose`
    pub(crate) fn from_optional_exit_code(code: Option<i32>) -> Self {
        code.map_or(OutputMode::Verbose, Self::from_exit_code)
    }
}

// ============================================================================
// PassthroughCleaner — module-level functions
// ============================================================================

/// Strip ANSI escape sequences from the input string.
///
/// Delegates to [`strip_escape_sequences`], which removes only ESC-rooted
/// sequences (CSI, OSC, 2-byte) while leaving every other byte — including
/// TABs and C0 controls — unchanged.  Unterminated or overlong sequences are
/// emitted literally rather than silently dropped (#317 / PF-006 / #465).
pub(crate) fn strip_ansi(input: &str) -> String {
    strip_escape_sequences(input)
}

/// Remove only ANSI/VT escape sequences from `input`, leaving every other byte
/// (including TABs and C0 controls) unchanged.
///
/// Handled sequence types:
/// - CSI: `ESC [ <parameter bytes> <final byte 0x40..=0x7e>`
/// - OSC: `ESC ] <text> BEL`  or  `ESC ] <text> ESC \` (ST)
/// - 2-byte: `ESC x` where `x` is in `0x20..=0x7e` — both bytes discarded
///
/// **Bounded scanning (#317 / "compress, never truncate"):**
/// Both CSI and OSC loops are capped at [`MAX_SEQ_SCAN`] bytes.  If that bound
/// is exceeded, if the input ends before a sequence terminator is found, or if a
/// byte that cannot legally appear in the sequence body is encountered (a CSI
/// byte outside `0x20..=0x3f`, or a C0 control other than BEL/ESC inside an OSC
/// body — e.g. a newline), the consumed bytes (including the leading `ESC` and
/// type byte) are emitted **literally** rather than silently dropped.  This
/// body-validity bail is what stops a malformed `ESC [ 3 2` from scanning across
/// a newline and swallowing it plus the next letter.  Losing content is the failure
/// mode we are eliminating; the safe direction is "if this doesn't look like a
/// real sequence, keep it".
///
/// **Bare-ESC safety:** the byte after `ESC` is peeked before consuming.  Only
/// bytes in `0x20..=0x7e` (valid 2-byte-sequence second bytes) are consumed.
/// A control character following `ESC` (e.g. `\n`, `\t`) is left in the stream
/// and the lone `ESC` is emitted literally, preventing two lines from merging.
///
/// # Why not `strip_ansi_escapes::strip_str`?
///
/// That crate drives a `vte` state machine that emits only printables + `\n`,
/// silently discarding ALL C0 control bytes — including TABs (`0x09`).  When one
/// ESC byte is present anywhere in the buffer the entire buffer is re-encoded,
/// destroying tabs on every line.  This is a #317 violation (PF-006 / issue #465).
fn strip_escape_sequences(input: &str) -> String {
    /// Maximum bytes to scan inside a single escape-sequence body.
    ///
    /// No valid ANSI/VT sequence exceeds a few hundred bytes.  Sequences that
    /// go beyond this bound are not real escape sequences; emit them literally
    /// so the reader sees the content rather than losing it silently (#317).
    const MAX_SEQ_SCAN: usize = 2048;

    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    loop {
        let c = match chars.next() {
            None => break,
            Some(c) => c,
        };
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        // ESC byte — peek at the next character to determine sequence type.
        // Peekable lets us inspect without consuming, so control characters
        // (e.g. `\n`) are never eaten by the bare-ESC arm.
        match chars.peek().copied() {
            // CSI: ESC [ <params> <final byte in 0x40..=0x7e>
            Some('[') => {
                chars.next(); // consume `[`
                let mut seq_buf = String::new();
                let mut terminated = false;
                for b in chars.by_ref() {
                    seq_buf.push(b);
                    if ('\x40'..='\x7e').contains(&b) {
                        terminated = true;
                        break; // final byte consumed; sequence complete
                    }
                    if !('\x20'..='\x3f').contains(&b) {
                        // Not a valid CSI parameter (0x30..=0x3f) or intermediate
                        // (0x20..=0x2f) byte — e.g. a newline, TAB, or non-ASCII
                        // char.  This is not a real CSI sequence, so stop scanning
                        // and emit what we consumed literally.  Without this bail a
                        // malformed `ESC [ 3 2` would keep scanning across the
                        // newline and swallow it plus the next letter (the first
                        // byte in 0x40..=0x7e), merging two lines and losing
                        // content (#317).
                        break;
                    }
                    if seq_buf.len() >= MAX_SEQ_SCAN {
                        break; // safety bound exceeded
                    }
                }
                if !terminated {
                    // Unterminated or overlong — emit literally to preserve content.
                    out.push('\x1b');
                    out.push('[');
                    out.push_str(&seq_buf);
                }
                // terminated == true: sequence fully consumed and stripped
            }
            // OSC: ESC ] <text> BEL  |  ESC ] <text> ESC \(ST)
            //
            // Track whether the previous char was ESC so ST (ESC \) can be
            // detected without calling `chars.next()` inside a `by_ref()` loop
            // (which would require a second mutable borrow of `chars`).
            Some(']') => {
                chars.next(); // consume `]`
                let mut seq_buf = String::new();
                let mut prev_esc = false;
                let mut terminated = false;
                for b in chars.by_ref() {
                    if b == '\x07' {
                        terminated = true;
                        break; // BEL terminates OSC
                    }
                    if prev_esc && b == '\\' {
                        terminated = true;
                        break; // ST = ESC \ terminates OSC
                    }
                    if b < '\x20' && b != '\x1b' {
                        // A C0 control other than BEL (handled above) or ESC (the
                        // first byte of ST) cannot appear in an OSC string body.
                        // Bail and emit literally rather than scanning across a
                        // newline and swallowing subsequent lines up to the next
                        // stray BEL/ST (#317).
                        seq_buf.push(b);
                        break;
                    }
                    seq_buf.push(b);
                    prev_esc = b == '\x1b';
                    if seq_buf.len() >= MAX_SEQ_SCAN {
                        break; // safety bound exceeded
                    }
                }
                if !terminated {
                    // Unterminated or overlong — emit literally to preserve content.
                    out.push('\x1b');
                    out.push(']');
                    out.push_str(&seq_buf);
                }
                // terminated == true: sequence fully consumed and stripped
            }
            // 2-byte sequence: only consume the second byte if it is in the
            // valid ESC-Fe / ESC-Fp / ESC-Fs range (0x20..=0x7e — covers both
            // finals and intermediates).  Control characters (< 0x20) are NOT
            // valid second bytes; leave them in the stream and emit `ESC`
            // literally so a lone ESC before `\n` does not merge two lines.
            Some(c2) if ('\x20'..='\x7e').contains(&c2) => {
                chars.next(); // consume valid 2-byte sequence second byte
                // ESC + c2 stripped (standard 2-byte VT sequence)
            }
            // ESC followed by a control character or end-of-input:
            // emit `ESC` literally and leave the following character in stream.
            _ => {
                out.push('\x1b');
            }
        }
    }
    out
}

/// Strip ANSI escape sequences with a fast-path borrow.
///
/// When no ESC byte (`0x1b`) is present in `input`, returns `Cow::Borrowed(input)`
/// without any allocation. Only allocates when ANSI escapes are actually present.
///
/// The underlying scanner ([`strip_escape_sequences`]) removes only ESC-rooted
/// sequences (CSI, OSC, 2-byte), leaving every other byte — including TABs and
/// other C0 controls — unchanged.  This avoids the PF-006 / #465 regression where
/// `strip_ansi_escapes::strip_str` discarded ALL C0 controls including TABs.
///
/// Use this in hot paths where the input may already be clean (e.g. when the
/// spawn path has already stripped ANSI before passing the string to a parser).
#[inline]
pub(crate) fn strip_ansi_cow(input: &str) -> std::borrow::Cow<'_, str> {
    if input.as_bytes().contains(&0x1b) {
        std::borrow::Cow::Owned(strip_escape_sequences(input))
    } else {
        std::borrow::Cow::Borrowed(input)
    }
}

// ============================================================================
// PassthroughTruncator
// ============================================================================

/// Token-aware truncation of passthrough content.
pub(crate) struct PassthroughTruncator;

impl PassthroughTruncator {
    /// Truncate content to fit within a token budget.
    ///
    /// Algorithm:
    /// - Split into lines, accumulate tokens line by line (O(n) forward scan)
    /// - Stop when budget is reached, append `\n[... truncated N lines]`
    /// - Edge case: single very long line → byte-level binary search for
    ///   approximate token boundary
    /// - Edge case: `token_budget == 0` → return just the truncation marker
    pub(crate) fn truncate_to_budget(content: &str, token_budget: usize) -> anyhow::Result<String> {
        // Edge case: zero budget
        if token_budget == 0 {
            let total_lines = content.lines().count();
            if total_lines == 0 && content.is_empty() {
                return Ok(String::new());
            }
            return Ok(format!("\n[... truncated {total_lines} lines]"));
        }

        // Check if content already fits
        let total_tokens = tokens::count_tokens(content)?;
        if total_tokens <= token_budget {
            return Ok(content.to_string());
        }

        let lines: Vec<&str> = content.split('\n').collect();

        // Edge case: single line (no \n breaks) that exceeds budget
        if lines.len() == 1 {
            return Self::truncate_single_line(content, token_budget);
        }

        // Forward scan: accumulate tokens line by line
        let mut accumulated_tokens: usize = 0;
        let mut kept_lines: Vec<&str> = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            // Count tokens for this line (include the \n separator except for last line)
            let line_with_newline = if i < lines.len() - 1 {
                format!("{line}\n")
            } else {
                line.to_string()
            };
            let line_tokens = tokens::count_tokens(&line_with_newline)?;

            if accumulated_tokens + line_tokens > token_budget {
                // This line would exceed budget
                let truncated_count = lines.len() - kept_lines.len();
                let mut result = kept_lines.join("\n");
                result.push_str(&format!("\n[... truncated {truncated_count} lines]"));
                return Ok(result);
            }

            accumulated_tokens += line_tokens;
            kept_lines.push(line);
        }

        // Should not reach here given the early total_tokens check, but be safe
        Ok(content.to_string())
    }

    /// Truncate a single long line using byte-level binary search to find
    /// the approximate token boundary.
    fn truncate_single_line(line: &str, token_budget: usize) -> anyhow::Result<String> {
        let bytes = line.as_bytes();
        let mut lo: usize = 0;
        let mut hi: usize = bytes.len();
        let mut best: usize = 0;

        while lo <= hi {
            let mid = lo + (hi - lo) / 2;

            // Find a valid UTF-8 char boundary at or before `mid`
            let boundary = Self::floor_char_boundary(line, mid);

            let substr = &line[..boundary];
            let count = tokens::count_tokens(substr)?;

            if count <= token_budget {
                best = boundary;
                if mid == bytes.len() || boundary == bytes.len() {
                    break;
                }
                lo = mid + 1;
            } else {
                if mid == 0 {
                    break;
                }
                hi = mid.saturating_sub(1);
            }
        }

        let mut result = line[..best].to_string();
        result.push_str("\n[... truncated 1 lines]");
        Ok(result)
    }

    /// Find the largest char boundary <= `index` in the string.
    fn floor_char_boundary(s: &str, index: usize) -> usize {
        if index >= s.len() {
            return s.len();
        }
        // Walk backwards to find a valid char boundary
        let mut i = index;
        while i > 0 && !s.is_char_boundary(i) {
            i -= 1;
        }
        i
    }
}

// ============================================================================
// Loud elision markers (#317)
// ============================================================================

/// Build a loud elision marker for output that was bounded by a safety guardrail.
///
/// Skim's contract is "compress, never truncate": wrappers may re-encode output
/// but must never silently show less than the raw tool. When a hard bound is
/// genuinely unavoidable, the elision must state exact counts and how to get
/// the rest. Returns `None` when nothing was omitted (`shown >= total`), so
/// callers can unconditionally `extend()`/`push()` the result.
///
/// # Preferred `unit` strings
///
/// Use these terms consistently across all call sites to keep elision notices
/// uniform (prevent string drift as new callers are added):
///
/// | Context                    | `unit` string     |
/// |----------------------------|-------------------|
/// | table / query result rows  | `"rows"`          |
/// | file body / release notes  | `"body lines"`    |
/// | release / workflow assets  | `"assets"`        |
/// | object keys (JSON/curl)    | `"object keys"`   |
/// | generic line output        | `"lines"`         |
/// | diff file counts           | `"files"`         |
///
/// For streaming sites where the total is unknowable, use
/// [`elision_marker_unbounded`] instead (its `unit` follows the same table).
pub(crate) fn elision_marker(shown: usize, total: usize, unit: &str) -> Option<String> {
    if shown >= total {
        return None;
    }
    let omitted = total - shown;
    Some(format!(
        "[skim] {omitted} {unit} omitted ({shown} of {total} shown) — SKIM_PASSTHROUGH=1 for full output"
    ))
}

/// [`elision_marker_unbounded`] with a caller-supplied remedy clause.
///
/// The standard remedy — `SKIM_PASSTHROUGH=1 for full output` — is only useful
/// when passthrough mode is *not* already active.  Inside the escape hatch it is
/// circular advice: the reader has already taken it, and repeating it tells them
/// nothing they can act on.  Loss-bearing markers are unconditional by ADR-011
/// class 1, so the marker must still fire there; this constructor is how such a
/// site keeps the marker while making its remedy actionable.
///
/// `remedy` is an imperative clause without leading punctuation, e.g.
/// `"run 'grep' directly for the full stream"`.
pub(crate) fn elision_marker_unbounded_with_remedy(
    shown_desc: &str,
    unit: &str,
    remedy: &str,
) -> String {
    format!("[skim] {unit} elided beyond {shown_desc} — {remedy}")
}

/// Streaming variant of [`elision_marker`] for sites where the total is
/// unknowable (the input is consumed incrementally and elision happens
/// mid-stream). `shown_desc` describes what WAS kept (e.g. `"first 64 KiB"`).
pub(crate) fn elision_marker_unbounded(shown_desc: &str, unit: &str) -> String {
    elision_marker_unbounded_with_remedy(shown_desc, unit, "SKIM_PASSTHROUGH=1 for full output")
}

/// Canonical compressed-output hint emitted to stderr after a non-zero exit
/// that skim has compressed (expected failure path).
///
/// Used by [`crate::cmd::test::shared::emit_failure_context`] and the notice
/// matrix in [`crate::cmd::execution::record_and_report`] to keep the hint
/// string byte-identical across both paths — single source of truth (#317).
pub(crate) fn compressed_output_hint(code: i32) -> String {
    format!("[skim] compressed output (exit {code}). SKIM_PASSTHROUGH=1 for full output.")
}

// ============================================================================
// Rewrite transparency (hook-rewritten file reads)
// ============================================================================

/// Env-var name injected by the rewrite engine to signal a cat/head/tail rewrite.
///
/// The engine sets this as a leading `KEY=val` prefix token so the executing
/// `skim <file>` process can detect it was launched via a hook rewrite.
/// Validated on read: only `"cat"`, `"head"`, and `"tail"` are accepted.
pub(crate) const REWRITE_ORIGIN_ENV: &str = "SKIM_REWRITTEN_FROM";

/// Return `Some(origin)` when `SKIM_REWRITTEN_FROM` is set to a recognised
/// file-read command name (`cat`, `head`, or `tail`).
///
/// Returns `None` for any other value to prevent stderr-text injection via
/// arbitrary env vars (closed vocabulary guard).
pub(crate) fn rewrite_origin() -> Option<String> {
    let val = std::env::var(REWRITE_ORIGIN_ENV).ok()?;
    match val.as_str() {
        "cat" | "head" | "tail" => Some(val),
        _ => None,
    }
}

/// Map a mode name to a human-readable class description (B4 / ADR-011 class 1).
///
/// The class label names what was elided so the reader knows what information
/// they are missing without needing to know skim internals.
fn mode_class_label(mode_str: &str) -> &'static str {
    match mode_str {
        "pseudo" => "pseudo view: bodies and syntactic detail removed",
        "minimal" => "minimal view: comments and bodies removed",
        "structure" => "structure view: bodies removed",
        "signatures" => "signatures view: bodies removed",
        "types" => "types view: non-type declarations removed",
        _ => "transformed view",
    }
}

/// Lossy-view marker for file reads (B3 / ADR-011 class 1 — unconditional).
///
/// Fires whenever the served view differs from raw bytes, regardless of whether
/// the read was triggered by a hook rewrite (`SKIM_REWRITTEN_FROM`) or an
/// explicit `skim <file>` invocation.  This is an ADR-011 class 1 marker:
/// it is unconditional and NOT gated by `SKIM_DEBUG`.
///
/// Returns `None` when `differing == 0` (view is byte-identical to raw).
///
/// # Marker format
///
/// With hook-rewrite origin (e.g. `SKIM_REWRITTEN_FROM=cat`):
/// ```text
/// [skim] transformed view (cat → skim --mode=pseudo): pseudo view: bodies and syntactic detail removed — SKIM_PASSTHROUGH=1 for raw output
/// ```
///
/// Without origin (explicit `skim file.ts --mode=pseudo`):
/// ```text
/// [skim] pseudo view: bodies and syntactic detail removed — SKIM_PASSTHROUGH=1 for raw output
/// ```
///
/// Multi-file (with or without origin):
/// ```text
/// [skim] transformed view (cat → skim --mode=pseudo): pseudo view: 2/3 files — SKIM_PASSTHROUGH=1 for raw output
/// ```
pub(crate) fn lossy_view_marker(
    origin: Option<&str>,
    mode_str: &str,
    differing: usize,
    total: usize,
) -> Option<String> {
    if differing == 0 {
        return None;
    }
    let class = mode_class_label(mode_str);
    let suffix = " \u{2014} SKIM_PASSTHROUGH=1 for raw output";

    let marker = match (origin, total) {
        // Hook-rewritten, single file
        (Some(orig), n) if n <= 1 => format!(
            "[skim] transformed view ({orig} \u{2192} skim --mode={mode_str}): {class}{suffix}"
        ),
        // Hook-rewritten, multi-file
        (Some(orig), _) => format!(
            "[skim] transformed view ({orig} \u{2192} skim --mode={mode_str}): {class}: {differing}/{total} files{suffix}"
        ),
        // Direct invocation, single file
        (None, n) if n <= 1 => format!("[skim] {class}{suffix}"),
        // Direct invocation, multi-file
        (None, _) => format!("[skim] {class}: {differing}/{total} files{suffix}"),
    };
    Some(marker)
}

#[cfg(test)]
mod lossy_view_marker_tests {
    use super::*;

    #[test]
    fn test_lossy_view_marker_with_origin_single_file() {
        // With origin: "transformed view" header + class description.
        let result = lossy_view_marker(Some("cat"), "pseudo", 1, 1);
        let marker = result.expect("differing=1 must produce a marker");
        assert!(
            marker.contains("[skim] transformed view"),
            "must have header"
        );
        assert!(marker.contains("cat"), "must name origin");
        assert!(marker.contains("pseudo"), "must name mode");
        assert!(marker.contains("bodies"), "B4: must name elided class");
        assert!(
            marker.contains("SKIM_PASSTHROUGH=1"),
            "must carry remedy hint"
        );
    }

    #[test]
    fn test_lossy_view_marker_with_origin_multi_file() {
        let result = lossy_view_marker(Some("cat"), "pseudo", 2, 3);
        let marker = result.expect("differing=2 must produce a marker");
        assert!(marker.contains("transformed view"), "must have header");
        assert!(marker.contains("2/3"), "must have file counts");
        assert!(marker.contains("pseudo"), "must name mode");
        assert!(
            marker.contains("SKIM_PASSTHROUGH=1"),
            "must carry remedy hint"
        );
    }

    #[test]
    fn test_lossy_view_marker_without_origin_direct_invocation() {
        // B3: fires without SKIM_REWRITTEN_FROM — no "transformed view" header.
        let result = lossy_view_marker(None, "pseudo", 1, 1);
        let marker = result.expect("differing=1 must produce a marker");
        // B4: class label is the primary identifier for direct invocations.
        assert!(marker.contains("pseudo"), "must name mode class");
        assert!(marker.contains("bodies"), "B4: must name elided class");
        assert!(
            marker.contains("SKIM_PASSTHROUGH=1"),
            "must carry remedy hint"
        );
    }

    #[test]
    fn test_lossy_view_marker_zero_differing_returns_none() {
        assert_eq!(lossy_view_marker(Some("cat"), "pseudo", 0, 1), None);
        assert_eq!(lossy_view_marker(None, "structure", 0, 5), None);
    }

    #[test]
    fn test_lossy_view_marker_head_structure_b4() {
        let m = lossy_view_marker(Some("head"), "structure", 1, 1)
            .expect("head + differing=1 must produce a marker");
        assert!(m.contains("head"), "marker must name the origin command");
        assert!(m.contains("structure"), "marker must name the mode");
        assert!(
            m.contains("SKIM_PASSTHROUGH=1"),
            "marker must include passthrough hint"
        );
        // B4: structure class is named
        assert!(
            m.contains("bodies removed"),
            "B4: structure marker must name 'bodies removed'"
        );
    }

    #[test]
    fn test_rewrite_origin_env_constant() {
        assert_eq!(REWRITE_ORIGIN_ENV, "SKIM_REWRITTEN_FROM");
    }

    #[test]
    fn test_mode_class_labels_cover_all_known_modes() {
        // Ensure mode_class_label returns meaningful strings for all known modes.
        for mode in &["pseudo", "minimal", "structure", "signatures", "types"] {
            let label = super::mode_class_label(mode);
            assert!(
                !label.is_empty(),
                "class label must be non-empty for mode {mode}"
            );
            assert!(
                label.len() > "transformed view".len(),
                "B4: class label must be more descriptive than 'transformed view' for mode {mode}"
            );
        }
    }
}

// ============================================================================
// FilterTransparencyHeader
// ============================================================================

/// Emits filter transparency headers to stderr for observability.
///
/// Format: `[skim:{filter_name}:{tier}] {input} → {output} ({savings}%)`
pub(crate) struct FilterTransparencyHeader;

impl FilterTransparencyHeader {
    /// Emit a filter transparency header to the given writer (DI for testing).
    pub(crate) fn emit_to(
        writer: &mut impl Write,
        filter_name: &str,
        tier: &str,
        input_tokens: usize,
        output_tokens: usize,
    ) -> io::Result<()> {
        let savings = if input_tokens == 0 || output_tokens >= input_tokens {
            0
        } else {
            // Use u128 to avoid overflow when (input - output) * 100 exceeds usize::MAX.
            ((input_tokens - output_tokens) as u128 * 100 / input_tokens as u128) as usize
        };

        writeln!(
            writer,
            "[skim:{filter_name}:{tier}] {} \u{2192} {} ({savings}%)",
            tokens::format_number(input_tokens),
            tokens::format_number(output_tokens),
        )
    }

    /// Convenience wrapper: emit to stderr, locking once for the whole write.
    pub(crate) fn emit(filter_name: &str, tier: &str, input_tokens: usize, output_tokens: usize) {
        let stderr = io::stderr();
        let mut handle = stderr.lock();
        // Best-effort: ignore write errors to stderr
        let _ = Self::emit_to(&mut handle, filter_name, tier, input_tokens, output_tokens);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // elision_marker tests
    // ========================================================================

    #[test]
    fn test_elision_marker_none_when_nothing_omitted() {
        assert_eq!(elision_marker(5, 5, "rows"), None);
        assert_eq!(elision_marker(6, 5, "rows"), None);
        assert_eq!(elision_marker(0, 0, "rows"), None);
    }

    #[test]
    fn test_elision_marker_exact_counts_and_escape_hatch() {
        let marker = elision_marker(300, 412, "lines").unwrap();
        assert!(
            marker.contains("112 lines omitted"),
            "must state exact omitted count: {marker}"
        );
        assert!(
            marker.contains("300 of 412 shown"),
            "must state shown/total: {marker}"
        );
        assert!(
            marker.contains("SKIM_PASSTHROUGH=1"),
            "must surface the escape hatch: {marker}"
        );
    }

    #[test]
    fn test_elision_marker_unbounded_describes_kept_portion() {
        let marker = elision_marker_unbounded("first 64 KiB", "line content");
        assert!(marker.contains("first 64 KiB"), "{marker}");
        assert!(marker.contains("SKIM_PASSTHROUGH=1"), "{marker}");
    }

    // ========================================================================
    // ParseResult tests
    // ========================================================================

    #[test]
    fn test_full_is_full() {
        let result: ParseResult<String> = ParseResult::Full("hello".to_string());
        assert!(result.is_full());
        assert!(!result.is_degraded());
        assert!(!result.is_passthrough());
    }

    #[test]
    fn test_degraded_is_degraded() {
        let markers = vec!["missing field".to_string(), "bad format".to_string()];
        let result: ParseResult<String> =
            ParseResult::Degraded("partial".to_string(), markers.clone());
        assert!(result.is_degraded());
        assert!(!result.is_full());
        assert!(!result.is_passthrough());
        // Markers are accessible
        if let ParseResult::Degraded(_, m) = &result {
            assert_eq!(m.len(), 2);
            assert_eq!(m[0], "missing field");
            assert_eq!(m[1], "bad format");
        } else {
            panic!("expected Degraded variant");
        }
    }

    #[test]
    fn test_passthrough_is_passthrough() {
        let result: ParseResult<String> = ParseResult::Passthrough("raw output".to_string());
        assert!(result.is_passthrough());
        assert!(!result.is_full());
        assert!(!result.is_degraded());
    }

    #[test]
    fn test_raw_passthrough_is_passthrough() {
        // RawPassthrough is the payload-less passthrough variant; is_passthrough()
        // must return true so execution.rs routes it through the same passthrough
        // path as Passthrough(String).
        let result: ParseResult<String> = ParseResult::RawPassthrough;
        assert!(
            result.is_passthrough(),
            "RawPassthrough must be recognised as passthrough tier"
        );
        assert!(!result.is_full());
        assert!(!result.is_degraded());
    }

    #[test]
    fn test_raw_passthrough_tier_name() {
        let result: ParseResult<String> = ParseResult::RawPassthrough;
        assert_eq!(
            result.tier_name(),
            "passthrough",
            "RawPassthrough tier_name must be \"passthrough\""
        );
    }

    #[test]
    fn test_raw_passthrough_content_is_empty() {
        // content() returns "" for RawPassthrough — the actual bytes live in
        // CommandOutput::stdout, not in the parse result.
        let result: ParseResult<String> = ParseResult::RawPassthrough;
        assert_eq!(
            result.content(),
            "",
            "RawPassthrough content() must be empty; bytes come from CommandOutput::stdout"
        );
    }

    #[test]
    fn test_tier_names() {
        let full: ParseResult<String> = ParseResult::Full("a".to_string());
        let degraded: ParseResult<String> =
            ParseResult::Degraded("b".to_string(), vec!["w".to_string()]);
        let passthrough: ParseResult<String> = ParseResult::Passthrough("c".to_string());
        let raw_passthrough: ParseResult<String> = ParseResult::RawPassthrough;

        assert_eq!(full.tier_name(), "full");
        assert_eq!(degraded.tier_name(), "degraded");
        assert_eq!(passthrough.tier_name(), "passthrough");
        assert_eq!(raw_passthrough.tier_name(), "passthrough");
    }

    #[test]
    fn test_content_accessor() {
        let full: ParseResult<String> = ParseResult::Full("full content".to_string());
        let degraded: ParseResult<String> =
            ParseResult::Degraded("degraded content".to_string(), vec![]);
        let passthrough: ParseResult<String> =
            ParseResult::Passthrough("passthrough content".to_string());
        let raw_passthrough: ParseResult<String> = ParseResult::RawPassthrough;

        assert_eq!(full.content(), "full content");
        assert_eq!(degraded.content(), "degraded content");
        assert_eq!(passthrough.content(), "passthrough content");
        // RawPassthrough carries no payload; content() returns "" so callers
        // know to read from CommandOutput::stdout directly.
        assert_eq!(raw_passthrough.content(), "");
    }

    #[test]
    fn test_into_content() {
        let full: ParseResult<String> = ParseResult::Full("full".to_string());
        assert_eq!(full.into_content(), "full");

        let degraded: ParseResult<String> =
            ParseResult::Degraded("degraded".to_string(), vec!["m".to_string()]);
        assert_eq!(degraded.into_content(), "degraded");

        let passthrough: ParseResult<String> = ParseResult::Passthrough("passthrough".to_string());
        assert_eq!(passthrough.into_content(), "passthrough");
    }

    #[test]
    fn test_emit_markers_full_is_noop() {
        let result: ParseResult<String> = ParseResult::Full("ok".to_string());
        let mut buf = Vec::new();
        result.emit_markers(&mut buf).unwrap();
        assert!(buf.is_empty(), "Full should write nothing");
    }

    #[test]
    fn test_emit_markers_degraded_writes_warnings() {
        let _guard = crate::debug::DebugTestGuard::acquire();
        crate::debug::force_enable_debug();
        let markers = vec!["issue one".to_string(), "issue two".to_string()];
        let result: ParseResult<String> = ParseResult::Degraded("content".to_string(), markers);
        let mut buf = Vec::new();
        result.emit_markers(&mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("[skim:warning] issue one"),
            "expected warning for first marker, got: {output}"
        );
        assert!(
            output.contains("[skim:warning] issue two"),
            "expected warning for second marker, got: {output}"
        );
    }

    #[test]
    fn test_emit_markers_passthrough_writes_notice() {
        let _guard = crate::debug::DebugTestGuard::acquire();
        crate::debug::force_enable_debug();
        let result: ParseResult<String> = ParseResult::Passthrough("raw".to_string());
        let mut buf = Vec::new();
        result.emit_markers(&mut buf).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("[skim:notice]"),
            "expected notice in output, got: {output}"
        );
    }

    #[test]
    fn test_emit_markers_degraded_silent_without_debug() {
        let _guard = crate::debug::DebugTestGuard::acquire();
        let markers = vec!["issue one".to_string(), "issue two".to_string()];
        let result: ParseResult<String> = ParseResult::Degraded("content".to_string(), markers);
        let mut buf = Vec::new();
        result.emit_markers(&mut buf).unwrap();
        assert!(
            buf.is_empty(),
            "Degraded should write nothing when debug is disabled"
        );
    }

    #[test]
    fn test_emit_markers_passthrough_silent_without_debug() {
        let _guard = crate::debug::DebugTestGuard::acquire();
        let result: ParseResult<String> = ParseResult::Passthrough("raw".to_string());
        let mut buf = Vec::new();
        result.emit_markers(&mut buf).unwrap();
        assert!(
            buf.is_empty(),
            "Passthrough should write nothing when debug is disabled"
        );
    }

    // ========================================================================
    // PassthroughCleaner tests
    // ========================================================================

    #[test]
    fn test_strip_ansi_cow_borrows_when_clean() {
        let input = "no ansi codes here";
        let result = strip_ansi_cow(input);
        assert!(
            matches!(result, std::borrow::Cow::Borrowed(_)),
            "expected Borrowed for input without ESC bytes"
        );
        assert_eq!(result, "no ansi codes here");
    }

    #[test]
    fn test_strip_ansi_cow_allocates_when_ansi_present() {
        let input = "\x1b[31mred\x1b[0m";
        let result = strip_ansi_cow(input);
        assert!(
            matches!(result, std::borrow::Cow::Owned(_)),
            "expected Owned for input with ESC bytes"
        );
        assert_eq!(result, "red");
    }

    #[test]
    fn test_strip_ansi_cow_empty_input() {
        let input = "";
        let result = strip_ansi_cow(input);
        assert!(
            matches!(result, std::borrow::Cow::Borrowed(_)),
            "expected Borrowed for empty input"
        );
        assert_eq!(result, "");
    }

    /// Issue #465 regression: `strip_ansi_cow` must preserve TABs when ESC bytes
    /// are present.  Before the fix, `strip_ansi_escapes::strip_str` (vte-based)
    /// discarded ALL C0 control bytes — including TABs — whenever a single ESC byte
    /// appeared anywhere in the buffer, even on completely different lines.
    #[test]
    fn test_strip_ansi_cow_preserves_tabs_when_esc_present() {
        // Three lines: line 1 has a TAB only, line 2 has both CSI sequences and a
        // TAB, line 3 has a TAB only.  One ESC byte was enough to destroy all three
        // tabs under the old implementation.
        let input = "a\tb\n\x1b[32mc\x1b[0m\td\n e\tf\n";
        let result = strip_ansi_cow(input);
        assert!(
            matches!(result, std::borrow::Cow::Owned(_)),
            "ESC byte present — must allocate (Cow::Owned)"
        );
        assert_eq!(
            &*result, "a\tb\nc\td\n e\tf\n",
            "CSI sequences stripped; all 3 TABs preserved (#465)"
        );
        assert_eq!(
            result.matches('\t').count(),
            3,
            "exactly 3 TABs must survive"
        );
        assert!(
            !result.contains('\x1b'),
            "no ESC bytes must remain after stripping"
        );
    }

    /// Negative / pinning test for #465: C0 controls other than ESC-sequence bytes
    /// themselves (BEL, BS, VT, FF, CR) must survive the strip.
    ///
    /// `strip_ansi_escapes::strip_str` would have destroyed every one of these (it
    /// emits only printables + `\n`); `strip_escape_sequences` must not.
    #[test]
    fn test_strip_ansi_cow_preserves_other_c0_controls_when_esc_present() {
        // A CSI sequence at the start + every problematic C0 control after it.
        let input = "\x1b[31mstart\x1b[0m\x07\x08\x0b\x0c\x0d";
        let result = strip_ansi_cow(input);
        assert!(
            matches!(result, std::borrow::Cow::Owned(_)),
            "ESC byte present — Cow::Owned"
        );
        assert_eq!(
            &*result, "start\x07\x08\x0b\x0c\x0d",
            "BEL(\\x07), BS(\\x08), VT(\\x0b), FF(\\x0c), CR(\\x0d) must survive — \
             the old strip_ansi_escapes::strip_str destroyed them (issue #465)"
        );
    }

    /// OSC termination variants and truncated-sequence robustness.
    #[test]
    fn test_strip_ansi_cow_osc_and_truncated_sequences() {
        // BEL-terminated OSC (e.g. terminal title set).
        let result = strip_ansi_cow("before\x1b]0;title\x07after");
        assert_eq!(
            &*result, "beforeafter",
            "OSC terminated by BEL must be fully stripped"
        );

        // ST-terminated OSC (ESC \).
        let result = strip_ansi_cow("before\x1b]0;title\x1b\\after");
        assert_eq!(
            &*result, "beforeafter",
            "OSC terminated by ST (ESC \\) must be fully stripped"
        );

        // Unterminated CSI: input ends before final byte — must emit literally,
        // not silently drop content (#317 / Group 1b).
        let result = strip_ansi_cow("ok\x1b[32");
        assert_eq!(
            &*result, "ok\x1b[32",
            "unterminated CSI: consumed bytes must be emitted literally, not dropped"
        );

        // Unterminated OSC: input ends without BEL or ST — must emit literally,
        // not silently drop content (#317 / Group 1a).
        let result = strip_ansi_cow("ok\x1b]0;title");
        assert_eq!(
            &*result, "ok\x1b]0;title",
            "unterminated OSC: consumed bytes must be emitted literally, not dropped"
        );

        // Content after a complete sequence (including a TAB) must be preserved.
        let result = strip_ansi_cow("\x1b[32mcolored\x1b[0m extra\ttext");
        assert_eq!(
            &*result, "colored extra\ttext",
            "content after sequence including TAB must be preserved"
        );
    }

    /// Group 1a: unterminated OSC with a non-empty body — the body MUST survive.
    ///
    /// Before the fix: the OSC body was silently consumed, violating #317.
    /// After the fix: the consumed bytes are emitted literally.
    #[test]
    fn test_strip_escape_sequences_unterminated_osc_emits_literally() {
        // OSC with a long body and no BEL/ST terminator.
        let input = "preamble\x1b]osc-body-must-survive";
        let result = strip_escape_sequences(input);
        assert_eq!(
            result, "preamble\x1b]osc-body-must-survive",
            "unterminated OSC body must not be silently discarded (#317 / Group 1a)"
        );
    }

    /// Group 1b: unterminated CSI with a non-empty body — the body MUST survive.
    ///
    /// Before the fix: the CSI body was silently consumed, violating #317.
    /// After the fix: the consumed bytes are emitted literally.
    #[test]
    fn test_strip_escape_sequences_unterminated_csi_emits_literally() {
        // CSI with parameter bytes (digits, all in 0x30..=0x3F) and no final byte.
        let input = "preamble\x1b[9999;9999";
        let result = strip_escape_sequences(input);
        assert_eq!(
            result, "preamble\x1b[9999;9999",
            "unterminated CSI body must not be silently discarded (#317 / Group 1b)"
        );
    }

    /// Group 1c: bare ESC immediately before `\n` must NOT consume the newline.
    ///
    /// Before the fix: the `_ => {}` arm consumed the next byte unconditionally,
    /// so ESC before `\n` caused two lines to merge.
    /// After the fix: control characters are not consumed by the bare-ESC arm.
    #[test]
    fn test_strip_escape_sequences_bare_esc_before_newline_preserves_newline() {
        let input = "line1\x1b\nline2";
        let result = strip_escape_sequences(input);
        // `\n` must survive; the bare ESC is emitted literally (safe direction).
        assert!(
            result.contains('\n'),
            "newline after bare ESC must survive — two lines must not merge (Group 1c); \
             got: {result:?}"
        );
        assert_eq!(
            result, "line1\x1b\nline2",
            "bare ESC emitted literally and newline preserved (Group 1c)"
        );
    }

    /// #317: a malformed CSI must not scan across a newline and swallow it.
    ///
    /// `ESC [ 3 2` with no final byte before the line break used to keep scanning
    /// until it hit the first byte in `0x40..=0x7e` — the `H` of the *next* line —
    /// consuming the `\n` and the `H` with it.  Two lines merged and two content
    /// bytes vanished.  The body-validity bail stops the scan at the newline and
    /// emits the consumed bytes literally.
    #[test]
    fn test_strip_escape_sequences_malformed_csi_does_not_swallow_newline() {
        let input = "line1 \x1b[32\nHello world\tkeep\n";
        let result = strip_escape_sequences(input);
        assert_eq!(
            result, input,
            "a malformed CSI is not a real sequence — every byte must survive (#317)"
        );
        assert_eq!(
            result.matches('\n').count(),
            2,
            "both newlines must survive — lines must not merge"
        );
    }

    /// #317: an unterminated OSC must not scan across a newline to a later BEL.
    ///
    /// Without the C0 bail, `ESC ]` would consume everything up to the next stray
    /// BEL anywhere downstream, silently deleting whole lines of real content.
    #[test]
    fn test_strip_escape_sequences_malformed_osc_does_not_swallow_lines() {
        let input = "one\x1b]notice\ntwo\tthree\x07four\n";
        let result = strip_escape_sequences(input);
        assert_eq!(
            result, input,
            "an OSC body cannot contain a newline — bail and emit literally (#317)"
        );
    }

    /// Byte-conservation property: output length equals input length minus the
    /// bytes of genuinely well-formed escape sequences, and never less.
    #[test]
    fn test_strip_escape_sequences_never_loses_content_bytes() {
        // (input, expected) — expected is input minus only well-formed sequences.
        let cases: &[(&str, &str)] = &[
            ("\x1b[1;31mred\x1b[0m", "red"),
            ("\x1b]0;title\x07body", "body"),
            ("\x1b]0;title\x1b\\body", "body"),
            // Malformed / non-sequence ESC usage — nothing may be dropped.
            ("a\x1b[32\nb", "a\x1b[32\nb"),
            ("a\x1b[9999;9999", "a\x1b[9999;9999"),
            ("a\x1b]unterminated", "a\x1b]unterminated"),
            ("a\x1b\nb", "a\x1b\nb"),
            ("a\x1b[\tb", "a\x1b[\tb"),
            ("a\x1b]x\ny\x07z", "a\x1b]x\ny\x07z"),
        ];
        for (input, expected) in cases {
            let result = strip_escape_sequences(input);
            assert_eq!(
                &result, expected,
                "byte conservation violated for input {input:?}"
            );
        }
    }

    /// Group 2 regression: `strip_ansi` must preserve TABs when an ESC is present.
    ///
    /// The old `strip_ansi_escapes::strip_str` destroyed ALL C0 controls including
    /// TABs when any ESC byte appeared anywhere in the buffer (issue #465 / PF-006).
    #[test]
    fn test_strip_ansi_preserves_tabs_when_esc_present() {
        // ESC sequence on line 2; TABs on all three lines.
        let input = "a\tb\n\x1b[32mc\x1b[0m\td\ne\tf\n";
        let result = strip_ansi(input);
        assert_eq!(
            result, "a\tb\nc\td\ne\tf\n",
            "CSI sequences stripped; all TABs must survive — \
             old strip_ansi_escapes::strip_str destroyed them (#465 / Group 2)"
        );
        assert!(!result.contains('\x1b'), "no ESC bytes must remain");
    }

    #[test]
    fn test_strip_ansi_removes_color_codes() {
        let input = "\x1b[31mred\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "red");
    }

    #[test]
    fn test_strip_ansi_preserves_plain_text() {
        let input = "plain text with no escapes";
        let result = strip_ansi(input);
        assert_eq!(result, "plain text with no escapes");
    }

    #[test]
    fn test_strip_ansi_handles_only_escape_codes() {
        // Input that is entirely ANSI escape sequences
        let input = "\x1b[31m\x1b[1m\x1b[0m";
        let result = strip_ansi(input);
        assert_eq!(result, "");
    }

    // ========================================================================
    // PassthroughTruncator tests
    // ========================================================================

    #[test]
    fn test_truncate_within_budget() {
        // Short content should be returned unchanged
        let content = "hello world";
        let budget = 1000;
        let result = PassthroughTruncator::truncate_to_budget(content, budget).unwrap();
        assert_eq!(result, content);
    }

    #[test]
    fn test_truncate_over_budget_reduces() {
        // Create content that exceeds budget
        let lines: Vec<String> = (0..100)
            .map(|i| format!("This is line number {i} with some extra content to use tokens"))
            .collect();
        let content = lines.join("\n");

        let budget = 20;
        let result = PassthroughTruncator::truncate_to_budget(&content, budget).unwrap();

        // Output tokens should be within budget (approximately — the truncation marker
        // adds a few tokens, but the kept lines should be <= budget)
        let output_tokens = tokens::count_tokens(&result).unwrap();
        // Allow some margin for the truncation marker itself
        assert!(
            output_tokens < budget + 20,
            "output tokens ({output_tokens}) should be close to budget ({budget})"
        );
        assert!(
            result.len() < content.len(),
            "truncated output should be shorter"
        );
    }

    #[test]
    fn test_truncate_appends_marker() {
        let lines: Vec<String> = (0..50)
            .map(|i| format!("Line {i} with enough content to generate several tokens"))
            .collect();
        let content = lines.join("\n");

        let budget = 10;
        let result = PassthroughTruncator::truncate_to_budget(&content, budget).unwrap();

        assert!(
            result.contains("[... truncated"),
            "expected truncation marker in output, got: {result}"
        );
    }

    #[test]
    fn test_truncate_empty_input() {
        let result = PassthroughTruncator::truncate_to_budget("", 100).unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn test_truncate_single_long_line() {
        // A single very long line with no \n
        let long_line = "word ".repeat(500);
        let budget = 10;
        let result = PassthroughTruncator::truncate_to_budget(&long_line, budget).unwrap();

        assert!(
            result.contains("[... truncated"),
            "expected truncation marker for single long line"
        );
        assert!(
            result.len() < long_line.len(),
            "truncated output should be shorter than input"
        );
    }

    // ========================================================================
    // FilterTransparencyHeader tests
    // ========================================================================

    #[test]
    fn test_header_format() {
        let mut buf = Vec::new();
        FilterTransparencyHeader::emit_to(&mut buf, "test-filter", "full", 1000, 200).unwrap();
        let output = String::from_utf8(buf).unwrap();

        assert!(
            output.contains("[skim:test-filter:full]"),
            "expected header prefix, got: {output}"
        );
        assert!(
            output.contains("\u{2192}"),
            "expected Unicode arrow, got: {output}"
        );
        assert!(
            output.contains("80%"),
            "expected 80% savings, got: {output}"
        );
    }

    #[test]
    fn test_header_zero_input_tokens() {
        let mut buf = Vec::new();
        FilterTransparencyHeader::emit_to(&mut buf, "filter", "passthrough", 0, 0).unwrap();
        let output = String::from_utf8(buf).unwrap();

        assert!(
            output.contains("0%"),
            "expected 0% savings for zero input, got: {output}"
        );
        // Should not panic from division by zero
    }

    #[test]
    fn test_header_output_exceeds_input_no_underflow() {
        // When output_tokens > input_tokens, savings should be 0% (not panic from usize underflow)
        let mut buf = Vec::new();
        FilterTransparencyHeader::emit_to(&mut buf, "filter", "degraded", 100, 200).unwrap();
        let output = String::from_utf8(buf).unwrap();

        assert!(
            output.contains("0%"),
            "expected 0% savings when output exceeds input, got: {output}"
        );
    }

    #[test]
    fn test_header_uses_format_number() {
        let mut buf = Vec::new();
        FilterTransparencyHeader::emit_to(&mut buf, "f", "full", 1_500_000, 500_000).unwrap();
        let output = String::from_utf8(buf).unwrap();

        assert!(
            output.contains("1,500,000"),
            "expected thousands-separated input tokens, got: {output}"
        );
        assert!(
            output.contains("500,000"),
            "expected thousands-separated output tokens, got: {output}"
        );
    }

    // ========================================================================
    // OutputMode tests
    // ========================================================================

    #[test]
    fn test_from_exit_code_zero_is_compact() {
        assert_eq!(OutputMode::from_exit_code(0), OutputMode::Compact);
    }

    #[test]
    fn test_from_exit_code_nonzero_is_verbose() {
        assert_eq!(OutputMode::from_exit_code(1), OutputMode::Verbose);
    }

    #[test]
    fn test_from_exit_code_negative_is_verbose() {
        assert_eq!(OutputMode::from_exit_code(-1), OutputMode::Verbose);
    }

    #[test]
    fn test_from_optional_none_is_verbose() {
        assert_eq!(
            OutputMode::from_optional_exit_code(None),
            OutputMode::Verbose
        );
    }

    #[test]
    fn test_from_optional_some_zero_is_compact() {
        assert_eq!(
            OutputMode::from_optional_exit_code(Some(0)),
            OutputMode::Compact
        );
    }

    #[test]
    fn test_from_optional_some_nonzero_is_verbose() {
        assert_eq!(
            OutputMode::from_optional_exit_code(Some(1)),
            OutputMode::Verbose
        );
    }

    #[test]
    fn test_truncate_zero_budget_non_empty() {
        let content = "line one\nline two\nline three";
        let result = PassthroughTruncator::truncate_to_budget(content, 0).unwrap();
        assert!(
            result.contains("[... truncated 3 lines]"),
            "expected truncation marker for 3 lines, got: {result}"
        );
        assert!(
            !result.contains("line one"),
            "expected no original content in zero-budget output, got: {result}"
        );
    }

    #[test]
    fn test_clean_unicode_with_ansi() {
        let input = "\x1b[31m🦀 hello\x1b[0m";
        assert_eq!(strip_ansi(input), "🦀 hello");
    }

    #[test]
    fn test_clean_8bit_color_codes() {
        // 256-color ANSI: ESC[38;5;196m (foreground color 196 = bright red)
        let input = "\x1b[38;5;196mred\x1b[0m";
        assert_eq!(strip_ansi(input), "red");
    }

    // ========================================================================
    // Adversarial PassthroughTruncator tests
    // ========================================================================

    #[test]
    fn test_truncate_budget_one_token() {
        // With budget of 1, almost all multi-token content should be truncated
        let content = "This is a fairly long line with many words\nAnother line here\nAnd a third";
        let result = PassthroughTruncator::truncate_to_budget(content, 1).unwrap();
        assert!(
            result.contains("[... truncated"),
            "expected truncation marker with budget=1, got: {result}"
        );
        assert!(
            result.len() < content.len(),
            "expected shorter output with budget=1"
        );
    }

    #[test]
    fn test_truncate_unicode_char_boundaries() {
        // Many multi-byte characters — binary search must not split a char
        let content = "🦀".repeat(200);
        let budget = 5;
        let result = PassthroughTruncator::truncate_to_budget(&content, budget).unwrap();
        // Must be valid UTF-8 (would panic on construction if not)
        assert!(
            result.contains("[... truncated"),
            "expected truncation marker for long unicode content"
        );
    }

    #[test]
    fn test_truncate_only_blank_lines() {
        // Input is only newlines — should not panic
        let content = "\n\n\n\n\n";
        let result = PassthroughTruncator::truncate_to_budget(content, 2).unwrap();
        // Either fits (blank lines are cheap tokens) or truncates gracefully
        assert!(!result.is_empty() || content.is_empty());
    }

    #[test]
    fn test_truncate_content_exactly_at_budget() {
        // Content that fits exactly should be returned unchanged
        let content = "hello";
        let tokens = tokens::count_tokens(content).unwrap();
        let result = PassthroughTruncator::truncate_to_budget(content, tokens).unwrap();
        assert_eq!(
            result, content,
            "content at exact budget should be unchanged"
        );
    }

    #[test]
    fn test_truncate_mixed_newline_styles() {
        // Content with \r\n — truncator splits on \n, leaving \r at end of lines
        let content = "line1\r\nline2\r\nline3\r\nline4\r\nline5\r\nline6\r\nline7\r\nline8\r\nline9\r\nline10";
        let result = PassthroughTruncator::truncate_to_budget(content, 5).unwrap();
        // Should not panic and should produce valid output
        assert!(!result.is_empty());
    }

    // ========================================================================
    // Adversarial FilterTransparencyHeader tests
    // ========================================================================

    #[test]
    fn test_header_large_token_no_overflow() {
        // usize::MAX / 2 * 100 would overflow usize — verify no panic
        let mut buf = Vec::new();
        FilterTransparencyHeader::emit_to(&mut buf, "filter", "full", usize::MAX / 2, 0).unwrap();
        let output = String::from_utf8(buf).unwrap();

        // Should produce ~100% savings without panic
        assert!(
            output.contains("100%") || output.contains("99%"),
            "expected ~100% savings for huge input with zero output, got: {output}"
        );
    }

    // ========================================================================
    // to_json_envelope tests
    // ========================================================================

    /// Minimal type for testing to_json_envelope without coupling to canonical types.
    #[derive(Debug, Clone, serde::Serialize)]
    struct StubResult {
        name: String,
        count: usize,
    }

    #[test]
    fn test_to_json_envelope_full_no_wrapper() {
        let inner = StubResult {
            name: "test".to_string(),
            count: 42,
        };
        let result: ParseResult<StubResult> = ParseResult::Full(inner);
        let json_str = result.to_json_envelope().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        // Full: direct serialization, no envelope
        assert_eq!(parsed["name"], "test");
        assert_eq!(parsed["count"], 42);
        assert!(parsed.get("tier").is_none(), "Full should have no tier key");
    }

    #[test]
    fn test_to_json_envelope_degraded_has_envelope() {
        let inner = StubResult {
            name: "partial".to_string(),
            count: 7,
        };
        let warnings = vec!["regex fallback".to_string(), "missing field".to_string()];
        let result: ParseResult<StubResult> = ParseResult::Degraded(inner, warnings);
        let json_str = result.to_json_envelope().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed["tier"], "degraded");
        assert_eq!(parsed["warnings"][0], "regex fallback");
        assert_eq!(parsed["warnings"][1], "missing field");
        assert_eq!(parsed["result"]["name"], "partial");
        assert_eq!(parsed["result"]["count"], 7);
    }

    #[test]
    fn test_to_json_envelope_passthrough_has_envelope() {
        let result: ParseResult<StubResult> =
            ParseResult::Passthrough("raw output here".to_string());
        let json_str = result.to_json_envelope().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed["tier"], "passthrough");
        assert_eq!(parsed["raw"], "raw output here");
        assert!(
            parsed.get("result").is_none(),
            "Passthrough should have no result key"
        );
    }

    /// Pins the exact field set of the passthrough JSON envelope so that
    /// `Passthrough(String)` (via `to_json_envelope`) and `RawPassthrough`
    /// (inline `serde_json::json!` in `execution.rs`) cannot drift apart.
    ///
    /// Both variants must produce exactly two fields — `tier` and `raw` — with
    /// no legacy `tool` field or any other additions.  Eight wrappers rely on
    /// this equivalence: `grep`, `rg` (Passthrough(String)) and `find`, `wc`,
    /// `df`, `du`, `ps`, `ls` (RawPassthrough).  The `RawPassthrough` half is
    /// covered by the `test_subcommand_file_json_passthrough_envelope`
    /// integration test in `cli_subcommand.rs`.
    #[test]
    fn test_to_json_envelope_passthrough_exact_fields() {
        let raw = "./src/main.rs\n./src/lib.rs\n";
        let result: ParseResult<String> = ParseResult::Passthrough(raw.to_string());
        let json_str = result.to_json_envelope().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_str).unwrap();
        let obj = parsed.as_object().unwrap();
        // Exactly two fields: tier and raw.
        assert_eq!(
            obj.len(),
            2,
            "passthrough envelope must have exactly 2 fields (tier, raw), got: {obj:?}"
        );
        assert_eq!(
            parsed["tier"], "passthrough",
            "tier field must be \"passthrough\""
        );
        assert_eq!(
            parsed["raw"].as_str(),
            Some(raw),
            "raw field must carry the payload verbatim"
        );
        // No 'tool' field — this was the pre-ADR-009 Full(FileResult) shape.
        assert!(
            parsed.get("tool").is_none(),
            "passthrough envelope must not contain 'tool' field"
        );
    }
}
