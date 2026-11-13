//! Tests for the query execution module (query.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::io::BufWriter;

use tempfile::tempdir;

use super::{
    bounded_page_notice, execute_query, format_json_output, format_text_output,
    near_diagnostic_notice, positional_inert_notice, verify_mode_for,
};
use crate::cmd::search::snippet::VerifyMode;
use crate::cmd::search::types::{QueryConfig, QueryOutput};

// ============================================================================
// Test helpers
// ============================================================================

/// Stub analytics config with analytics disabled.
const TEST_ANALYTICS: crate::analytics::AnalyticsConfig = crate::analytics::AnalyticsConfig {
    enabled: false,
    input_cost_per_mtok: None,
    session_id: None,
};

/// Create a minimal indexable project in `root` with a few Rust files.
fn create_test_project(root: &std::path::Path) {
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    // Create a .git dir so the project root is found
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    fs::write(
        src.join("auth.rs"),
        "/// Authenticate a user.\npub fn authenticate(token: &str) -> bool {\n    !token.is_empty()\n}\n",
    )
    .unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub mod auth;\npub fn parse_config(s: &str) -> Option<String> {\n    Some(s.to_string())\n}\n",
    )
    .unwrap();
}

/// Build a QueryConfig pointing at `root` and `cache_dir`.
fn make_config(root: &std::path::Path, cache_dir: &std::path::Path, text: &str) -> QueryConfig {
    QueryConfig {
        text: text.to_string(),
        limit: 20,
        offset: None,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        phrase: false,
        near: None,
        lang: None,
    }
}

// ============================================================================
// execute_query
// ============================================================================

#[test]
fn test_execute_query_auto_builds_index_on_cold_start() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    let config = make_config(&root, &cache_dir, "authenticate");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // Index was auto-built — query should succeed (0 or more results).
    assert_eq!(output.query, "authenticate");
    assert!(
        output.duration_ms < 60_000,
        "query should complete within 60s"
    );
}

#[test]
fn test_execute_query_finds_results_for_known_term() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    let config = make_config(&root, &cache_dir, "authenticate");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // "authenticate" is in auth.rs — should find at least one result.
    assert!(
        !output.results.is_empty(),
        "should find results for 'authenticate'"
    );
    // All results should have valid paths
    for r in &output.results {
        assert!(!r.path.is_empty(), "result path must not be empty");
    }
}

#[test]
fn test_execute_query_respects_limit() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    let mut config = make_config(&root, &cache_dir, "fn");
    config.limit = 1;
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();
    assert!(
        output.results.len() <= 1,
        "limit=1 must produce at most 1 result"
    );
}

#[test]
fn test_execute_query_empty_query_returns_empty_results() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    let config = make_config(&root, &cache_dir, "");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();
    assert!(output.results.is_empty(), "empty query → empty results");
}

// ============================================================================
// format_text_output
// ============================================================================

#[test]
fn test_format_text_output_empty_results() {
    let output = QueryOutput {
        query: "nothing".to_string(),
        total: 0,
        has_more: false,
        verify_mode: None,
        results: vec![],
        duration_ms: 5,
        index_stats: None,
    };
    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    // format_text_output writes "no results for <query>" on empty results.
    assert!(
        s.contains("no results"),
        "empty result output must contain 'no results', got: {s:?}"
    );
}

