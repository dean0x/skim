#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Integration tests for `rskim-tokens`.
//!
//! Covers all Acceptance Criteria:
//! - AC1: All four counter constructors work under default features.
//! - AC2: Closure adapter drives rskim_core::truncate_to_token_budget.
//! - AC3: cl100k parity with legacy tokens.rs (incl. special tokens).
//! - AC5: Anthropic offline is deterministic, network-free, >= cl100k.
//! - AC6: Heuristic never undercounts max(cl100k, o200k) incl. adversarial.
//! - AC7: encoding_for_model resolves all mandated IDs.
//! - AC8: Unknown IDs resolve via two-tier rule without panic.
//! - AC11: Counter is Send + Sync (compile-time static assertion).
//! - AC12: 100KB counting latency < 25ms.
//! - AC21: encoding_for_provider_model provider×model → Encoding truth table.

use rskim_tokens::{Counter, Encoding, TokenError, counter_for_model, encoding_for_model};
use std::sync::Arc;
use std::thread;
use std::time::Instant;

// ============================================================================
// AC1: All four counters constructible and usable under default features
// ============================================================================

#[test]
fn ac1_all_four_counters_construct_and_count() {
    let cl100k = Counter::new(Encoding::Cl100k).expect("cl100k");
    let o200k = Counter::new(Encoding::O200k).expect("o200k");
    let anthropic = Counter::new(Encoding::AnthropicOffline).expect("anthropic_offline");
    let heuristic = Counter::new(Encoding::Heuristic).expect("heuristic");

    let nonempty = "Hello, world! This is a test of token counting.";

    // Non-empty input: all counters must return a positive count
    assert!(cl100k.count(nonempty) > 0, "cl100k: positive count");
    assert!(o200k.count(nonempty) > 0, "o200k: positive count");
    assert!(
        anthropic.count(nonempty) > 0,
        "anthropic_offline: positive count"
    );
    assert!(heuristic.count(nonempty) > 0, "heuristic: positive count");

    // Empty input: all must return 0
    assert_eq!(cl100k.count(""), 0, "cl100k: empty → 0");
    assert_eq!(o200k.count(""), 0, "o200k: empty → 0");
    assert_eq!(anthropic.count(""), 0, "anthropic_offline: empty → 0");
    assert_eq!(heuristic.count(""), 0, "heuristic: empty → 0");
}

#[test]
fn ac1_encoding_roundtrip() {
    assert_eq!(
        Counter::new(Encoding::Cl100k).unwrap().encoding(),
        Encoding::Cl100k
    );
    assert_eq!(
        Counter::new(Encoding::O200k).unwrap().encoding(),
        Encoding::O200k
    );
    assert_eq!(
        Counter::new(Encoding::AnthropicOffline).unwrap().encoding(),
        Encoding::AnthropicOffline
    );
    assert_eq!(
        Counter::new(Encoding::Heuristic).unwrap().encoding(),
        Encoding::Heuristic
    );
}

#[test]
fn ac1_counter_for_model_convenience() {
    let counter = counter_for_model("gpt-4o").expect("gpt-4o counter");
    assert_eq!(counter.encoding(), Encoding::O200k);
    assert!(counter.count("test") > 0);
}

// ============================================================================
// AC2: Closure adapter drives rskim_core::truncate_to_token_budget
// ============================================================================

#[test]
fn ac2_closure_adapter_drives_truncate() {
    use rskim_core::{Language, truncate_to_token_budget};

    let text = "fn foo(x: i32) -> i32 {\n    x * 2\n}\n\
                fn bar(y: i32) -> i32 {\n    y + 1\n}\n\
                fn baz(z: i32) -> i32 {\n    z - 1\n}\n";

    for encoding in [
        Encoding::Cl100k,
        Encoding::O200k,
        Encoding::AnthropicOffline,
        Encoding::Heuristic,
    ] {
        let counter = Counter::new(encoding).unwrap();
        let budget = 20usize;

        let truncated =
            truncate_to_token_budget(text, Language::Rust, budget, counter.as_closure(), None)
                .unwrap_or_else(|e| panic!("truncate failed for {encoding:?}: {e}"));

        let actual_count = counter.count(&truncated);
        assert!(
            actual_count <= budget,
            "encoding {encoding:?}: output tokens {actual_count} > budget {budget}"
        );
    }
}

#[test]
fn ac2_near_zero_budget_returns_empty_or_marker() {
    use rskim_core::{Language, truncate_to_token_budget};

    let counter = Counter::new(Encoding::Cl100k).unwrap();
    let text = "fn hello() -> &'static str { \"world\" }";

    // Near-zero budget: per the documented invariant, if the budget is smaller
    // than the omission marker, an empty string is returned (not a panic).
    let result = truncate_to_token_budget(text, Language::Rust, 1, counter.as_closure(), None);
    match result {
        Ok(s) => {
            let count = counter.count(&s);
            assert!(
                count <= 1,
                "near-zero budget: output tokens {count} > budget 1; output: {s:?}"
            );
        }
        Err(e) => {
            // Some truncation errors are also acceptable per the invariant doc
            panic!("unexpected error on near-zero budget: {e}");
        }
    }
}

// ============================================================================
// AC3: cl100k parity with legacy tokens.rs (incl. special tokens)
// ============================================================================

/// Golden corpus captured from legacy `tokens.rs` before migration.
/// Each tuple is (input, expected_count).
///
/// These values were produced by the pre-migration `count_tokens` function
/// which uses `CoreBPE::encode_with_special_tokens`. They must match exactly
/// after migration (AC3).
/// Golden corpus: produced by `tiktoken-rs 0.7.0` `cl100k_base().encode_with_special_tokens()`.
/// Regeneration: `cargo run -p rskim-tokens --bin check_golden` (see src/bin/check_golden.rs).
/// Pinned against tiktoken-rs 0.7.0 (workspace version).
const CL100K_GOLDEN: &[(&str, usize)] = &[
    ("Hello, world!", 4),
    // Special token — must be counted as 1 token, not tokenised as text (AC3)
    ("<|endoftext|>", 1),
    // Multi-line code (verified: 22 tokens via tiktoken-rs 0.7.0 cl100k_base)
    ("fn add(a: i32, b: i32) -> i32 {\n    a + b\n}", 22),
    // Empty
    ("", 0),
    // Single character
    ("x", 1),
    // Unicode (CJK: 6 tokens in cl100k)
    ("日本語テスト", 6),
    // Long English text
    ("The quick brown fox jumps over the lazy dog", 9),
];

#[test]
fn ac3_cl100k_parity_with_legacy() {
    let counter = Counter::new(Encoding::Cl100k).expect("cl100k init");

    for (input, expected) in CL100K_GOLDEN {
        let got = counter.count(input);
        assert_eq!(
            got, *expected,
            "cl100k parity: input={input:?} expected={expected} got={got}"
        );
    }
}

#[test]
fn ac3_special_token_counted_as_one() {
    let counter = Counter::new(Encoding::Cl100k).expect("cl100k init");
    // <|endoftext|> must be a single special token, not plain text
    let count = counter.count("<|endoftext|>");
    assert_eq!(
        count, 1,
        "<|endoftext|> must be 1 token (special-token semantics)"
    );
}

// ============================================================================
// AC4: o200k golden vectors (checked-in values from pinned tiktoken-rs reference)
// ============================================================================
// Provenance: generated by `cargo run -p rskim-tokens --bin check_golden`
// (see src/bin/check_golden.rs) using tiktoken-rs 0.7.0 `o200k_base()`.
//
// tiktoken-rs 0.7.0 is a faithful Rust port of upstream Python tiktoken: it
// uses the same BPE vocabulary files and produces bytewise-identical token
// sequences for any input. The counts below therefore match what Python
// tiktoken 0.9.0 (the upstream that introduced o200k_base) produces for the
// same inputs — the Rust crate is not an independent implementation, it
// re-uses the same vocabulary assets.
//
// Pinned workspace version: tiktoken-rs = "0.7" (see workspace Cargo.toml).
// Regeneration: `cargo run -p rskim-tokens --bin check_golden`

/// Golden corpus: verified against `tiktoken-rs 0.7.0` `o200k_base().encode_with_special_tokens()`.
///
/// These counts are bytewise-identical to upstream Python `tiktoken` output for
/// o200k_base: tiktoken-rs uses the same BPE vocabulary assets as the Python
/// library (not an independent implementation).
///
/// Regeneration: `cargo run -p rskim-tokens --bin check_golden` (see src/bin/check_golden.rs).
/// Pinned against workspace version tiktoken-rs 0.7.0.
const O200K_GOLDEN: &[(&str, usize)] = &[
    ("Hello, world!", 4),
    // Special token for o200k — same as cl100k
    ("<|endoftext|>", 1),
    // Multi-line code (verified: 22 tokens via tiktoken-rs 0.7.0 o200k_base)
    ("fn add(a: i32, b: i32) -> i32 {\n    a + b\n}", 22),
    // Empty
    ("", 0),
    // Single character
    ("x", 1),
    // Unicode (CJK: 4 tokens in o200k — different from cl100k's 6)
    ("日本語テスト", 4),
    // English prose
    ("The quick brown fox jumps over the lazy dog", 9),
];

#[test]
fn ac4_o200k_golden_vectors() {
    let counter = Counter::new(Encoding::O200k).expect("o200k init");

    for (input, expected) in O200K_GOLDEN {
        let got = counter.count(input);
        assert_eq!(
            got, *expected,
            "o200k golden: input={input:?} expected={expected} got={got}"
        );
    }
}

// ============================================================================
// AC5: Anthropic offline — deterministic, >= cl100k per document
// ============================================================================

#[test]
fn ac5_anthropic_offline_deterministic() {
    let counter = Counter::new(Encoding::AnthropicOffline).expect("anthropic_offline init");

    for input in ["", "hello", "<|endoftext|>", "fn foo() -> i32 { 42 }"] {
        let first = counter.count(input);
        let second = counter.count(input);
        assert_eq!(
            first, second,
            "anthropic_offline must be deterministic for {input:?}"
        );
    }
}

