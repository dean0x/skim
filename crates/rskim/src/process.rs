//! Single-file processing pipeline.
//!
//! Handles reading, transforming, caching, and outputting a single file or
//! stdin stream. Multi-file orchestration lives in [`crate::multi`].

use std::fs;
use std::io::{self, BufWriter, Read, Write};
use std::path::Path;

use rskim_core::{
    ElidedSide, Language, Mode, TransformConfig, detect_language_from_path, elision_marker_line,
    simple_last_line_truncate_with_start, simple_line_truncate, transform_auto_with_config,
    transform_with_config, transform_with_line_map,
};

use crate::output::ELISION_HINT;
use crate::{cache, cascade, cascade::TruncationOptions, tokens};

/// Maximum input size to prevent memory exhaustion (50MB)
const MAX_INPUT_SIZE: usize = 50 * 1024 * 1024;

/// Options for processing a single file
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProcessOptions {
    /// Transformation mode
    pub(crate) mode: Mode,
    /// Explicit language override (None for auto-detection)
    pub(crate) explicit_lang: Option<Language>,
    /// Whether to use cache
    pub(crate) use_cache: bool,
    /// Whether to compute token statistics (for --show-stats)
    pub(crate) show_stats: bool,
    /// Truncation options (max_lines, last_lines, token_budget)
    pub(crate) trunc: TruncationOptions,
    /// Whether to annotate output with source line numbers (`--line-numbers` / `-n`)
    pub(crate) line_numbers: bool,
}

/// Result of processing a file
#[derive(Debug)]
#[must_use]
pub(crate) struct ProcessResult {
    /// Transformed output
    pub(crate) output: String,
    /// Original token count (if computed)
    pub(crate) original_tokens: Option<usize>,
    /// Transformed token count (if computed)
    pub(crate) transformed_tokens: Option<usize>,
    /// Whether the output guardrail was triggered (compressed > raw)
    pub(crate) guardrail_triggered: bool,
    /// Parse quality tier: "full", "degraded", or "passthrough".
    ///
    /// - "passthrough" — Mode::Full, no transformation applied
    /// - "degraded"    — tree-sitter reported syntax errors
    /// - "full"        — clean parse, no errors
    ///
    /// `None` for cache hits (tier was not recorded at write time).
    pub(crate) parse_tier: Option<&'static str>,
    /// Effective language used for transformation.
    ///
    /// Set in all three constructors (file, stdin, cache-hit) so the analytics
    /// layer can record the correct language without re-detecting from path.
    /// Priority mirrors the transform path: explicit_lang wins, else auto-detect.
    pub(crate) language: Option<Language>,
    /// Raw stdin buffer retained for background tokenization.
    ///
    /// `Some(buffer)` only from `process_stdin` when `!show_stats` (stdin
    /// cannot be re-read; the buffer must be kept).  All other constructors
    /// set this to `None` (files can be re-read from disk).
    pub(crate) stdin_raw: Option<String>,
    /// Whether the served view differs from raw file bytes.
    ///
    /// `true` when `SKIM_REWRITTEN_FROM` is set AND the transformed output
    /// is not byte-identical to the raw input.  Used by the transparency
    /// marker layer to emit a stderr notice on hook-rewritten file reads.
    /// Always `false` when the origin env var is absent (non-hook invocations).
    pub(crate) view_differs: bool,
}

/// Determine the parse quality tier from the mode, parse-error flag, and degraded flag.
///
/// - "passthrough" — Mode::Full; no transformation was applied
/// - "degraded" — tree-sitter reported syntax errors, OR a structural safety cap was exceeded and the output was degraded to raw passthrough
/// - "full" — clean parse, no syntax errors, and no cap-triggered degradation
pub(crate) fn parse_tier_from(mode: Mode, has_errors: bool, degraded: bool) -> &'static str {
    if mode == Mode::Full {
        "passthrough"
    } else if has_errors || degraded {
        "degraded"
    } else {
        "full"
    }
}

/// Apply line number annotations to `output` after the guardrail decision.
///
/// Priority for map selection (highest wins):
/// 1. `computed_map` when `Some` — either from the core transform or from a
///    post-guardrail truncation that returned the correct window start (PF-019 /
///    complexity-2: using `identity_line_map` after `--last-lines` truncation
///    labels tail lines as 1..N instead of their real source numbers).
/// 2. Identity map when `guardrail_triggered` — guardrail served raw with no
///    subsequent truncation, so output line N corresponds to source line N.
/// 3. Skip annotation — no map available (serde non-full modes, language
///    detection failure): restructured output has no source correspondence.
///
/// AC-11: Identity map is applied when guardrail emits unbounded raw source.
/// AC-15: Serde non-full modes skip line numbers (computed_map is None).
pub(crate) fn apply_line_numbers(
    output: String,
    line_numbers: bool,
    guardrail_triggered: bool,
    computed_map: Option<Vec<usize>>,
) -> String {
    if !line_numbers {
        return output;
    }
    // computed_map is checked first: it carries the correct source line numbers from
    // the core transform OR from the post-guardrail truncation helper, and takes
    // priority over the identity map that would otherwise be applied for the
    // guardrail path (the identity map is wrong after --last-lines truncation).
    if let Some(map) = computed_map {
        return crate::format::format_with_line_numbers(&output, &map);
    }
    if guardrail_triggered {
        // Guardrail served raw source; no subsequent truncation was applied (or
        // truncation produced no map). Identity map is correct: output line N = source line N.
        let map = crate::format::identity_line_map(&output);
        return crate::format::format_with_line_numbers(&output, &map);
    }
    // No line map available (serde non-full, language detection failure):
    // skip annotation — restructured output has no source correspondence.
    output
}

/// Count tokens for both original and transformed text, returning `(None, None)` on failure.
///
/// Centralises the paired token-counting pattern used across the processing pipeline.
pub(crate) fn count_token_pair(
    original: &str,
    transformed: &str,
) -> (Option<usize>, Option<usize>) {
    match (
        tokens::count_tokens(original),
        tokens::count_tokens(transformed),
    ) {
        (Ok(orig), Ok(trans)) => (Some(orig), Some(trans)),
        _ => (None, None),
    }
}

/// Report token statistics to stderr if token counts are available
pub(crate) fn report_token_stats(
    original_tokens: Option<usize>,
    transformed_tokens: Option<usize>,
    suffix: &str,
) {
    if let (Some(orig), Some(trans)) = (original_tokens, transformed_tokens) {
        let stats = tokens::TokenStats::new(orig, trans);
        eprintln!("\n[skim] {}{}", stats.format(), suffix);
    }
}

