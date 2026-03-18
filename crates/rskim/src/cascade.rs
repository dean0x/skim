//! Token-budget cascade logic.
//!
//! Progressively applies more aggressive transformation modes until the output
//! fits within a caller-specified token budget, with a final line-truncation
//! fallback.

use rskim_core::{truncate_to_token_budget, Language, Mode, TransformConfig};

use crate::tokens;

/// Error message when no transformation mode produces output.
const NO_OUTPUT_MSG: &str = "Token budget cascade: no transformation mode produced output. \
    Ensure the file is in a supported language or specify --language.";

/// Build a `TransformConfig` from mode and optional max_lines.
pub(crate) fn build_config(mode: Mode, max_lines: Option<usize>) -> TransformConfig {
    let mut config = TransformConfig::with_mode(mode);
    if let Some(n) = max_lines {
        config = config.with_max_lines(n);
    }
    config
}

/// Count tokens, returning `usize::MAX` on failure (treats errors as over-budget).
///
/// Centralises the warning-on-error pattern used throughout the cascade logic.
fn count_tokens_or_max(text: &str) -> usize {
    tokens::count_tokens(text).unwrap_or_else(|e| {
        eprintln!("[skim] warning: token counting failed, treating as over-budget: {e}");
        usize::MAX
    })
}

/// Apply line-based truncation as a final fallback when all modes exceed the budget.
///
/// Emits a diagnostic to stderr and delegates to `truncate_to_token_budget`.
fn fallback_line_truncate(
    output: &str,
    language: Language,
    token_budget: usize,
    mode: Mode,
    known_token_count: Option<usize>,
) -> anyhow::Result<(String, Mode)> {
    eprintln!(
        "[skim] token budget: all modes exceeded budget, applying line truncation ({} mode)",
        mode.name(),
    );
    let truncated = truncate_to_token_budget(
        output,
        language,
        token_budget,
        count_tokens_or_max,
        known_token_count,
    )?;
    Ok((truncated, mode))
}

/// Cascade through transformation modes until output fits within `token_budget`.
///
/// Tries each mode from `starting_mode` through increasingly aggressive modes.
/// If no mode fits, applies line-based truncation as a final fallback.
/// Diagnostics are emitted to stderr only when escalating beyond the starting mode.
pub(crate) fn cascade_for_token_budget<F>(
    starting_mode: Mode,
    max_lines: Option<usize>,
    token_budget: usize,
    language: Language,
    transform_fn: F,
) -> anyhow::Result<(String, Mode)>
where
    F: Fn(&TransformConfig) -> anyhow::Result<Option<String>>,
{
    let cascade = starting_mode.cascade_from_here();
    let mut last_output = String::new();
    let mut last_mode = starting_mode;
    let mut last_token_count: Option<usize> = None;

    let mut config = build_config(starting_mode, max_lines);

    // Serde-based languages produce at most 2 distinct outputs regardless of mode:
    // - Full/Minimal: original source (passthrough)
    // - Structure/Signatures/Types: structure-extracted (all identical)
    // Short-circuit to avoid up to 3 redundant parse+transform cycles.
    if language.is_serde_based() {
        return cascade_serde(
            starting_mode,
            &mut config,
            token_budget,
            language,
            &transform_fn,
        );
    }

    for &mode in cascade {
        config.mode = mode;

        let Some(output) = transform_fn(&config)? else {
            continue;
        };

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

        last_output = output;
        last_mode = mode;
        last_token_count = Some(token_count);
    }

    // Guard: no mode produced output (defensive; transform_fn currently always
    // returns Ok(Some(...)), but protects against future callers returning Ok(None)).
    if last_output.is_empty() {
        anyhow::bail!(NO_OUTPUT_MSG);
    }

    fallback_line_truncate(
        &last_output,
        language,
        token_budget,
        last_mode,
        last_token_count,
    )
}

/// Serde-based cascade short-circuit for `cascade_for_token_budget`.
///
/// Serde languages (JSON, YAML, TOML) produce at most two distinct outputs:
/// passthrough (Full/Minimal) and structure-extracted (Structure/Signatures/Types).
/// This avoids up to 3 redundant parse+transform cycles in the generic cascade.
fn cascade_serde<F>(
    starting_mode: Mode,
    config: &mut TransformConfig,
    token_budget: usize,
    language: Language,
    transform_fn: &F,
) -> anyhow::Result<(String, Mode)>
where
    F: Fn(&TransformConfig) -> anyhow::Result<Option<String>>,
{
    let first_output = transform_fn(config)?.ok_or_else(|| anyhow::anyhow!(NO_OUTPUT_MSG))?;

    let first_tokens = count_tokens_or_max(&first_output);
    if first_tokens <= token_budget {
        return Ok((first_output, starting_mode));
    }

    // If starting at Full/Minimal, try structure-extracted (the only other distinct output)
    if matches!(starting_mode, Mode::Full | Mode::Minimal) {
        config.mode = Mode::Structure;
        if let Some(extracted) = transform_fn(config)? {
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

        let (output, mode_used) =
            cascade_for_token_budget(Mode::Structure, None, 10, Language::TypeScript, transform)
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

        let (_output, mode_used) =
            cascade_for_token_budget(Mode::Structure, None, 10, Language::TypeScript, transform)
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

        let (output, mode_used) =
            cascade_for_token_budget(Mode::Structure, None, 5, Language::TypeScript, transform)
                .unwrap();

        // Should use the most aggressive mode that produced output
        assert_eq!(mode_used, Mode::Types);
        // Output should contain truncation marker or be within budget
        let token_count = tokens::count_tokens(&output).unwrap_or(usize::MAX);
        assert!(
            token_count <= 5 || output.is_empty(),
            "Final output should be within budget or empty, got {} tokens: {:?}",
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

        let (output, mode_used) =
            cascade_for_token_budget(Mode::Types, None, 10, Language::TypeScript, transform)
                .unwrap();

        assert_eq!(mode_used, Mode::Types);
        assert_eq!(output, "a b c d e");
    }

    #[test]
    fn test_cascade_errors_when_no_mode_produces_output() {
        // All modes return None → should error
        let transform = |_config: &TransformConfig| -> anyhow::Result<Option<String>> { Ok(None) };

        let result =
            cascade_for_token_budget(Mode::Structure, None, 100, Language::TypeScript, transform);

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("no transformation mode produced output"),);
    }
}
