//! Token-budget cascade logic.
//!
//! Progressively applies more aggressive transformation modes until the output
//! fits within a caller-specified token budget, with a final line-truncation
//! fallback.

use std::io::Write as _;

use rskim_core::{Language, Mode, TransformConfig, truncate_to_token_budget};

use crate::output::ELISION_HINT;
use crate::tokens;

/// Groups the three optional truncation parameters that frequently travel
/// together through cascade and cache functions.  Prevents accidental
/// transposition of same-typed `Option<usize>` positional parameters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct TruncationOptions {
    /// Maximum output lines (AST-aware truncation).
    pub(crate) max_lines: Option<usize>,
    /// Last N lines truncation (mutually exclusive with `max_lines`).
    pub(crate) last_lines: Option<usize>,
    /// Token budget for cascade mode.
    pub(crate) token_budget: Option<usize>,
}

/// Error message when no transformation mode produces output.
const NO_OUTPUT_MSG: &str = "Token budget cascade: no transformation mode produced output. \
    Ensure the file is in a supported language or specify --language.";

/// Returns `true` when `output` carries a compact elision marker (the bare
/// `… (N lines truncated)` form, including #511 literal-aware variants) *without*
/// the remedy `hint` appended to it.
///
/// When the token budget is too tight to include the hint inline on stdout,
/// `truncate_to_token_budget` drops it and emits only the line count.  The
/// caller must then emit the hint on stderr (ADR-016 channel split;
/// ADR-011 class 1 — unconditional).
///
/// | scenario                        | return  |
/// |--------------------------------|---------|
/// | bare marker — hint absent      | `true`  |
/// | full marker — hint appended    | `false` |
/// | no marker at all               | `false` |
/// | empty string                   | `false` |
///
/// ## Scoping to the last line
///
/// The scan is intentionally restricted to the **last line** of `output` for
/// two independent reasons:
///
/// 1. **Marker vocabulary coverage:** #511 extended the marker to literal-aware
///    forms (`… (N lines truncated; cut inside a string literal)`, `…; cut inside
///    a code fence)`).  These contain `"truncated;"` rather than `"truncated)"`,
///    so the previous whole-buffer regex missed them entirely and the stderr remedy
///    was never emitted.  A scan for `"lines truncated"` (without the closing
///    character) covers all present and future closing characters at once.
///
/// 2. **False-negative prevention:** scanning the whole buffer for the `hint`
///    string matches files that contain the literal text `SKIM_PASSTHROUGH=1 for
///    full output` (e.g. `CLAUDE.md`, `process.rs`, `truncate.rs`) and falsely
///    suppresses the stderr remedy even when the on-stdout marker is genuinely
///    compact.  The marker is always the last line of a truncated output, so
///    scoping to the last line eliminates this false negative.
///
/// **Contract:** do not change the marker wording without updating this function —
/// the scan is deliberately specific to catch vocabulary drift early.
pub(crate) fn compact_marker_without_hint(output: &str, hint: &str) -> bool {
    // Scope to the last line only (see doc above for the two-reason rationale).
    let last_line = output.lines().last().unwrap_or("");
    // "lines truncated" matches the plural standard form ("lines truncated)") and
    // the #511 literal-aware form ("lines truncated; cut inside …").
    // "line truncated" (no 's') matches the singular form.
    let has_marker =
        last_line.contains("lines truncated") || last_line.contains("line truncated");
    let has_hint = !hint.is_empty() && last_line.contains(hint);
    has_marker && !has_hint
}

/// Build a `TransformConfig` from mode, truncation options, and line number flag.
///
/// The `line_numbers` parameter controls whether the config requests a source line
/// map from `transform_with_line_map`. When building configs for token-budget cascade
/// mode selection, `line_numbers` should be `false` (line numbers are applied after
/// mode selection is complete).
pub(crate) fn build_config(mode: Mode, trunc: &TruncationOptions) -> TransformConfig {
    build_config_with_opts(mode, trunc, false)
}

