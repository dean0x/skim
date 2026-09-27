//! Structural oracle for `skim search --ast <pattern>` (#541): hand-written
//! tree-sitter queries that encode each catalog pattern's documented
//! description, run over the real grammars.
//!
//! # Independence
//!
//! This crate depends on no `rskim-*` crate (`tests/independence.rs` checks
//! every dependency table of its manifest), so nothing from skim's AST search
//! stack — linearization, n-grams, the query engine, re-parse verification —
//! can reach an answer. Not even skim's pattern catalog is an input: the
//! oracle knows its own registry by pattern NAME ([`query_sources`],
//! [`UNCOVERED`], [`coverage_of`]), and the scoreboard (`rskim-bench`) crosses
//! it with the catalog it reads, so a catalog pattern with no oracle query is
//! listed as uncovered instead of silently skipped. Where the oracle must
//! agree with skim (the extension table, the AST-indexed language list, the
//! size cap) it keeps its OWN copy with a citation, so a policy change on
//! skim's side shows up as a scoreboard diff (the `oracle.rs` / `universe.rs`
//! convention in `rskim-bench`, whose tests also check the extension table
//! against the lexical oracle's copy, [`extension_classes`]).
//!
//! # Ground truth
//!
//! Each `(pattern, language)` pair has one query file,
//! `crates/rskim-oracle/queries/<pattern>.<lang>.scm`, compiled in with
//! `include_str!` and hashed into the scoreboard's golden digest
//! ([`fingerprint`], with every other table an answer depends on), so editing
//! a query forces a re-bless. Each query file's second line,
//! `; Grammar: <crate> <version>`, names the grammar it was written against,
//! and a test keeps it equal to the version the workspace `Cargo.lock`
//! resolves: a grammar bump fails that test until the headers are edited,
//! and the edit changes the digest, so the gate asks for a bless.
//! `.tsx` files are parsed with the TSX grammar
//! (ADR-003: the oracle is the real grammar), although skim parses them with
//! the plain TypeScript grammar.
//!
//! Every query names one `@match` capture; the match line is the first
//! (1-based) line of that node. Four patterns (six queries) carry a small
//! post-filter ([`PostFilter`]) because their catalog description states a count
//! (`empty-catch`, `empty-function`: zero body elements; `god-function`:
//! at least 20; `excessive-params`: at least 5 parameters).
//!
//! Next to the definition oracle, the intent oracles ([`INTENTS`], reported
//! in [`FileReport::intent`]) answer the intent of the two nested-loop
//! patterns: a loop with a loop ancestor inside the same function, found in
//! one pass over the tree.
//!
//! [`StructuralOracle::file_matches`] parses a file once and answers every
//! pattern of its language; the parser and query cursor come from an
//! [`OracleScratch`] the caller keeps per worker thread. A query that needs
//! more in-progress matches than [`ORACLE_MATCH_LIMIT`] is an error, never a
//! partial answer.
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
use tree_sitter::{
    Node, Parser, Query, QueryCursor, QueryMatch, StreamingIterator, Tree, TreeCursor,
};

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
    fn counts_toward_size_cap(self) -> bool {
        match self {
            LangClass::Oracle(_) | LangClass::Unscored { .. } => true,
            LangClass::NotIndexed { size_capped, .. } => size_capped,
        }
    }
}

/// One row of the oracle's extension table.
#[derive(Debug, Clone, Copy)]
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

/// The class of a file whose extension is not in [`EXT_CLASSES`] (or that
/// has none): skim does not know the language, so it never AST-indexes the
/// file nor counts it toward the size cap.
const UNKNOWN_EXTENSION: LangClass = LangClass::NotIndexed {
    language: None,
    size_capped: false,
};

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
    .unwrap_or(UNKNOWN_EXTENSION)
}