#[test]
fn ac5_anthropic_offline_gte_cl100k() {
    let cl100k = Counter::new(Encoding::Cl100k).expect("cl100k");
    let anthropic = Counter::new(Encoding::AnthropicOffline).expect("anthropic_offline");

    let corpus = [
        "",
        "hello",
        "<|endoftext|>",
        "The quick brown fox jumps over the lazy dog.",
        "fn foo(a: i32, b: i32) -> i32 { a + b }",
        "日本語テスト",
        "SELECT * FROM users WHERE id = 1;",
        "{ \"key\": \"value\", \"number\": 42 }",
    ];

    for input in corpus {
        let cl_count = cl100k.count(input);
        let ap_count = anthropic.count(input);
        assert!(
            ap_count >= cl_count,
            "anthropic_offline must be >= cl100k: input={input:?} anthropic={ap_count} cl100k={cl_count}"
        );
    }
}

#[test]
fn ac5_anthropic_offline_zero_network_io() {
    // Structural proof: AnthropicOffline counter works without any network setup.
    // If it attempted network I/O, this test would fail in offline environments.
    // This is a compile-time structural guarantee (no network deps in default build),
    // confirmed by CI dependency-tree assertion (AC9).
    let counter = Counter::new(Encoding::AnthropicOffline).expect("anthropic_offline");
    let n = counter.count("hello world");
    assert!(n > 0, "anthropic_offline must count without network");
}

// ============================================================================
// AC6: Heuristic never undercounts max(cl100k, o200k) incl. adversarial inputs
// ============================================================================

#[test]
fn ac6_heuristic_gte_max_cl100k_o200k() {
    let cl100k = Counter::new(Encoding::Cl100k).expect("cl100k");
    let o200k = Counter::new(Encoding::O200k).expect("o200k");
    let heuristic = Counter::heuristic();

    let corpus: &[&str] = &[
        // Mixed corpus (prose, code, JSON, logs, CJK, emoji)
        "The quick brown fox jumps over the lazy dog.",
        "fn add(a: i32, b: i32) -> i32 { a + b }",
        "{ \"key\": \"value\", \"count\": 42 }",
        "[2024-01-01 12:00:00] INFO: Server started on port 8080",
        "日本語テスト — multilingual text with CJK characters",
        "Emoji: 🦀🔥💡✨🎉",
        // Adversarial inputs
        "",
        "x",
    ];

    for input in corpus {
        let cl_count = cl100k.count(input);
        let o2_count = o200k.count(input);
        let h_count = heuristic.count(input);
        let max_bpe = cl_count.max(o2_count);
        assert!(
            h_count >= max_bpe,
            "heuristic must >= max(cl100k, o200k): input={input:?} heuristic={h_count} cl100k={cl_count} o200k={o2_count}"
        );
    }
}

#[test]
fn ac6_heuristic_adversarial_large_repeat() {
    let cl100k = Counter::new(Encoding::Cl100k).expect("cl100k");
    let o200k = Counter::new(Encoding::O200k).expect("o200k");
    let heuristic = Counter::heuristic();

    // 10000x single-byte repeat
    let input = "a".repeat(10_000);
    let h = heuristic.count(&input);
    let cl = cl100k.count(&input);
    let o2 = o200k.count(&input);
    assert!(
        h >= cl.max(o2),
        "heuristic >= max(cl100k, o200k) for 10000x 'a': h={h} cl={cl} o2={o2}"
    );
}

#[test]
fn ac6_heuristic_adversarial_pure_cjk() {
    let cl100k = Counter::new(Encoding::Cl100k).expect("cl100k");
    let o200k = Counter::new(Encoding::O200k).expect("o200k");
    let heuristic = Counter::heuristic();

    // Pure CJK block (each char is 3 UTF-8 bytes)
    let input = "日".repeat(100);
    let h = heuristic.count(&input);
    let cl = cl100k.count(&input);
    let o2 = o200k.count(&input);
    assert!(
        h >= cl.max(o2),
        "heuristic >= max(cl100k, o200k) for CJK block: h={h} cl={cl} o2={o2}"
    );
}

#[test]
fn ac6_heuristic_adversarial_pure_emoji() {
    let cl100k = Counter::new(Encoding::Cl100k).expect("cl100k");
    let o200k = Counter::new(Encoding::O200k).expect("o200k");
    let heuristic = Counter::heuristic();

    // Pure emoji block (each emoji is 4 UTF-8 bytes)
    let input = "🦀".repeat(100);
    let h = heuristic.count(&input);
    let cl = cl100k.count(&input);
    let o2 = o200k.count(&input);
    assert!(
        h >= cl.max(o2),
        "heuristic >= max(cl100k, o200k) for emoji block: h={h} cl={cl} o2={o2}"
    );
}

#[test]
fn ac6_heuristic_never_errors() {
    let counter = Counter::heuristic();
    // Infallible: calling count on any input must not panic or fail
    assert_eq!(counter.count(""), 0);
    assert!(counter.count("hello") > 0);
}

// ============================================================================
// AC7: encoding_for_model resolves all mandated IDs
// ============================================================================

#[test]
fn ac7_mandated_ids_resolve_correctly() {
    // cl100k_base encodings
    assert_eq!(encoding_for_model("gpt-3.5-turbo"), Encoding::Cl100k);
    assert_eq!(encoding_for_model("gpt-4"), Encoding::Cl100k);
    assert_eq!(encoding_for_model("gpt-4-turbo"), Encoding::Cl100k);

    // o200k_base encodings
    assert_eq!(encoding_for_model("gpt-4o"), Encoding::O200k);
    assert_eq!(encoding_for_model("gpt-4o-mini"), Encoding::O200k);
    assert_eq!(encoding_for_model("o1"), Encoding::O200k);
    assert_eq!(encoding_for_model("o3"), Encoding::O200k);
    assert_eq!(encoding_for_model("gpt-4.1"), Encoding::O200k);

    // Anthropic offline
    assert_eq!(
        encoding_for_model("claude-sonnet-4-5"),
        Encoding::AnthropicOffline
    );
    assert_eq!(
        encoding_for_model("claude-opus-4-5"),
        Encoding::AnthropicOffline
    );
    assert_eq!(
        encoding_for_model("claude-haiku-4-5"),
        Encoding::AnthropicOffline
    );
}

#[test]
fn ac7_encoding_for_model_is_public() {
    // Verify the function is accessible from outside the crate
    let enc = rskim_tokens::encoding_for_model("gpt-4");
    assert_eq!(enc, Encoding::Cl100k);
}

/// AC21 / #306 — nine-cell truth table for `encoding_for_provider_model`.
///
/// This test lives in `rskim-tokens` because a `-p rskim-tokens` run never
/// sees `rskim`'s tests; coverage must exist on both sides (PF-007).
///
/// Discriminating case: `(OpenAI, Some("gpt-4"))` → `Cl100k` (not `O200k`),
/// proving the function delegates to `encoding_for_model` for recognised models
/// rather than always returning the family default. A naive "return family
/// default always" regression would return `O200k` here instead of `Cl100k`.
#[test]
fn ac21_encoding_for_provider_model_nine_cells() {
    use rskim_tokens::Encoding::*;
    use rskim_tokens::Provider::*;

    let cases: &[(
        &str,
        rskim_tokens::Provider,
        Option<&str>,
        rskim_tokens::Encoding,
    )] = &[
        // Unknown provider → always Heuristic regardless of model (AD-AN-11 #5b)
        ("Unknown+None", Unknown, None, Heuristic),
        ("Unknown+recognized", Unknown, Some("gpt-4o"), Heuristic),
        (
            "Unknown+unrecognized",
            Unknown,
            Some("mystery-llm"),
            Heuristic,
        ),
        // Anthropic + None → AnthropicOffline family default
        ("Anthropic+None", Anthropic, None, AnthropicOffline),
        // Anthropic + recognized model → keeps its encoding
        (
            "Anthropic+recognized",
            Anthropic,
            Some("claude-3-opus-20240229"),
            AnthropicOffline,
        ),
        // Anthropic + unrecognized → falls back to AnthropicOffline family default
        (
            "Anthropic+unrecognized",
            Anthropic,
            Some("mystery-llm"),
            AnthropicOffline,
        ),
        // OpenAI + None → O200k family default
        ("OpenAI+None", OpenAI, None, O200k),
        // OpenAI + "gpt-4" → Cl100k (discriminating: different from O200k family default)
        ("OpenAI+gpt-4(Cl100k)", OpenAI, Some("gpt-4"), Cl100k),
        // OpenAI + unrecognized → O200k family default
        ("OpenAI+unrecognized", OpenAI, Some("mystery-llm"), O200k),
    ];

    for &(label, provider, model, expected) in cases {
        let got = rskim_tokens::encoding_for_provider_model(provider, model);
        assert_eq!(
            got, expected,
            "encoding_for_provider_model case '{label}': expected {expected:?} got {got:?}"
        );
    }
}

// ============================================================================
// AC7 sole-mapping gate — source-text detector
//
// # Design decision: detect the mapping *construct*, not the function shape
//
// The predecessor of this detector matched a signature shape (`fn(&str) ->
// Encoding`) and never read a body. That is the wrong question. The invariant
// `encoding.rs` declares (module docs, lines 3-5) is that no *parallel
// model-string → Encoding mapping* may exist outside it — a property of a
// construct, not of a signature. The mismatch made the old detector
// simultaneously:
//
//   * over-inclusive — `rskim::analytics::select_encoding` is a delegating
//     wrapper that calls `encoding_for_model` and only supplies provider-level
//     family defaults. It holds no model table, yet matched the shape and
//     blocked a merge.
//   * under-inclusive — `fn f(model: String) -> Encoding` (owned parameter),
//     a module-level `const TABLE: &[(&str, Encoding)]`, and a
//     `HashMap<&str, Encoding>` with no enclosing function all carry real
//     mappings and were all invisible.
//   * self-triggering — the *prose* describing the shape matched the shape,
//     which forced a blanket "skip any file named integration.rs" exclusion
//     that silently exempted four other crates from a workspace-wide gate.
//
// The replacement scans a *sanitized* copy of the source (comment bodies and
// string/char-literal contents blanked; see `blank_comments_and_literals`) for
// three constructs. Prose and fixture snippets are therefore invisible by
// construction, and function bodies can be brace-matched safely.
//
// Residual, deliberately accepted gaps (documented so the next reader does not
// have to rediscover them): macro-generated tables (`include!`, `macro_rules!`
// expansion), tables built by runtime `insert()` calls with non-literal keys,
// prefix classifiers keyed on raw-string literals (`r"gpt-"`), prefix
// classifiers living in a module-level closure with no enclosing `fn`, and
// `&[(Cow<'static, str>, Encoding)]` pair slices (the Cow key type does not
// match the (&str / String) opener patterns in signal (b)).
// ============================================================================

