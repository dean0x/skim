//! AST Pattern Library — catalog of named structural code patterns (#196).
//!
//! This module provides a curated, data-driven catalog of code patterns indexed
//! by their structural AST n-gram signatures. Patterns are either exact (the
//! n-grams are a reliable SUBSET of any code exhibiting the pattern) or
//! approximate (the n-grams are a weak proxy; description says "approximation"
//! or "structural").
//!
//! # Design
//!
//! Each [`Pattern`] carries:
//! - A unique kebab-case `name` used as a query key.
//! - Human-readable `description` (honest about accuracy).
//! - `exact: true` iff the n-grams are a reliable subset of every occurrence.
//! - `bigrams`/`trigrams`: string pairs/triples to resolve via [`vocab_lookup`]
//!   or, for synthetic markers, via reserved-name mapping.
//! - `example`: a real code snippet GOLD-verified to emit the declared n-grams.
//!
//! # Honest limitations
//!
//! The following patterns are in LINTER TERRITORY and are NOT included because
//! they cannot be detected from structural n-grams alone:
//! - `hardcoded-secret` — requires semantic analysis of literal content.
//! - `single-use-variable` — requires data-flow analysis.
//! - `magic-number` — a weak numeric-literal-in-expression proxy is available
//!   (see `numeric-literal-in-expression`) but is NOT named "magic-number"
//!   to avoid overclaiming.
//!
//! # GOLD verification
//!
//! Every pattern's `example` is GOLD-verified in `patterns_tests.rs` (test F7):
//! `linearize_source(example, example_lang)` followed by
//! `extract_ast_ngrams_with_metrics` must emit every declared bigram and
//! trigram. Patterns that could not be verified were either fixed or dropped.
//!
//! # Ranking
//!
//! Patterns are queryable today but do NOT affect ranking — ranking integration
//! is deferred to Wave 4 (structural-complexity scoring dimension).

use std::collections::HashMap;
use std::sync::LazyLock;

use rskim_core::Language;

use super::{AstBigram, AstNgramSet, AstTrigram, NodeKindId, vocab_lookup};
use crate::ast_index::structural::{DEEP_NODE, EMPTY_BODY, LARGE_BODY, MANY_PARAMS, bucket_label};
use crate::{Result, SearchError};

// ============================================================================
// Pattern category
// ============================================================================

/// Broad category for grouping patterns in the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PatternCategory {
    /// Error handling constructs (try/catch, rescue, except, etc.)
    ErrorHandling,
    /// Performance-relevant patterns (nested loops, call-in-loop, deep nesting)
    Performance,
    /// Concurrency-related constructs (goroutines, channels, unsafe, synchronized)
    Concurrency,
    /// Code quality signals (empty bodies, god functions, excessive params)
    Quality,
    /// Structural constructs (match/switch/impl/class)
    Structure,
}

// ============================================================================
// Synthetic marker name mapping
// ============================================================================

/// Reserved name strings for synthetic structural markers.
///
/// These are NOT in the real vocabulary. They are mapped to synthetic IDs
/// by [`resolved_bigrams`][Pattern::resolved_bigrams] so patterns can reference
/// structural markers (EMPTY_BODY, LARGE_BODY, MANY_PARAMS, DEEP_NODE) by name.
///
/// # Parent marker names
///
/// - `"__empty_body__"` → `EMPTY_BODY` (65000)
/// - `"__large_body__"` → `LARGE_BODY` (65002)
/// - `"__many_params__"` → `MANY_PARAMS` (65003)
/// - `"__deep_node__"` → `DEEP_NODE` (65001)
///
/// # Bucket label names (child side of bucketed bigrams)
///
/// `"__large_body_b10__"` = bucket_label(0) (body stmts >= 10)
/// `"__large_body_b20__"` = bucket_label(1) (body stmts >= 20)
/// `"__large_body_b40__"` = bucket_label(2) (body stmts >= 40)
/// `"__many_params_b5__"` = bucket_label(0) (params >= 5)
/// `"__many_params_b8__"` = bucket_label(1) (params >= 8)
/// `"__many_params_b12__"` = bucket_label(2) (params >= 12)
/// `"__deep_node_b4__"` = bucket_label(0) (depth >= 4)
/// `"__deep_node_b6__"` = bucket_label(1) (depth >= 6)
/// `"__deep_node_b8__"` = bucket_label(2) (depth >= 8)
///
/// The correct bigram for "deep nesting detected" is:
/// `("__deep_node__", "__deep_node_b4__")` which encodes as
/// `AstBigram::encode(DEEP_NODE, bucket_label(0))`.
///
/// `"__empty_body__"` is always the parent side; the child side is a real
/// vocab string (the enclosing construct kind).
fn resolve_synthetic_name(name: &str) -> Option<NodeKindId> {
    match name {
        // Synthetic parent IDs
        "__empty_body__" => Some(EMPTY_BODY),
        "__large_body__" => Some(LARGE_BODY),
        "__many_params__" => Some(MANY_PARAMS),
        "__deep_node__" => Some(DEEP_NODE),
        // Bucket labels (child side of bucketed bigrams)
        "__large_body_b10__" => Some(bucket_label(0)),
        "__large_body_b20__" => Some(bucket_label(1)),
        "__large_body_b40__" => Some(bucket_label(2)),
        "__many_params_b5__" => Some(bucket_label(0)),
        "__many_params_b8__" => Some(bucket_label(1)),
        "__many_params_b12__" => Some(bucket_label(2)),
        "__deep_node_b4__" => Some(bucket_label(0)),
        "__deep_node_b6__" => Some(bucket_label(1)),
        "__deep_node_b8__" => Some(bucket_label(2)),
        _ => None,
    }
}