#[test]
fn test_format_text_output_includes_path_and_score() {
    use crate::cmd::search::types::{ResolvedResult, SnippetContext, SnippetLine};

    let result = ResolvedResult {
        path: "src/auth.rs".to_string(),
        score: 12.34,
        field: "function_signature".to_string(),
        line_number: Some(2),
        line_range: Some(2..3),
        snippet: Some(SnippetContext {
            lines: vec![
                SnippetLine {
                    line_number: 1,
                    content: "/// Authenticate".to_string(),
                    is_match: false,
                },
                SnippetLine {
                    line_number: 2,
                    content: "pub fn authenticate".to_string(),
                    is_match: true,
                },
            ],
        }),
        stale: false,
        match_positions: vec![],
    };

    let output = QueryOutput {
        query: "authenticate".to_string(),
        total: 1,
        has_more: false,
        verify_mode: None,
        results: vec![result],
        duration_ms: 3,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(s.contains("src/auth.rs"), "output should contain path");
}

// ============================================================================
// [stale] marker
// ============================================================================

#[test]
fn test_format_text_output_includes_stale_marker() {
    use crate::cmd::search::types::{ResolvedResult, SnippetContext, SnippetLine};

    let result = ResolvedResult {
        path: "src/old.rs".to_string(),
        score: 5.0,
        field: "function_signature".to_string(),
        line_number: Some(10),
        line_range: Some(10..11),
        snippet: Some(SnippetContext {
            lines: vec![SnippetLine {
                line_number: 10,
                content: "pub fn old_fn()".to_string(),
                is_match: true,
            }],
        }),
        stale: true,
        match_positions: vec![],
    };

    let output = QueryOutput {
        query: "old_fn".to_string(),
        total: 1,
        has_more: false,
        verify_mode: None,
        results: vec![result],
        duration_ms: 2,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();
    assert!(
        s.contains("[stale]"),
        "stale result must include '[stale]' marker in output, got: {s:?}"
    );
}

// ============================================================================
// format_text_output echoes effective query in non-empty summary (AC6)
// ============================================================================

/// AC6: The non-empty human-text summary includes the effective query so a
/// mangled query can never masquerade as a successful search.
///
/// PF-007 (discriminating): Uses a query token "xqzuniq_query" that does NOT
/// appear in the path or snippet, so `s.contains("xqzuniq_query")` can only
/// be satisfied by the summary line.  The composite assertion
/// `s.contains(r#"result(s) for "xqzuniq_query""#)` fails when the echo is
/// reverted to the old `"{} result(s) in {}ms"` format — it would produce
/// `1 result(s) in 2ms`, which does not contain `for "xqzuniq_query"`.
#[test]
fn test_format_text_output_non_empty_echoes_query() {
    use crate::cmd::search::types::{ResolvedResult, SnippetContext, SnippetLine};

    // Path and snippet intentionally contain no part of the query token
    // "xqzuniq_query" — that string can only appear in the summary line echo.
    let result = ResolvedResult {
        path: "src/module.rs".to_string(),
        score: 7.5,
        field: "function_signature".to_string(),
        line_number: Some(1),
        line_range: Some(1..2),
        snippet: Some(SnippetContext {
            lines: vec![SnippetLine {
                line_number: 1,
                content: "pub fn module_func() {}".to_string(),
                is_match: true,
            }],
        }),
        stale: false,
        match_positions: vec![],
    };

    let output = QueryOutput {
        query: "xqzuniq_query".to_string(),
        total: 1,
        has_more: false,
        verify_mode: None,
        results: vec![result],
        duration_ms: 2,
        index_stats: None,
    };

    let mut buf = BufWriter::new(Vec::new());
    format_text_output(&output, &mut buf).unwrap();
    let s = String::from_utf8(buf.into_inner().unwrap()).unwrap();

    // AC6 / PF-007 (discriminating): the summary line must be in the new
    // `N result(s) for "Q" in Xms` format.  This assertion fails when the echo
    // is reverted to `"{} result(s) in {}ms"` (old format produces
    // `1 result(s) in 2ms`, which contains neither `for` nor the quoted query).
    // Neither the path (`src/module.rs`) nor the snippet (`pub fn module_func()`)
    // contain "xqzuniq_query", so the only source of this substring is the
    // summary line — making the assertion a true discriminating guard for AD-412-4.
    assert!(
        s.contains(r#"result(s) for "xqzuniq_query""#),
        "AC6: summary line must echo effective query in the form \
         'N result(s) for \"xqzuniq_query\" in Xms' (AD-412-4); got: {s:?}"
    );
}

// ============================================================================
// Edge cases: no .git, corrupt index
// ============================================================================

#[test]
fn test_execute_query_no_git_dir_returns_ok_or_graceful_err() {
    // Project root with no .git — should not panic, must return Ok or Err.
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // No .git directory — non-git project.
    fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

    let config = make_config(&root, &cache_dir, "main");

    // Must not panic. Either succeeds (0 or more results) or fails gracefully.
    match execute_query(&config, &TEST_ANALYTICS) {
        Ok(output) => {
            assert_eq!(output.query, "main");
        }
        Err(e) => {
            // Acceptable: I/O or index error — but no panic.
            let msg = e.to_string();
            assert!(!msg.is_empty(), "error message must not be empty");
        }
    }
}

#[test]
fn test_execute_query_corrupt_index_returns_err_not_panic() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    // Write garbage bytes into the index file.
    fs::write(
        cache_dir.join("index.skidx"),
        b"this is not a valid index\xff\x00\xde\xad",
    )
    .unwrap();

    let config = make_config(&root, &cache_dir, "authenticate");

    // A corrupt index must return Err rather than panic.
    // (auto_refresh_if_stale may rebuild and succeed — both outcomes are acceptable.)
    match execute_query(&config, &TEST_ANALYTICS) {
        Ok(_) => {
            // Rebuild succeeded after detecting corruption — acceptable.
        }
        Err(e) => {
            // Graceful error — confirm non-empty message.
            assert!(!e.to_string().is_empty(), "error message must not be empty");
        }
    }
}

// ============================================================================
// ResolvedResult JSON serialization
// ============================================================================

/// line_range: Some(5..13) must serialise as {"start":5,"end":13} in JSON output.
#[test]
fn test_resolved_result_line_range_some_serializes_start_end() {
    use crate::cmd::search::types::ResolvedResult;

    let result = ResolvedResult {
        path: "src/lib.rs".to_string(),
        score: 1.0,
        field: "function_signature".to_string(),
        line_number: Some(5),
        line_range: Some(5..13),
        snippet: None,
        stale: false,
        match_positions: vec![],
    };

    let value = serde_json::to_value(&result).expect("ResolvedResult must serialize");
    assert_eq!(
        value["line_range"]["start"], 5,
        "line_range.start must be 5"
    );
    assert_eq!(value["line_range"]["end"], 13, "line_range.end must be 13");
}

/// line_range: None must serialise as JSON null.
#[test]
fn test_resolved_result_line_range_none_serializes_null() {
    use crate::cmd::search::types::ResolvedResult;

    let result = ResolvedResult {
        path: "src/lib.rs".to_string(),
        score: 1.0,
        field: "function_signature".to_string(),
        line_number: None,
        line_range: None,
        snippet: None,
        stale: false,
        match_positions: vec![],
    };

    let value = serde_json::to_value(&result).expect("ResolvedResult must serialize");
    assert!(
        value["line_range"].is_null(),
        "line_range must be null when None, got: {:?}",
        value["line_range"]
    );
}

// ============================================================================
// format_json_output
// ============================================================================

#[test]
fn test_format_json_output_is_valid_json() {
    let output = QueryOutput {
        query: "test".to_string(),
        total: 0,
        has_more: false,
        verify_mode: None,
        results: vec![],
        duration_ms: 1,
        index_stats: None,
    };
    let mut buf = BufWriter::new(Vec::new());
    format_json_output(&output, &mut buf).unwrap();
    let bytes = buf.into_inner().unwrap();
    let s = std::str::from_utf8(&bytes).unwrap();
    // Must be valid JSON
    let parsed: serde_json::Value = serde_json::from_str(s).expect("must be valid JSON");
    assert_eq!(parsed["query"], "test");
    assert_eq!(parsed["total"], 0);
}

// ============================================================================
// #355 Part A — Exact-match verification (AC1 / AC2 / AC3)
//
// PF-007: every test asserts a discriminating observable, not just exit-0.
// AC2: gibberish query → 0 results on ALL paths.
// AC3: every returned result literally contains the query token(s).
// AC1: an exact symbol query returns only files containing it.
// ============================================================================

/// AC2 — gibberish query produces 0 verified results on the pure-lexical path.
///
/// PF-007 (discriminating): asserts results.is_empty() for a query whose trigrams
/// are absent from the index — so the reader returns 0 candidates and verification
/// never runs.  This guards the trigram-miss path, not the verify gate.
/// The discriminating coverage for the verify gate is in:
///   - `test_ac1_verify_gate_drops_trigram_overlap_non_literal` (non-literal that shares trigrams)
///   - `test_ac3_every_result_contains_query_term_pure_lexical` (content check per result)
#[test]
fn test_ac2_gibberish_query_returns_zero_results_pure_lexical() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    // "xqzjvmblorp" is a provably absent gibberish string — its trigrams (e.g. "xqz",
    // "qzj", "zjv"…) do not appear in any natural code file, so the trigram index
    // returns 0 candidates before verification even runs.  The empty result here
    // comes from zero trigram overlap, not from the verify gate.
    let config = make_config(&root, &cache_dir, "xqzjvmblorp");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // AC2: verified result set must be empty (trigram-miss path).
    assert!(
        output.results.is_empty(),
        "AC2: gibberish query 'xqzjvmblorp' must return 0 results (no trigram overlap); \
        got {} results: {:?}",
        output.results.len(),
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
}

/// AC3 — every returned result literally contains the query term (pure-lexical).
///
/// PF-007 (discriminating): reads the content of each returned file and asserts
/// the query term is present as a literal substring.  This test would fail if
/// verification were disabled (bigram-noise false positives would appear).
#[test]
fn test_ac3_every_result_contains_query_term_pure_lexical() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_test_project(&root);

    // "authenticate" is a real term in src/auth.rs.
    let config = make_config(&root, &cache_dir, "authenticate");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    assert!(
        !output.results.is_empty(),
        "AC3: 'authenticate' must find at least one result"
    );

    for r in &output.results {
        let abs_path = root.join(&r.path);
        let content = fs::read_to_string(&abs_path).unwrap_or_default();
        assert!(
            content.contains("authenticate"),
            "AC3: result file '{}' must contain the literal query term 'authenticate'; \
            file content: {content:?}",
            r.path
        );
    }
}

/// AC1 — an exact symbol query returns ONLY files containing it; the defining
/// file ranks at position 0 (the highest-ranked result).
///
/// PF-007 (discriminating): asserts (a) the definer is present and (b) every
/// non-definer result is absent from the verified set when the symbol is unique.
/// This would fail without the wider pool + verify-then-truncate invariant.
#[test]
fn test_ac1_exact_symbol_returns_only_containing_files_and_definer_is_first() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    // Controlled corpus: auth.rs defines `frbnqlwx_unique_symbol`; lib.rs does NOT.
    // The symbol uses a nonsense prefix that can't appear in lib.rs accidentally.
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(
        src.join("auth.rs"),
        "/// The authoritative definer.\npub fn frbnqlwx_unique_symbol(x: u32) -> u32 { x }\n",
    )
    .unwrap();
    fs::write(
        src.join("lib.rs"),
        "pub struct Config { pub value: u32 }\nimpl Config { pub fn new(v: u32) -> Self { Self { value: v } } }\n",
    )
    .unwrap();

    let config = make_config(&root, &cache_dir, "frbnqlwx_unique_symbol");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // AC1(a): the definer file must be in results.
    let has_definer = output.results.iter().any(|r| r.path == "src/auth.rs");
    assert!(
        has_definer,
        "AC1: definer file 'src/auth.rs' must appear in results; got: {:?}",
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );

    // AC1(b): no result may be a file that does NOT contain the symbol.
    // lib.rs does not contain "frbnqlwx_unique_symbol" — it must be absent.
    let has_lib = output.results.iter().any(|r| r.path == "src/lib.rs");
    assert!(
        !has_lib,
        "AC1: 'src/lib.rs' does not contain 'frbnqlwx_unique_symbol' and must \
        NOT appear in verified results (this would fail without verification)"
    );

    // AC1(c): definer is the top-ranked result.
    let first_path = output
        .results
        .first()
        .map(|r| r.path.as_str())
        .unwrap_or("");
    assert_eq!(
        first_path,
        "src/auth.rs",
        "AC1: definer 'src/auth.rs' must be results[0]; got {:?}",
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );

    // AC3 (inline): every returned result must contain the query term.
    for r in &output.results {
        let abs_path = root.join(&r.path);
        let content = fs::read_to_string(&abs_path).unwrap_or_default();
        assert!(
            content.contains("frbnqlwx_unique_symbol"),
            "AC3: every verified result must contain 'frbnqlwx_unique_symbol'; \
            '{}' does not: {content:?}",
            r.path
        );
    }
}