/// Build a `TransformConfig` with explicit line_numbers flag.
///
/// B5 / ADR-011: always wires `elision_hint` so that every truncation/elision
/// marker produced by rskim-core carries the `SKIM_PASSTHROUGH=1 for full
/// output` remedy clause when the CLI calls this builder.
pub(crate) fn build_config_with_opts(
    mode: Mode,
    trunc: &TruncationOptions,
    line_numbers: bool,
) -> TransformConfig {
    let mut config = TransformConfig::with_mode(mode)
        .with_line_numbers(line_numbers)
        .with_elision_hint(ELISION_HINT);
    if let Some(n) = trunc.max_lines {
        config = config.with_max_lines(n);
    }
    if let Some(n) = trunc.last_lines {
        config = config.with_last_lines(n);
    }
    config
}

/// Count tokens, returning `usize::MAX` on failure (treats errors as over-budget).
fn count_tokens_or_max(text: &str) -> usize {
    tokens::count_tokens(text).unwrap_or_else(|e| {
        eprintln!("[skim] warning: token counting failed, treating as over-budget: {e}");
        usize::MAX
    })
}

/// Apply line-based truncation as a final fallback when all modes exceed the budget.
///
/// Emits a diagnostic to stderr and delegates to `truncate_to_token_budget`.
///
/// `source_line_count` is the number of lines in the **original source file**.
/// It is forwarded to `truncate_to_token_budget` so the elision marker reports
/// how many source lines were omitted (ADR-017 / reliability-8), rather than
/// an output-space count that includes synthetic marker lines from a prior pass.
fn fallback_line_truncate(
    output: &str,
    language: Language,
    token_budget: usize,
    mode: Mode,
    known_token_count: Option<usize>,
    source_line_count: usize,
) -> anyhow::Result<(String, Mode)> {
    eprintln!(
        "[skim] token budget: all modes exceeded budget, applying line truncation ({} mode)",
        mode.name(),
    );
    // B5: pass the CLI-level remedy hint so the token-budget truncation marker
    // carries the SKIM_PASSTHROUGH=1 remedy clause (ADR-011 class 1).
    // reliability-8: pass source_line_count so the elision count is in source
    // space, not output space.
    let truncated = truncate_to_token_budget(
        output,
        language,
        token_budget,
        count_tokens_or_max,
        known_token_count,
        Some(ELISION_HINT),
        Some(source_line_count),
    )?;
    // ADR-016 / ADR-011 class 1: when the budget is too tight to include the
    // remedy hint inline on stdout (compact marker form), emit it on stderr so
    // the reader always sees SKIM_PASSTHROUGH=1 regardless of how tight the
    // budget is.  Unconditional — not gated by SKIM_DEBUG.
    //
    // regression-6: use writeln! on the locked handle instead of eprintln! to
    // avoid panicking when fd 2 is broken (e.g. `2>&1 | head`); discard the
    // error — if stderr is closed there is nobody to disclose to.
    if compact_marker_without_hint(&truncated, ELISION_HINT) {
        let _ = writeln!(
            std::io::stderr().lock(),
            "[skim] output truncated to the --tokens budget — {ELISION_HINT}"
        );
    }
    Ok((truncated, mode))
}

