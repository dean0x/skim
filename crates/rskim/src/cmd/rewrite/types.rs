//! Type definitions for the rewrite engine.

use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum RewriteCategory {
    Test,
    Build,
    Git,
    Read,
    Lint,
    Pkg,
    Infra,
    FileOps,
}

pub(super) struct RewriteRule {
    pub(super) prefix: &'static [&'static str],
    pub(super) rewrite_to: &'static [&'static str],
    pub(super) skip_if_flag_prefix: &'static [&'static str],
    pub(super) category: RewriteCategory,
}

#[derive(Debug)]
pub(super) struct RewriteResult {
    pub(super) tokens: Vec<String>,
    pub(super) category: RewriteCategory,
}

// ---- Compound command types (#45) ----

/// Result of splitting a shell command string at compound operators.
#[derive(Debug)]
pub(super) enum CompoundSplitResult {
    /// No compound operators found — treat as a simple command.
    Simple(Vec<String>),
    /// Found compound operators — segments separated by `&&`, `||`, `;`, `|`.
    Compound(Vec<CommandSegment>),
    /// Unsupported shell syntax (heredocs, subshells, backticks, unmatched quotes).
    Bail,
}

/// A single command within a compound expression.
#[derive(Debug)]
pub(super) struct CommandSegment {
    pub(super) tokens: Vec<String>,
    pub(super) trailing_operator: Option<CompoundOp>,
}

/// Shell compound operators.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum CompoundOp {
    And,       // &&
    Or,        // ||
    Semicolon, // ;
    Pipe,      // |
}

impl CompoundOp {
    pub(super) fn as_str(self) -> &'static str {
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
pub(super) enum QuoteState {
    None,
    SingleQuote,
    DoubleQuote,
}

#[derive(Serialize)]
pub(super) struct SuggestOutput<'a> {
    pub(super) version: u8,
    #[serde(rename = "match")]
    pub(super) is_match: bool,
    pub(super) original: &'a str,
    pub(super) rewritten: &'a str,
    #[serde(serialize_with = "serialize_category")]
    pub(super) category: Option<RewriteCategory>,
    pub(super) confidence: &'a str,
    pub(super) compound: bool,
    pub(super) skim_hook_version: &'a str,
}

pub(super) fn serialize_category<S: serde::Serializer>(
    cat: &Option<RewriteCategory>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match cat {
        Some(c) => c.serialize(serializer),
        None => serializer.serialize_str(""),
    }
}