/// AC1 (verify gate specifically exercised) — lib.rs shares trigrams with the
/// query term but does NOT contain the literal string.
///
/// The original AC1 test uses a purely unique symbol with zero trigram overlap
/// in lib.rs — so lib.rs is trivially absent from candidates.  This test
/// specifically exercises the verify gate: lib.rs contains trigram-generating
/// substrings that share individual trigrams with the target query token, but
/// NOT the literal token.  Without the verify gate, lib.rs would be a false
/// positive.  With the gate, only the definer file survives.
///
/// PF-007 (discriminating): this test WOULD FAIL if verify gate were removed,
/// because the trigram index would return lib.rs as a candidate and it would
/// appear in results without the gate dropping it.
#[test]
fn test_ac1_verify_gate_drops_trigram_overlap_non_literal() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    // Query token: "authenticate_user".
    // auth.rs: contains the exact literal "authenticate_user".
    fs::write(
        src.join("auth.rs"),
        "/// Authenticate a user by token.\n\
         pub fn authenticate_user(token: &str) -> bool { !token.is_empty() }\n",
    )
    .unwrap();

    // lib.rs: contains the trigram-generating substrings "authenticate" and
    // "user" as SEPARATE words, generating many shared trigrams with
    // "authenticate_user", but the exact literal string "authenticate_user"
    // is NOT present.  The verify gate must drop lib.rs.
    fs::write(
        src.join("lib.rs"),
        "/// Authenticate the request.\n\
         pub fn check_user(id: u32) -> bool { id > 0 }\n\
         pub fn authenticate(token: &str) -> bool { !token.is_empty() }\n",
    )
    .unwrap();

    let config = make_config(&root, &cache_dir, "authenticate_user");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // AC1: definer must be present.
    let has_auth = output.results.iter().any(|r| r.path == "src/auth.rs");
    assert!(
        has_auth,
        "AC1 (verify gate): 'src/auth.rs' defines 'authenticate_user' and must appear; \
        got: {:?}",
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );

    // PF-007 (discriminating): lib.rs must NOT appear because although it shares
    // trigrams with "authenticate_user", it does not contain the literal string.
    // Without the verify gate, lib.rs would be a false positive.
    let has_lib = output.results.iter().any(|r| r.path == "src/lib.rs");
    assert!(
        !has_lib,
        "AC1 (verify gate): 'src/lib.rs' shares trigrams with 'authenticate_user' but \
        does NOT contain the literal string — verify gate must drop it; \
        found in results, which means verify gate is absent or broken"
    );
}

// ============================================================================
// AC10 — Snippet baseline: match line == known line for exact-symbol path
//
// RESOLVED Decision 2: positions collected from ALL intersected trigrams,
// snippet's match line must equal the file line that contains the token.
// PF-007: this test would fail if match_positions is empty (no positions
// forwarded from the intersection) because extract_snippet_and_verify would
// return SnippetOutcome::Unavailable → line_number == None.
// ============================================================================

/// AC10 — exact-symbol path must collect match_positions from ALL intersected
/// trigrams and produce a snippet whose match line equals the known token line.
///
/// PF-007 (discriminating): if positions are NOT collected from the intersection
/// (e.g. empty match_positions forwarded), `extract_snippet_and_verify` returns
/// `SnippetOutcome::Unavailable` and `line_number` is `None` — the assertion
/// below catches both the missing-position bug and the wrong-line bug.
///
/// The token is placed on line 7 (1-based) to avoid trivial pass from coincidental
/// line-0 defaults.
#[test]
fn test_ac10_snippet_match_line_equals_known_token_line() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    // Token: "qxzplumb_resolver" on line 7 (1-based), surrounded by 6 other lines.
    // Lines 1–6: filler; line 7: the token.
    let content = "// line 1 header\n\
                   // line 2\n\
                   // line 3\n\
                   // line 4\n\
                   // line 5\n\
                   // line 6\n\
                   pub fn qxzplumb_resolver(x: u32) -> u32 { x }\n\
                   // line 8 footer\n";

    fs::write(src.join("resolver.rs"), content).unwrap();
    // lib.rs: no "qxzplumb_resolver" at all (acts as negative control).
    fs::write(
        src.join("lib.rs"),
        "pub mod resolver;\npub struct Config { pub value: u32 }\n",
    )
    .unwrap();

    let config = make_config(&root, &cache_dir, "qxzplumb_resolver");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // The query must find at least one result (the definer file).
    assert!(
        !output.results.is_empty(),
        "AC10: 'qxzplumb_resolver' must find at least one result; got 0"
    );

    // Find the resolver.rs result.
    let resolver_result = output
        .results
        .iter()
        .find(|r| r.path.ends_with("resolver.rs"));
    assert!(
        resolver_result.is_some(),
        "AC10: 'src/resolver.rs' must appear in results; got: {:?}",
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );

    let r = resolver_result.unwrap();

    // AC10 core: line_number must be Some and equal line 7 (1-based).
    // If match_positions were empty (RESOLVED Decision 2 violated),
    // extract_snippet_and_verify would return Unavailable → line_number = None.
    assert!(
        r.line_number.is_some(),
        "AC10: snippet line_number must be Some — if None, match_positions was empty \
        (RESOLVED Decision 2 violated: positions not collected from ALL intersected trigrams); \
        result: {:?}",
        r
    );
    assert_eq!(
        r.line_number.unwrap(),
        7,
        "AC10: snippet match line must be 7 (1-based; the token 'qxzplumb_resolver' is \
        on line 7); got {}. Wrong line means trigram positions are off or the snippet \
        extractor is computing the wrong match line.",
        r.line_number.unwrap()
    );

    // AC10: lib.rs must NOT appear (it does not contain the token).
    let has_lib = output.results.iter().any(|r| r.path.ends_with("lib.rs"));
    assert!(
        !has_lib,
        "AC10: 'src/lib.rs' does not contain 'qxzplumb_resolver' and must be absent"
    );
}

// ============================================================================
// AC11b — End-to-end pagination: --limit + --offset produce disjoint pages
//
// RESOLVED Decision 3: offset applied AFTER verification on the pure-lexical
// CLI path.  execute_query_with_manifest must honor offset end-to-end.
// PF-007: disjoint-page assertion fails if offset is applied pre-verify (pages
// can overlap when stale/incidental-overlap candidates are dropped).
// ============================================================================

/// AC11b — end-to-end pagination via execute_query: --limit 1 --offset 0 and
/// --limit 1 --offset 1 must return disjoint, non-empty, correctly-ordered pages.
///
/// PF-007 (discriminating): if offset is applied pre-verify (inside the reader,
/// before the verify step drops stale/incidental-overlap files), both pages
/// could return the same file or the second page could be empty when
/// offset == 1 shifts past all candidates before verification runs.
/// This test catches both the pre-verify offset bug and the no-offset-wired bug.
#[test]
fn test_ac11b_end_to_end_pagination_disjoint_pages() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    // Two files both containing the token "qxzpag_token" so both survive the verify
    // gate; with limit=1 each page shows exactly one file.
    let token = "qxzpag_token";
    fs::write(
        src.join("file_a.rs"),
        format!("pub fn {token}_handler() {{ }}\n"),
    )
    .unwrap();
    // file_b.rs also contains the token — it must appear on page 2.
    fs::write(src.join("file_b.rs"), format!("pub use crate::{token};\n")).unwrap();

    // Page 0: limit=1, offset=0 (the default).
    let config_p0 = QueryConfig {
        text: token.to_string(),
        limit: 1,
        offset: None, // offset 0
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        phrase: false,
        near: None,
        lang: None,
    };
    let page0 = execute_query(&config_p0, &TEST_ANALYTICS).unwrap();

    // Page 1: same limit, offset=1 (skip the rank-1 result).
    let config_p1 = QueryConfig {
        text: token.to_string(),
        limit: 1,
        offset: Some(1),
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        phrase: false,
        near: None,
        lang: None,
    };
    let page1 = execute_query(&config_p1, &TEST_ANALYTICS).unwrap();

    // Both pages must return exactly 1 result.
    assert_eq!(
        page0.results.len(),
        1,
        "AC11b: page 0 (limit=1, offset=0) must return exactly 1 result; got {}",
        page0.results.len()
    );
    assert_eq!(
        page1.results.len(),
        1,
        "AC11b: page 1 (limit=1, offset=1) must return exactly 1 result; got {}. \
        If offset is not wired (always None), page 1 returns the same result as page 0 \
        or is empty (neither is correct).",
        page1.results.len()
    );

    // Pages must be disjoint: the result on page 1 must differ from page 0.
    let path0 = &page0.results[0].path;
    let path1 = &page1.results[0].path;
    assert_ne!(
        path0, path1,
        "AC11b: page 0 and page 1 must be disjoint (different files); both returned \
        {:?}. This means offset is not being applied (pre-verify or not at all).",
        path0
    );
}