/// Write a single-input result to stdout and optionally report token stats to stderr.
///
/// Used by both `process_stdin` and the single-file path in `main()`.
/// Multi-file paths use their own output logic in `process_files()`.
///
/// When the invocation was a hook-rewritten file read (`SKIM_REWRITTEN_FROM` is set)
/// and the served view differs from raw bytes, emits a one-line stderr transparency
/// marker so agents can distinguish structured views from byte-identical passthroughs
/// (per ADR-005: agents learn about passthrough via stderr hints, not guidance prose).
#[allow(clippy::disallowed_methods)] // Central result+stats emitter; BufWriter wraps the single stdout lock for coherent output
pub(crate) fn write_result_and_stats(
    result: &ProcessResult,
    show_stats: bool,
    mode_str: &str,
) -> anyhow::Result<()> {
    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());
    write!(writer, "{}", result.output)?;
    writer.flush()?;

    if show_stats {
        report_token_stats(result.original_tokens, result.transformed_tokens, "");
    }

    // B3 / ADR-011 class 1: emit lossy-view marker unconditionally when the
    // view differs from raw bytes.  Previously gated on `SKIM_REWRITTEN_FROM`;
    // now fires for any lossy read (direct or hook-rewritten).  Not gated by
    // `SKIM_DEBUG` — this is a loss-bearing marker (class 1), not a no-loss
    // fallback banner (class 2).
    if let Some(marker) = crate::output::lossy_view_marker(
        crate::output::rewrite_origin().as_deref(),
        mode_str,
        if result.view_differs { 1 } else { 0 },
        1,
    ) {
        eprintln!("{marker}");
    }

    Ok(())
}

/// Try to return a result from cache, handling token recount when needed.
///
/// Returns `Some(ProcessResult)` on cache hit, `None` on cache miss.
/// When stats are requested but the cached entry lacks token counts,
/// the original file is read to compute them on the fly.
fn try_cached_result(
    path: &Path,
    options: &ProcessOptions,
) -> anyhow::Result<Option<ProcessResult>> {
    if !options.use_cache {
        return Ok(None);
    }

    let Some(hit) = cache::read_cache(path, options.mode, &options.trunc, options.line_numbers)
    else {
        return Ok(None);
    };

    // If the cache entry was written without token counts, read the original
    // file and count tokens for both source and output -- but only when
    // --show-stats is active. Analytics background threads handle their own
    // token counting, so we don't erode cache speedup for analytics alone.
    //
    // Also read the raw file when a hook-rewrite origin tag is present so the
    // transparency marker can compare cached content against raw bytes.
    let needs_recount = hit.original_tokens.is_none() && options.show_stats;
    let origin_active = crate::output::rewrite_origin().is_some();
    let needs_raw_read = needs_recount || origin_active;

    // consistency-2: use the `view_differs` value stored in the cache record when
    // available (written from the authoritative byte comparison in process_file).
    // Fall back to mode-inference only for old cache entries that pre-date the field.
    // Mode-inference (`mode != Mode::Full`) is wrong when the ADR-001 guardrail chose
    // to serve raw bytes: the cached content IS the raw file, so view_differs is false,
    // but mode-inference would say true (causing the transparency marker to fire on a
    // byte-identical warm hit but not on the cold hit — inconsistent behaviour).
    let cache_hit_view_differs = hit.view_differs.unwrap_or(options.mode != Mode::Full);

    let (orig_tokens, trans_tokens, view_differs) = if needs_raw_read {
        match read_and_validate(path) {
            Ok(contents) => {
                let (orig, trans) = if needs_recount {
                    count_token_pair(&contents, &hit.content)
                } else {
                    (hit.original_tokens, hit.transformed_tokens)
                };
                // Use the stored view_differs when available.  When missing (old cache
                // entry), compare bytes — the file is in hand and this is the
                // authoritative check regardless of origin_active (consistency-2).
                let differs = hit.view_differs.unwrap_or_else(|| hit.content != contents);
                (orig, trans, differs)
            }
            Err(e) => {
                if needs_recount {
                    // Token recount failure is a hard error.
                    return Err(e);
                }
                // Read needed only for the transparency marker; default to
                // view_differs=true (conservative — a false positive is safe,
                // a false negative would be the incident class).
                (hit.original_tokens, hit.transformed_tokens, true)
            }
        }
    } else {
        (
            hit.original_tokens,
            hit.transformed_tokens,
            cache_hit_view_differs,
        )
    };

    // Effective language for a cache hit: explicit override wins, else detect from path.
    let cache_lang = options
        .explicit_lang
        .or_else(|| detect_language_from_path(path));

    Ok(Some(ProcessResult {
        output: hit.content,
        original_tokens: orig_tokens,
        transformed_tokens: trans_tokens,
        guardrail_triggered: false,
        parse_tier: None, // tier was not recorded at cache-write time
        language: cache_lang,
        stdin_raw: None,
        view_differs,
    }))
}

/// Read a file and validate it doesn't exceed the maximum input size.
///
/// Performs a pre-read metadata check to bail early before allocating memory,
/// which prevents a transient peak of `num_cpus × file_size` when this function
/// is called in parallel (e.g., via `into_par_iter` in the analytics recorder).
/// The post-read length check is retained for TOCTOU safety (the file may grow
/// between the stat and the read).
fn read_and_validate(path: &Path) -> anyhow::Result<String> {
    // Pre-read size guard: bail before allocating if the file is already over the limit.
    // This is a best-effort check; a file that is exactly at the limit may pass here
    // but fail the post-read check below if it grows between the stat and the read.
    if let Ok(meta) = fs::metadata(path)
        && meta.len() as usize > MAX_INPUT_SIZE
    {
        anyhow::bail!(
            "File too large: {} bytes exceeds maximum of {} bytes ({}MB)",
            meta.len(),
            MAX_INPUT_SIZE,
            MAX_INPUT_SIZE / 1024 / 1024
        );
    }
    let contents = fs::read_to_string(path)?;
    if contents.len() > MAX_INPUT_SIZE {
        anyhow::bail!(
            "File too large: {} bytes exceeds maximum of {} bytes ({}MB)",
            contents.len(),
            MAX_INPUT_SIZE,
            MAX_INPUT_SIZE / 1024 / 1024
        );
    }
    Ok(contents)
}

/// Transform file contents, trying auto-detection first and falling back to
/// `explicit_lang` when provided.
///
/// Output tuple of [`run_transform`]:
/// `(transformed_output, mode_used, has_errors, source_line_map, degraded)`.
type RunTransformOutput = (String, Mode, bool, Option<Vec<usize>>, bool);