/// Which construct fired. Carried into the failure message so the engineer who
/// trips this gate is told *what* was detected, not merely that something was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MappingSignal {
    /// `"gpt-4" | "gpt-4-turbo" => Encoding::Cl100k` — a match arm whose
    /// pattern contains a string literal and whose body is an `Encoding`
    /// variant. This is the canonical exact-match table (`encoding.rs` tier 1).
    LiteralMatchArm,
    /// `&[(&str, Encoding)]`, `[(&str, Encoding); N]`, `HashMap<&str, Encoding>`,
    /// `phf::Map<&'static str, Encoding>`, `Vec<(String, Encoding)>` — a
    /// two-element, string-keyed table *type*. Catches tables that exist as
    /// data with no enclosing function of any particular shape.
    StrKeyedTableType,
    /// A `fn` whose body combines a string-literal text test
    /// (`.starts_with("…")` / `.ends_with("…")` / `.contains("…")`) with an
    /// `Encoding` variant in a yielding position, scoped to a single function
    /// body by brace-matching. This is the family-prefix fallback shape
    /// (`encoding.rs::family_prefix_fallback`), which has neither literal arms
    /// nor a table type and would otherwise be missed.
    PrefixFamilyFn,
}

impl MappingSignal {
    /// Human-readable description used in the assertion message.
    fn describe(self) -> &'static str {
        match self {
            Self::LiteralMatchArm => "match arm mapping a string literal to an Encoding variant",
            Self::StrKeyedTableType => "string-keyed 2-element table type with Encoding values",
            Self::PrefixFamilyFn => {
                "fn body combining a string-literal text test with a returned Encoding variant"
            }
        }
    }
}

/// One detected mapping construct. `offset` is a byte offset valid in the
/// ORIGINAL source (`blank_comments_and_literals` preserves byte length), so
/// the reporter can quote the real, unblanked line.
#[derive(Debug, Clone, Copy)]
struct MappingHit {
    signal: MappingSignal,
    offset: usize,
}

/// Literal text tests that indicate a string-keyed mapping in a function body.
///
/// Three method-call forms cover `.starts_with("…")`, `.ends_with("…")`, and
/// `.contains("…")`; only literal-argument forms are listed (a variable argument
/// is not a table). Two equality forms cover `m == "gpt-4"` and `"gpt-4" == m`
/// (gap 2 of the false-negative analysis): after blanking string literal
/// *contents*, the respective substrings `== "` and `" ==` still appear and can
/// be scanned without matching non-string code.
const LITERAL_TEXT_TESTS: [&str; 5] = [
    ".starts_with(\"",
    ".ends_with(\"",
    ".contains(\"",
    "== \"", // m == "gpt-4"
    "\" ==", // "gpt-4" == m (reversed, less common)
];

/// Is `b` a byte that can occur inside a Rust identifier? Non-ASCII bytes count
/// (Rust permits non-ASCII identifiers) so word-boundary checks never split a
/// multi-byte identifier.
fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Byte width of the UTF-8 char starting with `first`.
fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Push one blanked byte, preserving `\n` so line numbers survive.
fn push_blank(out: &mut Vec<u8>, byte: u8) {
    out.push(if byte == b'\n' { b'\n' } else { b' ' });
}

/// Blank comment bodies and string/char-literal *contents*, preserving the
/// source's exact byte length, its newlines, and its literal delimiters.
///
/// # Why this exists
///
/// Every signal below is a *code* pattern. Without this pass, three things go
/// wrong:
///
/// 1. Rustdoc that names a banned construct trips the gate on its own
///    documentation — which is exactly why the predecessor gate had to exempt
///    every file named `integration.rs` in the workspace.
/// 2. The meta-test's fixture snippets (Rust source stored as string literals)
///    trip the gate when this very file is walked from disk. Blanking literal
///    *contents* while keeping the delimiters resolves this asymmetrically and
///    correctly: on disk a fixture is a literal (invisible); passed as a value
///    to the detector it is code (visible).
/// 3. Brace matching desyncs on a `'}'` inside a literal, so `fn` bodies could
///    not be delimited — and body scoping is what stops a `.starts_with("…")`
///    in function A from combining with an `Encoding::` in unrelated function B.
///
/// # Preserved properties
///
/// * `out.len() == src.len()` — byte offsets index the original source.
/// * Newline count and positions are unchanged — line numbers are exact.
/// * A `"` in the output is always a literal delimiter, never literal content.
///   Signal (a) relies on this: "does this pattern contain a string literal?"
///   reduces to `lhs.contains('"')`.
///
/// Handles `//`, `///`, `//!`, nested `/* */`, `"…"`, `b"…"`, `r"…"`,
/// `r#"…"#`, `br##"…"##`, `'c'`, `'\n'`, and disambiguates a char literal from
/// a lifetime (`&'static str`, `'a`, `'_`, `'outer:`) by the standard rule:
/// `'` begins a char literal only when followed by `\` or by exactly one char
/// then `'`.
fn blank_comments_and_literals(src: &str) -> String {
    let b = src.as_bytes();
    let n = b.len();
    let mut out: Vec<u8> = Vec::with_capacity(n);
    let mut i = 0usize;

    while i < n {
        let c = b[i];

        // ---- line comment (covers `//`, `///`, `//!`) ----
        if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < n && b[i] != b'\n' {
                push_blank(&mut out, b[i]);
                i += 1;
            }
            continue;
        }

        // ---- block comment, nesting-aware (covers `/*`, `/**`, `/*!`) ----
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 1usize;
            push_blank(&mut out, b'/');
            push_blank(&mut out, b'*');
            i += 2;
            while i < n && depth > 0 {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    push_blank(&mut out, b' ');
                    push_blank(&mut out, b' ');
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    push_blank(&mut out, b' ');
                    push_blank(&mut out, b' ');
                    i += 2;
                } else {
                    push_blank(&mut out, b[i]);
                    i += 1;
                }
            }
            continue;
        }

        // ---- raw string: r"…" / r#"…"# / br"…" / br##"…"## ----
        if (c == b'r' || (c == b'b' && b.get(i + 1) == Some(&b'r')))
            && (i == 0 || !is_ident_byte(b[i - 1]))
        {
            let mut k = if c == b'b' { i + 2 } else { i + 1 };
            let hash_start = k;
            while k < n && b[k] == b'#' {
                k += 1;
            }
            if b.get(k) == Some(&b'"') {
                let hashes = k - hash_start;
                out.extend_from_slice(&b[i..=k]); // keep `r#"` verbatim
                i = k + 1;
                while i < n {
                    if b[i] == b'"'
                        && i + hashes < n
                        && b[i + 1..=i + hashes].iter().all(|&h| h == b'#')
                    {
                        out.extend_from_slice(&b[i..=i + hashes]);
                        i += hashes + 1;
                        break;
                    }
                    push_blank(&mut out, b[i]);
                    i += 1;
                }
                continue;
            }
        }

        // ---- char literal vs lifetime ----
        if c == b'\'' {
            let is_char_lit = match b.get(i + 1) {
                Some(&b'\\') => true,
                Some(&first) => b.get(i + 1 + utf8_width(first)) == Some(&b'\''),
                None => false,
            };
            if !is_char_lit {
                out.push(c); // lifetime tick — harmless, keep it
                i += 1;
                continue;
            }
            out.push(b'\'');
            i += 1;
            while i < n {
                if b[i] == b'\\' {
                    push_blank(&mut out, b[i]);
                    i += 1;
                    if i < n {
                        push_blank(&mut out, b[i]);
                        i += 1;
                    }
                    continue;
                }
                if b[i] == b'\'' {
                    out.push(b'\'');
                    i += 1;
                    break;
                }
                push_blank(&mut out, b[i]);
                i += 1;
            }
            continue;
        }

        // ---- normal / byte string literal ----
        if c == b'"' {
            out.push(b'"');
            i += 1;
            while i < n {
                if b[i] == b'\\' {
                    push_blank(&mut out, b[i]);
                    i += 1;
                    if i < n {
                        push_blank(&mut out, b[i]);
                        i += 1;
                    }
                    continue;
                }
                if b[i] == b'"' {
                    out.push(b'"');
                    i += 1;
                    break;
                }
                push_blank(&mut out, b[i]);
                i += 1;
            }
            continue;
        }

        out.push(c);
        i += 1;
    }

    // Every byte written is either copied verbatim from a UTF-8 boundary run or
    // an ASCII blank, so this is always valid UTF-8; `lossy` avoids an unwrap.
    String::from_utf8_lossy(&out).into_owned()
}

/// Strip whitespace and lifetime tokens from a short type fragment, so
/// `&[( &'static str ,` and `&[(&str,` compare equal.
fn normalize_type_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        if c == '\'' {
            // Drop a lifetime token (`'static`, `'a`, `'_`).
            while chars
                .peek()
                .is_some_and(|n| n.is_alphanumeric() || *n == '_')
            {
                chars.next();
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// Index of the `}` closing the `{` at `open`. Sound only on sanitized text
/// (literals cannot contribute stray braces) — that is the whole reason
/// `blank_comments_and_literals` runs first.
fn matching_brace(code: &str, open: usize) -> Option<usize> {
    let b = code.as_bytes();
    if b.get(open) != Some(&b'{') {
        return None;
    }
    let mut depth = 0usize;
    for (off, &byte) in b[open..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(open + off);
                }
            }
            _ => {}
        }
    }
    None
}

/// Index of the `{` opening the body of the `fn` item beginning at `start`.
/// `None` for a bodiless declaration (`fn f();` in a trait). Paren/bracket
/// depth tracking keeps `fn f(buf: [u8; 4]) { … }` from being mistaken for a
/// bodiless declaration because of the `;` inside the array type.
fn fn_body_open(code: &str, start: usize) -> Option<usize> {
    let (mut paren, mut bracket) = (0usize, 0usize);
    for (off, &byte) in code.as_bytes()[start..].iter().enumerate() {
        match byte {
            b'(' => paren += 1,
            b')' => paren = paren.saturating_sub(1),
            b'[' => bracket += 1,
            b']' => bracket = bracket.saturating_sub(1),
            b';' if paren == 0 && bracket == 0 => return None,
            b'{' if paren == 0 && bracket == 0 => return Some(start + off),
            _ => {}
        }
    }
    None
}

// ============================================================================
// Gap-closing context (gaps 1–4 of the false-negative analysis)
// ============================================================================