// ============================================================================
// AC12 — End-to-end recall: execute_query_with_manifest on exact-symbol path
//
// RESOLVED Decision 3 / AD-372-3: the caller must NOT apply LEXICAL_CANDIDATE_POOL_K
// for single-token queries (sq.limit = None) so the full intersection reaches the
// verify step.  This integration test proves the definer appears in the final
// QueryOutput.results even when it is the only match across a multi-file corpus.
// ============================================================================

/// AC12 — end-to-end recall via execute_query_with_manifest: a single-token query
/// over a corpus with a large definer file must return the definer in the output.
///
/// PF-007 (discriminating): if sq.limit were set to LEXICAL_CANDIDATE_POOL_K × N
/// and the definer is at rank > pool_limit, it would be truncated before the verify
/// step and the assertion below would fail.  The exact-symbol path's sq.limit=None
/// guarantees the full intersection reaches verification.
#[test]
fn test_ac12_e2e_caller_recall_single_token_finds_definer() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    // Definer: file_0.rs defines the unique single-token symbol "qxzcalimba_def".
    let token = "qxzcalimba_def";
    fs::write(
        src.join("file_0.rs"),
        format!("/// The authoritative definer.\npub fn {token}(n: u32) -> u32 {{ n }}\n"),
    )
    .unwrap();
    // Noise files: do NOT contain the token so only file_0.rs survives the verify gate.
    for i in 1..=5u32 {
        fs::write(
            src.join(format!("noise_{i}.rs")),
            format!("pub fn helper_{i}(x: u32) -> u32 {{ x + {i} }}\n"),
        )
        .unwrap();
    }

    let config = make_config(&root, &cache_dir, token);
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // AC12: the definer must appear in results.
    let has_definer = output.results.iter().any(|r| r.path.ends_with("file_0.rs"));
    assert!(
        has_definer,
        "AC12: definer 'src/file_0.rs' must appear in e2e results for token {:?}; \
        got: {:?}. If missing, sq.limit was applied BEFORE verification (pool cap cut it out).",
        token,
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );

    // AC12 (negative): noise files must NOT appear (they don't contain the token).
    for i in 1..=5u32 {
        let noise_path = format!("src/noise_{i}.rs");
        let has_noise = output.results.iter().any(|r| r.path == noise_path);
        assert!(
            !has_noise,
            "AC12: noise file '{}' does not contain '{}' and must NOT appear in results",
            noise_path, token
        );
    }
}

// ============================================================================
// AC13 — K-pool branching: multi-word uses LEXICAL_CANDIDATE_POOL_K; single-token
// bypasses it (sq.limit = None on exact path).
//
// PF-007: a falsifiable test at the caller level verifying the branch.
// ============================================================================

/// AC13 — the execute_query caller must NOT apply LEXICAL_CANDIDATE_POOL_K for
/// single-token queries and MUST apply it for multi-word queries.
///
/// PF-007 (discriminating, two-sided):
/// (a) Single-token: with limit=1, a corpus where BOTH files contain the token
///     must still find the second-ranked file when offset=1 — possible only if
///     sq.limit=None (the full intersection reaches the verify step).
/// (b) Multi-word: with limit=1, the K-pool widening (5×) must surface a file
///     that would be missed if only `limit` (=1) candidates were fetched.
///
/// The test uses disjoint unique prefixes so n-gram overlap cannot contaminate
/// the single-token path into the multi-word branch.
#[test]
fn test_ac13_single_token_bypasses_k_pool_multi_word_uses_it() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    // Two files both contain single token "qxzac13_sym".
    // file_a.rs has MORE occurrences so it ranks #1; file_b.rs ranks #2.
    let token = "qxzac13_sym";
    fs::write(
        src.join("file_a.rs"),
        format!(
            "// many occurrences\n\
             pub fn {token}_a() {{ }}\n\
             pub fn {token}_b() {{ }}\n\
             pub fn {token}_c() {{ }}\n"
        ),
    )
    .unwrap();
    fs::write(
        src.join("file_b.rs"),
        format!("// single occurrence\npub fn {token}_entry() {{ }}\n"),
    )
    .unwrap();

    // AC13(a): single-token path — with limit=1, offset=0 → rank-1 file.
    //          with limit=1, offset=1 → rank-2 file.
    // Both pages must be non-empty, proving sq.limit=None (not capped to 1).
    let p0 = execute_query(
        &QueryConfig {
            text: token.to_string(),
            limit: 1,
            offset: None,
            json: false,
            root: root.to_path_buf(),
            cache_dir: cache_dir.to_path_buf(),
            phrase: false,
            near: None,
            lang: None,
        },
        &TEST_ANALYTICS,
    )
    .unwrap();
    let p1 = execute_query(
        &QueryConfig {
            text: token.to_string(),
            limit: 1,
            offset: Some(1),
            json: false,
            root: root.to_path_buf(),
            cache_dir: cache_dir.to_path_buf(),
            phrase: false,
            near: None,
            lang: None,
        },
        &TEST_ANALYTICS,
    )
    .unwrap();

    // AC13(a): both pages must be non-empty and disjoint.
    assert_eq!(
        p0.results.len(),
        1,
        "AC13(a): page 0 (single-token, limit=1, offset=0) must return 1 result; got {}",
        p0.results.len()
    );
    assert_eq!(
        p1.results.len(),
        1,
        "AC13(a): page 1 (single-token, limit=1, offset=1) must return 1 result; \
        got {} — if 0, sq.limit was capped to 1 (K-pool applied on single-token path, wrong).",
        p1.results.len()
    );
    assert_ne!(
        p0.results[0].path, p1.results[0].path,
        "AC13(a): pages must be disjoint; both returned {:?}",
        p0.results[0].path
    );

    // AC13(b): multi-word path — discriminating check that LEXICAL_CANDIDATE_POOL_K
    // widening is applied.
    //
    // With limit=1, the BM25F UNION pool is max(1*5, 100)=100, so file_b.rs is in
    // the pre-verify pool even though it would rank #2 in lexical relevance (file_a.rs
    // has more occurrences of the single token).  The two-word query "qxzac13_sym entry"
    // only matches file_b.rs (file_b contains "qxzac13_sym_entry" which passes the
    // substring verify for both tokens; file_a.rs lacks "entry").
    //
    // Discrimination: without K-pool widening (pool = limit = 1), the query would
    // fetch only 1 candidate; if that candidate is file_a.rs (which fails the "entry"
    // verify), the result would be empty.  With K-pool, pool=100 includes file_b.rs,
    // so the result is non-empty.  We assert non-empty (not just contains file_b) to
    // avoid dependence on BM25F's exact rank-1 assignment for file_a.rs.
    let multi = execute_query(
        &QueryConfig {
            text: format!("{token} entry"),
            limit: 1,
            offset: None,
            json: false,
            root: root.to_path_buf(),
            cache_dir: cache_dir.to_path_buf(),
            phrase: false,
            near: None,
            lang: None,
        },
        &TEST_ANALYTICS,
    )
    .unwrap();

    // file_b.rs contains both "qxzac13_sym" and "entry" as literal substrings
    // (in "qxzac13_sym_entry"); file_a.rs does NOT contain "entry".
    // The result must be non-empty — without K-pool widening this would fail
    // if file_a.rs is the only candidate fetched (fails "entry" verify → 0 results).
    assert!(
        !multi.results.is_empty(),
        "AC13(b): multi-word query '{} entry' with limit=1 must return >= 1 result via K-pool; \
        got 0 — suggests K-pool widening is not applied on the multi-word path. \
        Results: {:?}",
        token,
        multi.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
    let has_file_b = multi.results.iter().any(|r| r.path.ends_with("file_b.rs"));
    assert!(
        has_file_b,
        "AC13(b): multi-word query '{} entry' must surface file_b.rs (contains both tokens); \
        got: {:?}",
        token,
        multi.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
}

// ============================================================================
// AC15a — Measured SLA: short_query_fallback with N=5,000 files stays < 2,000 ms
//
// RESOLVED Decision 4: the de-truncation of short_query_fallback (AD-372-4)
// removed the internal .take(limit) so all indexed files are returned to the
// caller for verification.  This is O(file_count) fan-out.  The SLA test is
// the load-bearing reliability guarantee (reliability.md: every loop has a
// fixed upper bound) that licenses the de-truncation.
//
// PF-007 (discriminating): without this timed bound the de-truncation has no
// measured safety net; a pathological corpus could silently make short queries
// unbounded.  This test fails if the wall-clock cost exceeds 2,000 ms.
// ============================================================================

/// AC15a — short_query_fallback recall guard: every file containing "fn" in a
/// 5,000-file corpus must appear in the result, including files with high
/// sequential IDs (above the old CANDIDATE_POOL_FLOOR of 100).
///
/// Runs unconditionally in both debug and release builds so the de-truncation
/// contract (AD-372-4) is always guarded.  The companion SLA test below
/// (`test_ac15a_sla`) is release-only (wall-clock assertions need release timings).
///
/// PF-007 (discriminating): if the old `.take(limit)` pre-truncation is
/// re-added to `short_query_fallback`, files with sorted file_id >= limit will
/// disappear from the result set and `output.results.len() < min_expected` will
/// fire, catching the regression in every `cargo test` run, not just release.
#[test]
fn test_ac15a_short_query_fallback_5000_files_recall() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    // Build N=5_000 minimal Rust files each containing "fn".
    const N: usize = 5_000;
    for i in 0..N {
        fs::write(
            src.join(format!("f{i:04}.rs")),
            format!("pub fn proc_{i}(x: u32) -> u32 {{ x }}\n"),
        )
        .unwrap();
    }

    // Cold-start: build the index.
    {
        let build_config = QueryConfig {
            text: "proc_0000".to_string(),
            limit: 20,
            offset: None,
            json: false,
            root: root.to_path_buf(),
            cache_dir: cache_dir.to_path_buf(),
            phrase: false,
            near: None,
            lang: None,
        };
        let _ = execute_query(&build_config, &TEST_ANALYTICS).unwrap();
    }

    // "fn" is 2 bytes → short_query_fallback path (AD-355-7).
    // Limit is set high to ensure all 5,000 results can be returned.
    let query_config = QueryConfig {
        text: "fn".to_string(),
        limit: N + 100,
        offset: None,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        phrase: false,
        near: None,
        lang: None,
    };

    let output = execute_query(&query_config, &TEST_ANALYTICS).unwrap();

    // AC15a (recall): all N files contain "fn", so verify returns all of them.
    // At minimum 90% must be present — confirms no internal pre-truncation.
    let min_expected = N * 9 / 10;
    assert!(
        output.results.len() >= min_expected,
        "AC15a: short_query_fallback must return >= {min_expected} (90% of {N}) results for 'fn'; \
        got {} — old .take(limit) pre-truncation re-introduced (AD-372-4 violated).",
        output.results.len()
    );

    // AC15a: a specific known high-id file (f4999.rs) must appear, confirming
    // files beyond the old CANDIDATE_POOL_FLOOR=100 are not silently dropped.
    let has_high_id_file = output.results.iter().any(|r| r.path.contains("f4999.rs"));
    assert!(
        has_high_id_file,
        "AC15a: f4999.rs must appear in results; \
        suggests old CANDIDATE_POOL_FLOOR truncation still active (AD-372-4 violated)"
    );
}

