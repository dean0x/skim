//! Type definitions for the rewrite engine.

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RewriteCategory {
    Test,
    Build,
    Git,
    Read,
    Lint,
    Pkg,
    Infra,
    FileOps,
}

pub(crate) struct RewriteRule {
    pub(crate) prefix: &'static [&'static str],
    pub(crate) rewrite_to: &'static [&'static str],
    pub(crate) skip_if_flag_prefix: &'static [&'static str],
    pub(crate) category: RewriteCategory,
}

#[derive(Debug)]
pub(crate) struct RewriteResult {
    pub(crate) tokens: Vec<String>,
    pub(crate) category: RewriteCategory,
}

// ---- Compound command types (#45) ----

/// Result of splitting a shell command string at compound operators.
#[derive(Debug)]
pub(crate) enum CompoundSplitResult {
    /// No compound operators found — treat as a simple command.
    Simple(Vec<String>),
    /// Found compound operators — segments separated by `&&`, `||`, `;`, `|`.
    Compound(Vec<CommandSegment>),
    /// Unsupported shell syntax (heredocs, subshells, backticks, unmatched quotes).
    Bail,
}

/// A single command within a compound expression.
#[derive(Debug)]
pub(crate) struct CommandSegment {
    pub(crate) tokens: Vec<String>,
    pub(crate) trailing_operator: Option<CompoundOp>,
}

/// Shell compound operators.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CompoundOp {
    And,       // &&
    Or,        // ||
    Semicolon, // ;
    Pipe,      // |
}

impl CompoundOp {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            CompoundOp::And => "&&",
            CompoundOp::Or => "||",
            CompoundOp::Semicolon => ";",
            CompoundOp::Pipe => "|",
        }
    }
}

/// Quote-tracking state for the compound splitter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum QuoteState {
    None,
    SingleQuote,
    DoubleQuote,
}

#[derive(Serialize)]
pub(crate) struct SuggestOutput<'a> {
    pub(crate) version: u8,
    #[serde(rename = "match")]
    pub(crate) is_match: bool,
    pub(crate) original: &'a str,
    pub(crate) rewritten: &'a str,
    #[serde(serialize_with = "serialize_category")]
    pub(crate) category: Option<RewriteCategory>,
    pub(crate) confidence: &'a str,
    pub(crate) compound: bool,
    pub(crate) skim_hook_version: &'a str,
}

pub(crate) fn serialize_category<S: serde::Serializer>(
    cat: &Option<RewriteCategory>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match cat {
        Some(c) => c.serialize(serializer),
        None => serializer.serialize_str(""),
    }
}
