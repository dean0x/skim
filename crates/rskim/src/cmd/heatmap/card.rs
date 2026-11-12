//! The per-file "before you edit" card of `skim heatmap <file>`.
//!
//! Pure functions, no I/O. [`normalise_target`] maps a file argument to a
//! repo-relative path by text only, [`history_paths`] gives each target in
//! the path form of git's numstat output, and [`build_cards`] builds one card
//! per target from the commits and the metrics of the window. [`sanitize`]
//! runs once on each card string that comes from the repository, so the text
//! and the JSON of a card agree.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;

use super::metrics::without_quotes;
use super::noise;
use super::types::{CommitInfo, Count, CouplingEntry, FileMetrics, HeatmapResult};
use super::view::{ListCaps, Section};
use super::window::format_epoch;

/// Chars of a commit subject, an author name or a partner path on a card.
pub(crate) const CARD_TEXT_MAX_CHARS: usize = 72;

/// The end of a string that [`sanitize`] cut. It counts in the limit.
const CUT_MARK: &str = "...";

/// Chars of a short commit hash.
const SHORT_SHA_CHARS: usize = 7;

/// Distinct authors that a file needs before its card shows owners.
const MIN_OWNERS: usize = 2;

/// A co-change partner on a card.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Partner {
    pub(crate) path: String,
    /// Confidence of the pair, seen from the card's file (`0.0..=1.0`).
    pub(crate) confidence: f64,
    /// Commits that touch both files.
    pub(crate) support: usize,
    /// True for a test file. Test partners come first.
    pub(crate) test: bool,
}

/// A fix commit that touched the card's file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FixCommit {
    /// The first 7 chars of the commit hash.
    pub(crate) sha: String,
    /// The author date, `YYYY-MM-DD` (UTC).
    pub(crate) date: String,
    pub(crate) subject: String,
}

/// An author of the card's file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Owner {
    pub(crate) name: String,
    /// Commits of the file by this author.
    pub(crate) commits: usize,
    /// Share of the file's commits, `0.0..=100.0`.
    pub(crate) share_pct: f64,
}

/// The [`Count`] of each list on a card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct CardCounts {
    pub(crate) owners: Count,
    pub(crate) partners: Count,
    pub(crate) fix_commits: Count,
}

/// One card.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Card {
    pub(crate) path: String,
    /// False when no commit of the window touches the file.
    pub(crate) in_window: bool,
    /// Commits of the window that touch the file.
    pub(crate) commits: usize,
    pub(crate) counts: CardCounts,
    pub(crate) owners: Vec<Owner>,
    pub(crate) partners: Vec<Partner>,
    pub(crate) fix_commits: Vec<FixCommit>,
}

impl Card {
    /// The card of a file that no commit of the window touches.
    fn not_in_window(path: String) -> Self {
        let none = Count { total: 0, shown: 0 };
        Self {
            path,
            in_window: false,
            commits: 0,
            counts: CardCounts {
                owners: none,
                partners: none,
                fix_commits: none,
            },
            owners: Vec::new(),
            partners: Vec::new(),
            fix_commits: Vec::new(),
        }
    }
}

// ============================================================================
// Path arguments
// ============================================================================

/// Map the file argument `arg` to a repo-relative path, by text only.
///
/// An absolute `arg` loses the `repo_root` prefix (the root as git reports
/// it); a relative `arg` gets the cwd `prefix` from
/// `git rev-parse --show-prefix` in front. Then `.`, `..` and repeated `/`
/// fold. There is no file system call: the file is never opened, and a
/// symlink in the path is not followed, so an absolute path through a
/// symlinked folder is refused. The shared containment check is the last
/// step: a path that is still absolute or that starts with `..` is outside
/// the root.
pub(crate) fn normalise_target(arg: &str, repo_root: &str, prefix: &str) -> anyhow::Result<String> {
    let joined = if Path::new(arg).is_absolute() {
        arg.to_string()
    } else {
        format!("{prefix}{arg}")
    };
    let folded = fold_dots(&joined);
    let relative = below_root(&folded, repo_root).unwrap_or(&folded);
    let shown = sanitize(arg, CARD_TEXT_MAX_CHARS);
    if !crate::cmd::is_repo_relative_safe(Path::new(relative)) {
        anyhow::bail!("'{shown}' is outside the repository root");
    }
    if relative.is_empty() {
        anyhow::bail!("'{shown}' names the repository root, not a file");
    }
    Ok(relative.to_string())
}

