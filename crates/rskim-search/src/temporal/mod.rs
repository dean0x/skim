//! Temporal git history parsing and risk scoring for downstream ranking signals.
//!
//! # Architecture
//!
//! - [`GixSource`]: pure gix-based implementation of [`TemporalSource`].
//! - [`is_fix_commit`]: standalone fix-classification predicate.
//! - [`decay_weight`] / [`compute_file_risk_scores`]: exponential-decay hotspot
//!   and bug-fix density metrics (pure, no I/O).
//!
//! No I/O outside of [`GixSource::parse_history`]. All gix types are converted
//! to shared [`CommitInfo`]/[`FileChangeInfo`] types at the parser boundary.

mod git_parser;
mod scoring;
pub mod storage;

pub use git_parser::GixSource;
pub use scoring::{
    DEFAULT_HALF_LIFE_DAYS, compute_file_risk_scores, compute_file_temporal_stats, decay_weight,
    risk_score_wilson_decay, wilson_lower_bound,
};

use regex::Regex;

/// The fix keywords. A subject word matches one when it is equal to it after
/// its leading and trailing punctuation is removed, in any letter case.
const FIX_KEYWORDS: [&str; 5] = ["fix", "bug", "hotfix", "patch", "revert"];

/// Most `[tag]` groups read at the start of a subject.
const MAX_SUBJECT_TAGS: usize = 4;

/// A conventional-commit type prefix: `type:`, `type(scope):`, `type!:` or
/// `type(scope)!:`. Group 1 is the type. A type can hold `/` and `.`, so a
/// path-style prefix (`net/http:`, `mm/slab:`) is a type too.
///
/// Compiled once and reused via `std::sync::LazyLock`.
#[allow(clippy::expect_used)] // hardcoded pattern is always valid
static TYPE_PREFIX: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"^([A-Za-z][A-Za-z0-9_./-]*)(?:\([^)]*\))?!?:").expect("valid regex")
});

/// True when `word` is a fix keyword (see [`FIX_KEYWORDS`]).
fn is_fix_keyword(word: &str) -> bool {
    let word = word.trim_matches(|c: char| !c.is_alphanumeric());
    FIX_KEYWORDS.iter().any(|k| word.eq_ignore_ascii_case(k))
}

/// Returns `true` when a commit subject says that the commit fixes something.
///
/// The keywords are `fix`, `bug`, `hotfix`, `patch` and `revert`, in any
/// letter case. A subject is a fix when one of these holds:
///
/// - a leading `[tag]` is a keyword (`[fix] crash on load`);
/// - the conventional-commit type is a keyword (`fix: …`, `fix(scope): …`,
///   `fix!: …`);
/// - the first word of the subject, after any `[tag]` groups and type
///   prefix, is a keyword (`Fix typo`, `Revert "…"`, `docs: fix link`).
///
/// A keyword elsewhere in the subject does not count: `docs: explain
/// later-fix proximity` and `feat: add bug report template` are not fixes.
/// `skim heatmap` fix risk and `skim search --risky` both use this rule.
///
/// # Examples
///
/// ```rust
/// use rskim_search::is_fix_commit;
///
/// assert!(is_fix_commit("fix: null pointer dereference"));
/// assert!(is_fix_commit("fix(parser): handle empty input"));
/// assert!(is_fix_commit("Revert \"bad change\""));
/// assert!(!is_fix_commit("add feature X"));
/// assert!(!is_fix_commit("feat: add bug report template"));
/// assert!(!is_fix_commit("prefix_word"));
/// ```
#[must_use]
pub fn is_fix_commit(subject: &str) -> bool {
    let mut rest = subject.trim_start();
    for _ in 0..MAX_SUBJECT_TAGS {
        let Some((tag, after)) = rest
            .strip_prefix('[')
            .and_then(|inner| inner.split_once(']'))
        else {
            break;
        };
        if is_fix_keyword(tag) {
            return true;
        }
        rest = after.trim_start();
    }
    if let Some(caps) = TYPE_PREFIX.captures(rest) {
        if caps.get(1).is_some_and(|ty| is_fix_keyword(ty.as_str())) {
            return true;
        }
        let prefix_len = caps.get(0).map_or(0, |m| m.end());
        rest = rest.get(prefix_len..).unwrap_or_default();
    }
    rest.split_whitespace().next().is_some_and(is_fix_keyword)
}