/// Returns `(transformed_output, mode_used, has_errors, source_line_map, degraded)` where:
/// - `has_errors` reflects whether the parser encountered syntax errors
/// - `source_line_map` is `Some(map)` when `options.line_numbers` is true and the
///   transform produced a meaningful source line map; `None` otherwise
/// - `degraded` is `true` when a structural safety cap was exceeded and the output
///   was degraded to raw passthrough
///
/// For cascade paths (token_budget is set) `has_errors` is always `false` and
/// line numbers are applied after mode selection.
fn run_transform(
    contents: &str,
    path: &Path,
    options: &ProcessOptions,
) -> anyhow::Result<RunTransformOutput> {
    let explicit_lang = options.explicit_lang;
    // Non-line-number transform closure (used for cascade mode selection)
    let transform_file = |config: &TransformConfig| -> anyhow::Result<Option<String>> {
        // Try auto-detection first; fall back to an explicit language, then to a
        // shebang-sniffed language. The shebang fallback keeps this closure in
        // step with the language resolution used for the cascade budget below,
        // so an extensionless script with a `#!` line transforms here instead of
        // failing with UnsupportedLanguage (ADR-002).
        let auto_result = transform_auto_with_config(contents, path, config);
        if let Ok(output) = auto_result {
            return Ok(Some(output));
        }
        let fallback_lang = explicit_lang.or_else(|| detect_language_from_shebang(contents));
        let Some(language) = fallback_lang else {
            return Err(auto_result.unwrap_err().into());
        };
        Ok(Some(transform_with_config(contents, language, config)?))
    };

    match options.trunc.token_budget {
        Some(budget) => {
            // Resolve the language for cascade budget math: explicit override,
            // then extension, then a shebang sniff of the first line. Mirrors the
            // None branch so extensionless shebang scripts (e.g. `#!/bin/bash`)
            // behave consistently with and without a token budget (ADR-002).
            let language = explicit_lang
                .or_else(|| detect_language_from_path(path))
                .or_else(|| detect_language_from_shebang(contents));

            let Some(language) = language else {
                // No language detectable (unknown extension, no shebang match) —
                // degrade to a lossless raw passthrough (ADR-002) instead of
                // erroring with UnsupportedLanguage. Matches the None-branch
                // degrade; the SKIM_DEBUG notice is emitted by process_file once
                // this returns.
                //
                // Honour the token budget (ADR-016: a bound the tool can exceed is
                // not a bound). No tree-sitter grammar is available so mode
                // escalation is impossible, but line-level truncation to fit the
                // budget is both possible and correct. ADR-017: the text is raw at
                // this point, so line counts are in source space.
                let source_line_count = contents.lines().count();
                // Step 1: apply explicit max-lines / last-lines bounds.
                let bounded = passthrough_with_truncation(
                    contents,
                    None,
                    options.trunc.max_lines,
                    options.trunc.last_lines,
                );
                // Step 2: if the bounded output still exceeds the token budget,
                // shrink further. Binary search over head-truncation (max-lines)
                // since no grammar is available for semantic truncation. Explicit
                // iteration ceiling ≤ 64 satisfies CLAUDE.md "every loop has an
                // explicit bound" (log2(N)+1 ≤ 64 for any realistic file size).
                let final_output = match tokens::count_tokens(&bounded) {
                    Ok(tok) if tok <= budget => bounded,
                    _ => {
                        let mut lo = 1usize;
                        let mut hi = source_line_count;
                        for _ in 0..64 {
                            if lo >= hi {
                                break;
                            }
                            let mid = lo + (hi - lo).div_ceil(2);
                            let candidate = passthrough_with_truncation(
                                contents,
                                None,
                                Some(mid),
                                None,
                            );
                            let fits = tokens::count_tokens(&candidate)
                                .map(|c| c <= budget)
                                .unwrap_or(false);
                            if fits {
                                lo = mid;
                            } else {
                                hi = mid.saturating_sub(1);
                            }
                        }
                        passthrough_with_truncation(contents, None, Some(lo), None)
                    }
                };
                return Ok((final_output, options.mode, false, None, true)); // degraded=true
            };

            // AC-10: Token counting for mode selection does NOT include line number annotations.
            // Run cascade WITHOUT line_numbers to select the best mode.
            // reliability-8: pass the source line count so fallback_line_truncate can
            // report a source-space omission count rather than an output-space count.
            let source_line_count = contents.lines().count();
            let (output, mode) = cascade::cascade_for_token_budget(
                options.mode,
                &options.trunc,
                budget,
                language,
                source_line_count,
                transform_file,
            )?;

            // If line numbers requested, re-run the selected mode WITH line_numbers.
            // Use the re-run output directly as the final output (avoids double transform).
            let (final_output, line_map) = if options.line_numbers {
                let config = cascade::build_config_with_opts(mode, &options.trunc, true);
                let (rerun_output, _has_errors, map, _degraded) =
                    transform_with_line_map(contents, language, &config)?;
                (rerun_output, map)
            } else {
                (output, None)
            };

            Ok((final_output, mode, false, line_map, false)) // cascade path: degraded signal N/A
        }
        None => {
            let language = explicit_lang.or_else(|| detect_language_from_path(path));

            // Use transform_with_line_map when we can identify the language
            if let Some(lang) = language {
                let config = cascade::build_config_with_opts(
                    options.mode,
                    &options.trunc,
                    options.line_numbers,
                );
                let (output, has_errors, line_map, degraded) =
                    transform_with_line_map(contents, lang, &config)?;
                Ok((output, options.mode, has_errors, line_map, degraded))
            } else {
                // Language detection failed from extension — try shebang.
                let shebang_lang = detect_language_from_shebang(contents);

                if let Some(lang) = shebang_lang {
                    let config = cascade::build_config_with_opts(
                        options.mode,
                        &options.trunc,
                        options.line_numbers,
                    );
                    let (output, has_errors, line_map, degraded) =
                        transform_with_line_map(contents, lang, &config)?;
                    return Ok((output, options.mode, has_errors, line_map, degraded));
                }

                // No language detectable — degrade to lossless passthrough (ADR-002).
                // The SKIM_DEBUG notice is emitted by process_file after run_transform
                // returns, not here, to avoid duplicate notices when called from
                // process_file (which also checks degraded and emits one notice).
                let output = passthrough_with_truncation(
                    contents,
                    None,
                    options.trunc.max_lines,
                    options.trunc.last_lines,
                );
                Ok((output, options.mode, false, None, true)) // degraded=true
            }
        }
    }
}

/// Try to detect a language from a shebang (`#!`) on the first line of `text`.
///
/// Returns `None` when the first line is absent or not a recognised shebang.
/// Centralises the repeated `text.lines().next().and_then(Language::from_shebang)`
/// pattern used across the processing pipeline.
fn detect_language_from_shebang(text: &str) -> Option<Language> {
    text.lines().next().and_then(Language::from_shebang)
}

