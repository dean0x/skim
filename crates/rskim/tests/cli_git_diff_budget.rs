//! ADR-001 budget pin for the `git diff` hunk headers (commit 9).
//!
//! # What this pins
//!
//! `skim git diff` re-emits each hunk's `@@ -a,b +c,d @@` header so the reader
//! can see where one hunk ends and the next begins.  Those bytes are spent out
//! of the ADR-001 net-savings budget: the enriched view is served only while it
//! stays strictly smaller than the raw diff in both bytes and tokens, and a
//! decoration that overruns the budget does not degrade gracefully — it flips
//! the guard to raw, so the enrichment suppresses itself and the view gets
//! strictly worse.
//!
//! # The measurement this test encodes
//!
//! Measured on the fixture below with the binary at `67dace8`, before the
//! headers landed:
//!
//! ```text
//! raw      1376 B      served  1272 B      headroom  104 B     hunks 2
//! headers    36 B  (`@@ -18,7 +18,7 @@` and `@@ -58,7 +58,7 @@`, 18 B each)
//! served after the headers   1308 B        margin      68 B
//! ```
//!
//! A two-column scheme costing about [`TWO_COLUMN_BYTES`] on this view was
//! considered for the same job and rejected on exactly this arithmetic: 68 B of
//! margin does not hold it, so it would have tripped the guard into raw and the
//! enrichment would have suppressed itself.  `headers_fit_the_raw_budget`
//! asserts both halves: the headers fit, and the rejected alternative does not.
//!
//! # The fixture is deliberately two-hunk
//!
//! A header is a *boundary* marker, so it is emitted only when a file has more
//! than one hunk — a lone hunk has no next hunk to be separated from, and the
//! position the header would carry is already on every line, each stamped with
//! its own source line number.  This fixture therefore carries two edits far
//! enough apart to produce two hunks, so the budget it pins is the budget for
//! the case where headers are actually spent.
//! `a_single_hunk_diff_spends_nothing_on_a_header` pins the other side of that
//! rule from the same `BEFORE_RS` with one edit instead of two.
//!
//! # Bytes are the visible axis, not the binding one
//!
//! `fidelity::decide` keeps the compressed view only when it is strictly
//! smaller in **bytes AND tokens**, and on a small diff the token axis binds
//! first: a view 34 B under raw can still be served raw because the header's
//! ~12 punctuation-heavy tokens outweigh what it saves.  The arithmetic above
//! is therefore a lower bound on what the headers cost, not the whole verdict.
//!
//! This is why `headers_fit_the_raw_budget` asserts `served < raw` on the real
//! binary rather than re-deriving the decision from byte counts: the served
//! bytes are the verdict of both axes, and a byte model alone would pass while
//! the guard served raw.
//!
//! # Related history in this repo
//!
//! Comparable headroom on real commits, measured the same way: 2601 B over 29
//! hunks (`efac056`), 1928 B over 46 (`c76e228`), 804 B over 16 (`a7d12b5`) —
//! i.e. 40-90 B of headroom per hunk against a 16-20 B header.  The margin is
//! real but thin, which is why it is pinned rather than assumed.
//!
//! PF-027: no constant, fixture size or threshold here may be adjusted to make
//! an assertion pass.  The fixture is a verbatim excerpt of this repository's
//! own `crates/rskim/src/cmd/git/diff/parse.rs` and the two edits are ordinary
//! one-token changes; if the arithmetic stops holding, that is the finding.

mod common;

/// Bytes a two-column rendering scheme would add to this fixture's view.
///
/// Not a tunable: it is the size of the alternative that was rejected, and the
/// assertion it feeds states that the alternative does not fit.
const TWO_COLUMN_BYTES: usize = 76;

// ============================================================================
// Hermetic fixture
// ============================================================================

/// Run a git command in `dir`, asserting success with a step-labelled panic.
fn git_in(dir: &std::path::Path, args: &[&str]) {
    let step = args.join(" ");
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|e| panic!("hermetic setup: `git {step}` spawn failed: {e}"));
    assert!(
        out.status.success(),
        "hermetic setup: `git {step}` failed;\nstderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Create a hermetic repo with two commits writing `before` then `after` to
/// `src/{name}`.
///
/// PF-009: `-b main` plus repo-local `user.name`/`user.email`/`diff.algorithm`
/// so the developer's global git config cannot move the byte counts this file
/// asserts on.
fn two_commit_repo(
    name: &str,
    before: &str,
    after: &str,
) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).expect("create src dir");

    git_in(dir.path(), &["init", "-b", "main", repo.to_str().unwrap()]);
    git_in(&repo, &["config", "user.email", "test@example.com"]);
    git_in(&repo, &["config", "user.name", "Test"]);
    git_in(&repo, &["config", "diff.algorithm", "myers"]);
    git_in(&repo, &["config", "core.autocrlf", "false"]);

    let path = repo.join("src").join(name);
    std::fs::write(&path, before).expect("write before");
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-m", "before"]);

    std::fs::write(&path, after).expect("write after");
    git_in(&repo, &["add", "-A"]);
    git_in(&repo, &["commit", "-m", "after"]);

    (dir, repo)
}