/// AC15a — short_query_fallback SLA: N=5,000 indexed files, "fn" query must
/// complete in under 2,000 ms wall-clock (release profile).
///
/// Gated `#[cfg(not(debug_assertions))]` because:
/// - Debug builds are ~4–10× slower; the 2,000 ms SLA is calibrated for release.
/// - The recall contract (de-truncation correctness) is guarded by the
///   unconditional `test_ac15a_short_query_fallback_5000_files_recall` above.
///
/// PF-007 (discriminating): without this timed bound the de-truncated O(file_count)
/// fan-out has no measured safety net; a pathological corpus could silently make
/// short queries unbounded.
#[test]
#[cfg(not(debug_assertions))]
fn test_ac15a_short_query_fallback_5000_files_sla() {
    use std::time::Instant;

    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();

    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    const N: usize = 5_000;
    for i in 0..N {
        fs::write(
            src.join(format!("f{i:04}.rs")),
            format!("pub fn proc_{i}(x: u32) -> u32 {{ x }}\n"),
        )
        .unwrap();
    }

    // Cold-start: build the index.
    {
        let _ = execute_query(
            &QueryConfig {
                text: "proc_0000".to_string(),
                limit: 20,
                offset: None,
                json: false,
                root: root.to_path_buf(),
                cache_dir: cache_dir.to_path_buf(),
                phrase: false,
                near: None,
                lang: None,
            },
            &TEST_ANALYTICS,
        )
        .unwrap();
    }

    let query_config = QueryConfig {
        text: "fn".to_string(),
        limit: N + 100,
        offset: None,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        phrase: false,
        near: None,
        lang: None,
    };

    let t_start = Instant::now();
    let output = execute_query(&query_config, &TEST_ANALYTICS).unwrap();
    let elapsed_ms = t_start.elapsed().as_millis();

    eprintln!(
        "AC15a SLA: short_query_fallback N={N} files, 'fn' query: {elapsed_ms}ms, \
        results={}, SLA=2000ms",
        output.results.len()
    );

    // AC15a (SLA): the verified fan-out must stay under 2,000 ms wall-clock.
    assert!(
        elapsed_ms < 2_000,
        "AC15a: short_query_fallback for {N} files took {elapsed_ms}ms, exceeding the \
        RESOLVED Decision 4 SLA of 2,000ms. The O(file_count) verify fan-out is \
        unbounded — profile short_query_fallback or the verify step."
    );
}

// ============================================================================
// AD-396 anchor-trust: E2E ground-truth tests (SEARCH-ADR-007 / AC1 / AC2 / AC6 /
// AC7 / AC19)
// ============================================================================

/// Create a controlled "decoy + true-match" project for anchor tests.
///
/// - `auth.rs`: line 1 = comment with "authentic" (decoy — shares trigrams with
///   "authentic_fn" but is NOT the full token); line 2 = true match.
/// - `lib.rs`: unrelated content (controls recall baseline).
fn create_anchor_trust_project(root: &std::path::Path) {
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();

    // auth.rs: decoy on line 1, true token on line 2.
    // The "// authentic prefix" comment shares the "auth" / "authent" trigrams
    // with "authentic_fn" — the pre-#396 bug would anchor on line 1 when the
    // trigram reader returned a position inside the comment.
    fs::write(
        src.join("auth.rs"),
        "// authentic prefix comment\npub fn authentic_fn(s: &str) -> bool { !s.is_empty() }\n",
    )
    .unwrap();

    // multi.rs: for multi-token queries (AC16/AC17).
    // Tier 1 line: line 3 has BOTH "alfa_token" and "beta_token".
    fs::write(
        src.join("multi.rs"),
        "// alfa_token comment\n// beta_token comment\nfn alfa_token_beta_token() {}\n",
    )
    .unwrap();

    // lib.rs: no "authentic_fn", no "alfa_token"/"beta_token".
    fs::write(src.join("lib.rs"), "pub mod auth;\npub mod multi;\n").unwrap();
}

/// Build config for anchor-trust tests.
fn make_anchor_config(
    root: &std::path::Path,
    cache_dir: &std::path::Path,
    text: &str,
) -> QueryConfig {
    QueryConfig {
        text: text.to_string(),
        limit: 20,
        offset: None,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        phrase: false,
        near: None,
        lang: None,
    }
}