/// Apply optional line-count truncation to a raw view for the unknown-language path.
///
/// Only called when language detection failed (ADR-002 lossless passthrough) and
/// no tree-sitter grammar is available. Known-language paths delegate to
/// `enforce_line_bounds` which calls `rskim-core`'s `simple_line_truncate` /
/// `simple_last_line_truncate_with_start` for literal-aware, ADR-016-compliant
/// arithmetic (PF-033: one spelling of the bound, in one place).
///
/// Markers are built by [`elision_marker_line`] so the head form
/// (`… N lines truncated`) and the tail form (`… N lines above`) are spelled
/// exactly as rskim-core spells them: the `#` prefix (language=None), exact
/// omission counts in **source** space (ADR-017; text is raw, so source == output
/// space), and the `SKIM_PASSTHROUGH=1` remedy clause (ADR-011 class 1).
///
/// ADR-016 N=1 carve-out: when N=1, emit 1 content line + 1 marker (2 lines)
/// because spending the only slot on the marker returns a view with no code.
fn passthrough_with_truncation(
    text: &str,
    language: Option<Language>,
    max_lines: Option<usize>,
    last_lines: Option<usize>,
) -> String {
    if let Some(n) = max_lines {
        // Count before allocating: the no-op case (text already within budget)
        // is the common one on the hot path. A plain count avoids building the
        // Vec when the early return fires (performance-2 / CLAUDE.md MUST).
        // split_inclusive('\n') keeps each segment's original \r\n or \n
        // terminator, so the retained portion is byte-faithful (#317 / ADR-002).
        let total = text.split_inclusive('\n').count();
        if total <= n {
            return text.to_string();
        }
        // ADR-016: reserve 1 slot for the marker so --max-lines N ≡ head -N
        // (at most N total lines). N=1 is the documented exception: emit 1
        // content line + 1 marker (2 lines) rather than a bare marker with no
        // code content (ADR-016 N=1 carve-out).
        let keep = if n > 1 { n - 1 } else { n };
        let omitted = total - keep; // source-space count (text is raw; ADR-017)
        let segs: Vec<&str> = text.split_inclusive('\n').collect();
        let marker =
            elision_marker_line(language, omitted, ElidedSide::Truncated, Some(ELISION_HINT));
        // Retained segments already carry their terminators; segs[keep-1] is
        // not the last segment (total > n), so it is guaranteed to end with \n.
        let mut out: String = segs[..keep].concat();
        out.push_str(&marker);
        out.push('\n');
        out
    } else if let Some(n) = last_lines {
        let total = text.split_inclusive('\n').count();
        if total <= n {
            return text.to_string();
        }
        // ADR-016 N=1 carve-out: same as the head path.
        let keep = if n > 1 { n - 1 } else { n };
        let omitted = total - keep; // source-space count (ADR-017)
        let segs: Vec<&str> = text.split_inclusive('\n').collect();
        let marker = elision_marker_line(language, omitted, ElidedSide::Above, Some(ELISION_HINT));
        // Tail segments carry their original terminators (including \r\n).
        let tail_start = segs.len().saturating_sub(keep);
        let mut out = marker;
        if keep > 0 {
            out.push('\n');
            out.push_str(&segs[tail_start..].concat());
        }
        out
    } else {
        text.to_string()
    }
}

/// Enforce `--max-lines` / `--last-lines` bounds on the guardrail-served raw text.
///
/// Only called when `guardrail_triggered=true`: the guardrail returned raw
/// `contents` because the compressed view was larger, and the raw content still
/// must honour the line bound.  For the non-guardrail path (compressed view
/// selected), the core transform already applied the bound correctly, so no
/// outer enforcement is needed (PF-033: enforcing the same bound at two layers
/// is not idempotent when the inner layer emits synthetic marker lines).
///
/// Returns `(bounded_text, Some(line_map))` when truncation fired and the map
/// is usable for `-n` annotation, or `(text, None)` when the text fits within
/// the budget or when the language is unknown (no literal-aware grammar).
///
/// For `Some(language)`, delegates entirely to `rskim-core`'s
/// `simple_line_truncate` / `simple_last_line_truncate_with_start` so the
/// ADR-016 N=1 carve-out, source-space elision count (ADR-017), and the #511
/// literal-aware pull-back are inherited without re-implementing them.
///
/// For `None` language, falls back to `passthrough_with_truncation` (fixed
/// N=1 arithmetic, no literal-awareness — no grammar is available).
fn enforce_line_bounds(
    text: &str,
    language: Option<Language>,
    trunc: &crate::cascade::TruncationOptions,
    source_text: &str,
) -> (String, Option<Vec<usize>>) {
    if let Some(n) = trunc.max_lines {
        match language {
            Some(lang) => {
                // Delegate to core: literal-aware, ADR-016 N=1 carve-out,
                // source-space elision count (PF-033 / ADR-017).
                let source_count = source_text.lines().count();
                match simple_line_truncate(text, lang, n, Some(ELISION_HINT), Some(source_count)) {
                    Ok(truncated) => {
                        // Build a source-space line map by matching truncated lines
                        // back to the source — marker lines get 0 (no annotation).
                        let map = build_annotation_map_by_matching(source_text, &truncated);
                        (truncated, Some(map))
                    }
                    Err(_) => (text.to_string(), None),
                }
            }
            None => (
                passthrough_with_truncation(text, None, Some(n), None),
                None,
            ),
        }
    } else if let Some(n) = trunc.last_lines {
        match language {
            Some(lang) => {
                let source_count = source_text.lines().count();
                match simple_last_line_truncate_with_start(
                    text,
                    lang,
                    n,
                    Some(ELISION_HINT),
                    Some(source_count),
                ) {
                    Ok((truncated, start)) => {
                        // PF-019 / complexity-2: `start` comes from the truncator
                        // (the single authority on where the window begins after
                        // any #511 forward-move). Recomputing here would drift.
                        let n_content = truncated.lines().count().saturating_sub(1);
                        let mut map = Vec::with_capacity(1 + n_content);
                        map.push(0usize); // marker line — no annotation
                        for i in 0..n_content {
                            map.push(start + 1 + i); // 1-indexed source lines
                        }
                        (truncated, Some(map))
                    }
                    Err(_) => (text.to_string(), None),
                }
            }
            None => (
                passthrough_with_truncation(text, None, None, Some(n)),
                None,
            ),
        }
    } else {
        (text.to_string(), None)
    }
}

/// Build a source-space line map by matching `truncated` lines to `source` lines.
///
/// Content lines are verbatim source lines matched monotonically in order.
/// Marker lines (not present in source) receive source position 0, which
/// suppresses `-n` annotation per the `format_with_line_numbers` contract.
fn build_annotation_map_by_matching(source: &str, truncated: &str) -> Vec<usize> {
    let source_lines: Vec<&str> = source.lines().collect();
    let mut src_pos = 0usize;
    truncated
        .lines()
        .map(|line| {
            for (off, &sl) in source_lines[src_pos..].iter().enumerate() {
                if sl == line {
                    let num = src_pos + off + 1; // 1-indexed
                    src_pos += off + 1;
                    return num;
                }
            }
            0usize // marker or unmatched line
        })
        .collect()
}

/// Build the [`ProcessResult`] for the unknown-language stdin passthrough (ADR-002).
///
/// Extracted from [`process_stdin`] to collapse the duplicated passthrough
/// `ProcessResult` construction and `stdin_raw` retention. Emits the single
/// SKIM_DEBUG degrade notice, then returns a lossless windowed passthrough.
///
/// The caller must first rule out the `--filename` hard-error case (an explicit
/// but unrecognised extension); this path is only reached when no language is
/// detectable *and* no `--filename` hint was given.
///
/// `stdin_raw` follows the main-path invariant: `Some(buffer)` iff `!show_stats`
/// (stdin cannot be re-read, so the buffer must travel with the result for
/// background tokenization).
fn stdin_passthrough_result(buffer: String, options: &ProcessOptions) -> ProcessResult {
    crate::debug_log!(
        "[skim] notice: unknown language for stdin — degraded to lossless passthrough. \
         Use --language to specify, or SKIM_PASSTHROUGH=1 to bypass."
    );
    let output = passthrough_with_truncation(
        &buffer,
        None,
        options.trunc.max_lines,
        options.trunc.last_lines,
    );
    let stdin_raw = if !options.show_stats {
        Some(buffer)
    } else {
        None
    };
    ProcessResult {
        output,
        original_tokens: None,
        transformed_tokens: None,
        guardrail_triggered: false,
        parse_tier: Some("passthrough"),
        language: None,
        stdin_raw,
        view_differs: false, // passthrough output is byte-identical to input
    }
}