/// Resolve a kind string to a `NodeKindId`, checking synthetic names first.
fn resolve_kind_name(name: &str) -> Option<NodeKindId> {
    resolve_synthetic_name(name).or_else(|| vocab_lookup(name))
}

// ============================================================================
// Pattern struct
// ============================================================================

/// One entry in the AST Pattern Library catalog.
///
/// A pattern is a named structural code pattern identified by its AST n-gram
/// signatures. Patterns are either exact (reliable subset of every occurrence)
/// or approximate (structural heuristic; description uses "approximation" or
/// "structural proxy").
#[derive(Debug, Clone)]
pub struct Pattern {
    /// Unique kebab-case query key (e.g. `"try-catch"`, `"empty-catch"`).
    pub name: &'static str,
    /// Human-readable description. Honest: approximate patterns explicitly say
    /// "approximation" or "structural proxy".
    pub description: &'static str,
    /// Broad category.
    pub category: PatternCategory,
    /// `true` iff the declared n-grams are a reliable SUBSET of every occurrence
    /// of this pattern. `false` means the match is approximate.
    pub exact: bool,
    /// A real code snippet (not fictional) that EXHIBITS this pattern.
    /// GOLD-verified: `linearize_source(example, example_lang)` followed by
    /// `extract_ast_ngrams_with_metrics` must emit all declared bigrams/trigrams.
    pub example: &'static str,
    /// Language of `example`.
    pub example_lang: Language,
    /// Real or synthetic bigram name pairs. Each `(parent_name, child_name)` is
    /// resolved via `vocab_lookup` or synthetic-name mapping by `resolved_bigrams`.
    pub bigrams: &'static [(&'static str, &'static str)],
    /// Real or synthetic trigram name triples.
    pub trigrams: &'static [(&'static str, &'static str, &'static str)],
}

impl Pattern {
    /// Resolve declared bigram name pairs to [`AstBigram`] values.
    ///
    /// Pairs where either side does not resolve (unknown kind string or synthetic
    /// name) are silently dropped. Call sites can check the count against
    /// `self.bigrams.len()` to detect partial resolution.
    #[must_use]
    pub fn resolved_bigrams(&self) -> Vec<AstBigram> {
        self.bigrams
            .iter()
            .filter_map(|(parent, child)| {
                let p = resolve_kind_name(parent)?;
                let c = resolve_kind_name(child)?;
                Some(AstBigram::encode(p, c))
            })
            .collect()
    }

    /// Resolve declared trigram name triples to [`AstTrigram`] values.
    ///
    /// Triples where any name does not resolve are silently dropped.
    #[must_use]
    pub fn resolved_trigrams(&self) -> Vec<AstTrigram> {
        self.trigrams
            .iter()
            .filter_map(|(gp, p, c)| {
                let gp_id = resolve_kind_name(gp)?;
                let p_id = resolve_kind_name(p)?;
                let c_id = resolve_kind_name(c)?;
                Some(AstTrigram::encode(gp_id, p_id, c_id))
            })
            .collect()
    }
}

// ============================================================================
// Pattern catalog
// ============================================================================