/// Fold the `.` and `..` components and the repeated `/` of `path`, by
/// text. A leading `/` stays. A `..` with no folder before it stays too, so
/// the containment check refuses the path.
fn fold_dots(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    let body = parts.join("/");
    if path.starts_with('/') {
        format!("/{body}")
    } else {
        body
    }
}

/// The part of the absolute `path` below `root`: empty for the root itself,
/// `None` when `path` is not under `root`. A root with no name gives `None`.
fn below_root<'p>(path: &'p str, root: &str) -> Option<&'p str> {
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return None;
    }
    let rest = path.strip_prefix(root)?;
    if rest.is_empty() {
        return Some(rest);
    }
    rest.strip_prefix('/')
}

/// `path` as git writes it in its numstat output.
///
/// A path with a byte that git quotes goes in `"..."`, and each such byte
/// becomes a C escape (`\t`, `\"`, `\\`, ...) or a 3-digit octal escape. git
/// quotes the control bytes, `"` and `\`; with `quote_non_ascii` (git's
/// default `core.quotePath`) it quotes every byte from 0x80 too.
pub(crate) fn git_quoted(path: &str, quote_non_ascii: bool) -> String {
    let quotes =
        |b: u8| b < 0x20 || b == b'"' || b == b'\\' || b == 0x7f || (quote_non_ascii && b >= 0x80);
    if !path.bytes().any(quotes) {
        return path.to_string();
    }
    let mut out = String::with_capacity(path.len() + 8);
    out.push('"');
    for ch in path.chars() {
        let mut buf = [0u8; 4];
        let bytes = ch.encode_utf8(&mut buf).as_bytes();
        if !bytes.iter().copied().any(quotes) {
            out.push(ch);
            continue;
        }
        for &b in bytes {
            match b {
                0x07 => out.push_str("\\a"),
                0x08 => out.push_str("\\b"),
                b'\t' => out.push_str("\\t"),
                b'\n' => out.push_str("\\n"),
                0x0b => out.push_str("\\v"),
                0x0c => out.push_str("\\f"),
                b'\r' => out.push_str("\\r"),
                b'"' => out.push_str("\\\""),
                b'\\' => out.push_str("\\\\"),
                _ => out.push_str(&format!("\\{b:03o}")),
            }
        }
    }
    out.push('"');
    out
}

/// Each normalised target in the path form of the window's history.
///
/// git quotes unusual names in its numstat output, and with
/// `core.quotePath` off it keeps the bytes from 0x80 as they are. A target
/// takes the first of the two forms that the history holds, else git's
/// default quoted form.
#[must_use]
pub(crate) fn history_paths(targets: &[String], files: &[FileMetrics]) -> Vec<String> {
    let known: HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    targets
        .iter()
        .map(|target| {
            let quoted = git_quoted(target, true);
            if known.contains(quoted.as_str()) {
                return quoted;
            }
            let high_bytes_kept = git_quoted(target, false);
            if known.contains(high_bytes_kept.as_str()) {
                high_bytes_kept
            } else {
                quoted
            }
        })
        .collect()
}

// ============================================================================
// Sanitizer
// ============================================================================

/// True for a char that can change how a terminal shows the text around
/// it, or that shows nothing: every control char (C0, DEL and C1), the
/// Unicode bidi and line separator controls, each Unicode format char
/// (category Cf) that has no glyph of its own, and the whole tag block.
fn is_unsafe_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            // Bidi controls and line separators.
            '\u{061C}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{206F}'
                | '\u{2028}'
                | '\u{2029}'
                // Invisible format chars: soft hyphen, zero-width chars,
                // word joiner and invisible operators, byte order mark,
                // annotation marks, and the joiners and controls of some
                // scripts and of music notation.
                | '\u{00AD}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200D}'
                | '\u{2060}'..='\u{2064}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                // Tag chars: they show nothing, but each one maps to an
                // ASCII char.
                | '\u{E0000}'..='\u{E007F}'
        )
}

