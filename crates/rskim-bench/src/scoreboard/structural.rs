//! Structural oracle for `skim search --ast <pattern>` (#541): hand-written
//! tree-sitter queries that encode each catalog pattern's documented
//! description, run over the real grammars.
//!
//! # Independence
//!
//! Nothing here reuses skim's AST search stack — no linearization, n-gram,
//! query engine or re-parse verify code. The one item taken from
//! `rskim-search` is the pattern catalog's NAMES, through
//! `rskim_search::all_patterns`, so a catalog pattern with no oracle query is
//! listed as uncovered instead of silently skipped. Where the oracle must
//! agree with skim (the extension table, the AST-indexed language list, the
//! size cap) it keeps its OWN copy with a citation, so a policy change on
//! skim's side shows up as a scoreboard diff (the `oracle.rs` / `universe.rs`
//! convention). The unit tests enforce this with a source scan.
//!
//! # Ground truth
//!
//! Each `(pattern, language)` pair has one query file,
//! `crates/rskim-bench/scoreboard/structural/<pattern>.<lang>.scm`, compiled
//! in with `include_str!` and hashed into the golden digest by the caller
//! ([`query_sources`]), so editing a query forces a re-bless. `.tsx` files
//! are parsed with the TSX grammar (ADR-003: the oracle is the real grammar),
//! although skim parses them with the plain TypeScript grammar.
//!
//! Every query names one `@match` capture; the match line is the first
//! (1-based) line of that node. Four patterns (six queries) carry a small
//! post-filter ([`PostFilter`]) because their catalog description states a count
//! (`empty-catch`, `empty-function`: zero body elements; `god-function`:
//! at least 20; `excessive-params`: at least 5 parameters).
//!
//! Next to the definition oracle, [`StructuralOracle::intent_lines`] answers
//! the intent of the two nested-loop patterns: a loop with a loop ancestor
//! inside the same function.
//!
//! # Parse errors
//!
//! tree-sitter is error-tolerant: a file with syntax errors still yields a
//! tree whose ERROR / MISSING nodes sit next to well-formed subtrees, and the
//! oracle queries that whole tree, as skim does (its walk visits every node,
//! error nodes included, and it drops a file only when the parser returns no
//! tree — `crates/rskim-search/src/compound/reparse.rs:166-171`). A parser
//! that returns no tree is a harness error here, never "no match".
//!
//! Sources are `&str`: the scoreboard universe holds strict UTF-8 text only,
//! and skim drops non-UTF-8 files on this path too
//! (`crates/rskim-search/src/compound/reparse.rs:163`, `:296`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::str::FromStr;

use anyhow::Context;
use tree_sitter::{Node, Parser, Query, QueryCursor, QueryMatch, StreamingIterator, Tree};

// ============================================================================
// Languages (the oracle's own extension → grammar table)
// ============================================================================

/// A language the oracle has a tree-sitter grammar for.
///
/// `Tsx` is separate from `TypeScript` because the oracle parses `.tsx`
/// with the TSX grammar (ADR-003), whereas skim parses every TypeScript
/// extension with `LANGUAGE_TYPESCRIPT`
/// (`crates/rskim-core/src/types.rs:289-310`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OracleLang {
    /// `.rs` — tree-sitter-rust.
    Rust,
    /// `.py`, `.pyi` — tree-sitter-python.
    Python,
    /// `.ts`, `.mts`, `.cts` — tree-sitter-typescript `LANGUAGE_TYPESCRIPT`.
    TypeScript,
    /// `.tsx` — tree-sitter-typescript `LANGUAGE_TSX`.
    Tsx,
    /// `.js`, `.jsx`, `.cjs`, `.mjs` — tree-sitter-javascript.
    JavaScript,
    /// `.go` — tree-sitter-go.
    Go,
}

impl OracleLang {
    /// Every oracle language.
    pub const ALL: [OracleLang; 6] = [
        OracleLang::Rust,
        OracleLang::Python,
        OracleLang::TypeScript,
        OracleLang::Tsx,
        OracleLang::JavaScript,
        OracleLang::Go,
    ];

    /// The name used in query file names (`<pattern>.<name>.scm`) and golden
    /// entries.
    pub fn as_str(self) -> &'static str {
        match self {
            OracleLang::Rust => "rust",
            OracleLang::Python => "python",
            OracleLang::TypeScript => "typescript",
            OracleLang::Tsx => "tsx",
            OracleLang::JavaScript => "javascript",
            OracleLang::Go => "go",
        }
    }

    /// The tree-sitter grammar the oracle parses this language with.
    pub fn grammar(self) -> tree_sitter::Language {
        match self {
            OracleLang::Rust => tree_sitter_rust::LANGUAGE.into(),
            OracleLang::Python => tree_sitter_python::LANGUAGE.into(),
            OracleLang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            OracleLang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            OracleLang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            OracleLang::Go => tree_sitter_go::LANGUAGE.into(),
        }
    }
}