/// Cascade through transformation modes until output fits within `token_budget`.
///
/// Tries each mode from `starting_mode` through increasingly aggressive modes.
/// If no mode fits, applies line-based truncation as a final fallback.
/// Diagnostics are emitted to stderr only when escalating beyond the starting mode.
///
/// `source_line_count` is the number of lines in the original source file.
/// It is threaded to the final `fallback_line_truncate` call so the elision
/// marker reports a source-space count rather than an output-space count
/// (ADR-017 / reliability-8).
pub(crate) fn cascade_for_token_budget<F>(
    starting_mode: Mode,
    trunc: &TruncationOptions,
    token_budget: usize,
    language: Language,
    source_line_count: usize,
    transform_fn: F,
) -> anyhow::Result<(String, Mode)>
where
    F: Fn(&TransformConfig) -> anyhow::Result<Option<String>>,
{
    // Serde-based languages produce at most 2 distinct outputs regardless of mode:
    // - Full/Minimal: original source (passthrough)
    // - Structure/Signatures/Types: structure-extracted (all identical)
    // Short-circuit to avoid up to 3 redundant parse+transform cycles.
    if language.is_serde_based() {
        return cascade_serde(
            starting_mode,
            trunc,
            token_budget,
            language,
            source_line_count,
            &transform_fn,
        );
    }

    let cascade = starting_mode.cascade_from_here();
    let mut last_output: Option<String> = None;
    let mut last_mode = starting_mode;
    let mut last_token_count: Option<usize> = None;
    // Set to true when at least one mode returned Ok(Some("")) — an empty string is
    // distinct from Ok(None): it means the transform ran and produced no structural
    // content (e.g. a comment-only file, or an empty source file).
    let mut saw_empty_output = false;

    for &mode in cascade {
        let config = build_config(mode, trunc);

        let Some(output) = transform_fn(&config)? else {
            continue;
        };

        // Treat empty output the same as Ok(None): the mode produced no usable content.
        // A 0-token empty string would satisfy any budget ceiling and silently suppress
        // the fallback truncation path — that violates #317 (compress-never-truncate).
        if output.trim().is_empty() {
            saw_empty_output = true;
            continue;
        }

        let token_count = count_tokens_or_max(&output);

        if token_count <= token_budget {
            if mode != starting_mode {
                eprintln!(
                    "[skim] token budget: escalated from {} to {} mode ({} tokens)",
                    starting_mode.name(),
                    mode.name(),
                    token_count,
                );
            }
            return Ok((output, mode));
        }

        last_output = Some(output);
        last_mode = mode;
        last_token_count = Some(token_count);
    }

    // At least one mode returned Some("") but none returned non-empty content.
    // Recover the raw source via Mode::Full so we can either return empty success
    // (for an empty/whitespace-only source) or line-truncate the raw content
    // (for a source that transforms to nothing, e.g. a comment-only file).
    if last_output.is_none() && saw_empty_output {
        let full_config = build_config(Mode::Full, trunc);
        let raw = transform_fn(&full_config)?.unwrap_or_default();
        if raw.trim().is_empty() {
            // Empty or whitespace-only source: return an empty result with no marker —
            // nothing was elided (#317: never truncate, and there is nothing to truncate).
            return Ok((String::new(), starting_mode));
        }
        // Non-empty source where every structural mode produced empty output
        // (e.g. a Rust file containing only comments, no fn/type declarations):
        // line-truncate the raw source so the reader gets content.
        return fallback_line_truncate(
            &raw,
            language,
            token_budget,
            starting_mode,
            None,
            source_line_count,
        );
    }

    // Guard: no mode produced output at all (all returned Ok(None)).
    let last_output = last_output.ok_or_else(|| anyhow::anyhow!(NO_OUTPUT_MSG))?;

    fallback_line_truncate(
        &last_output,
        language,
        token_budget,
        last_mode,
        last_token_count,
        source_line_count,
    )
}