/// Process stdin input and return transformed content with optional token statistics.
///
/// Reads from stdin with a size limit, resolves the language from `--language` or
/// `--filename`, transforms the source (with optional token-budget cascade), and
/// computes token stats when `show_stats` is enabled.
pub(crate) fn process_stdin(
    options: ProcessOptions,
    filename_hint: Option<&str>,
) -> anyhow::Result<ProcessResult> {
    let mut buffer = String::with_capacity(64 * 1024);
    let bytes_read = io::stdin()
        .take(MAX_INPUT_SIZE as u64 + 1)
        .read_to_string(&mut buffer)?;

    if bytes_read > MAX_INPUT_SIZE {
        anyhow::bail!(
            "Input too large: {} bytes exceeds maximum of {} bytes ({}MB)",
            bytes_read,
            MAX_INPUT_SIZE,
            MAX_INPUT_SIZE / 1024 / 1024
        );
    }

    let filename_lang = filename_hint.and_then(|f| Language::from_path(Path::new(f)));

    // Shebang detection: if no language from explicit flag or filename extension,
    // sniff the first line of the stdin buffer.
    let shebang_lang = if options.explicit_lang.is_none() && filename_lang.is_none() {
        detect_language_from_shebang(&buffer)
    } else {
        None
    };

    let language_or_none = options.explicit_lang.or(filename_lang).or(shebang_lang);

    // ADR-002: when no language is detectable for plain stdin (no --language,
    // no --filename, no shebang), degrade to a lossless passthrough rather than
    // erroring — consistent with the file path behaviour in run_transform.
    // --filename with an unrecognised extension is still an error unless a shebang
    // in the stdin content provides a recognised language override: the user gave
    // an explicit hint that skim cannot honour without a shebang fallback.
    let Some(language) = language_or_none else {
        if let Some(fname) = filename_hint {
            // Explicit --filename with unknown extension → hard error (exit 3).
            // Returning SkimError::UnsupportedLanguage lets the exit-code map in
            // main.rs produce exit 3, consistent with the file-path code path
            // (transform_auto_with_config also returns UnsupportedLanguage for
            // unknown extensions). Avoids a hand-maintained extension list that
            // drifts from Language::from_extension (the source of truth).
            return Err(
                rskim_core::SkimError::UnsupportedLanguage(Path::new(fname).to_path_buf()).into(),
            );
        }
        // No --filename, no --language, no shebang — degrade to lossless passthrough.
        return Ok(stdin_passthrough_result(buffer, &options));
    };

    let (transformed, stdin_has_errors, stdin_line_map, stdin_degraded) = match options
        .trunc
        .token_budget
    {
        Some(budget) => {
            // AC-10: Cascade mode selection without line numbers, then re-run with line numbers.
            // reliability-8: pass the source line count so fallback_line_truncate can
            // report a source-space omission count rather than an output-space count.
            let source_line_count = buffer.lines().count();
            let (output, mode) = cascade::cascade_for_token_budget(
                options.mode,
                &options.trunc,
                budget,
                language,
                source_line_count,
                |config| Ok(Some(transform_with_config(&buffer, language, config)?)),
            )?;
            // Use the re-run output directly as the final output (avoids double transform).
            let (cascade_output, line_map) = if options.line_numbers {
                let config = cascade::build_config_with_opts(mode, &options.trunc, true);
                let (rerun, _errs, map, _degraded) =
                    transform_with_line_map(&buffer, language, &config)?;
                (rerun, map)
            } else {
                (output, None)
            };
            (cascade_output, false, line_map, false) // cascade path: degraded signal N/A
        }
        None => {
            let config =
                cascade::build_config_with_opts(options.mode, &options.trunc, options.line_numbers);
            let (output, has_errors, line_map, degraded) =
                transform_with_line_map(&buffer, language, &config)?;
            (output, has_errors, line_map, degraded)
        }
    };

    // Emit notice when debug output is enabled and the transform degraded to passthrough
    // due to a structural safety cap. The notice goes to stderr to avoid polluting stdout.
    if stdin_degraded {
        crate::debug_log!(
            "[skim] notice: file too large to compress in {:?} mode \
             (structural cap exceeded) — degraded to passthrough",
            options.mode
        );
    }

    // Determine parse quality tier before guardrail.
    let parse_tier = Some(parse_tier_from(
        options.mode,
        stdin_has_errors,
        stdin_degraded,
    ));

    // Apply output guardrail: if compressed output is larger than raw, emit raw instead.
    // Same protection as process_file; token counting happens after so stats reflect
    // the final output. Guardrail comparison uses UN-annotated output.
    //
    // ADR-001: the guardrail also runs when --tokens is set (the cascade path above
    // may have selected a mode that, after elision markers, is still larger than raw).
    // The clone is intentional: disclosure affects view selection and we need `buffer`
    // intact as the raw baseline for view_differs and the transparency marker.
    let (final_output, guardrail_triggered) = if options.mode != Mode::Full {
        let outcome = crate::output::guardrail::apply_to_stderr(buffer.clone(), transformed)?;
        let triggered = outcome.was_triggered();
        (outcome.into_output(), triggered)
    } else {
        (transformed, false)
    };

    // consistency-7: apply --max-lines / --last-lines to stdin when the guardrail
    // served raw (the compressed path already applies the bound via the core
    // transform; the raw path does not — PF-033 / ADR-016).
    //
    // enforce_line_bounds also returns the source-space line map for -n annotation
    // (PF-019 / complexity-2: the identity map used for guardrail-triggered paths
    // is wrong after --last-lines truncation, labelling tail lines as 1..N instead
    // of their actual source positions).
    let (final_output, post_trunc_map) =
        if guardrail_triggered
            && (options.trunc.max_lines.is_some() || options.trunc.last_lines.is_some())
        {
            enforce_line_bounds(&final_output, Some(language), &options.trunc, &buffer)
        } else {
            (final_output, None)
        };

    // Transparency marker: did the transformation produce a different view?
    // Computed AFTER post-guardrail truncation so a bound that cuts makes the
    // view lossy (view_differs = true), triggering the ADR-011 class-1 marker.
    // When the guardrail served raw and no truncation was applied,
    // final_output == buffer and view_differs is correctly false.
    //
    // B3 / ADR-011 class 1: view_differs is unconditional — does NOT require
    // SKIM_REWRITTEN_FROM to be set.
    let view_differs = final_output != buffer;

    // Apply line number formatting AFTER guardrail and post-guardrail truncation,
    // BEFORE token stats.
    // post_trunc_map (from enforce_line_bounds) takes priority over stdin_line_map
    // via .or(): it carries correct source line numbers from the truncator's window
    // start, which the identity fallback in apply_line_numbers cannot provide
    // after --last-lines moves the window (PF-019 / complexity-2).
    let combined_map = post_trunc_map.or(stdin_line_map);
    let final_output = apply_line_numbers(
        final_output,
        options.line_numbers,
        guardrail_triggered,
        combined_map,
    );

    // Only pay the tiktoken BPE cost on the main thread when --show-stats
    // is set. Analytics background threads compute their own token counts.
    let (orig_tokens, trans_tokens) = if options.show_stats {
        count_token_pair(&buffer, &final_output)
    } else {
        (None, None)
    };

    // Retain the raw buffer for analytics background tokenization only when
    // counts are not already known (i.e. !show_stats). Stdin cannot be re-read,
    // so the buffer must travel with the result.
    //
    // Invariant: stdin_raw is Some iff !show_stats; orig_tokens/trans_tokens are
    // Some iff show_stats (when the tokenizer is available). These two conditions
    // are mutually exclusive by construction: show_stats drives count_token_pair
    // above, and its negation drives stdin_raw here.
    //
    // The assert pins the always-guaranteed half: if we are NOT running show_stats,
    // counts must be None (we never computed them). The reverse (show_stats → Some)
    // is best-effort and depends on the tokenizer succeeding, so is not asserted.
    debug_assert!(
        options.show_stats || orig_tokens.is_none(),
        "BUG(process_stdin): show_stats=false but orig_tokens is Some — \
         token counts must not be present when show_stats is false \
         (stdin_raw invariant violated)"
    );
    let stdin_raw = if !options.show_stats {
        Some(buffer)
    } else {
        None
    };

    Ok(ProcessResult {
        output: final_output,
        original_tokens: orig_tokens,
        transformed_tokens: trans_tokens,
        guardrail_triggered,
        parse_tier,
        language: Some(language),
        stdin_raw,
        view_differs,
    })
}