impl std::fmt::Display for OracleLang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for OracleLang {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> anyhow::Result<Self> {
        OracleLang::ALL
            .into_iter()
            .find(|l| l.as_str() == s)
            .ok_or_else(|| anyhow::anyhow!("unknown oracle language {s:?}"))
    }
}

/// Serialized as [`OracleLang::as_str`] (golden `lang`, report fields).
impl serde::Serialize for OracleLang {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// Parsed from [`OracleLang::as_str`] names only, so a golden `lang` the
/// oracle has no grammar for fails the load.
impl<'de> serde::Deserialize<'de> for OracleLang {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        name.parse().map_err(serde::de::Error::custom)
    }
}

/// How skim treats a file's language on the `--ast` path, by extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LangClass {
    /// AST-indexed by skim and scored by the oracle.
    Oracle(OracleLang),
    /// AST-indexed by skim, but the oracle has no grammar for it: skim rows in
    /// this language are unscored (never silently dropped).
    Unscored {
        /// Language name (skim's `--lang` name).
        language: &'static str,
    },
    /// Never AST-indexed, so skim returns no `--ast` rows for it.
    NotIndexed {
        /// Language name, or `None` for an extension skim does not know.
        language: Option<&'static str>,
        /// Whether skim still counts the file in its AST size-cap accounting
        /// (`ast_coverage.size_excluded_files`): true for a language that has
        /// a tree-sitter grammar but no AST vocabulary (Bash).
        size_capped: bool,
    },
}

impl LangClass {
    /// Whether a file of this class participates in skim's size-cap
    /// accounting — skim's `ast_size_limit(lang)` is `Some`
    /// (`crates/rskim-core/src/ast_walk.rs:307-329`; the coverage predicate is
    /// `crates/rskim-search/src/ast_index/coverage.rs:185-203`).
    pub fn counts_toward_size_cap(self) -> bool {
        match self {
            LangClass::Oracle(_) | LangClass::Unscored { .. } => true,
            LangClass::NotIndexed { size_capped, .. } => size_capped,
        }
    }
}

/// One row of the oracle's extension table.
struct ExtClass {
    extensions: &'static [&'static str],
    class: LangClass,
}

/// The oracle's own copy of skim's extension → language table
/// (`rskim_core::Language::from_extension`,
/// `crates/rskim-core/src/types.rs:55-80`, case-sensitive) crossed with the
/// languages skim AST-indexes (`LANG_MAPS`,
/// `crates/rskim-search/src/ast_index/linearize.rs:119-134`: every
/// tree-sitter language except Bash, which has a grammar but no AST
/// vocabulary) and the grammar table (`crates/rskim-core/src/types.rs:289-310`;
/// JSON / YAML / TOML have none).
const EXT_CLASSES: &[ExtClass] = &[
    ExtClass {
        extensions: &["ts", "mts", "cts"],
        class: LangClass::Oracle(OracleLang::TypeScript),
    },
    ExtClass {
        extensions: &["tsx"],
        class: LangClass::Oracle(OracleLang::Tsx),
    },
    ExtClass {
        extensions: &["js", "jsx", "cjs", "mjs"],
        class: LangClass::Oracle(OracleLang::JavaScript),
    },
    ExtClass {
        extensions: &["py", "pyi"],
        class: LangClass::Oracle(OracleLang::Python),
    },
    ExtClass {
        extensions: &["rs"],
        class: LangClass::Oracle(OracleLang::Rust),
    },
    ExtClass {
        extensions: &["go"],
        class: LangClass::Oracle(OracleLang::Go),
    },
    ExtClass {
        extensions: &["java"],
        class: LangClass::Unscored { language: "java" },
    },
    ExtClass {
        extensions: &["md", "markdown"],
        class: LangClass::Unscored {
            language: "markdown",
        },
    },
    ExtClass {
        extensions: &["c", "h"],
        class: LangClass::Unscored { language: "c" },
    },
    ExtClass {
        extensions: &["cpp", "cc", "cxx", "hpp", "hxx", "hh"],
        class: LangClass::Unscored { language: "cpp" },
    },
    ExtClass {
        extensions: &["cs"],
        class: LangClass::Unscored { language: "csharp" },
    },
    ExtClass {
        extensions: &["rb"],
        class: LangClass::Unscored { language: "ruby" },
    },
    ExtClass {
        extensions: &["sql"],
        class: LangClass::Unscored { language: "sql" },
    },
    ExtClass {
        extensions: &["kt", "kts"],
        class: LangClass::Unscored { language: "kotlin" },
    },
    ExtClass {
        extensions: &["swift"],
        class: LangClass::Unscored { language: "swift" },
    },
    ExtClass {
        extensions: &["sh", "bash"],
        class: LangClass::NotIndexed {
            language: Some("bash"),
            size_capped: true,
        },
    },
    ExtClass {
        extensions: &["json"],
        class: LangClass::NotIndexed {
            language: Some("json"),
            size_capped: false,
        },
    },
    ExtClass {
        extensions: &["yaml", "yml"],
        class: LangClass::NotIndexed {
            language: Some("yaml"),
            size_capped: false,
        },
    },
    ExtClass {
        extensions: &["toml"],
        class: LangClass::NotIndexed {
            language: Some("toml"),
            size_capped: false,
        },
    },
];