/// Every extension of the oracle's extension table with its class, in table
/// order (an extension not listed is [`classify`]'s unknown-extension class).
/// `rskim-bench` checks it against the lexical oracle's copy of the same skim
/// table.
pub fn extension_classes() -> impl Iterator<Item = (&'static str, LangClass)> {
    EXT_CLASSES
        .iter()
        .flat_map(|row| row.extensions.iter().map(move |&ext| (ext, row.class)))
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
fn within_size_cap(len: u64) -> bool {
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
enum PostFilter {
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

    /// The filter as [`fingerprint`] tokens: `empty @<capture>` or
    /// `at-least <min> @<capture>`.
    fn fingerprint(self) -> String {
        match self {
            PostFilter::Empty { capture } => format!("empty @{capture}"),
            PostFilter::AtLeast { capture, min } => format!("at-least {min} @{capture}"),
        }
    }
}

/// Node kinds that annotate the next element instead of being one.
const ATTRIBUTE_KINDS: &[&str] = &["attribute_item", "inner_attribute_item"];

/// The capture every query marks its match node with.
const MATCH_CAPTURE: &str = "match";

/// One registered oracle query.
#[derive(Debug, Clone, Copy)]
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
/// `queries/<pattern>.<lang>.scm`; a unit test checks the file name matches
/// [`OracleLang::as_str`] and that every file is registered.
macro_rules! oracle_query {
    ($pattern:literal, $lang:ident, $file_lang:literal) => {
        oracle_query!($pattern, $lang, $file_lang, None)
    };
    ($pattern:literal, $lang:ident, $file_lang:literal, $filter:expr) => {
        OracleQuery {
            pattern: $pattern,
            lang: OracleLang::$lang,
            source: include_str!(concat!("../queries/", $pattern, ".", $file_lang, ".scm")),
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

/// Catalog patterns the oracle deliberately does not cover, as `(pattern
/// name, reason)`. They stay under ADR-007 manual dog-food.
pub const UNCOVERED: &[(&str, &str)] = &[
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
/// listed in [`UNCOVERED`] — a pattern added to skim's catalog after this
/// oracle was written. A test in `rskim-bench` keeps it from happening
/// silently.
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

/// `queries` ordered by `(pattern, language name)`.
fn sorted_queries(queries: &[OracleQuery]) -> Vec<&OracleQuery> {
    let mut sorted: Vec<&OracleQuery> = queries.iter().collect();
    sorted.sort_by_key(|q| (q.pattern, q.lang.as_str()));
    sorted
}

/// Every registered query, ordered by `(pattern, language name)`.
pub fn query_sources() -> Vec<QuerySource> {
    sorted_queries(QUERIES)
        .into_iter()
        .map(|q| QuerySource {
            pattern: q.pattern,
            lang: q.lang,
            source: q.source,
        })
        .collect()
}

/// The tables the oracle's answers depend on: what [`fingerprint`] renders,
/// through this type's `Display`. The grammars are not a table here; each
/// query file's `; Grammar:` header carries their identity (module docs,
/// "Ground truth"). Factored out so a test can render an edited copy.
struct OracleInputs<'a> {
    /// [`AST_SIZE_CAP_BYTES`].
    size_cap: u64,
    /// [`EXT_CLASSES`].
    ext_classes: &'a [ExtClass],
    /// [`UNKNOWN_EXTENSION`].
    unknown_extension: LangClass,
    /// [`ATTRIBUTE_KINDS`].
    attribute_kinds: &'a [&'a str],
    /// [`QUERIES`].
    queries: &'a [OracleQuery],
    /// [`INTENTS`].
    intents: &'a [IntentSpec],
}

/// The inputs compiled into the scoreboard.
const ORACLE_INPUTS: OracleInputs<'static> = OracleInputs {
    size_cap: AST_SIZE_CAP_BYTES,
    ext_classes: EXT_CLASSES,
    unknown_extension: UNKNOWN_EXTENSION,
    attribute_kinds: ATTRIBUTE_KINDS,
    queries: QUERIES,
    intents: INTENTS,
};

/// A canonical rendering of every table the oracle's answers depend on: the
/// size cap; the extension table ([`classify`]: every row's extensions and
/// class, in table order, and the class of an unknown extension); the
/// attribute kinds [`PostFilter`]'s body-element count skips; every
/// registered query (file name, post-filter, full text), ordered by pattern
/// and language name; and every intent spec. Each is written out explicitly
/// (never through `Debug`), so the text is stable across toolchains. The
/// golden digest folds it in (`rskim-bench`'s
/// `golden::structural_oracle_sha256`), so an edit
/// to any of them — a query, a post-filter threshold, an intent's node kinds,
/// a file's language class or an attribute kind — forces a re-bless.
///
/// The text names query files, never directories, so moving the queries does
/// not change it.
pub fn fingerprint() -> String {
    ORACLE_INPUTS.to_string()
}

/// The [`fingerprint`] text of these inputs, one line per table row (a
/// query's line is followed by its full text).
impl std::fmt::Display for OracleInputs<'_> {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(out, "size-cap {}", self.size_cap)?;
        for row in self.ext_classes {
            writeln!(
                out,
                "ext [{}] {}",
                row.extensions.join(" "),
                row.class.fingerprint()
            )?;
        }
        writeln!(out, "ext-unknown {}", self.unknown_extension.fingerprint())?;
        writeln!(out, "attribute-kinds [{}]", self.attribute_kinds.join(" "))?;
        for q in sorted_queries(self.queries) {
            let filter = q
                .filter
                .map_or_else(|| "none".to_string(), PostFilter::fingerprint);
            writeln!(
                out,
                "query {} filter {filter} bytes {}\n{}",
                q.file_name(),
                q.source.len(),
                q.source
            )?;
        }
        for spec in self.intents {
            let langs: Vec<&str> = spec.langs.iter().map(|l| l.as_str()).collect();
            writeln!(
                out,
                "intent {} langs [{}] loops [{}] boundaries [{}]",
                spec.pattern,
                langs.join(" "),
                spec.loop_kinds.join(" "),
                spec.boundary_kinds.join(" ")
            )?;
        }
        Ok(())
    }
}

impl LangClass {
    /// The class as [`fingerprint`] tokens.
    fn fingerprint(self) -> String {
        match self {
            LangClass::Oracle(lang) => format!("oracle {lang}"),
            LangClass::Unscored { language } => format!("unscored {language}"),
            LangClass::NotIndexed {
                language,
                size_capped,
            } => format!(
                "not-indexed {} size-capped {size_capped}",
                language.unwrap_or("-")
            ),
        }
    }
}

// ============================================================================
// Coverage by pattern name
// ============================================================================

/// Whether the oracle covers a pattern name, and why not if it does not.
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

/// The oracle's coverage of `pattern`, from its own registry: the languages
/// with a query, or else the [`UNCOVERED`] reason, or else
/// [`UNCLASSIFIED_REASON`] (a name the oracle has no record of — a catalog
/// pattern newer than the oracle).
pub fn coverage_of(pattern: &str) -> PatternCoverage {
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

// ============================================================================
// The compiled oracle
// ============================================================================

/// The most in-progress matches one definition query may hold at once
/// (`QueryCursor::set_match_limit`, whose contract is `0 < limit <= 65536`).
/// A tree-sitter query cursor otherwise has no limit of its own, and its
/// capture-list pool misbehaves past 65,535 lists (a `u16` id wraps), so the
/// bound is explicit and inside the contract. A query holds about one
/// in-progress match per enclosing node its pattern has started at and not
/// yet finished, so the need grows with nesting depth. Over the four
/// scoreboard corpora at their #541 pins no query ever needs more than 3
/// (the smallest limit none of their 1,181 scored files trips), so 4096
/// leaves three orders of magnitude of headroom for deeper code while
/// staying well inside the contract. Past the limit tree-sitter drops
/// matches, so exceeding it is a harness error naming the query
/// ([`StructuralOracle::file_matches`]), never a silent miss. It is not in
/// [`fingerprint`]: it can turn an answer into an error, never change one.
///
/// Each [`OracleScratch`] fixes the limit when it is made, and nothing
/// changes it: tree-sitter enforces the limit by capping how many capture
/// lists a cursor's pool ever allocates and reuses freed ones first, so a
/// limit lowered on a cursor whose pool had already grown would not bound
/// the lists it holds.
const ORACLE_MATCH_LIMIT: u32 = 4096;

/// The largest match limit tree-sitter's contract allows.
const MAX_MATCH_LIMIT: u32 = 65_536;

const _: () = assert!(ORACLE_MATCH_LIMIT >= 1 && ORACLE_MATCH_LIMIT <= MAX_MATCH_LIMIT);

struct CompiledQuery {
    spec: &'static OracleQuery,
    /// `<pattern>.<lang>.scm`, rendered once for errors.
    file_name: String,
    query: Query,
    match_capture: u32,
    filter: Option<(u32, PostFilter)>,
}

/// An intent oracle whose node kinds its grammar has.
struct CompiledIntent {
    spec: &'static IntentSpec,
    lang: OracleLang,
}

/// Every oracle query and intent oracle, compiled once. Cheap to share
/// across threads (`tree_sitter::Query` is `Send + Sync`); the parser and
/// query cursor a run needs live in an [`OracleScratch`], one per worker.
pub struct StructuralOracle {
    definitions: Vec<CompiledQuery>,
    intents: Vec<CompiledIntent>,
}

/// The parser and query cursor [`StructuralOracle::file_matches`] works
/// with, kept from one file to the next so a run allocates them once per
/// worker rather than once per file (or per query). It holds no answer: any
/// scratch gives a file the same result. Make one per thread (for example
/// with rayon's `map_init`).
pub struct OracleScratch {
    parser: Parser,
    /// Its match limit is fixed at creation ([`ORACLE_MATCH_LIMIT`]).
    cursor: QueryCursor,
}

impl OracleScratch {
    /// A fresh parser, and a query cursor limited to
    /// [`ORACLE_MATCH_LIMIT`] in-progress matches.
    pub fn new() -> Self {
        Self::limited(ORACLE_MATCH_LIMIT)
    }

    /// [`OracleScratch::new`] with another match limit (the tests force the
    /// limit's error path with it).
    ///
    /// # Errors
    ///
    /// A limit outside tree-sitter's `1..=65536`.
    #[cfg(test)]
    fn with_match_limit(match_limit: u32) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (1..=MAX_MATCH_LIMIT).contains(&match_limit),
            "match limit {match_limit} is outside tree-sitter's 1..={MAX_MATCH_LIMIT}"
        );
        Ok(Self::limited(match_limit))
    }

    /// A scratch whose cursor holds at most `match_limit` in-progress
    /// matches, a value inside tree-sitter's contract.
    fn limited(match_limit: u32) -> Self {
        let mut cursor = QueryCursor::new();
        cursor.set_match_limit(match_limit);
        OracleScratch {
            parser: Parser::new(),
            cursor,
        }
    }

    /// Parse `source` with `lang`'s grammar.
    ///
    /// # Errors
    ///
    /// The grammar cannot be loaded, or the parser returns no tree
    /// (tree-sitter does so only on cancellation or timeout, neither of
    /// which is set here).
    fn parse(&mut self, lang: OracleLang, source: &str) -> anyhow::Result<Tree> {
        self.parser
            .set_language(&lang.grammar())
            .with_context(|| format!("loading the {lang} grammar"))?;
        self.parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("tree-sitter returned no tree for {lang} source"))
    }
}