/// Process a single file and return transformed content with optional token statistics.
pub(crate) fn process_file(path: &Path, options: ProcessOptions) -> anyhow::Result<ProcessResult> {
    if let Some(result) = try_cached_result(path, &options)? {
        return Ok(result);
    }

    let contents = read_and_validate(path)?;
    let (result, mode_used, has_errors, line_map, degraded) =
        run_transform(&contents, path, &options)?;

    // Effective language, resolved exactly as run_transform resolves it: explicit
    // override, then extension, then a shebang sniff.  `None` means detection
    // failed, which drives both the degrade notice below and the comment prefix
    // of the post-guardrail elision marker.
    let language = options
        .explicit_lang
        .or_else(|| detect_language_from_path(path))
        .or_else(|| detect_language_from_shebang(&contents));

    // Emit notice when debug output is enabled and the transform degraded to passthrough.
    // Two distinct degrade reasons: unknown language (no extension/shebang match)
    // or file too large to compress (structural safety cap exceeded).
    if degraded {
        if language.is_some() {
            crate::debug_log!(
                "[skim] notice: file too large to compress in {:?} mode \
                 (structural cap exceeded) — degraded to passthrough",
                options.mode
            );
        } else {
            crate::debug_log!(
                "[skim] notice: unknown language for '{}' — degraded to lossless passthrough. \
                 Use --language to specify, or SKIM_PASSTHROUGH=1 to bypass.",
                path.display()
            );
        }
    }

    // Determine parse quality tier before guardrail (guardrail may swap output,
    // but the parse tier reflects the transformation, not the final selection).
    let parse_tier = Some(parse_tier_from(options.mode, has_errors, degraded));

    // Apply output guardrail: if compressed output is larger than raw, emit raw instead.
    // Token counting happens AFTER this decision so stats reflect the final output.
    // Guardrail comparison uses UN-annotated output (before line number formatting).
    //
    // ADR-001: the guardrail also runs when --tokens is set (the cascade may have
    // selected a mode that, after elision markers, is still larger than raw).
    // The clone is intentional: disclosure affects view selection and we need
    // `contents` intact as the raw baseline for view_differs and the transparency
    // marker below.
    let (final_output, guardrail_triggered) = if options.mode != Mode::Full {
        let outcome = crate::output::guardrail::apply_to_stderr(contents.clone(), result)?;
        let triggered = outcome.was_triggered();
        (outcome.into_output(), triggered)
    } else {
        (result, false)
    };

    // Post-guardrail line-bound enforcement (#317 / ADR-002 / PF-033).
    //
    // The guardrail may return raw `contents` when the compressed output (with
    // elision markers) exceeded raw in tokens.  Raw contents are still subject to
    // `--max-lines` / `--last-lines`; apply the bound here ONLY when the guardrail
    // fired so that raw is capped.
    //
    // When the guardrail did NOT fire (compressed output selected), the core
    // transform already applied the bound correctly via `simple_line_truncate` /
    // `simple_last_line_truncate_with_start`.  The outer pass must NOT re-apply on
    // already-bounded output: PF-033 shows that re-applying over text that already
    // has a synthetic marker line counts the marker as a source line (wrong
    // coordinate space — ADR-017) and undoes the ADR-016 N=1 carve-out (the inner
    // pass emits 2 lines for N=1 but the outer pass then sees 2 > 1 and keeps
    // only the marker alone with zero content lines).
    //
    // enforce_line_bounds also returns the source-space line map for -n annotation
    // (complexity-2 / PF-019): using the identity map on a tail-truncated view
    // labels retained lines as 1..N instead of their actual source positions.
    let (final_output, post_trunc_map) =
        if guardrail_triggered
            && (options.trunc.max_lines.is_some() || options.trunc.last_lines.is_some())
        {
            enforce_line_bounds(&final_output, language, &options.trunc, &contents)
        } else {
            (final_output, None)
        };

    // Transparency marker: did transformation produce a different view than raw bytes?
    // Computed AFTER post-guardrail truncation so a bound that cuts the raw view
    // correctly makes view_differs true (ADR-011 class 1: the truncation is lossy,
    // so the marker must fire).  When the guardrail served raw and no truncation was
    // applied, final_output == contents and view_differs is correctly false.
    //
    // B3 / ADR-011 class 1: unconditional — does NOT require SKIM_REWRITTEN_FROM.
    let view_differs = final_output != contents;

    // Apply line number formatting AFTER guardrail and post-guardrail truncation,
    // BEFORE cache write and token stats.
    // AC-12: Cache key includes line_numbers (handled in cache::read_cache/write_cache).
    //
    // post_trunc_map (from enforce_line_bounds) takes priority over line_map via .or():
    // it carries correct source line numbers from the truncator's window start,
    // which the identity fallback in apply_line_numbers cannot provide after
    // --last-lines moves the window (PF-019 / complexity-2).
    let combined_map = post_trunc_map.or(line_map);
    let final_output = apply_line_numbers(
        final_output,
        options.line_numbers,
        guardrail_triggered,
        combined_map,
    );

    // Only pay the tiktoken BPE cost on the main thread when --show-stats
    // is set. Analytics background threads compute their own token counts.
    let (orig_tokens, trans_tokens) = if options.show_stats {
        count_token_pair(&contents, &final_output)
    } else {
        (None, None)
    };

    // Cache the transform result (post-guardrail, post-line-number-formatting).
    // Cache write failures are non-fatal; don't fail the transformation.
    if options.use_cache {
        let effective_mode = (mode_used != options.mode).then_some(mode_used);
        let _ = cache::write_cache(&cache::CacheWriteParams {
            path,
            mode: options.mode,
            content: &final_output,
            original_tokens: orig_tokens,
            transformed_tokens: trans_tokens,
            trunc: options.trunc,
            effective_mode,
            parse_tier: parse_tier.map(str::to_string),
            line_numbers: options.line_numbers,
            // consistency-2: store the authoritative view_differs so the
            // cache-hit path does not have to re-derive it from the mode.
            view_differs,
        });
    }

    // `language` doubles as the analytics language: for unknown-language
    // passthrough it is None (zero-savings row).
    Ok(ProcessResult {
        output: final_output,
        original_tokens: orig_tokens,
        transformed_tokens: trans_tokens,
        guardrail_triggered,
        parse_tier,
        language,
        stdin_raw: None,
        view_differs,
    })
}