/// Classify `path` by its extension (case-sensitive, as `Path::extension`
/// reports it — skim does the same).
pub fn classify(path: &str) -> LangClass {
    let ext = Path::new(path).extension().and_then(|e| e.to_str());
    ext.and_then(|ext| {
        EXT_CLASSES
            .iter()
            .find(|row| row.extensions.contains(&ext))
            .map(|row| row.class)
    })
    .unwrap_or(LangClass::NotIndexed {
        language: None,
        size_capped: false,
    })
}

// ============================================================================
// Size cap (the oracle's own copy)
// ============================================================================

/// The oracle's copy of skim's AST size cap, 1 MiB
/// (`rskim_core::AST_SIZE_LIMIT_DEFAULT`, `crates/rskim-core/src/ast_walk.rs:277`).
///
/// The cap is INCLUSIVE: skim excludes a file only when its size is strictly
/// greater (`source.len() as u64 > cap`,
/// `crates/rskim-search/src/ast_index/linearize.rs:210-214`; `meta.len() >
/// cap` at re-parse, `crates/rskim-search/src/compound/reparse.rs:361`;
/// `sz <= cap` is eligible in coverage accounting,
/// `crates/rskim-search/src/ast_index/coverage.rs:195`).
pub const AST_SIZE_CAP_BYTES: u64 = 1024 * 1024;

/// Whether a file of `len` bytes is within the AST size cap (≤ 1 MiB).
pub fn within_size_cap(len: u64) -> bool {
    len <= AST_SIZE_CAP_BYTES
}

/// How many of `files` (`(path, byte length)`) skim excludes from the AST
/// index by size: files whose language takes part in the size-cap accounting
/// ([`LangClass::counts_toward_size_cap`]) and whose length exceeds the cap.
/// This is the number skim reports as `ast_coverage.size_excluded_files`.
pub fn over_cap_count<'a>(files: impl IntoIterator<Item = (&'a str, u64)>) -> u64 {
    let over = files
        .into_iter()
        .filter(|&(path, len)| classify(path).counts_toward_size_cap() && !within_size_cap(len))
        .count();
    u64::try_from(over).unwrap_or(u64::MAX)
}

// ============================================================================
// Query registry
// ============================================================================

/// A count condition a query's matches must also satisfy, for the catalog
/// descriptions that state a count. It counts the BODY ELEMENTS of one
/// captured node: its named, non-extra children that are not attributes
/// (comments are extras; an attribute annotates the next element rather
/// than being one). A Rust tail expression is a body element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostFilter {
    /// The capture has zero body elements.
    Empty {
        /// Capture name, without `@`.
        capture: &'static str,
    },
    /// The capture has at least `min` body elements.
    AtLeast {
        /// Capture name, without `@`.
        capture: &'static str,
        /// Inclusive lower bound.
        min: usize,
    },
}

impl PostFilter {
    fn capture(self) -> &'static str {
        match self {
            PostFilter::Empty { capture } | PostFilter::AtLeast { capture, .. } => capture,
        }
    }

    fn accepts(self, elements: usize) -> bool {
        match self {
            PostFilter::Empty { .. } => elements == 0,
            PostFilter::AtLeast { min, .. } => elements >= min,
        }
    }
}

/// Node kinds that annotate the next element instead of being one.
const ATTRIBUTE_KINDS: &[&str] = &["attribute_item", "inner_attribute_item"];

/// The capture every query marks its match node with.
const MATCH_CAPTURE: &str = "match";

/// One registered oracle query.
struct OracleQuery {
    pattern: &'static str,
    lang: OracleLang,
    source: &'static str,
    filter: Option<PostFilter>,
}

impl OracleQuery {
    fn file_name(&self) -> String {
        scm_file_name(self.pattern, self.lang)
    }
}

/// A query's file name, `<pattern>.<lang>.scm`.
fn scm_file_name(pattern: &str, lang: OracleLang) -> String {
    format!("{pattern}.{lang}.scm")
}