/// `s` with each unsafe char replaced by U+FFFD, cut to `max_chars` chars.
///
/// A cut string ends in [`CUT_MARK`], inside the `max_chars`. The cut
/// counts chars, never bytes, so it never splits a multibyte char. Never
/// lower or remove this guard or its tests: card text comes from the
/// repository's history.
#[must_use]
pub(crate) fn sanitize(s: &str, max_chars: usize) -> String {
    debug_assert!(max_chars >= CUT_MARK.len(), "the limit holds the cut mark");
    let safe = s.chars().map(|c| {
        if is_unsafe_char(c) {
            char::REPLACEMENT_CHARACTER
        } else {
            c
        }
    });
    if s.chars().count() <= max_chars {
        return safe.collect();
    }
    let mut cut: String = safe
        .take(max_chars.saturating_sub(CUT_MARK.len()))
        .collect();
    cut.push_str(CUT_MARK);
    cut
}

/// The first [`SHORT_SHA_CHARS`] chars of `hash`, or all of a shorter
/// hash.
fn short_sha(hash: &str) -> &str {
    hash.get(..SHORT_SHA_CHARS).unwrap_or(hash)
}

// ============================================================================
// Cards
// ============================================================================

/// One card for each of the first `caps.cards` targets, in argument order.
///
/// `targets` are in the path form of the history (see [`history_paths`]).
/// The partners come from each file's blast radius, so the pair gates and
/// the confidence are the ones of the coupling metric; this module counts
/// no pairs. The section total is the count of all targets.
#[must_use]
pub(crate) fn build_cards(
    commits: &[CommitInfo],
    result: &HeatmapResult,
    targets: &[String],
    caps: ListCaps,
) -> Section<Card> {
    let shown = targets.get(..caps.cards).unwrap_or(targets);
    let touching = commits_touching(commits, shown);
    let rows = shown
        .iter()
        .map(|target| {
            let metrics = result.files.iter().find(|f| f.path == *target);
            let touched = touching.get(target.as_str()).map_or(&[][..], Vec::as_slice);
            match metrics {
                Some(metrics) => card_of(metrics, touched, caps),
                None => Card::not_in_window(sanitize(target, usize::MAX)),
            }
        })
        .collect();
    Section {
        rows,
        total: targets.len(),
        left_out: 0,
    }
}

/// The commits that touch each target, in history order, each commit once.
/// One pass over the commits for all targets.
fn commits_touching<'t, 'c>(
    commits: &'c [CommitInfo],
    targets: &'t [String],
) -> HashMap<&'t str, Vec<&'c CommitInfo>> {
    let mut touching: HashMap<&str, Vec<&CommitInfo>> =
        targets.iter().map(|t| (t.as_str(), Vec::new())).collect();
    for commit in commits {
        for file in &commit.changed_files {
            if let Some(list) = touching.get_mut(file.path_str().as_ref())
                && !list.last().is_some_and(|last| std::ptr::eq(*last, commit))
            {
                list.push(commit);
            }
        }
    }
    touching
}

/// The card of a file in the window.
fn card_of(metrics: &FileMetrics, touched: &[&CommitInfo], caps: ListCaps) -> Card {
    let (owners, owner_count) = capped(owners_of(touched), caps.owners);
    let (partners, partner_count) = capped(partners_of(&metrics.blast_radius), caps.partners);
    let (fix_commits, fix_count) = capped(fix_commits_of(touched), caps.fix_commits);
    Card {
        path: sanitize(&metrics.path, usize::MAX),
        in_window: true,
        commits: metrics.churn.commits,
        counts: CardCounts {
            owners: owner_count,
            partners: partner_count,
            fix_commits: fix_count,
        },
        owners,
        partners,
        fix_commits,
    }
}

/// The first `cap` of `rows`, and their [`Count`].
fn capped<T>(mut rows: Vec<T>, cap: usize) -> (Vec<T>, Count) {
    let total = rows.len();
    rows.truncate(cap);
    let count = Count {
        total,
        shown: rows.len(),
    };
    (rows, count)
}