/// AC1 / AC2 / AC19 — SEARCH-ADR-007 anchor-trust ground truth: every result's
/// `line_number` must reference a line containing the query token.
///
/// PF-007: the discriminating observable is that the file's actual line N
/// contains the token — not just that line_number is Some or exit-0.
///
/// The corpus has a "decoy" comment line (shared trigrams) above the true
/// match line.  Pre-#396 the anchor landed on the comment; post-#396 it
/// must land on the true match line.
#[test]
fn test_anchor_line_contains_query_token_ac1_ac2_ac19() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_anchor_trust_project(&root);

    let config = make_anchor_config(&root, &cache_dir, "authentic_fn");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // Must find at least one result.
    assert!(
        !output.results.is_empty(),
        "AC1: 'authentic_fn' must be found in the corpus"
    );

    // SEARCH-ADR-007 / AC19: for every result with Some(line_number), the file's
    // actual line must contain the query token.
    for r in &output.results {
        if let Some(ln) = r.line_number {
            let file_content = fs::read_to_string(root.join(&r.path)).unwrap_or_default();
            let anchor_line_text = file_content
                .lines()
                .nth((ln as usize).saturating_sub(1))
                .unwrap_or("");
            assert!(
                anchor_line_text.contains("authentic_fn"),
                "AC1/AC19: result path={} line_number={ln} must contain 'authentic_fn'; \
                 got line text: {anchor_line_text:?}",
                r.path
            );
        }
    }
}

/// AC6 / AC5 — JSON contract: `line_range` must be the single anchor line
/// `{{n, n+1}}` and must agree with `line_number`.
///
/// PF-007: the discriminating observable is the exact `line_range.end` value.
/// With pre-#396 multi-trigram positions, line_range spanned from the first
/// to the last trigram position (could be a wide span); post-#396 it is the
/// single anchor line {{n, n+1}}.
///
/// Precondition (PF-007): assert results are non-empty before entering the
/// per-result loop — an empty result set would make every iteration vacuous
/// and let a broken index or verify gate silently pass the test.
#[test]
fn test_anchor_line_range_is_single_line_ac6() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_anchor_trust_project(&root);

    let config = make_anchor_config(&root, &cache_dir, "authentic_fn");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // PF-007 precondition: an empty result set makes the loop body vacuous.
    // auth.rs contains "authentic_fn" on line 2; if results are empty the
    // index or verify gate is broken and the AC6 contract goes untested.
    assert!(
        !output.results.is_empty(),
        "AC6 precondition: query 'authentic_fn' must produce ≥1 result; \
         corpus has auth.rs with the literal on line 2 — empty result set \
         means the verify gate or index is broken. Got 0 results."
    );

    for r in &output.results {
        if let (Some(ln), Some(lr)) = (r.line_number, r.line_range.clone()) {
            assert_eq!(
                lr,
                (ln as usize)..(ln as usize + 1),
                "AC6: line_range must be single anchor line {{n, n+1}} for line_number={ln}; \
                 got line_range={lr:?}"
            );
        }
    }
}

/// AC7 — Recall and ranking unchanged: the fix must NOT drop any file that
/// was returned before (the anchor change affects line_number/snippet only,
/// not which files are returned).
///
/// PF-007: the discriminating observable is the COMPLETE ordered path set and
/// total count, not merely the presence of auth.rs. The corpus contains three
/// files (auth.rs, lib.rs, multi.rs); only auth.rs contains "authentic_fn".
/// A regression that reorders results, changes count, or adds spurious files
/// would not be caught by a presence-only assertion.
#[test]
fn test_anchor_fix_does_not_drop_results_ac7() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_anchor_trust_project(&root);

    let config = make_anchor_config(&root, &cache_dir, "authentic_fn");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    let paths: Vec<&str> = output.results.iter().map(|r| r.path.as_str()).collect();

    // AC7 / PF-007: assert the COMPLETE path set and count.
    // Corpus: auth.rs (contains "authentic_fn"), lib.rs ("pub mod auth/multi"),
    // multi.rs ("alfa_token_beta_token"). Only auth.rs contains the literal.
    // Any regression that drops auth.rs, adds a spurious file, or changes
    // the count will fail this assertion.
    assert_eq!(
        paths,
        vec!["src/auth.rs"],
        "AC7: result set must be exactly ['src/auth.rs'] (count=1); \
         lib.rs and multi.rs do NOT contain 'authentic_fn' and must not appear. \
         A drop, spurious addition, or reorder regression will fail here. \
         Got: {paths:?}"
    );
}

/// AC16 — Multi-token Tier 1: earliest line containing ALL tokens wins.
///
/// PF-007: the discriminating observable is line_number == 3 (the "alfa_token
/// and beta_token" line), not line 1 (alfa_token-only) or line 2 (beta_token-only).
#[test]
fn test_multi_token_tier1_earliest_all_tokens_line_ac16() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    create_anchor_trust_project(&root);

    // "alfa_token beta_token" — multi.rs has them on separate lines (1, 2) and
    // on the same line at line 3 (in the function name "alfa_token_beta_token").
    // Note: "alfa_token_beta_token" contains "alfa_token" (substr) and
    // "beta_token" (substr) on the same line → Tier 1 fires.
    let config = make_anchor_config(&root, &cache_dir, "alfa_token beta_token");
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // PF-007 (non-vacuous): assert multi.rs IS in results before checking anchor.
    // multi.rs contains "alfa_token" and "beta_token" (as separate lines 1–2 and
    // combined in "alfa_token_beta_token" on line 3); it must survive the verify gate.
    // A regression that drops multi.rs or nulls its anchor passes vacuously under the
    // old `if let` guard — the unconditional assert below prevents that.
    let multi_result = output.results.iter().find(|r| r.path.contains("multi.rs"));
    assert!(
        multi_result.is_some(),
        "AC16 (PF-007): multi.rs must be in results for query 'alfa_token beta_token'; \
         it contains both tokens and must survive the verify gate. \
         Got results: {:?}",
        output.results.iter().map(|r| &r.path).collect::<Vec<_>>()
    );
    let r = multi_result.unwrap();

    // AC16 / AD-396-2 Tier 1: the EARLIEST line containing ALL tokens is line 3.
    // Line 1 = "// alfa_token comment" (alfa_token only).
    // Line 2 = "// beta_token comment" (beta_token only).
    // Line 3 = "fn alfa_token_beta_token() {}" — contains BOTH as substrings.
    // Tier 1 fires on line 3 (first line satisfying the AND condition).
    assert_eq!(
        r.line_number,
        Some(3),
        "AC16/Tier-1 (PF-007): line_number must be 3 ('fn alfa_token_beta_token()' — \
         earliest line with BOTH tokens); got {:?}. \
         Line 1 has only 'alfa_token'; line 2 has only 'beta_token'.",
        r.line_number
    );

    // SEARCH-ADR-007 ground truth: read the actual anchor line and confirm ≥1 token.
    let file_content = fs::read_to_string(root.join(&r.path)).unwrap_or_default();
    let anchor_line = file_content
        .lines()
        .nth(2) // line 3 is 0-indexed as 2
        .unwrap_or("");
    assert!(
        anchor_line.contains("alfa_token") || anchor_line.contains("beta_token"),
        "AC16/SEARCH-ADR-007: anchor line 3 must contain ≥1 query token; got: {anchor_line:?}"
    );
}

/// AC4 — `match_positions` field must be absent from JSON output.
///
/// PF-007: the discriminating observable is the absent key — if serde(skip) is
/// accidentally removed, the key would appear and this test would catch it.
#[test]
fn test_match_positions_absent_from_json_output_ac4() {
    use crate::cmd::search::types::{ResolvedResult, SnippetContext, SnippetLine};

    let result = ResolvedResult {
        path: "src/auth.rs".to_string(),
        score: 5.0,
        field: "function_signature".to_string(),
        line_number: Some(2),
        line_range: Some(2..3),
        snippet: Some(SnippetContext {
            lines: vec![SnippetLine {
                line_number: 2,
                content: "pub fn authentic_fn()".to_string(),
                is_match: true,
            }],
        }),
        stale: false,
        match_positions: vec![0..5, 10..15], // populated — must NOT appear in JSON
    };

    let value = serde_json::to_value(&result).expect("ResolvedResult must serialize");

    // AC4: match_positions must be absent (field is #[serde(skip)]).
    assert!(
        value.get("match_positions").is_none(),
        "AC4: match_positions must be absent from JSON; got: {:?}",
        value.get("match_positions")
    );

    // AC4: line_number must be an integer.
    assert_eq!(
        value["line_number"], 2,
        "AC4: line_number must be the integer 2"
    );
    // AC6: line_range must be {{start:2, end:3}}.
    assert_eq!(
        value["line_range"]["start"], 2,
        "AC6: line_range.start must be 2"
    );
    assert_eq!(
        value["line_range"]["end"], 3,
        "AC6: line_range.end must be 3 (single-anchor-line)"
    );
}