/// Registry row: `(pattern, OracleLang variant, file-name language)` plus an
/// optional post-filter. The query text is `include_str!`ed from
/// `scoreboard/structural/<pattern>.<lang>.scm`; a unit test checks the file
/// name matches [`OracleLang::as_str`] and that every file is registered.
macro_rules! oracle_query {
    ($pattern:literal, $lang:ident, $file_lang:literal) => {
        oracle_query!($pattern, $lang, $file_lang, None)
    };
    ($pattern:literal, $lang:ident, $file_lang:literal, $filter:expr) => {
        OracleQuery {
            pattern: $pattern,
            lang: OracleLang::$lang,
            source: include_str!(concat!(
                "../../scoreboard/structural/",
                $pattern,
                ".",
                $file_lang,
                ".scm"
            )),
            filter: $filter,
        }
    };
}

const EMPTY_BODY_FILTER: Option<PostFilter> = Some(PostFilter::Empty { capture: "body" });

/// Every oracle query, sorted by `(pattern, file-name language)`.
const QUERIES: &[OracleQuery] = &[
    oracle_query!("call-in-loop", JavaScript, "javascript"),
    oracle_query!("call-in-loop", Tsx, "tsx"),
    oracle_query!("call-in-loop", TypeScript, "typescript"),
    oracle_query!("class-method", JavaScript, "javascript"),
    oracle_query!("class-method", Tsx, "tsx"),
    oracle_query!("class-method", TypeScript, "typescript"),
    oracle_query!("empty-catch", JavaScript, "javascript", EMPTY_BODY_FILTER),
    oracle_query!("empty-catch", Tsx, "tsx", EMPTY_BODY_FILTER),
    oracle_query!("empty-catch", TypeScript, "typescript", EMPTY_BODY_FILTER),
    oracle_query!("empty-function", Rust, "rust", EMPTY_BODY_FILTER),
    oracle_query!(
        "excessive-params",
        Rust,
        "rust",
        Some(PostFilter::AtLeast {
            capture: "params",
            min: 5
        })
    ),
    oracle_query!("function-with-body", Rust, "rust"),
    oracle_query!("go-channel-send", Go, "go"),
    oracle_query!("go-defer", Go, "go"),
    oracle_query!("go-goroutine", Go, "go"),
    oracle_query!("go-select", Go, "go"),
    oracle_query!(
        "god-function",
        Rust,
        "rust",
        Some(PostFilter::AtLeast {
            capture: "body",
            min: 20
        })
    ),
    oracle_query!("impl-method", Rust, "rust"),
    oracle_query!("match-with-arms", Rust, "rust"),
    oracle_query!("method-with-body", JavaScript, "javascript"),
    oracle_query!("method-with-body", Tsx, "tsx"),
    oracle_query!("method-with-body", TypeScript, "typescript"),
    oracle_query!("nested-loop", JavaScript, "javascript"),
    oracle_query!("nested-loop", Tsx, "tsx"),
    oracle_query!("nested-loop", TypeScript, "typescript"),
    oracle_query!("numeric-literal-in-expression", JavaScript, "javascript"),
    oracle_query!("numeric-literal-in-expression", Tsx, "tsx"),
    oracle_query!("numeric-literal-in-expression", TypeScript, "typescript"),
    oracle_query!("python-nested-loop", Python, "python"),
    oracle_query!("python-try-except", Python, "python"),
    oracle_query!("rust-nested-loop", Rust, "rust"),
    oracle_query!("rust-unsafe-block", Rust, "rust"),
    oracle_query!("switch-with-cases", JavaScript, "javascript"),
    oracle_query!("switch-with-cases", Tsx, "tsx"),
    oracle_query!("switch-with-cases", TypeScript, "typescript"),
    oracle_query!("ternary-expression", JavaScript, "javascript"),
    oracle_query!("ternary-expression", Tsx, "tsx"),
    oracle_query!("ternary-expression", TypeScript, "typescript"),
    oracle_query!("try-catch", JavaScript, "javascript"),
    oracle_query!("try-catch", Tsx, "tsx"),
    oracle_query!("try-catch", TypeScript, "typescript"),
    oracle_query!("try-catch-finally", JavaScript, "javascript"),
    oracle_query!("try-catch-finally", Tsx, "tsx"),
    oracle_query!("try-catch-finally", TypeScript, "typescript"),
    oracle_query!("try-finally", JavaScript, "javascript"),
    oracle_query!("try-finally", Python, "python"),
    oracle_query!("try-finally", Tsx, "tsx"),
    oracle_query!("try-finally", TypeScript, "typescript"),
    oracle_query!("unhandled-result", Go, "go"),
    oracle_query!("unhandled-result", JavaScript, "javascript"),
    oracle_query!("unhandled-result", Rust, "rust"),
    oracle_query!("unhandled-result", Tsx, "tsx"),
    oracle_query!("unhandled-result", TypeScript, "typescript"),
];