/// Exhaustive list of `Encoding` variant names used by the glob-import
/// normalizer (gap 1). When `use Encoding::*` is in scope, a bare
/// `Cl100k` is indistinguishable from `Encoding::Cl100k` at the use site.
///
/// The anti-rot test `ac7_encoding_variant_list_is_complete` fails if a new
/// variant is added to `Encoding` without updating this constant, so the list
/// cannot silently fall out of sync.
const ENCODING_VARIANTS: [&str; 4] = ["Cl100k", "O200k", "AnthropicOffline", "Heuristic"];

/// Scanning context derived from the sanitized source before the three
/// signals run. Captures the three kinds of non-qualified `Encoding`
/// reference so the signals can be extended without restructuring each.
struct ScanCtx<'a> {
    /// `true` when the source contains `Encoding::*` (glob import).
    /// Bare variant names — `Cl100k`, `O200k`, etc. — are then equivalent
    /// to `Encoding::Cl100k` etc. at every use site.
    has_glob: bool,
    /// Names that are aliases of `Encoding` via:
    /// - `use … Encoding as Name;`
    /// - `type Name = Encoding;`
    aliases: Vec<&'a str>,
    /// `(open_brace, close_brace)` byte ranges of `impl Encoding { … }` blocks.
    /// Inside these ranges `Self::` is equivalent to `Encoding::`.
    impl_enc_ranges: Vec<(usize, usize)>,
}

impl<'a> ScanCtx<'a> {
    fn from_sanitized(code: &'a str) -> Self {
        Self {
            has_glob: code.contains("Encoding::*"),
            aliases: collect_encoding_aliases(code),
            impl_enc_ranges: impl_encoding_body_ranges(code),
        }
    }

    /// Is `offset` inside an `impl Encoding { … }` block?
    fn in_impl_enc(&self, offset: usize) -> bool {
        self.impl_enc_ranges
            .iter()
            .any(|&(s, e)| offset >= s && offset <= e)
    }

    /// Does `qualifier` name the `Encoding` type for the purposes of signals (a)
    /// and (c)? Accepts `Encoding`, each collected alias, and `Self` when
    /// `offset` is inside an `impl Encoding` block (gap 4).
    fn is_encoding_qualifier(&self, qualifier: &str, offset: usize) -> bool {
        qualifier == "Encoding"
            || self.aliases.contains(&qualifier)
            || (qualifier == "Self" && self.in_impl_enc(offset))
    }
}

/// Collect names that are aliases of `Encoding` in the sanitized source.
///
/// Handles:
/// - `use … Encoding as Name;` (gap 3a)
/// - `type Name = Encoding;`   (gap 3b)
fn collect_encoding_aliases(code: &str) -> Vec<&str> {
    let mut aliases: Vec<&str> = Vec::new();
    let bytes = code.as_bytes();

    // Pattern 3a: `Encoding as Name` — in use statements or re-exports.
    for (idx, _) in code.match_indices("Encoding as ") {
        // Reject `MyEncoding as`
        if idx > 0 && is_ident_byte(bytes[idx - 1]) {
            continue;
        }
        let rest = &code[idx + "Encoding as ".len()..];
        let end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        if end > 0 {
            aliases.push(&rest[..end]);
        }
    }

    // Pattern 3b: `type Name = Encoding` — type alias declaration.
    // After sanitizing, string-literal contents are blanked but keyword
    // sequences like `type Enc = Encoding;` are unchanged.
    for (idx, _) in code.match_indices("type ") {
        // Reject `retype ` etc.
        if idx > 0 && is_ident_byte(bytes[idx - 1]) {
            continue;
        }
        let rest = &code[idx + "type ".len()..];
        let name_end = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(0);
        if name_end == 0 {
            continue;
        }
        // After the name, look for `= Encoding` (possibly with intervening
        // generic params like `<T>`; handled by scanning for `= Encoding`).
        let after_name = &rest[name_end..];
        // Find `= Encoding` in the next 128 bytes (ample for `<T, U> = Encoding`).
        let window = &after_name[..after_name.len().min(128)];
        let Some(eq_rel) = window.find("= Encoding") else {
            continue;
        };
        // `= Encoding` must not be followed by `::` (that would be `= Encoding::Variant`).
        let after_enc = eq_rel + "= Encoding".len();
        if let Some(&b) = window.as_bytes().get(after_enc)
            && (is_ident_byte(b) || b == b':')
        {
            continue; // `= EncodingFoo` or `= Encoding::Variant`
        }
        aliases.push(&rest[..name_end]);
    }

    aliases
}

/// Return the `(open_brace, close_brace)` byte ranges of every
/// `impl Encoding { … }` block (including `impl Trait for Encoding { … }`)
/// in the sanitized source.
///
/// Within these ranges `Self::` is equivalent to `Encoding::` (gap 4).
fn impl_encoding_body_ranges(code: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let bytes = code.as_bytes();

    for (idx, _) in code.match_indices("impl ") {
        // Reject `reimpl ` etc.
        if idx > 0 && is_ident_byte(bytes[idx - 1]) {
            continue;
        }
        // Search for `Encoding` within the next 128 bytes (covers `impl<T>`,
        // `impl Trait for Encoding`, whitespace, generic bounds).
        let search_end = (idx + 5 + 128).min(code.len());
        let search = &code[idx + 5..search_end];
        let Some(enc_rel) = search.find("Encoding") else {
            continue;
        };
        let abs_enc = idx + 5 + enc_rel;
        // Reject `MyEncoding`
        if abs_enc > 0 && is_ident_byte(bytes[abs_enc - 1]) {
            continue;
        }
        // Reject `EncodingFoo`
        let after_enc = abs_enc + "Encoding".len();
        if after_enc < bytes.len() && is_ident_byte(bytes[after_enc]) {
            continue;
        }
        // Find the opening brace of the impl block.
        let Some(open_rel) = code[after_enc..].find('{') else {
            continue;
        };
        let open = after_enc + open_rel;
        let Some(close) = matching_brace(code, open) else {
            continue;
        };
        ranges.push((open, close));
    }
    ranges
}

// ============================================================================
// Signal helpers
// ============================================================================

/// Does `rhs` begin with a path whose final-but-one segment is `Encoding` and
/// which names a variant? Accepts `Encoding::Cl100k`, `crate::Encoding::O200k`,
/// `rskim_tokens::Encoding::Heuristic`. Rejects `RecordingProvider::OpenAI`,
/// `MyEncoding::X`, `"exact"`, `{`, and a bare `Encoding::`.
///
/// Extended (gaps 1, 3, 4): also accepts alias-qualified paths (`Enc::Cl100k`),
/// bare variant names when a glob import is in scope (`Cl100k`), and
/// `Self::Variant` when `at` is inside an `impl Encoding` block.
fn rhs_is_encoding_variant(rhs: &str, at: usize, ctx: &ScanCtx) -> bool {
    let end = rhs
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':'))
        .unwrap_or(rhs.len());
    let token = &rhs[..end];

    // Case 1: qualified path — `Encoding::Variant`, `crate::Encoding::Variant`,
    // `Alias::Variant` (gap 3), or `Self::Variant` inside `impl Encoding` (gap 4).
    if let Some((head, variant)) = token.rsplit_once("::") {
        if !variant.is_empty() {
            let qualifier = head.rsplit("::").next().unwrap_or("");
            if ctx.is_encoding_qualifier(qualifier, at) {
                return true;
            }
        }
        return false;
    }

    // Case 2: bare variant name when a glob import is in scope (gap 1).
    if ctx.has_glob && ENCODING_VARIANTS.contains(&token) {
        return true;
    }

    false
}