/// The control: what the reader would have got without skim at all.
fn raw_diff(repo: &std::path::Path, name: &str) -> String {
    let spec = format!("src/{name}");
    let out = std::process::Command::new("git")
        .args(["diff", "--no-color", "HEAD~1..HEAD", "--", &spec])
        .current_dir(repo)
        .output()
        .expect("raw git diff must run");
    assert!(out.status.success(), "raw git diff must exit 0");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// What skim actually serves for the same range.
fn served_diff(repo: &std::path::Path, name: &str) -> String {
    let spec = format!("src/{name}");
    let mut cmd = common::skim();
    // PF-026: the escape hatch would make the served view identical to raw and
    // every assertion below vacuously true.
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    let out = cmd
        .current_dir(repo)
        .args(["git", "diff", "HEAD~1..HEAD", "--", &spec])
        .output()
        .expect("skim git diff must run");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Count the `@@` hunk headers in a unified diff or in skim's rendered view.
fn hunk_headers(text: &str) -> Vec<&str> {
    text.lines().filter(|l| l.starts_with("@@ -")).collect()
}

/// Verbatim excerpt of this repository's own
/// `crates/rskim/src/cmd/git/diff/parse.rs` (lines 1-73).  Real code, so the
/// line lengths, comment density and AST shape are representative rather than
/// chosen; frozen here so the byte counts this file asserts on are stable.
const BEFORE_RS: &str = r##"//! Unified diff parsing — hunk extraction and file status detection.

use std::sync::LazyLock;

use regex::Regex;

use super::types::{DiffHunk, FileChange, FileDiff, FileMetadata};
use crate::output::canonical::DiffFileStatus;

/// Matches hunk headers: `@@ -N,M +N,M @@ optional context`
static HUNK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^@@\s+-(\d+)(?:,(\d+))?\s+\+(\d+)(?:,(\d+))?\s+@@").expect("valid regex")
});

/// Parse a hunk header line: `@@ -N,M +N,M @@`
///
/// Returns `(old_start, old_count, new_start, new_count)` on success.
pub(super) fn parse_hunk_header(line: &str) -> Option<(usize, usize, usize, usize)> {
    let caps = HUNK_RE.captures(line)?;
    let old_start: usize = caps.get(1)?.as_str().parse().ok()?;
    let old_count: usize = caps.get(2).map_or(1, |m| m.as_str().parse().unwrap_or(1));
    let new_start: usize = caps.get(3)?.as_str().parse().ok()?;
    let new_count: usize = caps.get(4).map_or(1, |m| m.as_str().parse().unwrap_or(1));
    Some((old_start, old_count, new_start, new_count))
}

/// Scan extended headers from a `diff --git` block.
///
/// Starting at `start`, reads lines until a hunk header (`@@`) or the next
/// `diff --git` header. Returns the collected metadata and the index of
/// the next unprocessed line.
pub(super) fn scan_extended_headers(lines: &[&str], start: usize) -> (FileMetadata, usize) {
    let mut meta = FileMetadata {
        change: FileChange::Modified,
        file_minus: String::new(),
        file_plus: String::new(),
    };

    let mut i = start;
    while i < lines.len() && !lines[i].starts_with("diff --git ") {
        let line = lines[i];

        if line.starts_with("new file mode") {
            meta.change = FileChange::New;
        } else if line.starts_with("deleted file mode") {
            meta.change = FileChange::Deleted;
        } else if line.starts_with("rename from ") {
            let from = line
                .strip_prefix("rename from ")
                .unwrap_or_default()
                .to_string();
            meta.change = FileChange::Renamed { from: Some(from) };
        } else if line.starts_with("rename to ") {
            // Only update if not already set to Renamed (rename from comes first)
            if !matches!(meta.change, FileChange::Renamed { .. }) {
                meta.change = FileChange::Renamed { from: None };
            }
        } else if line.starts_with("Binary files") && line.contains("differ") {
            meta.change = FileChange::Binary;
        } else if line.starts_with("--- ") {
            meta.file_minus = line.strip_prefix("--- ").unwrap_or_default().to_string();
        } else if line.starts_with("+++ ") {
            meta.file_plus = line.strip_prefix("+++ ").unwrap_or_default().to_string();
        } else if line.starts_with("@@") {
            // Hunk header — extended headers are done, stop before consuming it
            break;
        }

        i += 1;
    }

    (meta, i)
}
"##;