/// Catalog patterns the oracle deliberately does not cover, with the reason.
/// They stay under ADR-007 manual dog-food.
const UNCOVERED: &[(&str, &str)] = &[
    (
        "deep-nesting",
        "synthetic threshold not stated exactly: \"depth >= 4\" does not say where depth \
         is measured from or which nodes count",
    ),
    (
        "java-synchronized",
        "no oracle language: its construct (synchronized_statement -> block) exists only \
         in the Java grammar",
    ),
    (
        "ruby-begin-rescue",
        "no oracle language: its construct (body_statement -> rescue) exists only in the \
         Ruby grammar",
    ),
];

/// The reason reported for a catalog pattern that is neither registered nor
/// listed in `UNCOVERED` — a pattern added to skim's catalog after this
/// oracle was written. A unit test keeps it from happening silently.
pub const UNCLASSIFIED_REASON: &str =
    "no oracle query registered and no reason recorded (a catalog pattern newer than the oracle)";

/// The text of one registered query, for the golden digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuerySource {
    /// Catalog pattern name.
    pub pattern: &'static str,
    /// Grammar the query runs on.
    pub lang: OracleLang,
    /// The `.scm` file's full contents.
    pub source: &'static str,
}

impl QuerySource {
    /// The query's file name, `<pattern>.<lang>.scm`.
    pub fn file_name(&self) -> String {
        scm_file_name(self.pattern, self.lang)
    }
}

/// The registry ordered by `(pattern, language name)`.
fn sorted_queries() -> Vec<&'static OracleQuery> {
    let mut queries: Vec<&'static OracleQuery> = QUERIES.iter().collect();
    queries.sort_by_key(|q| (q.pattern, q.lang.as_str()));
    queries
}

/// Every registered query, ordered by `(pattern, language name)` — the input
/// for folding the `.scm` files into the golden digest.
pub fn query_sources() -> Vec<QuerySource> {
    sorted_queries()
        .into_iter()
        .map(|q| QuerySource {
            pattern: q.pattern,
            lang: q.lang,
            source: q.source,
        })
        .collect()
}

/// A canonical rendering of the oracle's per-pattern definitions: every
/// registered query (file name, post-filter, full text), every intent spec,
/// and the size cap. The golden digest folds it in
/// (`golden::structural_oracle_sha256`), so editing a query, a post-filter
/// threshold or an intent's node kinds forces a re-bless. The extension
/// table ([`classify`]) and the body-element rule ([`PostFilter`]'s
/// attribute kinds) are not folded in: an edit to them that changes an answer
/// shows up as a structural diff in the gate, like any `oracle.rs` edit.
pub fn fingerprint() -> String {
    let mut out = format!("size-cap {AST_SIZE_CAP_BYTES}\n");
    for q in sorted_queries() {
        let filter = match q.filter {
            None => "none".to_string(),
            Some(PostFilter::Empty { capture }) => format!("empty @{capture}"),
            Some(PostFilter::AtLeast { capture, min }) => format!("at-least {min} @{capture}"),
        };
        out.push_str(&format!(
            "query {} filter {filter} bytes {}\n{}\n",
            q.file_name(),
            q.source.len(),
            q.source
        ));
    }
    for spec in INTENTS {
        let langs: Vec<&str> = spec.langs.iter().map(|l| l.as_str()).collect();
        out.push_str(&format!(
            "intent {} langs [{}] loops [{}] boundaries [{}]\n",
            spec.pattern,
            langs.join(" "),
            spec.loop_kinds.join(" "),
            spec.boundary_kinds.join(" ")
        ));
    }
    out
}

/// Whether the oracle covers a catalog pattern, and why not if it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternCoverage {
    /// At least one oracle query exists; `langs` is sorted.
    Covered {
        /// Languages with a query for this pattern.
        langs: Vec<OracleLang>,
    },
    /// No oracle query; the pattern stays under manual dog-food.
    Uncovered {
        /// Why the pattern cannot be encoded exactly.
        reason: &'static str,
    },
}

/// Coverage of every pattern in skim's catalog (`rskim_search::all_patterns`
/// — the oracle's only use of skim's AST code), keyed by pattern name.
pub fn catalog_coverage() -> BTreeMap<&'static str, PatternCoverage> {
    rskim_search::all_patterns()
        .iter()
        .map(|p| (p.name, coverage_of(p.name)))
        .collect()
}

fn coverage_of(pattern: &str) -> PatternCoverage {
    let mut langs: Vec<OracleLang> = QUERIES
        .iter()
        .filter(|q| q.pattern == pattern)
        .map(|q| q.lang)
        .collect();
    if !langs.is_empty() {
        langs.sort();
        return PatternCoverage::Covered { langs };
    }
    let reason = UNCOVERED
        .iter()
        .find(|(name, _)| *name == pattern)
        .map_or(UNCLASSIFIED_REASON, |(_, reason)| *reason);
    PatternCoverage::Uncovered { reason }
}