impl Default for OracleScratch {
    fn default() -> Self {
        Self::new()
    }
}

/// One parsed file: its tree, its text, and a tree cursor over it that
/// every post-filter's body-element count reuses.
struct ParsedFile<'t> {
    tree: &'t Tree,
    source: &'t str,
    walk: TreeCursor<'t>,
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
    /// `(pattern, language)` pair; an intent loop or boundary kind the
    /// grammar does not have.
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

    /// Everything the oracle says about one universe file: its class, and for
    /// a scored file every covered pattern's (and intent's) match lines,
    /// parsing the file once with `scratch`'s parser.
    ///
    /// # Errors
    ///
    /// The parser produced no tree, or a definition query needed more
    /// in-progress matches than `scratch`'s match limit
    /// ([`ORACLE_MATCH_LIMIT`]), so tree-sitter dropped some; the error names
    /// `path` and the query file.
    pub fn file_matches(
        &self,
        scratch: &mut OracleScratch,
        path: &str,
        text: &str,
    ) -> anyhow::Result<FileMatches> {
        let lang = match classify(path) {
            LangClass::Oracle(lang) => lang,
            other => return Ok(FileMatches::NotScored(other)),
        };
        if !within_size_cap(u64::try_from(text.len()).unwrap_or(u64::MAX)) {
            return Ok(FileMatches::OverSizeCap(lang));
        }
        self.report(scratch, lang, text)
            .map(FileMatches::Scored)
            .with_context(|| format!("structural oracle on {path}"))
    }

    /// Parse `source` as `lang` once, then answer every definition query and
    /// intent oracle of `lang` over the tree.
    fn report(
        &self,
        scratch: &mut OracleScratch,
        lang: OracleLang,
        source: &str,
    ) -> anyhow::Result<FileReport> {
        let tree = scratch.parse(lang, source)?;
        let mut file = ParsedFile {
            tree: &tree,
            source,
            walk: tree.walk(),
        };
        let mut definition = BTreeMap::new();
        for compiled in self.definitions.iter().filter(|c| c.spec.lang == lang) {
            let lines = Self::definition_lines(compiled, &mut scratch.cursor, &mut file)?;
            definition.insert(compiled.spec.pattern, lines);
        }
        let intent = self
            .intents
            .iter()
            .filter(|c| c.lang == lang)
            .map(|c| Ok((c.spec.pattern, nested_loop_lines(c.spec, &tree)?)))
            .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
        Ok(FileReport {
            lang,
            definition,
            intent,
        })
    }

    /// Run the definition query `compiled` over `file` with `cursor`: the
    /// 1-based first line of every `@match` node of a match its post-filter
    /// accepts, sorted and de-duplicated.
    ///
    /// # Errors
    ///
    /// A match lacks its post-filter capture, a line does not fit a `u32`,
    /// or the query exceeded the cursor's match limit: tree-sitter may then
    /// have dropped matches, which is a harness error, never a silent miss.
    fn definition_lines(
        compiled: &CompiledQuery,
        cursor: &mut QueryCursor,
        file: &mut ParsedFile<'_>,
    ) -> anyhow::Result<Vec<u32>> {
        let (tree, source) = (file.tree, file.source);
        let mut lines = Vec::new();
        {
            let mut matches = cursor.matches(&compiled.query, tree.root_node(), source.as_bytes());
            while let Some(m) = matches.next() {
                if !filter_accepts(compiled, m, &mut file.walk)? {
                    continue;
                }
                for node in captured(m, compiled.match_capture) {
                    lines.push(first_line(node)?);
                }
            }
        }
        anyhow::ensure!(
            !cursor.did_exceed_match_limit(),
            "oracle query {} needed more than the oracle's match limit of {} in-progress matches, \
             so tree-sitter dropped matches",
            compiled.file_name,
            cursor.match_limit()
        );
        lines.sort_unstable();
        lines.dedup();
        Ok(lines)
    }
}