/// Does this body *yield* an `Encoding` variant (return it, produce it from a
/// match arm, or leave it as a tail expression)?
///
/// The distinction matters: `let e = Encoding::Cl100k;` and
/// `Counter::new(Encoding::Cl100k)` merely *mention* a variant and are
/// pervasive in ordinary construction and test code. Requiring a yielding
/// position is what makes it safe to include `.contains("…")` in
/// [`LITERAL_TEXT_TESTS`] without false-positiving on
/// `assert!(s.contains("…"))` in a test that also builds a `Counter`.
///
/// Extended (gaps 1, 3, 4): also checks alias-qualified paths, `Self::` inside
/// `impl Encoding` blocks, and bare variant names under a glob import.
fn body_yields_encoding_variant(body: &str, ctx: &ScanCtx, self_enc: bool) -> bool {
    let bytes = body.as_bytes();

    /// Returns `true` when the variant occurrence at `path_start..path_start+path_len`
    /// (beginning of the full qualifier path, e.g. `crate::Encoding::`) followed by a
    /// variant name of `vlen` bytes is in a yielding position.
    fn is_yielding(
        body: &str,
        bytes: &[u8],
        path_start: usize,
        path_len: usize,
        vlen: usize,
    ) -> bool {
        let var_end = path_start + path_len + vlen;
        let after = &body[var_end..];
        // Tail-expression: `… Variant }`
        if after.trim_start().starts_with('}') {
            return true;
        }
        // Walk back over any leading path qualification (`crate::`, `rskim_tokens::`)
        // to find what precedes the whole path expression.
        let mut ps = path_start;
        while ps > 0 && (is_ident_byte(bytes[ps - 1]) || bytes[ps - 1] == b':') {
            ps -= 1;
        }
        let head = body[..ps].trim_end();
        // Match-arm RHS: `"…" => Variant`
        if head.ends_with("=>") {
            return true;
        }
        // Return statement: `return Variant`
        if let Some(pre) = head.strip_suffix("return")
            && pre.as_bytes().last().is_none_or(|&b| !is_ident_byte(b))
        {
            return true;
        }
        false
    }

    // --- Original: `Encoding::Variant` (and qualified forms) ---
    for (idx, _) in body.match_indices("Encoding::") {
        if idx > 0 && is_ident_byte(bytes[idx - 1]) {
            continue; // `MyEncoding::`
        }
        let rest = &body[idx + "Encoding::".len()..];
        let vlen = rest
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        if vlen > 0 && is_yielding(body, bytes, idx, "Encoding::".len(), vlen) {
            return true;
        }
    }

    // --- Gap 3: alias-qualified paths (`Enc::Variant`) ---
    for alias in &ctx.aliases {
        let prefix = format!("{}::", alias);
        for (idx, _) in body.match_indices(prefix.as_str()) {
            if idx > 0 && is_ident_byte(bytes[idx - 1]) {
                continue;
            }
            let rest = &body[idx + prefix.len()..];
            let vlen = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            if vlen > 0 && is_yielding(body, bytes, idx, prefix.len(), vlen) {
                return true;
            }
        }
    }

    // --- Gap 4: `Self::Variant` inside `impl Encoding { … }` ---
    if self_enc {
        for (idx, _) in body.match_indices("Self::") {
            let rest = &body[idx + "Self::".len()..];
            let vlen = rest
                .find(|c: char| !(c.is_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            if vlen > 0 && is_yielding(body, bytes, idx, "Self::".len(), vlen) {
                return true;
            }
        }
    }

    // --- Gap 1: bare variant names under a glob import ---
    if ctx.has_glob {
        for &variant in ENCODING_VARIANTS.iter() {
            for (idx, _) in body.match_indices(variant) {
                if idx > 0 && is_ident_byte(bytes[idx - 1]) {
                    continue;
                }
                let after_pos = idx + variant.len();
                if after_pos < bytes.len() && is_ident_byte(bytes[after_pos]) {
                    continue;
                }
                // For bare variants there is no qualifier prefix; walk back from idx.
                if is_yielding(body, bytes, idx, 0, variant.len()) {
                    return true;
                }
            }
        }
    }

    false
}

/// Signal (a): a match arm whose pattern contains a string literal and whose
/// body is an `Encoding` variant.
///
/// Direction is load-bearing. `Encoding::Cl100k | Encoding::O200k => "exact"`
/// (the label projection in `analytics`) is an Encoding→label direction, not a
/// mapping, and fails the right-hand test. `Encoding::Heuristic =>
/// Encoding::AnthropicOffline` (`analytics::select_encoding`, the delegating
/// wrapper that caused the false positive this detector replaces) passes the
/// right-hand test but fails the left-hand one: its pattern is a variant, not a
/// string literal.
///
/// Extended (gaps 1, 3, 4): `rhs_is_encoding_variant` now also accepts
/// alias-qualified paths, bare variant names, and `Self::` in `impl Encoding`.
fn scan_literal_match_arms(code: &str, hits: &mut Vec<MappingHit>, ctx: &ScanCtx) {
    for (idx, _) in code.match_indices("=>") {
        if !rhs_is_encoding_variant(code[idx + 2..].trim_start(), idx, ctx) {
            continue;
        }
        // Pattern text: back to the previous arm boundary.
        let lhs_start = code[..idx].rfind([',', '{', '}', ';']).map_or(0, |p| p + 1);
        // After sanitizing, every `"` in `code` is a literal delimiter.
        if code[lhs_start..idx].contains('"') {
            hits.push(MappingHit {
                signal: MappingSignal::LiteralMatchArm,
                offset: idx,
            });
        }
    }
}

/// Signal (b): a string-keyed two-element table *type* with `Encoding` values —
/// `&[(&str, Encoding)]`, `[(&str, Encoding); N]`, `HashMap<&str, Encoding>`,
/// `phf::Map<&'static str, Encoding>`, `Vec<(String, Encoding)>`.
///
/// The shape is matched *exactly*, which is what excludes the AC21 fixture
/// table `&[(&str, RecordingProvider, Option<&str>, Encoding)]`: the text
/// preceding its `Encoding` normalizes to `…Option<&str>`, which ends in `>`
/// rather than opening the tuple with `(&str`. A looser "an array mentioning
/// both `&str` and `Encoding`" test would flag it.
///
/// `)` and `>` are the only accepted closers. `}` is deliberately excluded so
/// `use rskim_tokens::{Counter, Encoding};` (a use-tree, not a type) is not a hit.
///
/// Extended (gap 3): also scans for alias names in the value position of a
/// two-element table type (e.g. `HashMap<&str, Enc>` where `Enc` aliases
/// `Encoding`).
fn scan_str_keyed_table_types(code: &str, hits: &mut Vec<MappingHit>, ctx: &ScanCtx) {
    const KEY_OPENERS: [&str; 4] = ["(&str", "<&str", "(String", "<String"];
    let bytes = code.as_bytes();

    // Build list of type names to scan: the real name plus any aliases (gap 3).
    let mut terms: Vec<&str> = vec!["Encoding"];
    terms.extend(ctx.aliases.iter().copied());

    for term in &terms {
        for (idx, _) in code.match_indices(*term) {
            if idx > 0 && is_ident_byte(bytes[idx - 1]) {
                continue; // `MyEncoding`
            }
            let after = code[idx + term.len()..].trim_start();
            if !(after.starts_with(')') || after.starts_with('>')) {
                continue; // not in value position of a 2-element table type
            }
            let Some(before) = code[..idx].trim_end().strip_suffix(',') else {
                continue; // not the second element of anything
            };
            let before = before.trim_end();
            // Last 48 chars are ample for `&[(&'static str` / `HashMap<&str`.
            let window = before
                .char_indices()
                .rev()
                .nth(47)
                .map_or(before, |(p, _)| &before[p..]);
            let norm = normalize_type_text(window);
            if KEY_OPENERS.iter().any(|k| norm.ends_with(k)) {
                hits.push(MappingHit {
                    signal: MappingSignal::StrKeyedTableType,
                    offset: idx,
                });
            }
        }
    }
}

/// Signal (c): a `fn` whose body combines a string-literal text test with an
/// `Encoding` variant in a yielding position — the family-prefix fallback
/// shape (`encoding.rs::family_prefix_fallback`), which has neither literal
/// arms nor a table type.
///
/// Scoping to a single `fn` body — enabled by brace matching over sanitized
/// text — is what prevents a `.starts_with("…")` in one function from combining
/// with an `Encoding::` in an unrelated sibling function.
///
/// Extended (gaps 1–4): the body check now also recognises equality comparisons
/// (`== "…"`), alias-qualified variants, `Self::` inside `impl Encoding`, and
/// bare variant names under a glob import (all delegated to
/// `body_yields_encoding_variant`).
fn scan_prefix_family_fns(code: &str, hits: &mut Vec<MappingHit>, ctx: &ScanCtx) {
    let bytes = code.as_bytes();
    for (start, _) in code.match_indices("fn ") {
        if start > 0 && is_ident_byte(bytes[start - 1]) {
            continue;
        }
        let Some(open) = fn_body_open(code, start) else {
            continue;
        };
        let Some(close) = matching_brace(code, open) else {
            continue;
        };
        let body = &code[open..=close];
        // `self_enc`: true when this `fn` lives inside an `impl Encoding` block.
        let self_enc = ctx.in_impl_enc(start);
        if LITERAL_TEXT_TESTS.iter().any(|n| body.contains(n))
            && body_yields_encoding_variant(body, ctx, self_enc)
        {
            hits.push(MappingHit {
                signal: MappingSignal::PrefixFamilyFn,
                offset: start,
            });
        }
    }
}

/// Find every parallel model-string → `Encoding` mapping construct in `src`.
///
/// Returns hits in source order, each naming the construct that fired and
/// carrying a byte offset into the ORIGINAL `src`.
fn find_parallel_encoding_mappings(src: &str) -> Vec<MappingHit> {
    let mut hits = Vec::new();
    // Cheap superset pre-filter. Every signal requires the identifier
    // `Encoding` in the sanitized text, and sanitizing only ever *blanks*
    // bytes — it never introduces new ones. So "sanitized contains Encoding"
    // implies "raw contains Encoding", and skipping here cannot miss a hit.
    // This is what keeps the gate cheap: of ~504 `.rs` files / ~20 MB under
    // `crates/`, only 14 files (~250 KB) are sanitized.
    if !src.contains("Encoding") {
        return hits;
    }
    let code = blank_comments_and_literals(src);
    let ctx = ScanCtx::from_sanitized(&code);
    scan_literal_match_arms(&code, &mut hits, &ctx);
    scan_str_keyed_table_types(&code, &mut hits, &ctx);
    scan_prefix_family_fns(&code, &mut hits, &ctx);
    hits.sort_by_key(|h| h.offset);
    hits
}

/// Boolean convenience wrapper used by the meta-tests.
fn has_parallel_encoding_mapping(src: &str) -> bool {
    !find_parallel_encoding_mappings(src).is_empty()
}

/// 1-based line number and trimmed text of the line containing `offset`,
/// read from the ORIGINAL (unblanked) source so the message quotes real code.
fn line_and_snippet(src: &str, offset: usize) -> (usize, String) {
    let Some(head) = src.get(..offset) else {
        return (0, String::new());
    };
    let line_no = head.bytes().filter(|&b| b == b'\n').count() + 1;
    let start = head.rfind('\n').map_or(0, |p| p + 1);
    let end = src[start..].find('\n').map_or(src.len(), |p| start + p);
    let line = src[start..end].trim();
    let snippet = if line.chars().count() > 100 {
        line.chars().take(97).collect::<String>() + "..."
    } else {
        line.to_string()
    };
    (line_no, snippet)
}

/// AC7 (sole-mapping gate): assert that `encoding.rs` is the ONLY workspace
/// source file containing a model-string → `Encoding` mapping construct. This
/// is the repository assertion AC7 mandates ("a test MUST assert this table is
/// the sole mapping in the workspace") — it fails the moment a ticket
/// reintroduces a parallel table instead of calling `encoding_for_model`.
///
/// # Exclusions
///
/// Exactly one: `crates/rskim-tokens/src/encoding.rs`, by **full path**.
///
/// The predecessor also excluded *any file named* `integration.rs`, which
/// blanket-exempted all five `integration.rs` files in the workspace
/// (rskim-bench, rskim-compress, rskim-core, rskim-llm, rskim-tokens) from a
/// workspace-wide invariant. That exclusion existed only because the gate's own
/// explanatory prose matched its own detector. Blanking comments and literal
/// contents (`blank_comments_and_literals`) removes the cause, so the exclusion
/// is gone — and its removal is pinned by `ac7_gate_needs_no_self_exclusion`,
/// so a future edit that reintroduces the problem fails a test named for the
/// reason rather than this gate.
#[test]
fn ac7_model_encoding_mapping_is_sole_in_workspace() {
    use std::path::{Path, PathBuf};

    /// Cap on reported hits so a systemic regression produces a readable
    /// message rather than a wall of text.
    const MAX_REPORTED: usize = 12;

    // Resolve the workspace `crates/` root relative to this crate's manifest.
    let crates_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rskim-tokens has a parent (crates/)")
        .to_path_buf();
    assert!(
        crates_root.join("rskim-tokens").is_dir(),
        "expected crates/ root at {crates_root:?}"
    );

    // The single authorised location for the model→encoding table.
    let authorised = crates_root
        .join("rskim-tokens")
        .join("src")
        .join("encoding.rs");
    assert!(
        authorised.is_file(),
        "the authorised mapping must exist at {authorised:?} — if this file \
         moved, update the exclusion rather than deleting it"
    );

    let mut offenders: Vec<(PathBuf, Vec<MappingHit>, String)> = Vec::new();
    let mut stack = vec![crates_root.clone()];
    // Bounded directory walk (finite tree; symlinks are not followed).
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_symlink() {
                continue; // no symlink following — the walk stays a finite tree
            }
            if file_type.is_dir() {
                // Skip build artefacts (including crates/rskim-llm/fuzz/target).
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") || path == authorised {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap_or_default();
            let hits = find_parallel_encoding_mappings(&src);
            if !hits.is_empty() {
                offenders.push((path, hits, src));
            }
        }
    }

    if offenders.is_empty() {
        return;
    }

    // Fail loud, and name *which construct* fired (CLAUDE.md: "fail loud with
    // actionable messages, never silently"). The predecessor named only the
    // file, which is what made its one false positive expensive to diagnose.
    offenders.sort_by(|a, b| a.0.cmp(&b.0));
    let total: usize = offenders.iter().map(|(_, h, _)| h.len()).sum();
    let mut report = String::new();
    let mut shown = 0usize;
    for (path, hits, src) in &offenders {
        for hit in hits {
            if shown == MAX_REPORTED {
                break;
            }
            let (line, snippet) = line_and_snippet(src, hit.offset);
            let rel = path.strip_prefix(&crates_root).unwrap_or(path);
            report.push_str(&format!(
                "\n  crates/{}:{line}\n      signal: {}\n      {snippet}\n",
                rel.display(),
                hit.signal.describe()
            ));
            shown += 1;
        }
    }
    if total > shown {
        report.push_str(&format!("\n  ... and {} more\n", total - shown));
    }

    panic!(
        "AC7 violation: {total} parallel model→encoding mapping construct(s) found \
         outside the single source of truth (crates/rskim-tokens/src/encoding.rs, \
         module docs lines 3-5).\n{report}\n\
         Fix: call `rskim_tokens::encoding_for_model(model_id)` instead of \
         re-deriving the mapping.\n\
         Note: a thin *delegating wrapper* that calls `encoding_for_model` and only \
         adds provider-level defaults is NOT a violation and is NOT flagged — this \
         detector reads bodies and table types, not signatures. If you believe this \
         hit is spurious, the fix is to sharpen the named signal in \
         crates/rskim-tokens/tests/integration.rs, not to add a path exclusion."
    );
}

// ---------------------------------------------------------------------------
// Fixtures for the meta-test below. Raw strings so the embedded Rust reads
// naturally.
//
// NOTE on non-vacuity of the *negative* fixtures: `find_parallel_encoding_mappings`
// short-circuits on sources that never mention `Encoding`. A negative fixture
// without the identifier would pass on the pre-filter alone and prove nothing
// about the signals, so every negative below mentions `Encoding` in code.
// ---------------------------------------------------------------------------

/// The `analytics::select_encoding` shape verbatim: a delegating wrapper whose
/// only match arms are Encoding→Encoding. The predecessor detector flagged this
/// and blocked a merge. It MUST NOT be flagged.
const FIXTURE_DELEGATING_WRAPPER: &str = r#"
use rskim_tokens::{Encoding, encoding_for_model};
pub(crate) fn select_encoding(provider: &RecordingProvider, model: Option<&str>) -> Encoding {
    match provider {
        RecordingProvider::Unknown => Encoding::Heuristic,
        RecordingProvider::Anthropic => match model {
            None => Encoding::AnthropicOffline,
            Some(m) => match encoding_for_model(m) {
                Encoding::Heuristic => Encoding::AnthropicOffline,
                e => e,
            },
        },
        RecordingProvider::OpenAI => match model {
            None => Encoding::O200k,
            Some(m) => match encoding_for_model(m) {
                Encoding::Heuristic => Encoding::O200k,
                e => e,
            },
        },
    }
}
"#;

/// The AC21 table-driven test fixture from `analytics/mod.rs`: a 4-tuple
/// table whose elements happen to include both `&str` and `Encoding`. A loose
/// "array mentioning &str and Encoding" check would flag it. It MUST NOT be.
const FIXTURE_AC21_FOUR_TUPLE_TABLE: &str = r#"
fn test_select_encoding_table_driven() {
    let cases: &[(&str, RecordingProvider, Option<&str>, Encoding)] = &[
        ("Unknown+None", RecordingProvider::Unknown, None, Encoding::Heuristic),
        ("OpenAI+gpt-4(Cl100k)", RecordingProvider::OpenAI, Some("gpt-4"), Encoding::Cl100k),
    ];
    for &(label, ref provider, model, expected) in cases {
        assert_eq!(select_encoding(provider, model), expected, "case {label}");
    }
}
"#;

/// Guard against a vacuous AC7 gate (PF-007): the detector must FIRE on every
/// realistic parallel-mapping construct and stay SILENT on everything else.
///
/// DELIBERATE BEHAVIOUR CHANGE vs. predecessor: the four original positive
/// fixtures were bare signatures with `{ todo!() }` bodies. Under body-based
/// detection an empty body is not a mapping and no longer fires — correctly,
/// because a bare `-> Encoding` signature is precisely the delegating-wrapper
/// shape that caused the false positive. Each fixture now carries a realistic
/// table body while preserving the return-path coverage the originals were
/// written for.
#[test]
fn ac7_detector_catches_parallel_mappings_and_ignores_getters() {
    // === POSITIVES — these MUST be flagged =================================

    // (1) bare `Encoding::` variants
    assert!(has_parallel_encoding_mapping(
        r#"pub fn t(model: &str) -> Encoding {
               match model { "gpt-4" => Encoding::Cl100k, _ => Encoding::Heuristic }
           }"#
    ));
    // (2) `crate::`-qualified variants
    assert!(has_parallel_encoding_mapping(
        r#"fn t(m: &str) -> crate::Encoding {
               match m { "gpt-4o" => crate::Encoding::O200k, _ => crate::Encoding::Heuristic }
           }"#
    ));
    // (3) `rskim_tokens::`-qualified variants
    assert!(has_parallel_encoding_mapping(
        r#"fn t(m: &str) -> rskim_tokens::Encoding {
               match m {
                   "claude-opus-4-5" => rskim_tokens::Encoding::AnthropicOffline,
                   _ => rskim_tokens::Encoding::Heuristic,
               }
           }"#
    ));
    // (4) multi-line signature + `Result<Encoding, Error>` wrapper
    assert!(has_parallel_encoding_mapping(
        r#"pub fn t(m: &str)
               -> Result<Encoding, Error> {
               Ok(match m { "gpt-4" => Encoding::Cl100k, _ => Encoding::Heuristic })
           }"#
    ));

    // (5) OWNED parameter — invisible to the predecessor (closes under-inclusiveness).
    assert!(
        has_parallel_encoding_mapping(
            r#"fn t(model: String) -> Encoding {
                   match model.as_str() { "gpt-4" => Encoding::Cl100k, _ => Encoding::Heuristic }
               }"#
        ),
        "owned `model: String` parameter must not hide a table"
    );

    // (6) module-level const table, NO enclosing fn at all.
    assert!(
        has_parallel_encoding_mapping(
            r#"const TABLE: &[(&str, Encoding)] = &[("gpt-4", Encoding::Cl100k)];"#
        ),
        "module-level &[(&str, Encoding)] table must be flagged"
    );

    // (7) HashMap keyed by &'static str.
    assert!(
        has_parallel_encoding_mapping(
            r#"fn table() -> HashMap<&'static str, Encoding> { HashMap::new() }"#
        ),
        "HashMap<&str, Encoding> must be flagged (lifetime must not hide it)"
    );

    // (8) fixed-size array table.
    assert!(
        has_parallel_encoding_mapping(
            r#"static T: [(&str, Encoding); 2] =
                   [("gpt-4", Encoding::Cl100k), ("gpt-4o", Encoding::O200k)];"#
        ),
        "[(&str, Encoding); N] table must be flagged"
    );

    // (9) prefix-family classifier — no arms, no table type.
    assert!(
        has_parallel_encoding_mapping(
            r#"fn fam(m: &str) -> Encoding {
                   if m.starts_with("gpt-") { return Encoding::O200k; }
                   if m.starts_with("claude-") { return Encoding::AnthropicOffline; }
                   Encoding::Heuristic
               }"#
        ),
        "family-prefix fallback must be flagged (this is encoding.rs tier 2)"
    );

    // (10) multi-line `|`-separated pattern list (the real encoding.rs shape).
    assert!(
        has_parallel_encoding_mapping(
            r#"fn t(m: &str) -> Encoding {
                   match m {
                       "gpt-3.5-turbo"
                       | "gpt-4"
                       | "gpt-4-turbo" => Encoding::Cl100k,
                       _ => Encoding::Heuristic,
                   }
               }"#
        ),
        "multi-line |-separated pattern list must be flagged"
    );

    // (11) `Option`-wrapped literal pattern.
    assert!(
        has_parallel_encoding_mapping(
            r#"fn t(m: Option<&str>) -> Encoding {
                   match m { Some("gpt-4") => Encoding::Cl100k, _ => Encoding::Heuristic }
               }"#
        ),
        "Some(\"literal\") pattern must be flagged"
    );

    // === POSITIVES: gap-closing fixtures (gaps 1–4 of the false-negative analysis) ===

    // (12) Gap 1 — glob-import bare variants in a literal match arm.
    // `use Encoding::*;` brings bare `Cl100k`, `O200k` etc. into scope; a
    // future table written as `match m { "gpt-4" => Cl100k, … }` was invisible.
    assert!(
        has_parallel_encoding_mapping(
            r#"use rskim_tokens::Encoding;
               use Encoding::*;
               pub fn enc(m: &str) -> Encoding {
                   match m { "gpt-4" => Cl100k, _ => Heuristic }
               }"#
        ),
        "glob-import bare variant in match arm must be flagged (gap 1)"
    );

    // (13) Gap 1b — glob-import bare variants in a prefix-family classifier.
    assert!(
        has_parallel_encoding_mapping(
            r#"use Encoding::*;
               pub fn fam(m: &str) -> Encoding {
                   if m.starts_with("gpt-") { return O200k; }
                   Heuristic
               }"#
        ),
        "glob-import bare variant in prefix classifier must be flagged (gap 1)"
    );

    // (14) Gap 2 — equality-ladder table (`m == "literal"` instead of starts_with).
    assert!(
        has_parallel_encoding_mapping(
            r#"use rskim_tokens::Encoding;
               pub fn enc(m: &str) -> Encoding {
                   if m == "gpt-4" { return Encoding::Cl100k; }
                   if m == "gpt-4o" { return Encoding::O200k; }
                   Encoding::Heuristic
               }"#
        ),
        "equality-ladder `m == \"…\"` classifier must be flagged (gap 2)"
    );

    // (15) Gap 3a — `use Encoding as Enc;` alias in match arms.
    assert!(
        has_parallel_encoding_mapping(
            r#"use rskim_tokens::Encoding as Enc;
               pub fn enc(m: &str) -> Enc {
                   match m { "gpt-4" => Enc::Cl100k, _ => Enc::Heuristic }
               }"#
        ),
        "`use Encoding as Enc` alias must be flagged (gap 3a)"
    );

    // (16) Gap 3b — `type Enc = Encoding;` type alias in match arms.
    assert!(
        has_parallel_encoding_mapping(
            r#"use rskim_tokens::Encoding;
               type Enc = Encoding;
               pub fn enc(m: &str) -> Enc {
                   match m { "gpt-4" => Enc::Cl100k, _ => Enc::Heuristic }
               }"#
        ),
        "`type Enc = Encoding` alias must be flagged (gap 3b)"
    );

    // (17) Gap 4 — `Self::Variant` inside an `impl Encoding` block.
    // The orphan rule confines `impl Encoding { … }` to `rskim-tokens` itself,
    // but any file within that crate is a valid host (e.g. `counter.rs`).
    assert!(
        has_parallel_encoding_mapping(
            r#"use rskim_tokens::Encoding;
               impl Encoding {
                   pub fn for_model(m: &str) -> Self {
                       match m { "gpt-4" => Self::Cl100k, _ => Self::Heuristic }
                   }
               }"#
        ),
        "`Self::Variant` inside `impl Encoding` must be flagged (gap 4)"
    );

    // === NEGATIVES — these MUST NOT be flagged =============================

    // Hazard 1: the delegating wrapper that caused the predecessor's false positive.
    assert!(
        !has_parallel_encoding_mapping(FIXTURE_DELEGATING_WRAPPER),
        "a delegating wrapper that calls encoding_for_model and only adds \
         provider-level defaults is NOT a parallel table — this is the exact \
         false positive that blocked a merge under the signature-based detector"
    );

    // Hazard 2: the AC21 4-tuple test table.
    assert!(
        !has_parallel_encoding_mapping(FIXTURE_AC21_FOUR_TUPLE_TABLE),
        "a 4-tuple test-case table is not a (&str, Encoding) mapping"
    );

    // Hazard 3: prose. Doc, line, and block comments describing banned shapes.
    assert!(
        !has_parallel_encoding_mapping(
            "/// Maps \"gpt-4\" => Encoding::Cl100k via a HashMap<&str, Encoding>.\n\
             //! Module doc: `const T: &[(&str, Encoding)]` is banned.\n\
             // static TABLE: &[(&str, Encoding)] = &[(\"gpt-4\", Encoding::Cl100k)];\n\
             /* Encoding::Cl100k, /* nested */ &[(&str, Encoding)] */\n\
             fn nothing() {}"
        ),
        "comment text must never trip the gate — this is why the blanket \
         integration.rs exclusion could be removed"
    );

    // Hazard 4: fixture source stored as a string literal (this file's own shape).
    assert!(
        !has_parallel_encoding_mapping(
            "const FIXTURE: &str = \"match m { \\\"gpt-4\\\" => Encoding::Cl100k }\";\n\
             const RAW: &str = r#\"static T: &[(&str, Encoding)] = &[];\"#;"
        ),
        "Rust source held in a string literal is data, not code — this is what \
         lets the meta-test fixtures coexist with the workspace walk"
    );

    // Predecessor negatives, retained.
    assert!(
        !has_parallel_encoding_mapping("pub fn encoding(&self) -> Encoding { self.enc }"),
        "&self getter must not be flagged"
    );
    assert!(
        !has_parallel_encoding_mapping(
            "use rskim_tokens::Encoding;\nfn name(&self) -> &str { self.name }"
        ),
        "&str return (not Encoding) must not be flagged"
    );
    assert!(
        !has_parallel_encoding_mapping(
            "use rskim_tokens::Encoding;\nfn count(text: &str) -> usize { text.len() }"
        ),
        "unrelated &str fn must not be flagged"
    );

    // Reverse direction: Encoding → label (analytics::counting_basis_label).
    assert!(
        !has_parallel_encoding_mapping(
            r#"fn basis(e: Encoding) -> &'static str {
                   match e {
                       Encoding::Cl100k | Encoding::O200k => "exact",
                       Encoding::AnthropicOffline => "approximation",
                       Encoding::Heuristic => "heuristic",
                   }
               }"#
        ),
        "Encoding -> label projection is not a model -> Encoding mapping"
    );

    // String literals mapping to a NON-Encoding enum.
    assert!(
        !has_parallel_encoding_mapping(
            r#"use rskim_tokens::Encoding;
               fn provider(s: Option<&str>) -> RecordingProvider {
                   match s {
                       Some("anthropic") => RecordingProvider::Anthropic,
                       Some("openai") => RecordingProvider::OpenAI,
                       _ => RecordingProvider::Unknown,
                   }
               }"#
        ),
        "string arms producing a non-Encoding type are not a mapping"
    );

    // Ordinary construction + assertion — the most common shape in this crate.
    assert!(
        !has_parallel_encoding_mapping(
            r#"#[test]
               fn t() {
                   let e = Encoding::Cl100k;
                   let c = Counter::new(Encoding::Cl100k).unwrap();
                   assert_eq!(encoding_for_model("gpt-4"), Encoding::Cl100k);
                   assert!(c.label().contains("exact"));
                   let _ = e;
               }"#
        ),
        "constructing a Counter and asserting on a model id is not a mapping; \
         `.contains(\"…\")` must not fire without a yielded Encoding variant"
    );

    // Sanitizer invariant: byte length is preserved, so hit offsets index the
    // original source and reported line numbers are exact.
    let tricky = "let a: &'static str = \"}{\"; /* } */ let c = '}'; let r = r#\"}\"#;\n";
    assert_eq!(
        blank_comments_and_literals(tricky).len(),
        tricky.len(),
        "sanitizer must preserve byte length (offsets index the original source)"
    );
    assert_eq!(
        blank_comments_and_literals(tricky).matches('\n').count(),
        tricky.matches('\n').count(),
        "sanitizer must preserve newlines (line numbers must stay exact)"
    );
}