/// [`BEFORE_RS`] with two one-token edits, far enough apart to produce two
/// separate hunks at git's default three lines of context.
const AFTER_RS: &str = r##"//! Unified diff parsing — hunk extraction and file status detection.

use std::sync::LazyLock;

use regex::Regex;

use super::types::{DiffHunk, FileChange, FileDiff, FileMetadata};
use crate::output::canonical::DiffFileStatus;

/// Matches hunk headers: `@@ -N,M +N,M @@ optional context`
static HUNK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^@@\s+-(\d+)(?:,(\d+))?\s+\+(\d+)(?:,(\d+))?\s+@@").expect("valid regex")
});

/// Parse a hunk header line: `@@ -N,M +N,M @@`
///
/// Returns `(old_start, old_count, new_start, new_count)` on success.
pub(super) fn parse_hunk_header(line: &str) -> Option<(usize, usize, usize, usize)> {
    let caps = HUNK_RE.captures(line)?;
    let old_start: usize = caps.get(1)?.as_str().parse().ok()?;
    let old_count: usize = caps.get(2).map_or(1, |m| m.as_str().parse().unwrap_or(0));
    let new_start: usize = caps.get(3)?.as_str().parse().ok()?;
    let new_count: usize = caps.get(4).map_or(1, |m| m.as_str().parse().unwrap_or(1));
    Some((old_start, old_count, new_start, new_count))
}

/// Scan extended headers from a `diff --git` block.
///
/// Starting at `start`, reads lines until a hunk header (`@@`) or the next
/// `diff --git` header. Returns the collected metadata and the index of
/// the next unprocessed line.
pub(super) fn scan_extended_headers(lines: &[&str], start: usize) -> (FileMetadata, usize) {
    let mut meta = FileMetadata {
        change: FileChange::Modified,
        file_minus: String::new(),
        file_plus: String::new(),
    };

    let mut i = start;
    while i < lines.len() && !lines[i].starts_with("diff --git ") {
        let line = lines[i];

        if line.starts_with("new file mode") {
            meta.change = FileChange::New;
        } else if line.starts_with("deleted file mode") {
            meta.change = FileChange::Deleted;
        } else if line.starts_with("rename from ") {
            let from = line
                .strip_prefix("rename from ")
                .unwrap_or_default()
                .to_string();
            meta.change = FileChange::Renamed { from: Some(from) };
        } else if line.starts_with("rename to ") {
            // Only update if not already set to Renamed (rename from comes first)
            if !matches!(meta.change, FileChange::Renamed { .. }) {
                meta.change = FileChange::Renamed { from: None };
            }
        } else if line.starts_with("Binary files") && line.contains("differ") {
            meta.change = FileChange::Binary;
        } else if line.starts_with("--- ") {
            meta.file_minus = line.strip_prefix("--- ").unwrap_or("").to_string();
        } else if line.starts_with("+++ ") {
            meta.file_plus = line.strip_prefix("+++ ").unwrap_or_default().to_string();
        } else if line.starts_with("@@") {
            // Hunk header — extended headers are done, stop before consuming it
            break;
        }

        i += 1;
    }

    (meta, i)
}
"##;

// ============================================================================
// The budget
// ============================================================================

#[test]
fn headers_fit_the_raw_budget() {
    let (_dir, repo) = two_commit_repo("lib.rs", BEFORE_RS, AFTER_RS);
    let raw = raw_diff(&repo, "lib.rs");
    let served = served_diff(&repo, "lib.rs");

    // Non-vacuity first: an assertion about the enriched view is worthless if
    // the guard already elected raw and `served` IS the raw diff.
    assert!(
        !served.starts_with("diff --git"),
        "expected the AST view, got the raw-diff fallback:\n{served}"
    );

    let (raw_len, served_len) = (raw.len(), served.len());
    assert!(
        served_len < raw_len,
        "ADR-001: the enriched view must stay inside the raw budget, \
         but served {served_len} B against raw {raw_len} B.  The hunk headers \
         and any other per-hunk decoration are spent out of that budget; when \
         it is overrun the guard serves raw and the enrichment suppresses \
         itself.\n{served}"
    );

    let margin = raw_len - served_len;
    assert!(
        margin < TWO_COLUMN_BYTES,
        "the budget has grown to {margin} B of margin, which now holds the \
         {TWO_COLUMN_BYTES} B two-column scheme this design rejected.  That is \
         not a failure of the render — it is a signal to re-open the rejected \
         alternative, or to re-derive this bound from a fresh measurement.  Do \
         not raise the constant to silence this (PF-027)."
    );
}