/// Re-parsing lookups by pattern, for tests: the scoreboard calls
/// [`StructuralOracle::file_matches`], which parses a file once for every
/// pattern.
#[cfg(test)]
impl StructuralOracle {
    /// Definition-oracle match lines of `pattern` in `source` parsed as
    /// `lang`, with `scratch`'s parser and cursor.
    ///
    /// # Errors
    ///
    /// No query registered for `(pattern, lang)`, or as
    /// [`StructuralOracle::file_matches`].
    fn match_lines(
        &self,
        scratch: &mut OracleScratch,
        pattern: &str,
        lang: OracleLang,
        source: &str,
    ) -> anyhow::Result<Vec<u32>> {
        let compiled = self
            .definitions
            .iter()
            .find(|c| c.spec.pattern == pattern && c.spec.lang == lang)
            .ok_or_else(|| anyhow::anyhow!("no oracle query for {pattern} in {lang}"))?;
        let tree = scratch.parse(lang, source)?;
        let mut file = ParsedFile {
            tree: &tree,
            source,
            walk: tree.walk(),
        };
        Self::definition_lines(compiled, &mut scratch.cursor, &mut file)
    }

    /// Intent-oracle match lines of `pattern` in `source` parsed as `lang`.
    ///
    /// # Errors
    ///
    /// No intent oracle for `(pattern, lang)`, or as
    /// [`StructuralOracle::file_matches`].
    fn intent_lines(
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
        let tree = OracleScratch::new().parse(lang, source)?;
        nested_loop_lines(compiled.spec, &tree)
    }
}