/// Full pattern catalog. All patterns GOLD-verified against real examples.
///
/// # Coverage
///
/// | Category | Count |
/// |----------|-------|
/// | ErrorHandling | 6 |
/// | Performance | 5 |
/// | Concurrency | 6 |
/// | Quality | 7 |
/// | Structure | 5 |
/// | **Total** | **29** |
pub static PATTERNS: &[Pattern] = &[
    // ── ErrorHandling ────────────────────────────────────────────────────────

    Pattern {
        name: "try-catch",
        description: "A try/catch block (TypeScript/JavaScript). Exact: try_statement always \
                       contains a catch_clause.",
        category: PatternCategory::ErrorHandling,
        exact: true,
        example: "try { doWork(); } catch (e) { handle(e); }",
        example_lang: Language::TypeScript,
        bigrams: &[("try_statement", "catch_clause")],
        trigrams: &[],
    },
    Pattern {
        name: "try-finally",
        description: "A try/finally block (TypeScript/JavaScript). Exact: try_statement always \
                       contains a finally_clause.",
        category: PatternCategory::ErrorHandling,
        exact: true,
        example: "try { open(); } finally { close(); }",
        example_lang: Language::TypeScript,
        bigrams: &[("try_statement", "finally_clause")],
        trigrams: &[],
    },
    Pattern {
        name: "python-try-except",
        description: "A try/except block (Python). Exact: try_statement contains except_clause.",
        category: PatternCategory::ErrorHandling,
        exact: true,
        example: "try:\n    do_work()\nexcept Exception as e:\n    handle(e)",
        example_lang: Language::Python,
        bigrams: &[("try_statement", "except_clause")],
        trigrams: &[],
    },
    Pattern {
        name: "ruby-begin-rescue",
        description: "A rescue clause in a Ruby method body. Structural approximation: \
                       body_statement contains rescue — body_statement is the node wrapping \
                       a Ruby method's body and inline rescue clauses. The bigram \
                       body_statement → rescue appears whenever rescue is used directly in a \
                       method body (the common form). Cannot distinguish from a stand-alone \
                       rescue block outside a method.",
        category: PatternCategory::ErrorHandling,
        exact: false,
        example: "def call\n  do_work\nrescue => e\n  handle(e)\nend",
        example_lang: Language::Ruby,
        bigrams: &[("body_statement", "rescue")],
        trigrams: &[],
    },
    Pattern {
        name: "empty-catch",
        description: "A catch clause with an empty body (TypeScript/JavaScript). Exact: \
                       EMPTY_BODY is emitted for a statement_block with zero counted children, \
                       keyed on the enclosing catch_clause.",
        category: PatternCategory::ErrorHandling,
        exact: true,
        example: "try { f(); } catch (e) {}",
        example_lang: Language::TypeScript,
        bigrams: &[("__empty_body__", "catch_clause")],
        trigrams: &[],
    },
    Pattern {
        name: "try-catch-finally",
        description: "A try/catch/finally block (TypeScript/JavaScript). Exact: all three \
                       clauses are children of try_statement.",
        category: PatternCategory::ErrorHandling,
        exact: true,
        example: "try { f(); } catch (e) { g(); } finally { h(); }",
        example_lang: Language::TypeScript,
        bigrams: &[("try_statement", "catch_clause"), ("try_statement", "finally_clause")],
        trigrams: &[],
    },

    // ── Performance ──────────────────────────────────────────────────────────

    Pattern {
        name: "nested-loop",
        description: "A loop nested inside another loop (TypeScript/JavaScript for-statements). \
                       Exact: for_statement → statement_block → for_statement is the structural \
                       trigram that identifies an outer for loop whose body contains an inner \
                       for loop.",
        category: PatternCategory::Performance,
        exact: true,
        example: "for (let i=0; i<n; i++) { for (let j=0; j<m; j++) { work(i,j); } }",
        example_lang: Language::TypeScript,
        bigrams: &[],
        trigrams: &[("for_statement", "statement_block", "for_statement")],
    },
    Pattern {
        name: "rust-nested-loop",
        description: "A for-expression inside a block statement (Rust). Structural approximation: \
                       block → expression_statement → for_expression — this trigram appears in \
                       nested loops (the inner loop is an expression_statement in the outer \
                       loop's block) but also matches any for loop inside a block. Cannot \
                       distinguish between nested and non-nested without outer context.",
        category: PatternCategory::Performance,
        exact: false,
        example: "fn outer() { for i in 0..n { for j in 0..m { work(i, j); } } }",
        example_lang: Language::Rust,
        bigrams: &[],
        trigrams: &[("block", "expression_statement", "for_expression")],
    },
    Pattern {
        name: "call-in-loop",
        description: "A function call inside a for-of loop body (TypeScript/JavaScript). \
                       Structural approximation: for_in_statement → statement_block → \
                       expression_statement is the trigram showing a loop body with a statement. \
                       Does not confirm the statement is a call; use the separate \
                       expression_statement → call_expression bigram together for higher confidence. \
                       Cannot distinguish between a call and other statement forms.",
        category: PatternCategory::Performance,
        exact: false,
        example: "for (const x of items) { process(x); }",
        example_lang: Language::TypeScript,
        bigrams: &[],
        trigrams: &[("for_in_statement", "statement_block", "expression_statement")],
    },
    Pattern {
        name: "deep-nesting",
        description: "Code with deeply nested structures (depth >= 4). Exact: DEEP_NODE marker \
                       is emitted at exact depth bucket boundaries (4, 6, 8) — the synthetic \
                       bigram DEEP_NODE → bucket_label(0) appears in every file with a node at \
                       depth >= 4. Bucket granularity is fixed at index time.",
        category: PatternCategory::Performance,
        exact: true,
        // The synthetic bigram is DEEP_NODE (parent) → bucket_label(0) (child).
        // This resolves to AstBigram::encode(DEEP_NODE=65001, bucket_label(0)=64900).
        example: "function a() { if (x) { for (;;) { if (y) { doWork(); } } } }",
        example_lang: Language::TypeScript,
        bigrams: &[("__deep_node__", "__deep_node_b4__")],
        trigrams: &[],
    },
    Pattern {
        name: "python-nested-loop",
        description: "A loop nested inside another loop (Python). Structural approximation: \
                       for_statement → block → for_statement — the inner loop appears in the \
                       body block of the outer loop. May also match an outer loop whose body \
                       contains a nested function that itself has a loop.",
        category: PatternCategory::Performance,
        exact: false,
        example: "for i in range(n):\n    for j in range(m):\n        work(i, j)",
        example_lang: Language::Python,
        bigrams: &[],
        trigrams: &[("for_statement", "block", "for_statement")],
    },

    // ── Concurrency ──────────────────────────────────────────────────────────

    Pattern {
        name: "go-goroutine",
        description: "A goroutine launch (Go). Exact: go_statement is the tree-sitter node for \
                       `go f()` calls.",
        category: PatternCategory::Concurrency,
        exact: true,
        // Full function body so tree-sitter can produce a valid Go source unit.
        example: "package main\nfunc serve() { go handle(conn) }",
        example_lang: Language::Go,
        bigrams: &[("go_statement", "call_expression")],
        trigrams: &[],
    },
    Pattern {
        name: "go-defer",
        description: "A defer statement (Go). Exact: defer_statement is emitted for `defer f()`.",
        category: PatternCategory::Concurrency,
        exact: true,
        example: "package main\nfunc run() { defer cleanup() }",
        example_lang: Language::Go,
        bigrams: &[("defer_statement", "call_expression")],
        trigrams: &[],
    },
    Pattern {
        name: "go-channel-send",
        description: "A channel send operation (Go). Exact: send_statement is the CST node for \
                       `ch <- val`.",
        category: PatternCategory::Concurrency,
        exact: true,
        example: "package main\nfunc send(ch chan string, msg string) { ch <- msg }",
        example_lang: Language::Go,
        bigrams: &[("send_statement", "identifier")],
        trigrams: &[],
    },
    Pattern {
        name: "go-select",
        description: "A select statement (Go). Exact: select_statement is the CST node for \
                       `select { case ... }`.",
        category: PatternCategory::Concurrency,
        exact: true,
        example: "package main\nfunc recv(ch chan string) { select { case msg := <-ch: handle(msg) } }",
        example_lang: Language::Go,
        bigrams: &[("select_statement", "communication_case")],
        trigrams: &[],
    },
    Pattern {
        name: "rust-unsafe-block",
        description: "An unsafe block (Rust). Exact: unsafe_block is the CST node for \
                       `unsafe { ... }`. unsafe_block always contains a block node as its body.",
        category: PatternCategory::Concurrency,
        exact: true,
        example: "fn write_raw(ptr: *mut i32, val: i32) { unsafe { *ptr = val; } }",
        example_lang: Language::Rust,
        bigrams: &[("unsafe_block", "block")],
        trigrams: &[],
    },
    Pattern {
        name: "java-synchronized",
        description: "A synchronized block (Java). Exact: synchronized_statement is the CST \
                       node for `synchronized (obj) { ... }`.",
        category: PatternCategory::Concurrency,
        exact: true,
        example: "class C { void inc() { synchronized (this) { count++; } } }",
        example_lang: Language::Java,
        bigrams: &[("synchronized_statement", "block")],
        trigrams: &[],
    },

    // ── Quality ──────────────────────────────────────────────────────────────

    Pattern {
        name: "function-with-body",
        description: "A function item with a body (Rust). Exact: function_item always contains \
                       a block.",
        category: PatternCategory::Quality,
        exact: true,
        example: "fn foo() { let x = 1; }",
        example_lang: Language::Rust,
        bigrams: &[("function_item", "block")],
        trigrams: &[],
    },
    Pattern {
        name: "method-with-body",
        description: "A method definition with a body (TypeScript/JavaScript). Exact: \
                       method_definition always contains a statement_block.",
        category: PatternCategory::Quality,
        exact: true,
        example: "class C { foo() { return 1; } }",
        example_lang: Language::TypeScript,
        bigrams: &[("method_definition", "statement_block")],
        trigrams: &[],
    },
    Pattern {
        name: "match-with-arms",
        description: "A match expression with match arms (Rust). Exact: match_expression always \
                       contains a match_block, which contains match_arm nodes.",
        category: PatternCategory::Quality,
        exact: true,
        example: "fn check(x: Result<i32,()>) -> i32 { match x { Ok(v) => v, Err(_) => 0 } }",
        example_lang: Language::Rust,
        bigrams: &[("match_expression", "match_block")],
        trigrams: &[("match_expression", "match_block", "match_arm")],
    },
    Pattern {
        name: "empty-function",
        description: "A function item with an empty body (Rust). Exact: EMPTY_BODY is emitted \
                       when a block has zero counted children, keyed on the enclosing \
                       function_item.",
        category: PatternCategory::Quality,
        exact: true,
        example: "fn todo_later() {}",
        example_lang: Language::Rust,
        bigrams: &[("__empty_body__", "function_item")],
        trigrams: &[],
    },
    Pattern {
        name: "god-function",
        description: "A function with a very large body (Rust, >= 20 statements). Exact: \
                       LARGE_BODY → bucket_label(1) is emitted when a function body has >= 20 \
                       counted children. Bucket granularity is fixed at index time; only \
                       function/method bodies emit this marker.",
        category: PatternCategory::Quality,
        exact: true,
        // Synthetic bigram: LARGE_BODY (65002) parent → bucket_label(1) (edge >= 20 stmts) child.
        // 20 let-statements in one function body crosses BODY_STMT_EDGES[1] = 20.
        example: "fn big() { let a=1; let b=2; let c=3; let d=4; let e=5; let f=6; let g=7; \
                   let h=8; let i=9; let j=10; let k=11; let l=12; let m=13; let n=14; \
                   let o=15; let p=16; let q=17; let r=18; let s=19; let t=20; }",
        example_lang: Language::Rust,
        bigrams: &[("__large_body__", "__large_body_b20__")],
        trigrams: &[],
    },
    Pattern {
        name: "excessive-params",
        description: "A function with many parameters (Rust, >= 5). Exact: \
                       MANY_PARAMS → bucket_label(0) is emitted when a parameter list has >= 5 \
                       counted children. Bucket granularity is fixed at index time.",
        category: PatternCategory::Quality,
        exact: true,
        example: "fn many(a: i32, b: i32, c: i32, d: i32, e: i32) -> i32 { a + b + c + d + e }",
        example_lang: Language::Rust,
        bigrams: &[("__many_params__", "__many_params_b5__")],
        trigrams: &[],
    },
    Pattern {
        name: "unhandled-result",
        description: "Structural approximation: an expression_statement containing a \
                       call_expression (TypeScript/JavaScript). This is a weak structural proxy \
                       that matches any top-level call — it cannot confirm whether the call \
                       returns a Result type or that the caller ignores an error.",
        category: PatternCategory::Quality,
        exact: false,
        example: "doSomething();",
        example_lang: Language::TypeScript,
        bigrams: &[("expression_statement", "call_expression")],
        trigrams: &[],
    },

    // ── Structure ────────────────────────────────────────────────────────────

    Pattern {
        name: "switch-with-cases",
        description: "A switch statement with a switch body (TypeScript/JavaScript). Exact: \
                       switch_statement always contains a switch_body.",
        category: PatternCategory::Structure,
        exact: true,
        example: "switch (x) { case 1: break; default: other(); }",
        example_lang: Language::TypeScript,
        bigrams: &[("switch_statement", "switch_body")],
        trigrams: &[],
    },
    Pattern {
        name: "impl-method",
        description: "A method inside a Rust impl block. Exact: impl_item contains a \
                       declaration_list, which contains function_item nodes.",
        category: PatternCategory::Structure,
        exact: true,
        example: "impl Foo { fn bar(&self) { } }",
        example_lang: Language::Rust,
        bigrams: &[("impl_item", "declaration_list")],
        trigrams: &[("impl_item", "declaration_list", "function_item")],
    },
    Pattern {
        name: "class-method",
        description: "A method inside a TypeScript class. Exact: class_declaration contains \
                       a class_body, which contains method_definition nodes.",
        category: PatternCategory::Structure,
        exact: true,
        example: "class Foo { bar() { return 1; } }",
        example_lang: Language::TypeScript,
        bigrams: &[("class_declaration", "class_body")],
        trigrams: &[("class_declaration", "class_body", "method_definition")],
    },
    Pattern {
        name: "ternary-expression",
        description: "A ternary conditional expression in assignment context (TypeScript/JavaScript). \
                       Structural approximation: variable_declarator contains ternary_expression \
                       — this covers the assignment form but not all ternary positions (e.g., \
                       return statements, call arguments). Cannot detect all ternary usages from \
                       this bigram alone.",
        category: PatternCategory::Structure,
        exact: false,
        // `const x = flag ? a : b;` → lexical_declaration > variable_declarator > ternary_expression
        example: "const x = flag ? a : b;",
        example_lang: Language::TypeScript,
        bigrams: &[("variable_declarator", "ternary_expression")],
        trigrams: &[],
    },
    Pattern {
        name: "numeric-literal-in-expression",
        description: "Structural approximation: a number literal appearing inside a binary \
                       expression (TypeScript/JavaScript). This is a weak structural proxy for \
                       'magic number' — it matches any numeric literal in an arithmetic context, \
                       not just unexplained constants. Cannot determine whether the number is a \
                       named constant or a magic value.",
        category: PatternCategory::Structure,
        exact: false,
        example: "const result = x * 42 + 7;",
        example_lang: Language::TypeScript,
        bigrams: &[("binary_expression", "number")],
        trigrams: &[],
    },
];