/// The header is a *boundary* marker, and a boundary is a relation between two
/// hunks.  A single-hunk file has no next hunk, so it spends nothing on one —
/// the position the header would carry is already on every rendered line, each
/// stamped with its own source line number.
///
/// Derived from [`BEFORE_RS`] by applying only the first of the two edits, so
/// the one-hunk and two-hunk cases differ in exactly that and nothing else.
#[test]
fn a_single_hunk_diff_spends_nothing_on_a_header() {
    let after_one_edit = BEFORE_RS.replace(
        "let old_count: usize = caps.get(2).map_or(1, |m| m.as_str().parse().unwrap_or(1));",
        "let old_count: usize = caps.get(2).map_or(1, |m| m.as_str().parse().unwrap_or(0));",
    );
    assert_ne!(after_one_edit, BEFORE_RS, "the edit must actually apply");

    let (_dir, repo) = two_commit_repo("lib.rs", BEFORE_RS, &after_one_edit);
    let raw = raw_diff(&repo, "lib.rs");
    let served = served_diff(&repo, "lib.rs");

    assert_eq!(
        hunk_headers(&raw).len(),
        1,
        "fixture must produce exactly one hunk:\n{raw}"
    );
    assert!(
        !served.starts_with("diff --git"),
        "expected the AST view, got the raw-diff fallback:\n{served}"
    );
    assert!(
        hunk_headers(&served).is_empty(),
        "a lone hunk has no boundary to mark, so no header may be emitted:\n{served}"
    );
    // Non-vacuity: the change itself still reaches the reader (#317).
    assert!(
        served.contains("parse().unwrap_or(0)"),
        "the changed line must still be served:\n{served}"
    );
}

#[test]
fn every_hunk_is_opened_by_its_own_header() {
    let (_dir, repo) = two_commit_repo("lib.rs", BEFORE_RS, AFTER_RS);
    let raw = raw_diff(&repo, "lib.rs");
    let served = served_diff(&repo, "lib.rs");

    let raw_hunks = hunk_headers(&raw).len();
    assert_eq!(raw_hunks, 2, "fixture must produce two hunks:\n{raw}");

    let served_headers = hunk_headers(&served);
    assert_eq!(
        served_headers.len(),
        raw_hunks,
        "every hunk must be opened by its own header, so the reader can see \
         where one ends and the next begins:\n{served}"
    );
    // The counts are always spelled out, including the `,1` git omits — the
    // header is rendered from the parsed fields, not echoed from git's bytes.
    assert_eq!(
        served_headers,
        vec!["@@ -18,7 +18,7 @@", "@@ -58,7 +58,7 @@"],
        "headers must carry the parsed ranges:\n{served}"
    );
}

#[test]
fn breadcrumbs_are_marked_distinctly_from_source_lines() {
    let (_dir, repo) = two_commit_repo("lib.rs", BEFORE_RS, AFTER_RS);
    let served = served_diff(&repo, "lib.rs");

    // `scan_extended_headers` is the enclosing declaration for the second hunk
    // and sits outside every hunk window, so skim pulls it in as a breadcrumb.
    let crumb = served
        .lines()
        .find(|l| l.contains("fn scan_extended_headers"))
        .unwrap_or_else(|| panic!("expected a breadcrumb for the second hunk:\n{served}"));
    assert!(
        crumb.starts_with('~'),
        "a breadcrumb is skim's own addition and must not be passed off as a \
         context line git printed; got {crumb:?}\n{served}"
    );
}

// ============================================================================
// Markdown: the breadcrumb is the H1, which the file header already said
// ============================================================================

const BEFORE_MD: &str = "\
# Release Notes

Intro paragraph that stays put.

## Alpha

The alpha section body.

## Beta

The beta section body.

## Gamma

The gamma section body.
";

const AFTER_MD: &str = "\
# Release Notes

Intro paragraph that stays put.

## Alpha

The alpha section body.

## Beta

The beta section body, revised.

## Gamma

The gamma section body.
";

#[test]
fn markdown_omits_the_h1_breadcrumb() {
    let (_dir, repo) = two_commit_repo("lib.md", BEFORE_MD, AFTER_MD);
    let raw = raw_diff(&repo, "lib.md");
    let served = served_diff(&repo, "lib.md");

    assert!(
        !served.starts_with("diff --git"),
        "expected the AST view, got the raw-diff fallback:\n{served}"
    );
    // The file header one line above already names the file; the H1 restates it.
    assert!(
        served.contains("src/lib.md (modified)"),
        "expected skim's file header:\n{served}"
    );
    assert!(
        !served.contains("# Release Notes"),
        "Markdown's breadcrumb is always the enclosing H1, which carries no \
         information the file header has not already given — it must be \
         suppressed, not merely re-styled:\n{served}"
    );
    // The change itself must still be there, or the assertion above would pass
    // for a render that dropped everything.
    assert!(
        served.contains("The beta section body, revised."),
        "the changed line must still reach the reader (#317):\n{served}"
    );
    assert!(
        served.len() < raw.len(),
        "served {} B against raw {} B",
        served.len(),
        raw.len()
    );
}