// ============================================================================
// D-5 / AD-404-11: has_more on pure-lexical text path
// ============================================================================

/// Create a project where N files each contain a unique function that also
/// uses the shared token "qxz_shared_probe" so that a query for that token
/// matches all N files.
fn create_multi_match_project(root: &std::path::Path, n: usize) {
    let src = root.join("src");
    fs::create_dir_all(&src).unwrap();
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
    for i in 1..=n {
        fs::write(
            src.join(format!("f{i:02}.rs")),
            format!("/// File {i}.\npub fn qxz_shared_probe_{i}() -> u32 {{ {i} }}\n"),
        )
        .unwrap();
    }
}

/// D-5 / AD-404-11: `has_more` must be `true` on the pure-lexical text path
/// when verified results exceed the page (limit < total matches).
///
/// Pre-fix: `has_more` was hardcoded `false` at all `QueryOutput` construction
/// sites in `query.rs`, so callers could not distinguish "last page" from "more
/// pages exist" on the text-query surface (D-5 contract unfulfilled).
///
/// Post-fix: `resolve_paths_and_snippets_verified` uses "probe one more" to
/// set `has_more = true` when verified results > limit + offset.
#[test]
fn test_has_more_true_on_pure_lexical_text_path_when_limit_less_than_total() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    // 3 files each containing "qxz_shared_probe"; limit=1 → has_more must be true.
    create_multi_match_project(&root, 3);

    let config = QueryConfig {
        text: "qxz_shared_probe".to_string(),
        limit: 1,
        offset: None,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        phrase: false,
        near: None,
        lang: None,
    };
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    // Exactly 1 result returned (limit honored).
    assert_eq!(
        output.results.len(),
        1,
        "D-5: limit=1 must return exactly 1 result; got {}",
        output.results.len()
    );

    // D-5: has_more must be true because 3 files match but only 1 is on this page.
    assert!(
        output.has_more,
        "D-5 (pure-lexical text path): has_more must be true when limit=1 \
         and multiple files match; got has_more=false. Pre-fix regression — \
         has_more was hardcoded false at the QueryOutput construction site."
    );
}

/// D-5 / AD-404-11: `has_more` must be `false` when all verified results fit
/// on one page (last-page / single-page invariant).
///
/// Paired with `test_has_more_true_on_pure_lexical_text_path_when_limit_less_than_total`
/// to guard both directions of the D-5 contract.
#[test]
fn test_has_more_false_when_all_results_fit_on_page() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache_dir = dir.path().join("cache");
    fs::create_dir_all(&cache_dir).unwrap();
    // 2 files; limit=100 → all results fit on one page → has_more must be false.
    create_multi_match_project(&root, 2);

    let config = QueryConfig {
        text: "qxz_shared_probe".to_string(),
        limit: 100,
        offset: None,
        json: false,
        root: root.to_path_buf(),
        cache_dir: cache_dir.to_path_buf(),
        phrase: false,
        near: None,
        lang: None,
    };
    let output = execute_query(&config, &TEST_ANALYTICS).unwrap();

    assert!(
        !output.has_more,
        "D-5: has_more must be false when all results fit on one page; \
         got has_more=true with {} results and limit=100",
        output.results.len()
    );
}

/// The bounded-page notice names the page size and the next offset, so a reader
/// can page forward without guessing.
#[test]
fn bounded_page_notice_names_count_and_next_offset() {
    let notice = bounded_page_notice(5, 0, 5);
    assert!(
        notice.contains("more results exist"),
        "notice must say 'more results exist', got: {notice:?}"
    );
    assert!(
        notice.contains("showing 5"),
        "notice must include the page count, got: {notice:?}"
    );
    assert!(
        notice.contains("--offset 5"),
        "notice must include the next-offset remedy, got: {notice:?}"
    );
}

/// A hostile offset near `usize::MAX` must not overflow the next-offset hint.
#[test]
fn bounded_page_notice_saturates_next_offset() {
    let notice = bounded_page_notice(1, usize::MAX, 10);
    assert!(
        notice.contains(&format!("--offset {}", usize::MAX)),
        "next offset must saturate at usize::MAX, got: {notice:?}"
    );
}

// ============================================================================
// AC-403: positional composition unit tests (#403)
// ============================================================================

// ── AC-403-T1: verify_mode_for truth table ───────────────────────────────────

/// AC-403 / AD-403-1: verify_mode_for must map all four (phrase, near) cells
/// to the correct VerifyMode variant (exhaustive tuple match — no combination
/// should fall through to a default).
#[test]
fn ac403_verify_mode_for_truth_table() {
    // (false, None) → Substring
    assert!(
        matches!(verify_mode_for(false, None), VerifyMode::Substring),
        "AD-403-1: (false, None) must map to Substring"
    );
    // (false, Some(5)) → Near(5)
    assert!(
        matches!(verify_mode_for(false, Some(5)), VerifyMode::Near(5)),
        "AD-403-1: (false, Some(5)) must map to Near(5)"
    );
    // (true, None) → Phrase
    assert!(
        matches!(verify_mode_for(true, None), VerifyMode::Phrase),
        "AD-403-1: (true, None) must map to Phrase"
    );
    // (true, Some(4)) → PhraseNear(4)
    assert!(
        matches!(verify_mode_for(true, Some(4)), VerifyMode::PhraseNear(4)),
        "AD-403-1: (true, Some(4)) must map to PhraseNear(4)"
    );
}

// ── AC-403-T2: positional_inert_notice truth table ───────────────────────────

/// AC-403 / AD-403-5: positional_inert_notice truth table.
///
/// The notice fires iff at least one positional flag is set AND has_text=false.
/// When has_text=true the flags are active — no notice. When no positional flag
/// is set — no notice regardless of has_text.
#[test]
fn ac403_positional_inert_notice_truth_table() {
    // No positional flag → always None.
    assert_eq!(
        positional_inert_notice(false, None, false),
        None,
        "no flags + no text: must be None"
    );
    assert_eq!(
        positional_inert_notice(false, None, true),
        None,
        "no flags + text: must be None"
    );

    // Flags present + text → None (flags are active).
    assert_eq!(
        positional_inert_notice(true, None, true),
        None,
        "--phrase + text: must be None (flag is honored)"
    );
    assert_eq!(
        positional_inert_notice(false, Some(5), true),
        None,
        "--near 5 + text: must be None (flag is honored)"
    );
    assert_eq!(
        positional_inert_notice(true, Some(3), true),
        None,
        "--phrase --near 3 + text: must be None (flags are honored)"
    );

    // Flags present + no text → Some(notice).
    let n1 = positional_inert_notice(true, None, false);
    assert!(n1.is_some(), "--phrase + no text: must be Some(notice)");
    assert!(
        n1.as_deref().unwrap_or("").contains("--phrase"),
        "--phrase notice must name the flag; got {:?}",
        n1
    );

    let n2 = positional_inert_notice(false, Some(7), false);
    assert!(n2.is_some(), "--near 7 + no text: must be Some(notice)");
    assert!(
        n2.as_deref().unwrap_or("").contains("--near 7"),
        "--near 7 notice must name the flag and value; got {:?}",
        n2
    );

    let n3 = positional_inert_notice(true, Some(3), false);
    assert!(
        n3.is_some(),
        "--phrase --near 3 + no text: must be Some(notice)"
    );
    let n3_text = n3.as_deref().unwrap_or("");
    assert!(
        n3_text.contains("--phrase") && n3_text.contains("--near 3"),
        "--phrase --near 3 notice must name both flags; got {:?}",
        n3
    );
}