/// PF-007 anti-vacuity: the detector MUST fire on the real, authorised table.
///
/// This is the strongest guard in the file. A hardened detector that quietly
/// stopped matching `encoding.rs` would make
/// `ac7_model_encoding_mapping_is_sole_in_workspace` pass unconditionally —
/// worse than the detector it replaced. This test proves the gate is saved by
/// the *exclusion*, not by a detector miss, and pins BOTH tiers of the real
/// table independently.
#[test]
fn ac7_detector_fires_on_the_authorised_table() {
    use std::path::Path;

    let authorised = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("encoding.rs");
    let src = std::fs::read_to_string(&authorised)
        .unwrap_or_else(|e| panic!("must read {authorised:?}: {e}"));

    let hits = find_parallel_encoding_mappings(&src);
    assert!(
        !hits.is_empty(),
        "PF-007: the detector must fire on the authorised table itself — a \
         detector that misses encoding.rs makes the AC7 gate vacuous"
    );
    assert!(
        hits.iter()
            .any(|h| h.signal == MappingSignal::LiteralMatchArm),
        "tier 1 (exact-match literal arms, encoding.rs:71-108) must be detected; \
         got {:?}",
        hits.iter().map(|h| h.signal).collect::<Vec<_>>()
    );
    assert!(
        hits.iter()
            .any(|h| h.signal == MappingSignal::PrefixFamilyFn),
        "tier 2 (family_prefix_fallback, encoding.rs:118-139) must be detected; \
         got {:?}",
        hits.iter().map(|h| h.signal).collect::<Vec<_>>()
    );
}