/// The authors of `touched`, most commits first, then by name. Empty when
/// the file has fewer than [`MIN_OWNERS`] distinct authors.
fn owners_of(touched: &[&CommitInfo]) -> Vec<Owner> {
    let mut by_author: HashMap<&str, usize> = HashMap::new();
    for commit in touched {
        *by_author.entry(commit.author.as_str()).or_insert(0) += 1;
    }
    if by_author.len() < MIN_OWNERS {
        return Vec::new();
    }
    let mut ranked: Vec<(&str, usize)> = by_author.into_iter().collect();
    ranked.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(y.0)));
    let file_commits = touched.len() as f64;
    ranked
        .into_iter()
        .map(|(name, commits)| Owner {
            name: sanitize(name, CARD_TEXT_MAX_CHARS),
            commits,
            share_pct: commits as f64 / file_commits * 100.0,
        })
        .collect()
}

/// The partners of a blast radius in card order: test files first, then
/// confidence desc, support desc and path asc.
fn partners_of(blast_radius: &[CouplingEntry]) -> Vec<Partner> {
    let mut ranked: Vec<&CouplingEntry> = blast_radius.iter().collect();
    ranked.sort_by(|x, y| partner_order(x, y));
    ranked
        .into_iter()
        .map(|entry| Partner {
            path: sanitize(&entry.path, CARD_TEXT_MAX_CHARS),
            confidence: entry.confidence,
            support: entry.support,
            test: is_test_partner(&entry.path),
        })
        .collect()
}

fn partner_order(x: &CouplingEntry, y: &CouplingEntry) -> Ordering {
    is_test_partner(&y.path)
        .cmp(&is_test_partner(&x.path))
        .then_with(|| y.confidence.total_cmp(&x.confidence))
        .then_with(|| y.support.cmp(&x.support))
        .then_with(|| x.path.cmp(&y.path))
}

/// True when the partner `path` (a git-quoted path keeps its quotes) is a
/// test file.
fn is_test_partner(path: &str) -> bool {
    noise::is_test_path(without_quotes(path).0)
}