fn compile_definition(spec: &'static OracleQuery) -> anyhow::Result<CompiledQuery> {
    let file_name = spec.file_name();
    let query = Query::new(&spec.lang.grammar(), spec.source)
        .with_context(|| format!("compiling oracle query {file_name}"))?;
    let match_capture = query.capture_index_for_name(MATCH_CAPTURE).ok_or_else(|| {
        anyhow::anyhow!("oracle query {file_name} has no @{MATCH_CAPTURE} capture")
    })?;
    let filter = spec
        .filter
        .map(|f| {
            query
                .capture_index_for_name(f.capture())
                .map(|index| (index, f))
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "oracle query {file_name} has no @{} capture for its post-filter",
                        f.capture()
                    )
                })
        })
        .transpose()?;
    Ok(CompiledQuery {
        spec,
        file_name,
        query,
        match_capture,
        filter,
    })
}

fn compile_intent(spec: &'static IntentSpec, lang: OracleLang) -> anyhow::Result<CompiledIntent> {
    let grammar = lang.grammar();
    for (role, kinds) in [("loop", spec.loop_kinds), ("boundary", spec.boundary_kinds)] {
        if let Some(kind) = kinds
            .iter()
            .find(|k| grammar.id_for_node_kind(k, true) == 0)
        {
            anyhow::bail!(
                "intent oracle {} ({lang}): {role} kind {kind:?} is not a named node of the grammar",
                spec.pattern
            );
        }
    }
    Ok(CompiledIntent { spec, lang })
}