// ============================================================================
// Intent oracles (nested loops)
// ============================================================================

/// The intent of a nested-loop pattern: a loop node with a loop ancestor
/// inside the same function. The ancestor walk stops at a function boundary,
/// so a loop inside a closure or function that itself sits in a loop is NOT
/// nested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntentSpec {
    /// Catalog pattern name.
    pub pattern: &'static str,
    /// Languages the intent is answered for.
    pub langs: &'static [OracleLang],
    /// Loop node kinds.
    pub loop_kinds: &'static [&'static str],
    /// Function-boundary node kinds.
    pub boundary_kinds: &'static [&'static str],
}

/// The two intent oracles (#541): TS/JS `nested-loop` and `rust-nested-loop`.
pub const INTENTS: &[IntentSpec] = &[
    IntentSpec {
        pattern: "nested-loop",
        langs: &[
            OracleLang::TypeScript,
            OracleLang::Tsx,
            OracleLang::JavaScript,
        ],
        loop_kinds: &[
            "for_statement",
            "for_in_statement",
            "while_statement",
            "do_statement",
        ],
        boundary_kinds: &[
            "function_declaration",
            "function_expression",
            "arrow_function",
            "method_definition",
            "generator_function",
            "generator_function_declaration",
        ],
    },
    IntentSpec {
        pattern: "rust-nested-loop",
        langs: &[OracleLang::Rust],
        loop_kinds: &["for_expression", "while_expression", "loop_expression"],
        boundary_kinds: &["function_item", "closure_expression"],
    },
];

/// The capture an intent's loop query marks each loop with.
const LOOP_CAPTURE: &str = "loop";

// ============================================================================
// The compiled oracle
// ============================================================================

struct CompiledQuery {
    spec: &'static OracleQuery,
    query: Query,
    match_capture: u32,
    filter: Option<(u32, PostFilter)>,
}

struct CompiledIntent {
    spec: &'static IntentSpec,
    lang: OracleLang,
    query: Query,
    loop_capture: u32,
}

/// Every oracle query and intent oracle, compiled once. Cheap to share
/// across threads (`tree_sitter::Query` is `Send + Sync`); each run creates
/// its own parser and cursor.
pub struct StructuralOracle {
    definitions: Vec<CompiledQuery>,
    intents: Vec<CompiledIntent>,
}

/// What the oracle says about one file of the scoreboard universe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileMatches {
    /// Not an oracle language: skim rows for it are unscored, or skim never
    /// AST-indexes it.
    NotScored(LangClass),
    /// An oracle language, but over the AST size cap: outside the AST
    /// universe (skim never indexes it).
    OverSizeCap(OracleLang),
    /// Parsed and queried.
    Scored(FileReport),
}

/// The oracle's answers for one scored file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReport {
    /// The grammar the file was parsed with.
    pub lang: OracleLang,
    /// Definition-oracle match lines per covered pattern of `lang` (an empty
    /// list means "no match", not "not applicable").
    pub definition: BTreeMap<&'static str, Vec<u32>>,
    /// Intent-oracle match lines per intent pattern of `lang`.
    pub intent: BTreeMap<&'static str, Vec<u32>>,
}

impl StructuralOracle {
    /// Compile every registered query and intent oracle.
    ///
    /// # Errors
    ///
    /// A query that does not compile against its grammar, lacks the `@match`
    /// capture or its post-filter capture, or duplicates another
    /// `(pattern, language)` pair; an intent boundary kind the grammar does
    /// not have.
    pub fn new() -> anyhow::Result<Self> {
        let definitions = QUERIES
            .iter()
            .map(compile_definition)
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut registered = BTreeSet::new();
        for q in QUERIES {
            anyhow::ensure!(
                registered.insert((q.pattern, q.lang)),
                "oracle query {} is registered twice",
                q.file_name()
            );
        }
        let intents = INTENTS
            .iter()
            .flat_map(|spec| spec.langs.iter().map(move |&lang| (spec, lang)))
            .map(|(spec, lang)| compile_intent(spec, lang))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(StructuralOracle {
            definitions,
            intents,
        })
    }

    /// Definition-oracle match lines of `pattern` in `source` parsed as
    /// `lang`: the first line (1-based) of every match node, sorted and
    /// de-duplicated.
    ///
    /// # Errors
    ///
    /// No query registered for `(pattern, lang)`; the parser produced no
    /// tree; a query hit tree-sitter's in-progress match limit.
    pub fn match_lines(
        &self,
        pattern: &str,
        lang: OracleLang,
        source: &str,
    ) -> anyhow::Result<Vec<u32>> {
        let compiled = self
            .definitions
            .iter()
            .find(|c| c.spec.pattern == pattern && c.spec.lang == lang)
            .ok_or_else(|| anyhow::anyhow!("no oracle query for {pattern} in {lang}"))?;
        let tree = parse(lang, source)?;
        run_definition(compiled, &tree, source)
    }

