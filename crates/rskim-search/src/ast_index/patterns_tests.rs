//! Tests for the AST Pattern Library catalog.
//!
//! # Test IDs (from acceptance criteria)
//!
//! - F6: Catalog integrity (≥25 patterns, all categories non-empty, unique names, lookup hit/miss)
//! - F7: GOLD honesty gate (each resolved bigram/trigram is in the example's produced n-grams)
//! - F8: Approximate patterns explicitly mention "approximation" or "structural" in description
//! - A6: Synthetic name resolution (resolve_synthetic_name roundtrip)

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashSet;

use rskim_core::Language;

use super::*;
use crate::ast_index::structural::{DEEP_NODE, EMPTY_BODY, LARGE_BODY, MANY_PARAMS, bucket_label};
use crate::ast_index::{
    AstBigram, AstTrigram, extract_ast_ngrams_with_metrics, linearize_source, vocab_resolve,
};

// ============================================================================
// F6: Catalog integrity
// ============================================================================

#[test]
fn f6_at_least_25_patterns() {
    assert!(
        PATTERNS.len() >= 25,
        "pattern catalog must have >= 25 entries, got {}",
        PATTERNS.len()
    );
}

#[test]
fn f6_all_categories_non_empty() {
    let categories = [
        PatternCategory::ErrorHandling,
        PatternCategory::Performance,
        PatternCategory::Concurrency,
        PatternCategory::Quality,
        PatternCategory::Structure,
    ];
    for cat in categories {
        let count = PATTERNS.iter().filter(|p| p.category == cat).count();
        assert!(
            count > 0,
            "category {:?} must have at least one pattern",
            cat
        );
    }
}

#[test]
fn f6_pattern_names_are_unique() {
    let mut seen: HashSet<&str> = HashSet::new();
    for p in PATTERNS {
        assert!(seen.insert(p.name), "duplicate pattern name: '{}'", p.name);
    }
}

#[test]
fn f6_lookup_hit_and_miss() {
    // Hit: every pattern in the catalog must be reachable by name
    for p in PATTERNS {
        let result = lookup_pattern(p.name);
        assert!(result.is_ok(), "lookup_pattern('{}') must succeed", p.name);
        assert_eq!(
            result.unwrap().name,
            p.name,
            "lookup_pattern('{}') must return the correct pattern",
            p.name
        );
    }

    // Miss: a random non-existent name must return Err
    let miss = lookup_pattern("__not_a_real_pattern__");
    assert!(
        miss.is_err(),
        "lookup_pattern for unknown name must return Err"
    );
}

#[test]
fn f6_all_patterns_have_at_least_one_ngram() {
    for p in PATTERNS {
        let total = p.bigrams.len() + p.trigrams.len();
        assert!(
            total > 0,
            "pattern '{}' must declare at least one bigram or trigram",
            p.name
        );
    }
}

// ============================================================================
// F7: GOLD honesty gate
//
// For EVERY pattern:
// 1. linearize example → extract n-grams (with metrics, so synthetic markers appear)
// 2. Resolve the declared bigrams/trigrams
// 3. Every resolved bigram/trigram must appear in the produced n-gram set
// 4. Skip gracefully when a bigram/trigram can't be resolved (unknown vocab word)
//    so that vocab differences across platforms don't fail the test suite
// ============================================================================

/// Helper: run linearize+extract on source, return all bigram keys in a HashSet.
fn extract_bigrams(source: &str, lang: Language) -> HashSet<AstBigram> {
    let result = linearize_source(source, lang).expect("linearize_source must not fail");
    let (set, _) = extract_ast_ngrams_with_metrics(&result.nodes, lang);
    set.bigrams.iter().map(|e| e.ngram).collect()
}

/// Helper: run linearize+extract on source, return all trigram keys in a HashSet.
fn extract_trigrams(source: &str, lang: Language) -> HashSet<AstTrigram> {
    let result = linearize_source(source, lang).expect("linearize_source must not fail");
    let (set, _) = extract_ast_ngrams_with_metrics(&result.nodes, lang);
    set.trigrams.iter().map(|e| e.ngram).collect()
}