/// Serde-based cascade short-circuit for `cascade_for_token_budget`.
///
/// Serde languages (JSON, YAML, TOML) produce at most two distinct outputs:
/// passthrough (Full/Minimal) and structure-extracted (Structure/Signatures/Types).
/// This avoids up to 3 redundant parse+transform cycles in the generic cascade.
fn cascade_serde<F>(
    starting_mode: Mode,
    trunc: &TruncationOptions,
    token_budget: usize,
    language: Language,
    source_line_count: usize,
    transform_fn: &F,
) -> anyhow::Result<(String, Mode)>
where
    F: Fn(&TransformConfig) -> anyhow::Result<Option<String>>,
{
    let config = build_config(starting_mode, trunc);
    let first_output = transform_fn(&config)?.ok_or_else(|| anyhow::anyhow!(NO_OUTPUT_MSG))?;

    let first_tokens = count_tokens_or_max(&first_output);
    if first_tokens <= token_budget {
        return Ok((first_output, starting_mode));
    }

    // If starting at Full/Minimal/Pseudo, try structure-extracted (the only other distinct output)
    if matches!(starting_mode, Mode::Full | Mode::Minimal | Mode::Pseudo) {
        let structure_config = build_config(Mode::Structure, trunc);
        if let Some(extracted) = transform_fn(&structure_config)? {
            let extracted_tokens = count_tokens_or_max(&extracted);
            if extracted_tokens <= token_budget {
                eprintln!(
                    "[skim] token budget: escalated from {} to structure mode ({} tokens)",
                    starting_mode.name(),
                    extracted_tokens,
                );
                return Ok((extracted, Mode::Structure));
            }
            return fallback_line_truncate(
                &extracted,
                language,
                token_budget,
                Mode::Structure,
                Some(extracted_tokens),
                source_line_count,
            );
        }
    }

    // Starting mode was already Structure/Signatures/Types, or structure extraction
    // returned None (defensive). Fall back to line truncation on the first output.
    fallback_line_truncate(
        &first_output,
        language,
        token_budget,
        starting_mode,
        Some(first_tokens),
        source_line_count,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tokens;

    /// Mock transform: returns the first N words from source for the matching mode.
    fn mock_transform<'a>(
        source: &'a str,
        mode_sizes: &'a [(Mode, usize)],
    ) -> impl Fn(&TransformConfig) -> anyhow::Result<Option<String>> + 'a {
        move |config: &TransformConfig| {
            for &(mode, size) in mode_sizes {
                if config.mode == mode {
                    let words: Vec<&str> = source.split_whitespace().take(size).collect();
                    return Ok(Some(words.join(" ")));
                }
            }
            Ok(None)
        }
    }

    #[test]
    fn test_cascade_returns_first_mode_when_within_budget() {
        // Structure output = 3 tokens, budget = 10 → no escalation
        let source = "word1 word2 word3 word4 word5 word6 word7 word8 word9 word10";
        let mode_sizes = vec![
            (Mode::Structure, 3),
            (Mode::Signatures, 2),
            (Mode::Types, 1),
        ];
        let transform = mock_transform(source, &mode_sizes);

        let trunc = TruncationOptions::default();

        let (output, mode_used) =
            cascade_for_token_budget(Mode::Structure, &trunc, 10, Language::TypeScript, 100, transform)
                .unwrap();

        assert_eq!(mode_used, Mode::Structure);
        assert_eq!(output, "word1 word2 word3");
    }

    #[test]
    fn test_cascade_escalates_to_more_aggressive_mode() {
        // Structure = 20 tokens (over budget), Signatures = 8 (within budget)
        let source = "a b c d e f g h i j k l m n o p q r s t";
        let mode_sizes = vec![
            (Mode::Structure, 20),
            (Mode::Signatures, 8),
            (Mode::Types, 3),
        ];
        let transform = mock_transform(source, &mode_sizes);
        let trunc = TruncationOptions::default();

        let (_output, mode_used) =
            cascade_for_token_budget(Mode::Structure, &trunc, 10, Language::TypeScript, 100, transform)
                .unwrap();

        assert_eq!(mode_used, Mode::Signatures);
    }

    #[test]
    fn test_cascade_falls_through_to_line_truncation() {
        // All modes exceed budget → should hit line truncation fallback
        let source = "a b c d e f g h i j k l m n o p q r s t";
        let mode_sizes = vec![
            (Mode::Structure, 20),
            (Mode::Signatures, 15),
            (Mode::Types, 12),
        ];
        let transform = mock_transform(source, &mode_sizes);
        let trunc = TruncationOptions::default();

        let (output, mode_used) =
            cascade_for_token_budget(Mode::Structure, &trunc, 5, Language::TypeScript, 100, transform)
                .unwrap();

        // Should use the most aggressive mode that produced output
        assert_eq!(mode_used, Mode::Types);
        // ADR-011 class 1 / #317: when no content fits within budget even after
        // line truncation, the compact elision marker is still emitted (never empty).
        // The marker alone may exceed budget=5; that is acceptable — disclosure beats
        // the budget. Old assertion `output.is_empty()` encoded the pre-ADR-011 silent-
        // loss behavior and is now wrong.
        let token_count = tokens::count_tokens(&output).unwrap_or(usize::MAX);
        assert!(
            token_count <= 5 || output.contains("truncated"),
            "Final output should be within budget or contain elision marker, got {} tokens: {:?}",
            token_count,
            output
        );
    }

    #[test]
    fn test_cascade_single_mode_types() {
        // Starting at Types → only one mode in cascade, must fit or truncate
        let source = "a b c d e f g h i j";
        let mode_sizes = vec![(Mode::Types, 5)];
        let transform = mock_transform(source, &mode_sizes);

        let trunc = TruncationOptions::default();

        let (output, mode_used) =
            cascade_for_token_budget(Mode::Types, &trunc, 10, Language::TypeScript, 100, transform)
                .unwrap();

        assert_eq!(mode_used, Mode::Types);
        assert_eq!(output, "a b c d e");
    }

    #[test]
    fn test_cascade_errors_when_no_mode_produces_output() {
        // All modes return None → should error
        let transform = |_config: &TransformConfig| -> anyhow::Result<Option<String>> { Ok(None) };

        let trunc = TruncationOptions::default();

        let result = cascade_for_token_budget(
            Mode::Structure,
            &trunc,
            100,
            Language::TypeScript,
            100,
            transform,
        );

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no transformation mode produced output"),
        );
    }

    // ── Serde cascade path tests ────────────────────────────────────────

    #[test]
    fn test_serde_cascade_returns_starting_mode_when_within_budget() {
        // Serde language (JSON) with Full mode output fitting within budget
        let source = "a b c d e f g h i j";
        let mode_sizes = vec![(Mode::Full, 5), (Mode::Structure, 3)];
        let transform = mock_transform(source, &mode_sizes);

        let trunc = TruncationOptions::default();

        let (output, mode_used) =
            cascade_for_token_budget(Mode::Full, &trunc, 10, Language::Json, 100, transform).unwrap();

        assert_eq!(mode_used, Mode::Full);
        assert_eq!(output, "a b c d e");
    }

    #[test]
    fn test_serde_cascade_escalates_from_full_to_structure() {
        // Serde language: Full mode exceeds budget, Structure fits
        let source = "a b c d e f g h i j k l m n o p q r s t";
        let mode_sizes = vec![(Mode::Full, 20), (Mode::Structure, 5)];
        let transform = mock_transform(source, &mode_sizes);
        let trunc = TruncationOptions::default();

        let (output, mode_used) =
            cascade_for_token_budget(Mode::Full, &trunc, 10, Language::Json, 100, transform).unwrap();

        assert_eq!(mode_used, Mode::Structure);
        assert_eq!(output, "a b c d e");
    }

    #[test]
    fn test_serde_cascade_full_to_structure_exceeds_falls_to_truncation() {
        // Serde language: both Full and Structure exceed budget, falls to line truncation
        let source = "a b c d e f g h i j k l m n o p q r s t";
        let mode_sizes = vec![(Mode::Full, 20), (Mode::Structure, 15)];
        let transform = mock_transform(source, &mode_sizes);

        let trunc = TruncationOptions::default();

        let (output, mode_used) =
            cascade_for_token_budget(Mode::Full, &trunc, 5, Language::Json, 100, transform).unwrap();

        assert_eq!(mode_used, Mode::Structure);
        // ADR-011 class 1 / #317: compact elision marker always emitted when no content
        // fits; may exceed budget=5. Old `output.is_empty()` encoded silent-loss behavior.
        let token_count = tokens::count_tokens(&output).unwrap_or(usize::MAX);
        assert!(
            token_count <= 5 || output.contains("truncated"),
            "Expected within budget or elision marker, got {} tokens: {:?}",
            token_count,
            output
        );
    }

    #[test]
    fn test_serde_cascade_structure_start_exceeds_falls_to_truncation() {
        // Serde language starting at Structure: exceeds budget, falls to line truncation
        let source = "a b c d e f g h i j k l m n o p q r s t";
        let mode_sizes = vec![(Mode::Structure, 20)];
        let transform = mock_transform(source, &mode_sizes);

        let trunc = TruncationOptions::default();

        let (output, mode_used) =
            cascade_for_token_budget(Mode::Structure, &trunc, 5, Language::Yaml, 100, transform)
                .unwrap();

        assert_eq!(mode_used, Mode::Structure);
        // ADR-011 class 1 / #317: compact elision marker always emitted when no content
        // fits; may exceed budget=5. Old `output.is_empty()` encoded silent-loss behavior.
        let token_count = tokens::count_tokens(&output).unwrap_or(usize::MAX);
        assert!(
            token_count <= 5 || output.contains("truncated"),
            "Expected within budget or elision marker, got {} tokens: {:?}",
            token_count,
            output
        );
    }

    #[test]
    fn test_serde_cascade_errors_when_no_output() {
        // Serde language where transform returns None for starting mode
        let transform = |_config: &TransformConfig| -> anyhow::Result<Option<String>> { Ok(None) };

        let trunc = TruncationOptions::default();

        let result = cascade_for_token_budget(Mode::Full, &trunc, 100, Language::Toml, 100, transform);

        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no transformation mode produced output"),
        );
    }

    // ── compact_marker_without_hint unit tests ──────────────────────────────────

    #[test]
    fn compact_marker_without_hint_bare_marker_is_true() {
        // A bare compact marker (no hint) → true; the caller must emit the hint on stderr.
        assert!(compact_marker_without_hint(
            "// ... (12 lines truncated)",
            ELISION_HINT
        ));
        assert!(compact_marker_without_hint(
            "// ... (1 line truncated)",
            ELISION_HINT
        ));
    }

    #[test]
    fn compact_marker_without_hint_full_marker_is_false() {
        // Full marker already carries the hint → no need to emit it again on stderr.
        let full = format!("fn foo() {{}}\n// ... (3 lines truncated) — {ELISION_HINT}");
        assert!(!compact_marker_without_hint(&full, ELISION_HINT));
    }

    #[test]
    fn compact_marker_without_hint_no_marker_is_false() {
        // No elision marker at all → not a compact-form case.
        assert!(!compact_marker_without_hint("fn foo() {}", ELISION_HINT));
    }

    #[test]
    fn compact_marker_without_hint_empty_is_false() {
        // Empty output → false; no marker to classify.
        assert!(!compact_marker_without_hint("", ELISION_HINT));
    }

    // ── rust-2: #511 literal-aware marker vocabulary ────────────────────────

    #[test]
    fn compact_marker_without_hint_cut_inside_string_literal_is_true() {
        // #511 extended the marker vocabulary: when the cut falls inside a string
        // literal the marker reads "lines truncated; cut inside a string literal)"
        // (no closing ')' after 'truncated').  The old whole-buffer scan for
        // "lines truncated)" missed this form; the new last-line scan finds it.
        assert!(compact_marker_without_hint(
            "fn foo() {}\n// ... (3 lines truncated; cut inside a string literal)",
            ELISION_HINT
        ));
    }

    #[test]
    fn compact_marker_without_hint_cut_inside_code_fence_is_true() {
        // Same as above but the "cut inside a code fence" variant.
        assert!(compact_marker_without_hint(
            "# heading\n# ... (7 lines truncated; cut inside a code fence)",
            ELISION_HINT
        ));
    }

    #[test]
    fn compact_marker_without_hint_hint_in_file_content_still_fires() {
        // rust-2 second defect: if the file content contains the literal hint
        // string, the old whole-buffer `has_hint` check would return true and
        // suppress the stderr remedy even though the last-line marker is compact.
        //
        // This output simulates a file that documents the hint (like CLAUDE.md or
        // process.rs) followed by a compact elision marker on the last line.
        let output = format!(
            "// Example: {ELISION_HINT}\nfn foo() {{}}\n// ... (5 lines truncated)"
        );
        // has_hint must be false because the hint is NOT on the last line.
        assert!(
            compact_marker_without_hint(&output, ELISION_HINT),
            "compact_marker_without_hint must return true even when hint text \
             appears earlier in the output (only the last line should be scanned)"
        );
    }

    #[test]
    fn compact_marker_without_hint_hint_on_last_line_with_marker_is_false() {
        // Sanity: a full marker (hint ON the last line alongside the count)
        // must still return false so we do not double-emit the hint on stderr.
        let full =
            format!("fn foo() {{}}\n// ... (3 lines truncated) — {ELISION_HINT}");
        assert!(
            !compact_marker_without_hint(&full, ELISION_HINT),
            "full marker (hint on last line) must not trigger the stderr remedy"
        );
    }

    // ── Empty-output skip tests (RED fixture regression) ────────────────────

    #[test]
    fn test_cascade_empty_escalated_mode_falls_to_truncation() {
        // Mirrors the RED CLI fixture: structure mode is over-budget and every
        // escalated mode (Signatures, Types) returns Ok(Some("")) — e.g. a Rust
        // file that has fn items but no type declarations.  The empty modes must
        // be skipped; the non-empty structure output is line-truncated instead.
        let long_output = "a b c d e f g h i j k l m n o p q r s t u v w x y z";
        let transform = move |config: &TransformConfig| -> anyhow::Result<Option<String>> {
            match config.mode {
                Mode::Structure => Ok(Some(long_output.to_string())),
                // Escalated modes produce empty output (no type/signature nodes).
                Mode::Signatures | Mode::Types => Ok(Some(String::new())),
                _ => Ok(None),
            }
        };

        let trunc = TruncationOptions::default();

        let (output, _mode_used) =
            cascade_for_token_budget(Mode::Structure, &trunc, 5, Language::Rust, 100, transform)
                .unwrap();

        // #317 / ADR-011: result must be non-empty and carry the elision marker.
        assert!(
            !output.is_empty(),
            "Output must not be empty when empty modes are skipped and fallback truncates",
        );
        assert!(
            output.contains("truncated"),
            "Elision marker must be present; got: {:?}",
            output,
        );
    }

    #[test]
    fn test_cascade_all_modes_empty_output_falls_back_to_raw_source() {
        // All structural modes (Structure, Signatures, Types) return Ok(Some(""))
        // for a non-empty source (e.g. a comment-only file). The cascade must
        // recover via Mode::Full and line-truncate the raw source.
        let raw_source = "// line 1\n// line 2\n// line 3\n// line 4\n// line 5\n\
                          // line 6\n// line 7\n// line 8\n// line 9\n// line 10\n";
        let transform = move |config: &TransformConfig| -> anyhow::Result<Option<String>> {
            match config.mode {
                // Structural modes produce nothing (all comments, no declarations).
                Mode::Structure | Mode::Signatures | Mode::Types => Ok(Some(String::new())),
                // Full mode returns the raw source (as the real transform would).
                Mode::Full => Ok(Some(raw_source.to_string())),
                _ => Ok(None),
            }
        };

        let trunc = TruncationOptions::default();

        // Budget of 5 tokens — the raw source exceeds it, so line-truncation applies.
        let result =
            cascade_for_token_budget(Mode::Structure, &trunc, 5, Language::Rust, 100, transform);

        let (output, _mode_used) = result.expect("should not error when raw source is available");
        assert!(
            !output.is_empty(),
            "output must be non-empty when raw source is used as fallback; got: {output:?}",
        );
    }

    #[test]
    fn test_cascade_empty_intermediate_mode_uses_later_non_empty_mode() {
        // Signatures returns Ok(Some("")) (empty intermediate), but Types is
        // non-empty and within budget — the cascade must skip the empty mode and
        // return the Types output.
        let long_output = "a b c d e f g h i j k l m n o p q r s t";
        let transform = move |config: &TransformConfig| -> anyhow::Result<Option<String>> {
            match config.mode {
                Mode::Structure => Ok(Some(long_output.to_string())),
                // Empty intermediate — must be skipped.
                Mode::Signatures => Ok(Some(String::new())),
                // Later mode is non-empty and within budget.
                Mode::Types => Ok(Some("type Foo = u32;".to_string())),
                _ => Ok(None),
            }
        };

        let trunc = TruncationOptions::default();

        // Budget of 10 tokens: Structure exceeds it, Signatures is empty (skipped),
        // Types ("type Foo = u32;" ≈ 6 tokens) fits.
        let (output, mode_used) =
            cascade_for_token_budget(Mode::Structure, &trunc, 10, Language::Rust, 100, transform)
                .unwrap();

        assert_eq!(
            mode_used,
            Mode::Types,
            "Should have escalated to Types mode"
        );
        assert_eq!(output, "type Foo = u32;");
    }
}