    /// Intent-oracle match lines of `pattern` in `source` parsed as `lang`:
    /// the first line of every loop that has a loop ancestor inside the same
    /// function, sorted and de-duplicated.
    ///
    /// # Errors
    ///
    /// No intent oracle for `(pattern, lang)`; the parser produced no tree; a
    /// query hit tree-sitter's in-progress match limit.
    pub fn intent_lines(
        &self,
        pattern: &str,
        lang: OracleLang,
        source: &str,
    ) -> anyhow::Result<Vec<u32>> {
        let compiled = self
            .intents
            .iter()
            .find(|c| c.spec.pattern == pattern && c.lang == lang)
            .ok_or_else(|| anyhow::anyhow!("no intent oracle for {pattern} in {lang}"))?;
        let tree = parse(lang, source)?;
        run_intent(compiled, &tree, source)
    }

    /// Everything the oracle says about one universe file: its class, and for
    /// a scored file every covered pattern's (and intent's) match lines,
    /// parsing the file once.
    ///
    /// # Errors
    ///
    /// The parser produced no tree, or a query hit tree-sitter's in-progress
    /// match limit; the error names `path`.
    pub fn file_matches(&self, path: &str, text: &str) -> anyhow::Result<FileMatches> {
        let lang = match classify(path) {
            LangClass::Oracle(lang) => lang,
            other => return Ok(FileMatches::NotScored(other)),
        };
        if !within_size_cap(u64::try_from(text.len()).unwrap_or(u64::MAX)) {
            return Ok(FileMatches::OverSizeCap(lang));
        }
        let tree = parse(lang, text).with_context(|| format!("structural oracle on {path}"))?;
        let definition = self
            .definitions
            .iter()
            .filter(|c| c.spec.lang == lang)
            .map(|c| Ok((c.spec.pattern, run_definition(c, &tree, text)?)))
            .collect::<anyhow::Result<BTreeMap<_, _>>>()
            .with_context(|| format!("structural oracle on {path}"))?;
        let intent = self
            .intents
            .iter()
            .filter(|c| c.lang == lang)
            .map(|c| Ok((c.spec.pattern, run_intent(c, &tree, text)?)))
            .collect::<anyhow::Result<BTreeMap<_, _>>>()
            .with_context(|| format!("structural oracle on {path}"))?;
        Ok(FileMatches::Scored(FileReport {
            lang,
            definition,
            intent,
        }))
    }
}

fn compile_definition(spec: &'static OracleQuery) -> anyhow::Result<CompiledQuery> {
    let file = spec.file_name();
    let query = Query::new(&spec.lang.grammar(), spec.source)
        .map_err(|e| anyhow::anyhow!("compiling oracle query {file}: {e}"))?;
    let match_capture = query
        .capture_index_for_name(MATCH_CAPTURE)
        .ok_or_else(|| anyhow::anyhow!("oracle query {file} has no @{MATCH_CAPTURE} capture"))?;
    let filter = spec
        .filter
        .map(|f| {
            query
                .capture_index_for_name(f.capture())
                .map(|index| (index, f))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "oracle query {file} has no @{} capture for its post-filter",
                        f.capture()
                    )
                })
        })
        .transpose()?;
    Ok(CompiledQuery {
        spec,
        query,
        match_capture,
        filter,
    })
}

fn compile_intent(spec: &'static IntentSpec, lang: OracleLang) -> anyhow::Result<CompiledIntent> {
    let grammar = lang.grammar();
    if let Some(kind) = spec
        .boundary_kinds
        .iter()
        .find(|k| grammar.id_for_node_kind(k, true) == 0)
    {
        anyhow::bail!(
            "intent oracle {} ({lang}): boundary kind {kind:?} is not a named node of the grammar",
            spec.pattern
        );
    }
    let alternatives: Vec<String> = spec.loop_kinds.iter().map(|k| format!("({k})")).collect();
    let source = format!("[{}] @{LOOP_CAPTURE}", alternatives.join(" "));
    let query = Query::new(&grammar, &source)
        .map_err(|e| anyhow::anyhow!("compiling intent oracle {} ({lang}): {e}", spec.pattern))?;
    let loop_capture = query
        .capture_index_for_name(LOOP_CAPTURE)
        .ok_or_else(|| anyhow::anyhow!("intent oracle {} has no @{LOOP_CAPTURE}", spec.pattern))?;
    Ok(CompiledIntent {
        spec,
        lang,
        query,
        loop_capture,
    })
}

// ============================================================================
// Running
// ============================================================================