#[test]
fn f7_gold_all_patterns() {
    for p in PATTERNS {
        let produced_bigrams = extract_bigrams(p.example, p.example_lang);
        let produced_trigrams = extract_trigrams(p.example, p.example_lang);

        // Resolved bigrams: every one must be in the produced set.
        // If a bigram has unresolvable names, resolved_bigrams() drops it —
        // we additionally track how many declared bigrams resolved to detect
        // patterns that fully resolve but still fail (GOLD violation).
        let resolved_bs = p.resolved_bigrams();
        let declared_bs = p.bigrams.len();
        let resolved_bs_count = resolved_bs.len();

        for &bigram in &resolved_bs {
            assert!(
                produced_bigrams.contains(&bigram),
                "GOLD VIOLATION for pattern '{}': declared bigram ({}, {}) = key {:?} \
                 is NOT in the n-gram set produced from the example.\n\
                 Produced bigrams count: {}\n\
                 This means either the example does not exhibit the pattern or \
                 the bigram name strings are wrong.",
                p.name,
                // Find which pair produced this bigram (reverse-lookup for diagnostics)
                p.bigrams
                    .iter()
                    .find(|(a, b)| {
                        resolve_kind_name(a)
                            .zip(resolve_kind_name(b))
                            .map(|(pa, pb)| AstBigram::encode(pa, pb) == bigram)
                            .unwrap_or(false)
                    })
                    .map(|(a, _)| a)
                    .unwrap_or(&"?"),
                p.bigrams
                    .iter()
                    .find(|(a, b)| {
                        resolve_kind_name(a)
                            .zip(resolve_kind_name(b))
                            .map(|(pa, pb)| AstBigram::encode(pa, pb) == bigram)
                            .unwrap_or(false)
                    })
                    .map(|(_, b)| b)
                    .unwrap_or(&"?"),
                bigram.key(),
                produced_bigrams.len(),
            );
        }

        // Warn in test output if some bigrams couldn't resolve (vocab differences)
        // but don't fail the test — unresolvable names are a separate concern.
        if resolved_bs_count < declared_bs {
            // This is informational — pattern has vocab-unknown names.
            // The F6 test "all_patterns_have_at_least_one_ngram" ensures at least
            // one is declared; we only fail on GOLD violations for resolved ones.
            eprintln!(
                "[F7 info] pattern '{}': {}/{} bigrams resolved (some vocab names may not be \
                 in the global vocabulary — check if the kind strings are correct)",
                p.name, resolved_bs_count, declared_bs
            );
        }

        // Resolved trigrams
        let resolved_ts = p.resolved_trigrams();
        for &trigram in &resolved_ts {
            assert!(
                produced_trigrams.contains(&trigram),
                "GOLD VIOLATION for pattern '{}': declared trigram is NOT in the n-gram set \
                 produced from the example.",
                p.name,
            );
        }

        // Guard: at least one declared n-gram must resolve for the GOLD loops
        // above to verify anything. If ALL bigrams AND trigrams fail to resolve
        // (e.g. a future vocab regen renames a node kind), both loops are no-ops
        // and the pattern passes GOLD without verifying anything — a silent
        // disarming of the honesty gate.
        //
        // applies ADR-003: test assertions must genuinely verify.
        assert!(
            !resolved_bs.is_empty() || !resolved_ts.is_empty(),
            "GOLD GATE DISARMED for pattern '{}': zero declared n-grams resolved — \
             both bigrams and trigrams failed to resolve. The GOLD verification loops \
             are no-ops. Check that kind strings match the current vocabulary.",
            p.name,
        );
    }
}

// ============================================================================
// F8: Approximate patterns must say "approximation" or "structural"
// ============================================================================

#[test]
fn f8_approximate_patterns_say_approximation_or_structural() {
    for p in PATTERNS {
        if !p.exact {
            let desc_lower = p.description.to_lowercase();
            assert!(
                desc_lower.contains("approximation") || desc_lower.contains("structural"),
                "pattern '{}' has exact=false but description does not contain \
                 'approximation' or 'structural': '{}'",
                p.name,
                p.description
            );
        }
    }
}

// ============================================================================
// A6: Synthetic name resolution
// ============================================================================

#[test]
fn a6_synthetic_name_resolution_roundtrip() {
    use crate::ast_index::patterns::resolve_kind_name;

    // Parent markers
    assert_eq!(resolve_kind_name("__empty_body__"), Some(EMPTY_BODY));
    assert_eq!(resolve_kind_name("__large_body__"), Some(LARGE_BODY));
    assert_eq!(resolve_kind_name("__many_params__"), Some(MANY_PARAMS));
    assert_eq!(resolve_kind_name("__deep_node__"), Some(DEEP_NODE));

    // Bucket labels
    assert_eq!(
        resolve_kind_name("__large_body_b10__"),
        Some(bucket_label(0))
    );
    assert_eq!(
        resolve_kind_name("__large_body_b20__"),
        Some(bucket_label(1))
    );
    assert_eq!(
        resolve_kind_name("__large_body_b40__"),
        Some(bucket_label(2))
    );
    assert_eq!(
        resolve_kind_name("__many_params_b5__"),
        Some(bucket_label(0))
    );
    assert_eq!(
        resolve_kind_name("__many_params_b8__"),
        Some(bucket_label(1))
    );
    assert_eq!(
        resolve_kind_name("__many_params_b12__"),
        Some(bucket_label(2))
    );
    assert_eq!(resolve_kind_name("__deep_node_b4__"), Some(bucket_label(0)));
    assert_eq!(resolve_kind_name("__deep_node_b6__"), Some(bucket_label(1)));
    assert_eq!(resolve_kind_name("__deep_node_b8__"), Some(bucket_label(2)));

    // Unknown synthetic names return None
    assert_eq!(resolve_kind_name("__not_a_real_marker__"), None);
    assert_eq!(resolve_kind_name("__large_body_b99__"), None);

    // Real vocab names still resolve through vocab_lookup
    let fn_item = resolve_kind_name("function_item");
    assert!(
        fn_item.is_some(),
        "function_item must resolve via vocab_lookup"
    );

    // Synthetic IDs are NOT in the vocabulary (isolation guarantee from F5)
    assert!(
        vocab_resolve(EMPTY_BODY).is_none(),
        "EMPTY_BODY must not be in the vocabulary"
    );
    assert!(
        vocab_resolve(LARGE_BODY).is_none(),
        "LARGE_BODY must not be in the vocabulary"
    );
    assert!(
        vocab_resolve(MANY_PARAMS).is_none(),
        "MANY_PARAMS must not be in the vocabulary"
    );
    assert!(
        vocab_resolve(DEEP_NODE).is_none(),
        "DEEP_NODE must not be in the vocabulary"
    );
}