// ============================================================================
// Running
// ============================================================================

fn filter_accepts<'t>(
    compiled: &CompiledQuery,
    m: &QueryMatch<'_, 't>,
    walk: &mut TreeCursor<'t>,
) -> anyhow::Result<bool> {
    let Some((index, filter)) = compiled.filter else {
        return Ok(true);
    };
    let node = captured(m, index).next().ok_or_else(|| {
        anyhow::anyhow!(
            "oracle query {} matched without its @{} capture",
            compiled.file_name,
            filter.capture()
        )
    })?;
    Ok(filter.accepts(body_elements(node, walk)))
}

/// The body elements of `node`: named, non-extra children that are not
/// attributes (see [`PostFilter`]), counted with `walk` (any cursor over
/// `node`'s tree; it is reset to `node`).
fn body_elements<'t>(node: Node<'t>, walk: &mut TreeCursor<'t>) -> usize {
    node.named_children(walk)
        .filter(|child| !child.is_extra() && !ATTRIBUTE_KINDS.contains(&child.kind()))
        .count()
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

/// The intent oracle `spec` over `tree`: the 1-based first line of every
/// loop whose nearest loop-or-boundary ancestor is a loop, sorted and
/// de-duplicated. One pre-order pass with a tree cursor carries the "inside
/// a loop of this function" flag down the tree, so the cost is linear in the
/// tree's size however deeply it nests (walking each loop's ancestors with
/// `Node::parent`, which re-descends from the root at every step, is
/// quadratic in the depth).
///
/// # Errors
///
/// A line does not fit a `u32`, or the walk does not end within the tree's
/// node count (a tree-sitter invariant: a cursor enters every node once).
fn nested_loop_lines(spec: &IntentSpec, tree: &Tree) -> anyhow::Result<Vec<u32>> {
    let root = tree.root_node();
    let nodes = root.descendant_count();
    let mut cursor = root.walk();
    // Whether the nearest loop-or-boundary strict ancestor of the cursor's
    // node is a loop; `above` holds the same flag for every node on the path
    // down to it, restored on the way back up.
    let mut in_loop = false;
    let mut above: Vec<bool> = Vec::new();
    let mut lines = Vec::new();
    for _ in 0..nodes {
        let node = cursor.node();
        let kind = node.kind();
        let is_loop = spec.loop_kinds.contains(&kind);
        if is_loop && in_loop && node.is_named() {
            lines.push(first_line(node)?);
        }
        if cursor.goto_first_child() {
            above.push(in_loop);
            in_loop = is_loop || (in_loop && !spec.boundary_kinds.contains(&kind));
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                lines.sort_unstable();
                lines.dedup();
                return Ok(lines);
            }
            in_loop = above
                .pop()
                .context("the tree walk rose above the node it entered")?;
        }
    }
    anyhow::bail!("the tree walk did not end within the tree's {nodes} nodes")
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