/// Parse `source` with `lang`'s grammar using a fresh parser.
///
/// # Errors
///
/// The grammar cannot be loaded, or the parser returns no tree (tree-sitter
/// does so only on cancellation or timeout, neither of which is set here).
pub fn parse(lang: OracleLang, source: &str) -> anyhow::Result<Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&lang.grammar())
        .with_context(|| format!("loading the {lang} grammar"))?;
    parser
        .parse(source, None)
        .ok_or_else(|| anyhow::anyhow!("tree-sitter returned no tree for {lang} source"))
}

fn run_definition(compiled: &CompiledQuery, tree: &Tree, source: &str) -> anyhow::Result<Vec<u32>> {
    let spec = compiled.spec;
    first_lines(
        &compiled.query,
        tree,
        source,
        spec.pattern,
        spec.lang,
        |m| {
            if !filter_accepts(compiled, m)? {
                return Ok(Vec::new());
            }
            Ok(captured(m, compiled.match_capture).collect())
        },
    )
}

fn filter_accepts(compiled: &CompiledQuery, m: &QueryMatch<'_, '_>) -> anyhow::Result<bool> {
    let Some((index, filter)) = compiled.filter else {
        return Ok(true);
    };
    let node = captured(m, index).next().ok_or_else(|| {
        anyhow::anyhow!(
            "oracle query {} matched without its @{} capture",
            compiled.spec.file_name(),
            filter.capture()
        )
    })?;
    Ok(filter.accepts(body_elements(node)))
}

/// The body elements of `node`: named, non-extra children that are not
/// attributes (see [`PostFilter`]).
fn body_elements(node: Node<'_>) -> usize {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| !child.is_extra() && !ATTRIBUTE_KINDS.contains(&child.kind()))
        .count()
}

fn run_intent(compiled: &CompiledIntent, tree: &Tree, source: &str) -> anyhow::Result<Vec<u32>> {
    let spec = compiled.spec;
    first_lines(
        &compiled.query,
        tree,
        source,
        spec.pattern,
        compiled.lang,
        |m| {
            Ok(captured(m, compiled.loop_capture)
                .filter(|&node| has_loop_ancestor_in_same_function(node, spec))
                .collect())
        },
    )
}

/// Run `query` over `tree`: the 1-based first line of every node `select`
/// picks from a match, sorted and de-duplicated. `pattern` and `lang` name
/// the query in errors.
///
/// # Errors
///
/// `select` fails, a line does not fit a `u32`, or the query exceeded
/// tree-sitter's in-progress match limit: it may have dropped matches, which
/// is a harness error, never a silent miss.
fn first_lines<'tree>(
    query: &Query,
    tree: &'tree Tree,
    source: &str,
    pattern: &str,
    lang: OracleLang,
    mut select: impl FnMut(&QueryMatch<'_, 'tree>) -> anyhow::Result<Vec<Node<'tree>>>,
) -> anyhow::Result<Vec<u32>> {
    let mut cursor = QueryCursor::new();
    let mut lines = Vec::new();
    {
        let mut matches = cursor.matches(query, tree.root_node(), source.as_bytes());
        while let Some(m) = matches.next() {
            for node in select(m)? {
                lines.push(first_line(node)?);
            }
        }
    }
    anyhow::ensure!(
        !cursor.did_exceed_match_limit(),
        "oracle query {} exceeded tree-sitter's match limit",
        scm_file_name(pattern, lang)
    );
    lines.sort_unstable();
    lines.dedup();
    Ok(lines)
}

/// The nodes `m` captured under capture `index`, in capture order.
fn captured<'m, 'tree>(
    m: &'m QueryMatch<'_, 'tree>,
    index: u32,
) -> impl Iterator<Item = Node<'tree>> + 'm {
    m.captures
        .iter()
        .filter(move |c| c.index == index)
        .map(|c| c.node)
}

/// Walk `node`'s ancestors (bounded by the tree's depth) up to the nearest
/// function boundary: true iff a loop kind comes first.
fn has_loop_ancestor_in_same_function(node: Node<'_>, spec: &IntentSpec) -> bool {
    std::iter::successors(node.parent(), Node::parent)
        .map(|ancestor| ancestor.kind())
        .find(|kind| spec.loop_kinds.contains(kind) || spec.boundary_kinds.contains(kind))
        .is_some_and(|kind| spec.loop_kinds.contains(&kind))
}

/// The 1-based first line of `node`.
fn first_line(node: Node<'_>) -> anyhow::Result<u32> {
    let row = node.start_position().row;
    row.checked_add(1)
        .and_then(|line| u32::try_from(line).ok())
        .ok_or_else(|| anyhow::anyhow!("match on row {row} does not fit a u32 line number"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code — unwrap/expect/panic acceptable for test assertions
#[path = "structural_tests.rs"]
mod tests;