#[test]
fn a6_synthetic_bigram_encode_decode_matches_extract() {
    // Verify that the resolved bigram for "deep-nesting" matches what extract emits.
    // deep-nesting: DEEP_NODE → bucket_label(0)
    let expected_bigram = AstBigram::encode(DEEP_NODE, bucket_label(0));

    let p = lookup_pattern("deep-nesting").expect("deep-nesting must be in catalog");
    let resolved = p.resolved_bigrams();
    assert_eq!(
        resolved.len(),
        1,
        "deep-nesting must resolve exactly 1 bigram"
    );
    assert_eq!(
        resolved[0], expected_bigram,
        "deep-nesting bigram must equal AstBigram::encode(DEEP_NODE, bucket_label(0))"
    );

    // Also verify it appears in the produced set for the example
    let produced = extract_bigrams(p.example, p.example_lang);
    assert!(
        produced.contains(&expected_bigram),
        "DEEP_NODE→bucket_label(0) must appear in deep-nesting example produced bigrams"
    );
}

#[test]
fn a6_empty_body_bigram_matches_extract() {
    // empty-catch: EMPTY_BODY → catch_clause
    let p = lookup_pattern("empty-catch").expect("empty-catch must be in catalog");
    let resolved = p.resolved_bigrams();
    assert_eq!(
        resolved.len(),
        1,
        "empty-catch must resolve exactly 1 bigram"
    );

    let produced = extract_bigrams(p.example, p.example_lang);
    assert!(
        produced.contains(&resolved[0]),
        "EMPTY_BODY→catch_clause must appear in empty-catch example produced bigrams"
    );
}

#[test]
fn a6_god_function_bigram_matches_extract() {
    // god-function: LARGE_BODY → bucket_label(1) (>= 20 stmts)
    let expected = AstBigram::encode(LARGE_BODY, bucket_label(1));

    let p = lookup_pattern("god-function").expect("god-function must be in catalog");
    let resolved = p.resolved_bigrams();
    assert_eq!(
        resolved.len(),
        1,
        "god-function must resolve exactly 1 bigram"
    );
    assert_eq!(
        resolved[0], expected,
        "god-function bigram must be LARGE_BODY→bucket_label(1)"
    );

    let produced = extract_bigrams(p.example, p.example_lang);
    assert!(
        produced.contains(&expected),
        "LARGE_BODY→bucket_label(1) must appear in god-function example produced bigrams"
    );
}

#[test]
fn a6_excessive_params_bigram_matches_extract() {
    // excessive-params: MANY_PARAMS → bucket_label(0) (>= 5 params)
    let expected = AstBigram::encode(MANY_PARAMS, bucket_label(0));

    let p = lookup_pattern("excessive-params").expect("excessive-params must be in catalog");
    let resolved = p.resolved_bigrams();
    assert_eq!(
        resolved.len(),
        1,
        "excessive-params must resolve exactly 1 bigram"
    );
    assert_eq!(
        resolved[0], expected,
        "excessive-params bigram must be MANY_PARAMS→bucket_label(0)"
    );

    let produced = extract_bigrams(p.example, p.example_lang);
    assert!(
        produced.contains(&expected),
        "MANY_PARAMS→bucket_label(0) must appear in excessive-params example produced bigrams"
    );
}

// ============================================================================
// A6: pattern_to_query_set integration
// ============================================================================

#[test]
fn a6_pattern_to_query_set_contains_resolved_bigrams() {
    // For every pattern, the query set must contain all resolved bigrams/trigrams.
    for p in PATTERNS {
        let qset = pattern_to_query_set(p);
        let resolved_bs = p.resolved_bigrams();
        let qset_bigram_keys: HashSet<AstBigram> = qset.bigrams.iter().map(|e| e.ngram).collect();
        for &b in &resolved_bs {
            assert!(
                qset_bigram_keys.contains(&b),
                "pattern_to_query_set('{}') must contain resolved bigram {:?}",
                p.name,
                b.key()
            );
        }
    }
}