// ── AC-403-T3: near_diagnostic_notice truth table ────────────────────────────

/// AC-403 / AD-403-6: near_diagnostic_notice truth table (4 cells × has_text).
///
/// Fires on single-word query and N < word_count - 1. Does NOT fire when near
/// is None, when query is empty, or when N is large enough.
#[test]
fn ac403_near_diagnostic_notice_truth_table() {
    // near=None → always None.
    assert_eq!(near_diagnostic_notice(None, "alpha beta"), None);
    assert_eq!(near_diagnostic_notice(None, "alpha"), None);

    // Empty query → None (covered by positional_inert_notice upstream).
    assert_eq!(near_diagnostic_notice(Some(3), ""), None);
    assert_eq!(near_diagnostic_notice(Some(3), "   "), None);

    // Single-word query + any N → Some(notice).
    let single = near_diagnostic_notice(Some(5), "alpha");
    assert!(
        single.is_some(),
        "single-word query + --near 5 must produce notice"
    );
    assert!(
        single.as_deref().unwrap_or("").contains("single-word"),
        "notice must explain 'single-word'; got {:?}",
        single
    );

    // N < word_count - 1 → Some(notice).
    // 3-word query: word_count=3, N must be >= 2 to be satisfiable.
    // N=1 < 2 → notice.
    let n1 = near_diagnostic_notice(Some(1), "alpha beta gamma");
    assert!(
        n1.is_some(),
        "--near 1 on 3-word query must produce notice (unsatisfiable: span >= 2)"
    );

    // N=2 == word_count-1 → no notice (smallest satisfiable span for 3-word query).
    assert_eq!(
        near_diagnostic_notice(Some(2), "alpha beta gamma"),
        None,
        "--near 2 on 3-word query is satisfiable (span == word_count - 1): must be None"
    );

    // N=10 >> word_count-1 → no notice.
    assert_eq!(
        near_diagnostic_notice(Some(10), "alpha beta gamma"),
        None,
        "--near 10 on 3-word query is satisfiable: must be None"
    );

    // 2-word query: word_count=2, N must be >= 1; N=0 is rejected by parse,
    // so any N >= 1 is satisfiable for a 2-word query.
    assert_eq!(
        near_diagnostic_notice(Some(1), "alpha beta"),
        None,
        "--near 1 on 2-word query is satisfiable: must be None"
    );
}

/// Verify that `near_diagnostic_notice` uses the punctuation-aware word tokenizer
/// (count_query_word_tokens / collect_word_spans) rather than whitespace splitting,
/// so punctuated code-search queries produce the correct word count.
///
/// The two divergence cases are:
/// - Undercount: `"foo.bar baz"` → whitespace=2, real=3 tokens.
///   --near 1 is unsatisfiable for 3 real tokens (need span >= 2), so the notice
///   MUST fire.  With a broken whitespace count (2 tokens, need span >= 1) it
///   would silently be skipped.
/// - Overcount / misfire: `"foo::bar"` → whitespace=1, real=2 tokens.
///   --near 5 is satisfiable for 2 real tokens, so the notice MUST NOT fire.
///   With a broken whitespace count (1 token) the "single-word" notice would
///   erroneously fire even though --near is meaningful.
#[test]
fn ac403_near_diagnostic_punctuation_tokenizer() {
    // Undercount divergence: "foo.bar baz" is 3 real tokens (foo, bar, baz).
    // --near 1 requires span >= 2, so it is unsatisfiable → notice must fire.
    let undercnt = near_diagnostic_notice(Some(1), "foo.bar baz");
    assert!(
        undercnt.is_some(),
        "--near 1 on 3-real-token query 'foo.bar baz' must produce notice (unsatisfiable)"
    );

    // Verify the notice says it cannot match (not "single-word").
    assert!(
        undercnt.as_deref().unwrap_or("").contains("cannot match"),
        "notice for 3-token unsatisfiable near must say 'cannot match'; got {:?}",
        undercnt
    );

    // Satisfiable threshold: --near 2 == word_count - 1 for 3 tokens → no notice.
    assert_eq!(
        near_diagnostic_notice(Some(2), "foo.bar baz"),
        None,
        "--near 2 on 3-token 'foo.bar baz' is satisfiable: must be None"
    );

    // Overcount / misfire divergence: "foo::bar" is 2 real tokens (foo, bar).
    // --near 5 is satisfiable (5 >= 2-1 = 1) → no notice must fire.
    assert_eq!(
        near_diagnostic_notice(Some(5), "foo::bar"),
        None,
        "--near 5 on 2-token 'foo::bar' is satisfiable: must not emit single-word notice"
    );

    // "foo::bar" with --near 0 is blocked by parse, so test --near 1 (satisfiable).
    assert_eq!(
        near_diagnostic_notice(Some(1), "foo::bar"),
        None,
        "--near 1 on 2-token 'foo::bar' is satisfiable: must be None"
    );

    // Single punctuation-joined token still fires: "foo_bar" is one token.
    let single_token = near_diagnostic_notice(Some(3), "foo_bar");
    assert!(
        single_token.is_some(),
        "--near 3 on single-token 'foo_bar' must produce single-word notice"
    );
    assert!(
        single_token
            .as_deref()
            .unwrap_or("")
            .contains("single-word"),
        "notice for single-token near must say 'single-word'; got {:?}",
        single_token
    );
}

// ============================================================================
// #405 — ast_coverage_notice pure decision seam (AD-405-4 / PF-008 / D-4)
// ============================================================================
//
// `ast_coverage_notice` is the single source of truth for the AST size-coverage
// notice wording, shared by run_build / run_update / run_ast_standalone /
// run_query / run_stats. AD-405-4 requires every emitting site to print a string
// starting with the shared `AST_COVERAGE_PREFIX` prefix, and requires the seam to
// return `None` on a clean corpus so callers never gate on `is_clean()` themselves.
// These assertions are falsifiable (PF-007): each fails if the #405 notice contract
// regresses.

/// NEGATIVE (AC-405-8): a clean corpus (every file within the AST size cap) is
/// silent — the seam returns `None`, so no site emits a spurious notice and the
/// "pure-lexical carries no AST caveat" promise holds.
#[test]
fn ast_coverage_notice_none_on_clean_corpus() {
    use super::ast_coverage_notice;
    use rskim_search::{CoverageEntry, ast_coverage};

    let clean = ast_coverage(std::iter::once(CoverageEntry {
        path: "src/small.rs",
        lang: "rust",
        size: Some(100),
    }));
    assert!(
        clean.is_clean(),
        "an in-cap file must yield a clean coverage"
    );
    assert_eq!(
        ast_coverage_notice(&clean),
        None,
        "clean corpus must produce no coverage notice (AC-405-8)"
    );
}

/// POSITIVE (AD-405-4 / PF-008): an excluded file (over the 1 MiB cap) fires the
/// notice, which MUST start with the shared `AST_COVERAGE_PREFIX` prefix, report
/// the excluded count, and carry the per-language breakdown.
#[test]
fn ast_coverage_notice_fires_with_shared_prefix_when_excluded() {
    use super::{AST_COVERAGE_PREFIX, ast_coverage_notice};
    use rskim_search::{CoverageEntry, ast_coverage};

    let over_cap = rskim_core::AST_SIZE_LIMIT_DEFAULT + 1;
    let dirty = ast_coverage(std::iter::once(CoverageEntry {
        path: "src/huge.rs",
        lang: "rust",
        size: Some(over_cap),
    }));
    assert!(
        !dirty.is_clean(),
        "an over-cap file must yield a non-clean coverage"
    );

    let notice =
        ast_coverage_notice(&dirty).expect("an excluded file must produce a coverage notice");
    assert!(
        notice.starts_with(AST_COVERAGE_PREFIX),
        "notice must start with the shared prefix (AD-405-4/PF-008): got {notice:?}"
    );
    assert!(
        notice.contains("1 file(s) exceed"),
        "notice must report the excluded count: got {notice:?}"
    );
    assert!(
        notice.contains("By language: rust 1"),
        "notice must carry the per-language breakdown: got {notice:?}"
    );
}