/// Pins the removal of the predecessor's blanket `integration.rs` exclusion.
///
/// That exclusion exempted all five `integration.rs` files in the workspace
/// from a workspace-wide invariant, purely because this file's own prose
/// matched its own detector. Comment/literal blanking removed the cause. If a
/// future edit reintroduces a self-trip (e.g. by writing a fixture as real code
/// instead of a string literal), this test fails with an explanation rather
/// than the AC7 gate failing mysteriously.
#[test]
fn ac7_gate_needs_no_self_exclusion() {
    use std::path::Path;

    let this_file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("integration.rs");
    let src = std::fs::read_to_string(&this_file)
        .unwrap_or_else(|e| panic!("must read {this_file:?}: {e}"));

    let hits = find_parallel_encoding_mappings(&src);
    assert!(
        hits.is_empty(),
        "this test file must not trip its own detector (that is why the blanket \
         `integration.rs` exclusion could be deleted). Keep detector fixtures as \
         string literals, and needle constants as string literals. Hits: {:?}",
        hits.iter()
            .map(|h| (h.signal, line_and_snippet(&src, h.offset)))
            .collect::<Vec<_>>()
    );
}

/// Anti-rot guard for `ENCODING_VARIANTS`: verifies the constant lists EXACTLY
/// the variants declared in `Encoding`, no more and no less.
///
/// Without this test, a new variant added to `Encoding` (e.g. `O1`) would be
/// silently invisible to the glob-import normalizer (gap 1), allowing a file
/// with `use Encoding::*;` to map `"o1-mini" => O1` without triggering the
/// AC7 gate. This test MUST fail before that regression ships.
#[test]
fn ac7_encoding_variant_list_is_complete() {
    use std::path::Path;

    let enc_path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("encoding.rs");
    let src =
        std::fs::read_to_string(&enc_path).unwrap_or_else(|e| panic!("must read encoding.rs: {e}"));

    // Locate the `pub enum Encoding {` block and extract variant names.
    // This is a simple line-scanner; it handles attributes (`#[…]`) and
    // doc comments (`///`) that precede individual variants.
    let mut in_enum = false;
    let mut found: Vec<String> = Vec::new();
    for line in src.lines() {
        let t = line.trim();
        if !in_enum {
            // Match the enum declaration line
            if t.starts_with("pub enum Encoding") && (t.ends_with('{') || t.contains('{')) {
                in_enum = true;
            }
            continue;
        }
        // The closing brace ends the enum
        if t == "}" {
            break;
        }
        // Skip attributes and comments
        if t.starts_with('#') || t.starts_with("//") || t.is_empty() {
            continue;
        }
        // The variant name is the trimmed line with a trailing comma stripped.
        let name = t.trim_end_matches(',').trim();
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            found.push(name.to_owned());
        }
    }

    assert!(
        !found.is_empty(),
        "failed to extract any variant names from Encoding — check the parser \
         if the enum layout changed"
    );

    // Every found variant must be in ENCODING_VARIANTS.
    for v in &found {
        assert!(
            ENCODING_VARIANTS.contains(&v.as_str()),
            "Encoding variant `{v}` is not listed in ENCODING_VARIANTS — add it \
             and update the glob-import normalizer so the AC7 gate catches bare \
             uses of the new variant"
        );
    }

    // Every ENCODING_VARIANTS entry must be an actual variant.
    for &listed in ENCODING_VARIANTS.iter() {
        assert!(
            found.iter().any(|f| f == listed),
            "ENCODING_VARIANTS lists `{listed}` but it is not a variant of \
             Encoding — remove the stale entry"
        );
    }
}