/// Read a file and validate it doesn't exceed the maximum input size.
///
/// Public thin wrapper over `read_and_validate` for use by the background
/// analytics re-read path (`analytics::RawSource::Reread`).  Reuses the
/// 50 MB guard and naturally rejects TOCTOU-grown files.
pub(crate) fn read_source(path: &std::path::Path) -> anyhow::Result<String> {
    read_and_validate(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // count_token_pair tests
    // ========================================================================

    #[test]
    fn count_token_pair_returns_some_for_valid_input() {
        let (orig, trans) = count_token_pair("hello world", "hello");
        assert!(orig.is_some(), "original tokens should be Some");
        assert!(trans.is_some(), "transformed tokens should be Some");
        assert!(
            orig.unwrap() > trans.unwrap(),
            "original should have more tokens than transformed"
        );
    }

    #[test]
    fn count_token_pair_returns_some_for_empty_strings() {
        let (orig, trans) = count_token_pair("", "");
        assert_eq!(orig, Some(0));
        assert_eq!(trans, Some(0));
    }

    #[test]
    fn count_token_pair_original_equals_transformed_for_identical_input() {
        let text = "fn main() { println!(\"hello\"); }";
        let (orig, trans) = count_token_pair(text, text);
        assert_eq!(orig, trans);
    }

    // ========================================================================
    // report_token_stats tests
    // ========================================================================

    #[test]
    fn report_token_stats_does_not_panic_with_none_values() {
        // Should be a no-op when tokens are None
        report_token_stats(None, None, "");
        report_token_stats(Some(100), None, "");
        report_token_stats(None, Some(50), "");
    }

    #[test]
    fn report_token_stats_does_not_panic_with_valid_values() {
        // Should write to stderr without panicking
        report_token_stats(Some(1000), Some(200), " (test)");
    }

    // ========================================================================
    // read_and_validate tests
    // ========================================================================

    #[test]
    fn read_and_validate_rejects_nonexistent_file() {
        let result = read_and_validate(Path::new("/nonexistent/file.txt"));
        assert!(result.is_err());
    }

    // ========================================================================
    // parse_tier_from tests (B4-B5)
    // ========================================================================

    #[test]
    fn test_parse_tier_passthrough() {
        assert_eq!(parse_tier_from(Mode::Full, false, false), "passthrough");
        assert_eq!(parse_tier_from(Mode::Full, true, false), "passthrough");
        // degraded is irrelevant for Full mode (always "passthrough")
        assert_eq!(parse_tier_from(Mode::Full, false, true), "passthrough");
    }

    #[test]
    fn test_parse_tier_degraded() {
        assert_eq!(parse_tier_from(Mode::Structure, true, false), "degraded");
        assert_eq!(parse_tier_from(Mode::Signatures, true, false), "degraded");
        assert_eq!(parse_tier_from(Mode::Minimal, true, false), "degraded");
    }

    #[test]
    fn test_parse_tier_full() {
        assert_eq!(parse_tier_from(Mode::Structure, false, false), "full");
        assert_eq!(parse_tier_from(Mode::Signatures, false, false), "full");
        assert_eq!(parse_tier_from(Mode::Types, false, false), "full");
    }

    #[test]
    fn test_parse_tier_complexity_limited() {
        // A file degraded via ComplexityLimit (oversized) must report "degraded",
        // not "full", so analytics and callers are not misled. (#A7 tier-mislabel fix)
        assert_eq!(parse_tier_from(Mode::Structure, false, true), "degraded");
        assert_eq!(parse_tier_from(Mode::Pseudo, false, true), "degraded");
        assert_eq!(parse_tier_from(Mode::Minimal, false, true), "degraded");
    }

    // ========================================================================
    // apply_line_numbers tests
    // ========================================================================

    /// Branch: line_numbers disabled — output is returned unchanged regardless of
    /// guardrail_triggered or computed_map.
    #[test]
    fn apply_line_numbers_disabled_returns_output_unchanged() {
        let output = "fn foo() {}\nfn bar() {}\n".to_string();
        let result = apply_line_numbers(output.clone(), false, false, Some(vec![1, 2]));
        assert_eq!(
            result, output,
            "disabled line numbers must not modify output"
        );

        let result2 = apply_line_numbers(output.clone(), false, true, None);
        assert_eq!(
            result2, output,
            "disabled line numbers must not modify output even when guardrail triggered"
        );
    }

    /// Branch: guardrail_triggered with no computed_map — identity map is applied.
    ///
    /// When `computed_map` is `Some`, it takes priority (post-guardrail truncation
    /// returns the correct source-space map; the identity map would be wrong for
    /// --last-lines). This test covers the no-map guardrail path only.
    #[test]
    fn apply_line_numbers_guardrail_uses_identity_map() {
        let output = "line one\nline two\n".to_string();
        // computed_map is None here; the guardrail path must build its own identity map.
        let result = apply_line_numbers(output.clone(), true, true, None);
        // Identity map annotates each line with its 1-based output line number.
        // format_with_line_numbers uses "<n>\t<content>" format.
        assert!(
            result.contains("1\t") && result.contains("2\t"),
            "guardrail path must annotate both output lines; got: {result:?}"
        );
        // The raw content must still be present.
        assert!(
            result.contains("line one"),
            "output text must survive annotation"
        );
    }

    /// Branch: computed_map is None (serde non-full modes or language detection failure) —
    /// output is returned unannotated even when line_numbers is true.
    #[test]
    fn apply_line_numbers_none_map_skips_annotation() {
        let output = "key: value\n".to_string();
        let result = apply_line_numbers(output.clone(), true, false, None);
        assert_eq!(
            result, output,
            "None computed_map must skip line-number annotation"
        );
    }

    /// Branch: computed_map is Some — the provided map is forwarded to
    /// format_with_line_numbers to produce annotated output.
    #[test]
    fn apply_line_numbers_some_map_annotates_output() {
        // Simulate a 2-line transform output that maps to source lines 1 and 5.
        let output = "fn foo() {}\nfn bar() {}\n".to_string();
        let map = vec![1usize, 5];
        let result = apply_line_numbers(output.clone(), true, false, Some(map));
        // The annotation must include the source line numbers from the provided map.
        // format_with_line_numbers uses "<n>\t<content>" format.
        assert!(
            result.contains("1\t") && result.contains("5\t"),
            "provided map line numbers must appear in output; got: {result:?}"
        );
        assert!(
            result.contains("fn foo()"),
            "content must survive annotation"
        );
    }

    // ========================================================================
    // stdin_passthrough_result tests (Issue C refactor: characterization)
    // ========================================================================

    /// Build ProcessOptions for the stdin-passthrough helper tests.
    fn passthrough_opts(show_stats: bool) -> ProcessOptions {
        ProcessOptions {
            mode: Mode::Structure,
            explicit_lang: None,
            use_cache: false,
            show_stats,
            trunc: TruncationOptions::default(),
            line_numbers: false,
        }
    }

    /// !show_stats → the raw buffer is retained for background tokenization and
    /// the ProcessResult carries the passthrough tier with no language/tokens.
    #[test]
    fn stdin_passthrough_result_retains_buffer_when_no_stats() {
        let src = "line one\nline two\n";
        let result = stdin_passthrough_result(src.to_string(), &passthrough_opts(false));

        assert_eq!(result.parse_tier, Some("passthrough"));
        assert_eq!(result.language, None);
        assert!(!result.guardrail_triggered);
        assert_eq!(result.original_tokens, None);
        assert_eq!(result.transformed_tokens, None);
        // No truncation options → lossless passthrough.
        assert_eq!(result.output, src);
        // Buffer retained (stdin cannot be re-read).
        assert_eq!(result.stdin_raw.as_deref(), Some(src));
    }

    /// show_stats → the buffer is NOT retained (counts are computed on the main
    /// thread), preserving the stdin_raw invariant.
    #[test]
    fn stdin_passthrough_result_drops_buffer_when_stats() {
        let src = "alpha\nbeta\n";
        let result = stdin_passthrough_result(src.to_string(), &passthrough_opts(true));

        assert_eq!(result.stdin_raw, None);
        assert_eq!(result.output, src);
        assert_eq!(result.parse_tier, Some("passthrough"));
    }

    /// max_lines is honoured on the passthrough with the hinted elision marker.
    #[test]
    fn stdin_passthrough_result_honours_max_lines() {
        let src = "one\ntwo\nthree\nfour\n";
        let mut opts = passthrough_opts(false);
        opts.trunc.max_lines = Some(2);

        let result = stdin_passthrough_result(src.to_string(), &opts);

        assert!(
            result
                .output
                .contains(&format!("# ... (3 lines truncated) — {ELISION_HINT}")),
            "truncated passthrough must carry the canonical hinted marker: {:?}",
            result.output
        );
        // Buffer retention is unaffected by truncation.
        assert_eq!(result.stdin_raw.as_deref(), Some(src));
    }

    // ========================================================================
    // passthrough_with_truncation: CRLF byte-fidelity tests (Issue 1 fold-in)
    // ========================================================================

    /// The lossless passthrough must be byte-faithful: CRLF retained lines must
    /// keep their \r\n, not be silently converted to \n.  The old lines()+join("\n")
    /// approach stripped \r, violating #317 and ADR-002.
    #[test]
    fn passthrough_with_truncation_preserves_crlf() {
        let crlf = "line1\r\nline2\r\nline3\r\nline4\r\n";

        // max_lines: first retained line must keep \r\n.
        // n=2 → keep=1 → retain "line1\r\n", omit 3 lines.
        let out = passthrough_with_truncation(crlf, None, Some(2), None);
        assert!(
            out.starts_with("line1\r\n"),
            "max_lines: \\r\\n must be preserved in retained line; got: {out:?}"
        );
        assert!(
            out.contains("lines truncated"),
            "max_lines: elision marker must be present: {out:?}"
        );

        // last_lines: last retained tail line must keep \r\n.
        // n=2 → keep=1 → retain "line4\r\n", omit 3 lines above.
        let out2 = passthrough_with_truncation(crlf, None, None, Some(2));
        assert!(
            out2.ends_with("line4\r\n"),
            "last_lines: \\r\\n must be preserved in tail line; got: {out2:?}"
        );
        assert!(
            out2.contains("lines above"),
            "last_lines: elision marker must be present: {out2:?}"
        );

        // No truncation: entire content returned byte-for-byte.
        let out3 = passthrough_with_truncation(crlf, None, None, None);
        assert_eq!(out3, crlf, "no truncation must be byte-faithful");
    }

    /// LF-only input is unaffected by the split_inclusive change.
    #[test]
    fn passthrough_with_truncation_lf_unaffected() {
        let lf = "alpha\nbeta\ngamma\ndelta\n";

        let out = passthrough_with_truncation(lf, None, Some(2), None);
        assert!(
            out.starts_with("alpha\n"),
            "LF: first line must end with \\n: {out:?}"
        );
        assert!(out.contains("lines truncated"), "{out:?}");

        let out2 = passthrough_with_truncation(lf, None, None, Some(2));
        assert!(
            out2.ends_with("delta\n"),
            "LF: last line must end with \\n: {out2:?}"
        );
        assert!(out2.contains("lines above"), "{out2:?}");
    }

    /// The elision marker adopts the file's own comment syntax and the canonical
    /// A-form phrasing, matching what rskim-core emits on the compressed path.
    /// `None` (detection failed) keeps the neutral `#` prefix.
    #[test]
    fn passthrough_with_truncation_uses_language_comment_prefix() {
        let src = "one\ntwo\nthree\nfour\n";

        let ts = passthrough_with_truncation(src, Some(Language::TypeScript), Some(2), None);
        assert!(
            ts.ends_with(&format!("// ... (3 lines truncated) — {ELISION_HINT}\n")),
            "TypeScript head marker: {ts:?}"
        );

        let md = passthrough_with_truncation(src, Some(Language::Markdown), Some(2), None);
        assert!(
            md.ends_with(&format!(
                "<!-- ... (3 lines truncated) — {ELISION_HINT} -->\n"
            )),
            "Markdown head marker: {md:?}"
        );

        let py = passthrough_with_truncation(src, Some(Language::Python), None, Some(2));
        assert!(
            py.starts_with(&format!("# ... (3 lines above) — {ELISION_HINT}\n")),
            "Python tail marker: {py:?}"
        );

        let unknown = passthrough_with_truncation(src, None, Some(2), None);
        assert!(
            unknown.ends_with(&format!("# ... (3 lines truncated) — {ELISION_HINT}\n")),
            "unknown language falls back to the neutral `#` prefix: {unknown:?}"
        );
    }
}
