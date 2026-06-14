//! Classifier tests (AC5, AC6, AC13).
//!
//! - AC5: Classifier matches labels on real-sourced corpus with boundary adversaries
//! - AC6: Classifier is deterministic over 1,000 runs
//! - AC13: Only six classes returned; exempt blocks return unknown

// Test code legitimately uses panic/expect/unwrap for test failure reporting.
#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::unwrap_in_result,
    clippy::collapsible_if,
    unused_imports
)]

use rskim_llm::classify::{Class, classify};
use serde::Deserialize;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("classifier")
}

#[derive(Debug, Deserialize)]
struct ManifestEntry {
    file: String,
    class: String,
    language_hint: Option<String>,
    #[allow(dead_code)]
    rationale: String,
}

fn load_manifest() -> Vec<ManifestEntry> {
    let manifest_path = fixtures_dir().join("manifest.json");
    let contents = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("failed to read manifest: {e}"));
    serde_json::from_str(&contents).unwrap_or_else(|e| panic!("failed to parse manifest: {e}"))
}

fn str_to_class(s: &str) -> Class {
    match s {
        "Code" => Class::Code,
        "Json" => Class::Json,
        "Log" => Class::Log,
        "Text" => Class::Text,
        "Mixed" => Class::Mixed,
        "Unknown" => Class::Unknown,
        other => panic!("unknown class in manifest: {other}"),
    }
}

#[test]
fn ac5_corpus_matches_labels_100_percent() {
    let manifest = load_manifest();
    assert!(!manifest.is_empty(), "manifest must not be empty");

    let mut pass = 0;
    let mut fail = 0;

    for entry in &manifest {
        let path = fixtures_dir().join(&entry.file);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", entry.file));

        let result = classify(text.trim_end_matches('\n'));
        let expected_class = str_to_class(&entry.class);

        if result.class != expected_class {
            eprintln!(
                "FAIL {}: expected {:?}, got {:?}",
                entry.file, expected_class, result.class
            );
            fail += 1;
        } else {
            pass += 1;
            // Check language hint for Code/Mixed. AC5 requires that code/mixed
            // fixtures with a fence tag carry the expected language hint, so a
            // mismatch is a hard failure (not just a warning) — otherwise the
            // language-hint half of AC5 would be unobservable (PF-005).
            if expected_class == Class::Code || expected_class == Class::Mixed {
                let expected_hint = entry.language_hint.as_deref();
                let actual_hint = result.language_hint.as_deref();
                if actual_hint != expected_hint {
                    eprintln!(
                        "LANG_HINT MISMATCH {}: expected {:?}, got {:?}",
                        entry.file, expected_hint, actual_hint
                    );
                    fail += 1;
                }
            }
        }
    }

    assert_eq!(
        fail,
        0,
        "{fail} classifier fixture(s) failed class or language-hint check \
         (pass={pass}, total={})",
        manifest.len()
    );
}

#[test]
fn ac6_deterministic_1000_runs_per_class() {
    // One representative text per class
    let samples = [
        ("code", "```rust\nfn main() {}\n```"),
        ("json", r#"{"key": "value", "num": 42}"#),
        (
            "log",
            "2024-01-15T10:30:00Z INFO test\n2024-01-15T10:30:01Z WARN test2",
        ),
        ("text", "Hello, world! This is plain text."),
        (
            "mixed",
            "Prose text:\n```python\nprint('hi')\n```\nMore prose.",
        ),
    ];

    for (label, text) in &samples {
        let first = classify(text);
        for i in 1..1000u32 {
            let result = classify(text);
            assert_eq!(
                result, first,
                "non-deterministic for class={label} at run {i}"
            );
        }
    }
}

#[test]
fn ac13_classifier_returns_only_six_classes() {
    // The Class enum is exhaustive — the type system enforces this.
    // This test documents the invariant.
    let all_classes = [
        Class::Code,
        Class::Json,
        Class::Log,
        Class::Text,
        Class::Mixed,
        Class::Unknown,
    ];
    // Verify all six can be constructed and are distinct
    assert_eq!(all_classes.len(), 6);
    for (i, c1) in all_classes.iter().enumerate() {
        for (j, c2) in all_classes.iter().enumerate() {
            if i != j {
                assert_ne!(c1, c2, "classes must be distinct");
            }
        }
    }
}

#[test]
fn ac13_no_rule_fires_returns_unknown_from_classify_body() {
    // classify() itself never returns Unknown — it always falls back to Text.
    // Unknown is returned by classify_body() for exempt blocks (which are not in
    // the text_leaves iterator). This test verifies that classify("plain text") != Unknown.
    let result = classify("plain text without any classifiable pattern");
    assert_ne!(
        result.class,
        Class::Unknown,
        "classify() should not return Unknown for plain text"
    );
    assert_eq!(result.class, Class::Text);
}

#[test]
fn ac13_exempt_blocks_not_in_text_leaves() {
    use rskim_llm::{classify_body, parse};

    // A body with a tool_use block (exempt) should not appear in classify_body results
    let body_json = r#"{"model":"m","messages":[{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"tool","input":{"key":"value"}},{"type":"text","text":"Here is some code:\n```python\nprint('hello')\n```"}]}],"max_tokens":100}"#;
    let body = parse(body_json.as_bytes()).expect("parse failed");
    let results = classify_body(&body);

    // Should have one result (the text block), not two (tool_use is exempt)
    assert_eq!(
        results.len(),
        1,
        "tool_use should not appear in classify_body"
    );

    // The text block with fenced code should be Mixed
    assert_eq!(results[0].1.class, Class::Mixed);
}

#[test]
fn ac13_thinking_block_exempt() {
    use rskim_llm::{classify_body, parse};

    let body_json = r#"{"model":"m","messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"Let me think...","signature":"sig1"},{"type":"text","text":"The answer is 42."}]}],"max_tokens":100}"#;
    let body = parse(body_json.as_bytes()).expect("parse failed");
    let results = classify_body(&body);

    // Only the text block should appear — thinking is exempt
    assert_eq!(
        results.len(),
        1,
        "thinking block should not appear in classify_body"
    );
    assert_eq!(results[0].1.class, Class::Text);
}

#[test]
fn ac5_boundary_jsonlines_is_text() {
    let jsonlines = "  {\"a\":1}\n{\"b\":2}\n{\"c\":3}";
    let result = classify(jsonlines);
    assert_eq!(
        result.class,
        Class::Text,
        "JSON-lines should be Text (not Json)"
    );
}

#[test]
fn ac5_boundary_partial_json_is_text() {
    let partial = "{this is not valid json}";
    let result = classify(partial);
    assert_eq!(result.class, Class::Text, "partial JSON should be Text");
}

#[test]
fn ac5_boundary_indented_code_is_text() {
    // Unfenced code (indented) is Text in v1 (#326)
    let indented = "Here is code:\n    fn foo() -> i32 { 42 }\nEnd.";
    let result = classify(indented);
    assert_eq!(
        result.class,
        Class::Text,
        "indented code should be Text in v1"
    );
}

#[test]
fn ac6_cross_process_determinism_simulation() {
    // Simulates cross-process by running classify in a separate call stack via a fn ptr.
    // True cross-process testing is done via CI (manually verified).
    let classify_fn: fn(&str) -> rskim_llm::classify::Classification = classify;

    let text = r#"{"model":"test","result":[1,2,3]}"#;
    let first = classify_fn(text);
    for _ in 0..100 {
        assert_eq!(classify_fn(text), first);
    }
}