// ============================================================================
// Pattern catalog API
// ============================================================================

/// Return all patterns in the catalog.
#[must_use]
pub fn all_patterns() -> &'static [Pattern] {
    PATTERNS
}

/// Internal index: name → position in PATTERNS.
static PATTERN_INDEX: LazyLock<HashMap<&'static str, usize>> = LazyLock::new(|| {
    PATTERNS
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name, i))
        .collect()
});

/// Look up a pattern by its kebab-case `name`.
///
/// # Errors
///
/// Returns [`SearchError::InvalidQuery`] if no pattern with that name exists.
pub fn lookup_pattern(name: &str) -> Result<&'static Pattern> {
    PATTERN_INDEX
        .get(name)
        .map(|&i| &PATTERNS[i])
        .ok_or_else(|| {
            SearchError::InvalidQuery(format!(
                "unknown pattern name '{name}'; available patterns: {}",
                PATTERNS
                    .iter()
                    .map(|p| p.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

/// Build an [`AstNgramSet`] from a pattern's resolved bigrams and trigrams,
/// with count=1 for each resolved entry.
///
/// Useful for feeding a pattern directly into the search pipeline.
/// Bigrams/trigrams that fail to resolve (unknown names) are silently dropped.
#[must_use]
pub fn pattern_to_query_set(pattern: &Pattern) -> AstNgramSet {
    use crate::ast_index::{AstBigramEntry, AstTrigramEntry, DEFAULT_AST_WEIGHT};

    let bigrams = pattern
        .resolved_bigrams()
        .into_iter()
        .map(|ngram| AstBigramEntry {
            ngram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        })
        .collect();

    let trigrams = pattern
        .resolved_trigrams()
        .into_iter()
        .map(|ngram| AstTrigramEntry {
            ngram,
            weight: DEFAULT_AST_WEIGHT,
            count: 1,
        })
        .collect();

    AstNgramSet { bigrams, trigrams }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[path = "patterns_tests.rs"]
mod tests;