/// The fix commits of `touched`, newest first, then by hash.
fn fix_commits_of(touched: &[&CommitInfo]) -> Vec<FixCommit> {
    let mut fixes: Vec<&CommitInfo> = touched
        .iter()
        .copied()
        .filter(|c| super::metrics::is_fix_commit(&c.message))
        .collect();
    fixes.sort_by(|x, y| {
        y.timestamp
            .cmp(&x.timestamp)
            .then_with(|| x.hash.cmp(&y.hash))
    });
    fixes
        .into_iter()
        .map(|c| FixCommit {
            sha: short_sha(&c.hash).to_string(),
            date: u64::try_from(c.timestamp).map_or_else(|_| "unknown".to_string(), format_epoch),
            subject: sanitize(&c.message, CARD_TEXT_MAX_CHARS),
        })
        .collect()
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::cmd::heatmap::types::{
        AuthorMetrics, ChurnMetrics, CouplingEntry, FileChangeInfo, FixRiskMetrics, WindowInfo,
    };
    use crate::cmd::heatmap::view::ViewSpec;

    const ROOT: &str = "/work/repo";

    fn default_caps() -> ListCaps {
        ViewSpec {
            detail: None,
            top: None,
            include_docs: false,
        }
        .lists()
    }

    // -----------------------------------------------------------------------
    // Path arguments
    // -----------------------------------------------------------------------

    #[test]
    fn test_normalise_maps_every_form_of_one_file_to_one_path() {
        let forms = [
            ("src/core.rs", ""),
            ("./src/core.rs", ""),
            ("/work/repo/src/core.rs", ""),
            ("/work/repo/src/core.rs", "tests/"),
            ("core.rs", "src/"),
            ("./core.rs", "src/"),
            ("../src/core.rs", "tests/"),
            ("src/../src/./core.rs", ""),
            ("src//core.rs", ""),
            ("/work/repo/tests/../src/core.rs", ""),
        ];
        for (arg, prefix) in forms {
            assert_eq!(
                normalise_target(arg, ROOT, prefix).unwrap(),
                "src/core.rs",
                "{arg} from {prefix:?}"
            );
        }
        // The root path from git may end in `/`.
        assert_eq!(
            normalise_target("/work/repo/src/core.rs", "/work/repo/", "").unwrap(),
            "src/core.rs"
        );
    }

    #[test]
    fn test_normalise_refuses_paths_outside_the_root() {
        let outside = [
            ("../outside.rs", ""),
            ("../../outside.rs", "src/"),
            ("src/../../outside.rs", ""),
            ("/etc/hosts", ""),
            ("/work/repository/src/core.rs", ""),
            ("/work/repo/../other/x.rs", ""),
            ("/work", ""),
        ];
        for (arg, prefix) in outside {
            let err = normalise_target(arg, ROOT, prefix).unwrap_err().to_string();
            assert_eq!(
                err,
                format!("'{arg}' is outside the repository root"),
                "{arg} from {prefix:?}"
            );
        }
    }

    #[test]
    fn test_normalise_refuses_the_root_itself() {
        for (arg, prefix) in [(".", ""), ("/work/repo", ""), ("..", "src/")] {
            let err = normalise_target(arg, ROOT, prefix).unwrap_err().to_string();
            assert_eq!(
                err,
                format!("'{arg}' names the repository root, not a file"),
                "{arg} from {prefix:?}"
            );
        }
    }

    #[test]
    fn test_git_quoted_follows_the_numstat_path_form() {
        assert_eq!(git_quoted("src/main.rs", true), "src/main.rs");
        assert_eq!(git_quoted("src/a b.rs", true), "src/a b.rs");
        assert_eq!(git_quoted("src/café.rs", true), r#""src/caf\303\251.rs""#);
        assert_eq!(git_quoted("src/café.rs", false), "src/café.rs");
        assert_eq!(git_quoted("src/a\tb.rs", false), r#""src/a\tb.rs""#);
        assert_eq!(git_quoted("src/q\"x\\y.rs", true), r#""src/q\"x\\y.rs""#);
        assert_eq!(git_quoted("src/d\u{7f}e.rs", true), r#""src/d\177e.rs""#);
        assert_eq!(
            git_quoted("a\u{7}\u{8}\n\u{b}\u{c}\r\u{1b}", true),
            r#""a\a\b\n\v\f\r\033""#
        );
    }

    fn metrics(path: &str, commits: usize) -> FileMetrics {
        FileMetrics {
            path: path.to_string(),
            churn: ChurnMetrics { commits, rate: 0.0 },
            stability_score: 50,
            authors: AuthorMetrics::default(),
            fix_risk: FixRiskMetrics::default(),
            blast_radius: Vec::new(),
        }
    }

    #[test]
    fn test_history_paths_use_the_form_that_the_history_holds() {
        let targets = vec!["src/café.rs".to_string(), "src/main.rs".to_string()];
        let quoted = [metrics(r#""src/caf\303\251.rs""#, 1)];
        assert_eq!(
            history_paths(&targets, &quoted),
            [r#""src/caf\303\251.rs""#, "src/main.rs"]
        );
        // `core.quotePath` off: the history keeps the bytes from 0x80.
        let raw = [metrics("src/café.rs", 1)];
        assert_eq!(
            history_paths(&targets, &raw),
            ["src/café.rs", "src/main.rs"]
        );
        // Not in the history: the default quoted form.
        assert_eq!(
            history_paths(&targets, &[]),
            [r#""src/caf\303\251.rs""#, "src/main.rs"]
        );
    }

    // -----------------------------------------------------------------------
    // Sanitizer
    // -----------------------------------------------------------------------

    #[test]
    fn test_sanitize_replaces_control_and_bidi_chars_with_exact_bytes() {
        let input = "a\u{1b}[31mb\u{1b}]0;title c\rd\u{202E}e\u{2066}f";
        let expected = "a\u{FFFD}[31mb\u{FFFD}]0;title c\u{FFFD}d\u{FFFD}e\u{FFFD}f";
        assert_eq!(
            sanitize(input, CARD_TEXT_MAX_CHARS).as_bytes(),
            expected.as_bytes()
        );
    }

    #[test]
    fn test_sanitize_replaces_every_listed_char() {
        let listed = [
            '\u{0}', '\u{7}', '\u{9}', '\n', '\u{7f}', '\u{85}', '\u{9b}', '\u{61c}', '\u{200e}',
            '\u{200f}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}', '\u{2066}',
            '\u{2067}', '\u{2068}', '\u{2069}', '\u{2028}', '\u{2029}',
        ];
        for c in listed {
            assert_eq!(
                sanitize(&format!("x{c}y"), 72),
                "x\u{FFFD}y",
                "U+{:04X}",
                c as u32
            );
        }
        assert_eq!(sanitize("fix: café ✓ 日本", 72), "fix: café ✓ 日本");
    }

    #[test]
    fn test_sanitize_replaces_invisible_format_chars_with_exact_bytes() {
        let input = "a\u{200B}b\u{200C}c\u{200D}d\u{2060}e\u{FEFF}f\u{AD}g";
        let expected = "a\u{FFFD}b\u{FFFD}c\u{FFFD}d\u{FFFD}e\u{FFFD}f\u{FFFD}g";
        assert_eq!(
            sanitize(input, CARD_TEXT_MAX_CHARS).as_bytes(),
            expected.as_bytes()
        );
    }

    #[test]
    fn test_sanitize_replaces_each_tag_char_with_exact_bytes() {
        // Tag chars that spell "hi", between a language tag and a cancel tag.
        let input = "ok\u{E0001}\u{E0068}\u{E0069}\u{E007F}.";
        let expected = "ok\u{FFFD}\u{FFFD}\u{FFFD}\u{FFFD}.";
        assert_eq!(
            sanitize(input, CARD_TEXT_MAX_CHARS).as_bytes(),
            expected.as_bytes()
        );
    }

    #[test]
    fn test_sanitize_replaces_the_first_and_last_char_of_each_format_range() {
        let listed = [
            '\u{ad}',
            '\u{180e}',
            '\u{200b}',
            '\u{200d}',
            '\u{2060}',
            '\u{2064}',
            '\u{206a}',
            '\u{206f}',
            '\u{feff}',
            '\u{fff9}',
            '\u{fffb}',
            '\u{13430}',
            '\u{1343f}',
            '\u{1bca0}',
            '\u{1bca3}',
            '\u{1d173}',
            '\u{1d17a}',
            '\u{e0000}',
            '\u{e007f}',
        ];
        for c in listed {
            assert_eq!(
                sanitize(&format!("x{c}y"), 72),
                "x\u{FFFD}y",
                "U+{:04X}",
                c as u32
            );
        }
        // Visible neighbours of those ranges stay as they are.
        assert_eq!(sanitize("¬®‐\u{FFFC}", 72), "¬®‐\u{FFFC}");
    }

    #[test]
    fn test_sanitize_cuts_a_multibyte_subject_by_char() {
        let subject = "é".repeat(100);
        let cut = sanitize(&subject, CARD_TEXT_MAX_CHARS);
        assert_eq!(cut.chars().count(), CARD_TEXT_MAX_CHARS);
        assert_eq!(cut, format!("{}...", "é".repeat(69)));
        // At the limit: no cut.
        let exact = "日".repeat(72);
        assert_eq!(sanitize(&exact, CARD_TEXT_MAX_CHARS), exact);
    }

    #[test]
    fn test_short_sha_takes_seven_chars_without_a_panic() {
        assert_eq!(short_sha(&"a".repeat(40)), "aaaaaaa");
        assert_eq!(short_sha("abc"), "abc");
        assert_eq!(short_sha("ééééééé"), "ééééééé");
    }

    // -----------------------------------------------------------------------
    // Cards
    // -----------------------------------------------------------------------

    fn commit(n: u8, author: &str, message: &str, timestamp: i64, files: &[&str]) -> CommitInfo {
        CommitInfo {
            hash: format!("{n:02x}").repeat(20),
            timestamp,
            author: author.to_string(),
            message: message.to_string(),
            changed_files: files
                .iter()
                .map(|p| FileChangeInfo {
                    path: std::path::PathBuf::from(p),
                    additions: 1,
                    deletions: 0,
                })
                .collect(),
        }
    }

    fn partner(path: &str, confidence: f64, support: usize) -> CouplingEntry {
        CouplingEntry {
            path: path.to_string(),
            confidence,
            support,
        }
    }

    fn result_with(files: Vec<FileMetrics>) -> HeatmapResult {
        HeatmapResult {
            generated_at: "2026-11-12".to_string(),
            repository: "/work/repo".to_string(),
            window: WindowInfo {
                mode: "dual".to_string(),
                since: "2026-08-14".to_string(),
                until: "2026-11-12".to_string(),
                commits_analyzed: 9,
                effective_strategy: None,
            },
            files,
            modules: Vec::new(),
            coupling_graph: Vec::new(),
            excluded_patterns: Vec::new(),
            warnings: Vec::new(),
            file_targets: None,
            fix_commits_total: 0,
            window_authors: 2,
        }
    }

    const DAY: i64 = 86_400;

    #[test]
    fn test_card_ranks_test_partners_first_then_confidence_support_and_path() {
        let mut core = metrics("src/core.rs", 6);
        core.blast_radius = vec![
            partner("src/b.rs", 0.9, 4),
            partner("src/a.rs", 0.9, 4),
            partner("src/c.rs", 0.9, 5),
            partner("tests/core_test.rs", 0.6, 3),
            partner("src/d.rs", 0.95, 3),
        ];
        let result = result_with(vec![core]);
        let cards = build_cards(&[], &result, &["src/core.rs".to_string()], default_caps());

        let card = &cards.rows[0];
        let order: Vec<(&str, bool)> = card
            .partners
            .iter()
            .map(|p| (p.path.as_str(), p.test))
            .collect();
        assert_eq!(
            order,
            [
                ("tests/core_test.rs", true),
                ("src/d.rs", false),
                ("src/c.rs", false),
                ("src/a.rs", false),
            ]
        );
        assert_eq!(card.counts.partners, Count { total: 5, shown: 4 });
    }

    /// git writes a non-ASCII name in quotes, so the first folder of the
    /// raw text is `"tests`. The partner is still a test file.
    #[test]
    fn test_card_ranks_a_git_quoted_test_partner_first() {
        let quoted_test = r#""tests/caf\303\251.rs""#;
        let mut core = metrics("src/core.rs", 6);
        core.blast_radius = vec![partner("src/a.rs", 0.9, 4), partner(quoted_test, 0.5, 3)];
        let result = result_with(vec![core]);
        let cards = build_cards(&[], &result, &["src/core.rs".to_string()], default_caps());

        let order: Vec<(&str, bool)> = cards.rows[0]
            .partners
            .iter()
            .map(|p| (p.path.as_str(), p.test))
            .collect();
        assert_eq!(order, [(quoted_test, true), ("src/a.rs", false)]);
    }

    #[test]
    fn test_card_lists_fix_commits_newest_first_then_by_hash() {
        let commits = [
            commit(1, "Ann", "fix: one", DAY, &["src/core.rs"]),
            commit(2, "Ann", "feat: two", 2 * DAY, &["src/core.rs"]),
            commit(4, "Ann", "fix: four", 3 * DAY, &["src/core.rs"]),
            commit(3, "Ann", "fix: three", 3 * DAY, &["src/core.rs"]),
            commit(5, "Ann", "fix: other file", 4 * DAY, &["src/other.rs"]),
            commit(6, "Ann", "fix: five", 5 * DAY, &["src/core.rs"]),
        ];
        let result = result_with(vec![metrics("src/core.rs", 5)]);
        let cards = build_cards(
            &commits,
            &result,
            &["src/core.rs".to_string()],
            default_caps(),
        );

        let card = &cards.rows[0];
        assert_eq!(
            card.fix_commits,
            [
                FixCommit {
                    sha: "0606060".to_string(),
                    date: "1970-01-06".to_string(),
                    subject: "fix: five".to_string(),
                },
                FixCommit {
                    sha: "0303030".to_string(),
                    date: "1970-01-04".to_string(),
                    subject: "fix: three".to_string(),
                },
                FixCommit {
                    sha: "0404040".to_string(),
                    date: "1970-01-04".to_string(),
                    subject: "fix: four".to_string(),
                },
            ]
        );
        assert_eq!(card.counts.fix_commits, Count { total: 4, shown: 3 });
    }

    #[test]
    fn test_card_owners_need_two_authors() {
        let one = [
            commit(1, "Ann", "feat: a", DAY, &["src/core.rs"]),
            commit(2, "Ann", "feat: b", 2 * DAY, &["src/core.rs"]),
        ];
        let result = result_with(vec![metrics("src/core.rs", 2)]);
        let target = ["src/core.rs".to_string()];
        let card = &build_cards(&one, &result, &target, default_caps()).rows[0];
        assert!(card.owners.is_empty());
        assert_eq!(card.counts.owners, Count { total: 0, shown: 0 });

        let two = [
            commit(1, "Ann", "feat: a", DAY, &["src/core.rs"]),
            commit(2, "Bob", "feat: b", 2 * DAY, &["src/core.rs"]),
            commit(3, "Ann", "feat: c", 3 * DAY, &["src/core.rs"]),
            commit(4, "Cy", "feat: d", 4 * DAY, &["src/core.rs"]),
            commit(5, "Bob", "feat: e", 5 * DAY, &["src/core.rs"]),
            commit(6, "Ann", "feat: f", 6 * DAY, &["src/core.rs"]),
            commit(7, "Dee", "feat: g", 7 * DAY, &["src/core.rs"]),
        ];
        let result = result_with(vec![metrics("src/core.rs", 7)]);
        let card = &build_cards(&two, &result, &target, default_caps()).rows[0];
        let owners: Vec<(&str, usize)> = card
            .owners
            .iter()
            .map(|o| (o.name.as_str(), o.commits))
            .collect();
        assert_eq!(owners, [("Ann", 3), ("Bob", 2), ("Cy", 1)]);
        assert_eq!(card.counts.owners, Count { total: 4, shown: 3 });
    }

    #[test]
    fn test_card_sanitizes_subjects_names_and_partner_paths_once() {
        let commits = [
            commit(
                1,
                "Eve\u{202e}x",
                "fix: \u{1b}]0;t\rend",
                DAY,
                &["src/core.rs"],
            ),
            commit(2, "Ann", "feat: b", 2 * DAY, &["src/core.rs"]),
        ];
        let mut core = metrics("src/core.rs", 2);
        core.blast_radius = vec![partner("src/b\u{2066}c.rs", 1.0, 3)];
        let result = result_with(vec![core]);
        let card = &build_cards(
            &commits,
            &result,
            &["src/core.rs".to_string()],
            default_caps(),
        )
        .rows[0];

        assert_eq!(card.fix_commits[0].subject, "fix: \u{FFFD}]0;t\u{FFFD}end");
        assert_eq!(card.owners[1].name, "Eve\u{FFFD}x");
        assert_eq!(card.partners[0].path, "src/b\u{FFFD}c.rs");
    }

    #[test]
    fn test_card_of_a_file_not_in_the_window_has_no_lists() {
        let result = result_with(vec![metrics("src/core.rs", 1)]);
        let cards = build_cards(&[], &result, &["src/gone.rs".to_string()], default_caps());
        let zero = Count { total: 0, shown: 0 };
        assert_eq!(
            cards.rows,
            [Card {
                path: "src/gone.rs".to_string(),
                in_window: false,
                commits: 0,
                counts: CardCounts {
                    owners: zero,
                    partners: zero,
                    fix_commits: zero,
                },
                owners: Vec::new(),
                partners: Vec::new(),
                fix_commits: Vec::new(),
            }]
        );
    }

    #[test]
    fn test_cards_keep_argument_order_and_the_cap() {
        let targets: Vec<String> = (0..7).map(|i| format!("src/f{i}.rs")).collect();
        let result = result_with(Vec::new());
        let cards = build_cards(&[], &result, &targets, default_caps());
        let paths: Vec<&str> = cards.rows.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "src/f0.rs",
                "src/f1.rs",
                "src/f2.rs",
                "src/f3.rs",
                "src/f4.rs"
            ]
        );
        assert_eq!(cards.count(), Count { total: 7, shown: 5 });
    }
}