// ============================================================================
// AC8: Unknown IDs resolve via two-tier rule without panic
// ============================================================================

#[test]
fn ac8_unknown_ids_resolve_via_two_tier_rule() {
    // Unknown OpenAI family → O200k
    assert_eq!(
        encoding_for_model("gpt-5-ultra"),
        Encoding::O200k,
        "gpt-5-ultra"
    );
    assert_eq!(
        encoding_for_model("o4-preview"),
        Encoding::O200k,
        "o4-preview"
    );
    assert_eq!(
        encoding_for_model("chatgpt-foo"),
        Encoding::O200k,
        "chatgpt-foo"
    );
    assert_eq!(encoding_for_model("gpt-4.2"), Encoding::O200k, "gpt-4.2");

    // Unknown claude family → AnthropicOffline
    assert_eq!(
        encoding_for_model("claude-zeta-9"),
        Encoding::AnthropicOffline,
        "claude-zeta-9"
    );
    assert_eq!(
        encoding_for_model("claude-opus-5"),
        Encoding::AnthropicOffline,
        "claude-opus-5"
    );

    // Unknown provider → Heuristic
    assert_eq!(
        encoding_for_model("llama-3"),
        Encoding::Heuristic,
        "llama-3"
    );
    assert_eq!(encoding_for_model(""), Encoding::Heuristic, "empty string");
    assert_eq!(
        encoding_for_model("gemini-pro"),
        Encoding::Heuristic,
        "gemini-pro"
    );
}

#[test]
fn ac8_unknown_ids_produce_counts() {
    // Every unknown ID must resolve to an encoding and produce a count
    for model in &[
        "gpt-5-ultra",
        "o4-preview",
        "chatgpt-foo",
        "claude-zeta-9",
        "llama-3",
        "",
    ] {
        let encoding = encoding_for_model(model);
        let counter = Counter::new(encoding).expect("counter");
        let count = counter.count("test input");
        assert!(
            count > 0 || model.is_empty(),
            "model {model:?} must produce a count: got {count}"
        );
    }
}

// ============================================================================
// AC11: Counter is Send + Sync (compile-time + runtime concurrency test)
// ============================================================================

// Compile-time static assertions (AC11)
// These are in the library itself, but we confirm here via the trait bounds
static_assertions::assert_impl_all!(Counter: Send, Sync);

#[test]
fn ac11_concurrent_counting_correct() {
    let counter = Arc::new(Counter::new(Encoding::Cl100k).expect("cl100k"));
    let text = "Hello, concurrent world!";
    let expected = counter.count(text);

    // Spawn 200 threads (> tiktoken MAX_NUM_THREADS = 128) to stress-test thread safety
    let handles: Vec<_> = (0..200)
        .map(|_| {
            let c = Arc::clone(&counter);
            let t = text.to_owned();
            thread::spawn(move || c.count(&t))
        })
        .collect();

    for (i, h) in handles.into_iter().enumerate() {
        let count = h.join().expect("thread panic");
        assert_eq!(
            count, expected,
            "thread {i}: concurrent count {count} != expected {expected}"
        );
    }
}

// ============================================================================
// AC12: 100KB counting latency < 25ms (hard CI ceiling per AC12)
// ============================================================================

#[test]
fn ac12_100kb_latency_under_25ms() {
    // Build ~100KB input
    let snippet = "fn process(items: &[Item]) -> Result<Vec<Output>, Error> {\n\
                   items.iter().map(|item| transform(item)).collect()\n}\n\n";
    let repeat = (100 * 1024) / snippet.len() + 1;
    let input = snippet.repeat(repeat);
    assert!(input.len() >= 100 * 1024);

    let counter = Counter::new(Encoding::Cl100k).expect("cl100k");
    // Warm up (important: tiktoken initializes regex caches on first use)
    let _ = counter.count(&input);

    let start = Instant::now();
    let count = counter.count(&input);
    let elapsed = start.elapsed();

    assert!(count > 0, "must count tokens in 100KB input");

    // Per AC12 (ADR-003): the 25ms bound is the CI ceiling for release builds.
    // Debug builds are 5-10x slower due to unoptimised regex/BPE code.
    // The hard gate applies to `--release` builds in CI; debug builds use a
    // generous 2000ms sentinel to catch catastrophic regressions only.
    #[cfg(debug_assertions)]
    let ceiling_ms = 2000u128;
    #[cfg(not(debug_assertions))]
    let ceiling_ms = 25u128;

    assert!(
        elapsed.as_millis() < ceiling_ms,
        "100KB cl100k counting took {}ms, must be < {}ms ({})",
        elapsed.as_millis(),
        ceiling_ms,
        if cfg!(debug_assertions) {
            "debug build"
        } else {
            "release build"
        },
    );
}

// ============================================================================
// AC10: No unwrap/expect/panic in non-test code (clippy enforced).
// The Err path for TiktokenInit is verified here via the counter::new
// contract — we assert the Result type is returned correctly.
// ============================================================================

#[test]
fn ac10_construction_returns_result() {
    // Normal path always returns Ok
    let result: Result<Counter, TokenError> = Counter::new(Encoding::Cl100k);
    assert!(result.is_ok(), "cl100k construction must return Ok");

    let result: Result<Counter, TokenError> = Counter::new(Encoding::Heuristic);
    assert!(result.is_ok(), "heuristic construction must return Ok");
}

/// `Counter::heuristic()` is the infallible constructor used by the tokens.rs
/// fallback path to avoid `unreachable!()` / `unwrap()` on the provably-dead
/// error arm (AC10 no-panic invariant). Verify it produces correct counts.
#[test]
fn ac10_heuristic_constructor_is_infallible_and_counts() {
    let counter = Counter::heuristic();
    assert_eq!(counter.encoding(), Encoding::Heuristic);
    // Byte-length heuristic: ASCII input → byte count
    assert_eq!(counter.count("hello"), 5, "hello = 5 ASCII bytes");
    // Must be a safe ceiling: >= any cl100k count (verified by ac6 tests for correctness)
    let cl = Counter::new(Encoding::Cl100k).expect("cl100k");
    let text = "fn foo() -> i32 { 42 }";
    assert!(
        counter.count(text) >= cl.count(text),
        "heuristic() must not undercount cl100k: heuristic={} cl100k={}",
        counter.count(text),
        cl.count(text),
    );
}

// ============================================================================
// AC5 additional: Anthropic offline formula verification
// ============================================================================

#[test]
fn ac5_anthropic_formula_is_ceil_cl100k_times_1_25() {
    use rskim_tokens::anthropic_offline::{UPLIFT_FACTOR, count_anthropic_offline};
    use rskim_tokens::heuristic::count_heuristic;

    assert!(
        (UPLIFT_FACTOR - 1.25).abs() < f64::EPSILON,
        "UPLIFT_FACTOR must be 1.25"
    );

    // Verify formula: ceil(cl100k * 1.25)
    let cl100k = Counter::new(Encoding::Cl100k).unwrap();
    let anthropic = Counter::new(Encoding::AnthropicOffline).unwrap();

    let samples = ["hello", "world", "The quick brown fox", "日本語テスト"];
    for s in samples {
        let cl_count = cl100k.count(s);
        let expected = count_anthropic_offline(cl_count);
        let actual = anthropic.count(s);
        assert_eq!(
            actual, expected,
            "anthropic formula: input={s:?} cl100k={cl_count} expected={expected} actual={actual}"
        );
        let _ = count_heuristic(s); // exercise heuristic too
    }
}

// ============================================================================
// AC6 additional: Property test — heuristic is >= max(cl100k, o200k) for large inputs
// ============================================================================

#[test]
fn ac6_heuristic_gte_for_single_byte() {
    let cl100k = Counter::new(Encoding::Cl100k).unwrap();
    let o200k = Counter::new(Encoding::O200k).unwrap();
    let heuristic = Counter::heuristic();

    // Single byte
    let s = "a";
    let h = heuristic.count(s);
    let cl = cl100k.count(s);
    let o2 = o200k.count(s);
    assert!(
        h >= cl.max(o2),
        "heuristic >= max for single byte: h={h} cl={cl} o2={o2}"
    );
}

// True property-style coverage of the AC6 invariant: for ARBITRARY UTF-8 input
// (not just a fixed corpus), the byte-length heuristic must never undercount the
// real BPE tokenizers. Generated inputs probe cases the fixed corpus cannot.
//
// Counters are built ONCE outside the proptest closure (BPE construction decodes
// the embedded vocab and is comparatively expensive); the closure only borrows
// them, keeping the generated-case loop fast.
#[test]
fn ac6_heuristic_gte_max_bpe_property() {
    let cl100k = Counter::new(Encoding::Cl100k).unwrap();
    let o200k = Counter::new(Encoding::O200k).unwrap();
    let heuristic = Counter::heuristic();

    let mut runner = proptest::test_runner::TestRunner::default();
    runner
        .run(&proptest::prelude::any::<String>(), |s| {
            let h = heuristic.count(&s);
            let cl = cl100k.count(&s);
            let o2 = o200k.count(&s);
            proptest::prop_assert!(
                h >= cl.max(o2),
                "heuristic must be >= max(cl100k, o200k) for arbitrary input: \
                 input={s:?} heuristic={h} cl100k={cl} o200k={o2}"
            );
            Ok(())
        })
        .expect("AC6 property must hold for all generated inputs");
}
