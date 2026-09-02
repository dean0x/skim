//! CLI tests for elision-marker comment-prefix and phrasing fidelity.
//!
//! The core (rskim-core) marker — the canonical "A-form" — uses the language's
//! own comment prefix via `get_comment_prefix` and reads:
//!   `// ... (N lines truncated) — SKIM_PASSTHROUGH=1 for full output`
//!
//! `process::passthrough_with_truncation` is the fallback used by four call
//! sites, notably the post-guardrail hard-cap enforcement (~process.rs:782)
//! that fires for ANY language whenever `--max-lines`/`--last-lines` is set
//! and the guardrail selected the raw view.  As of HEAD it hardcodes `#`
//! regardless of language and uses a different phrasing ("C-form"):
//!   `# ... (N lines truncated; use SKIM_PASSTHROUGH=1 to see all)`
//!
//! These RED tests pin the *correct* behaviour (A-form, language-aware prefix)
//! so the fix can be validated end-to-end.  The single control test
//! (`max_lines_marker_keeps_hash_prefix_on_python`) must stay GREEN throughout.

use assert_cmd::Command;
use tempfile::TempDir;

mod common;

// ---------------------------------------------------------------------------
// Shared fixture
// ---------------------------------------------------------------------------

/// 9-line TypeScript fixture — two exported functions and a constant.
/// Matches the live reproduction used to confirm the defect at HEAD.
const TS_FIXTURE: &str = "\
export function add(a: number, b: number): number {\n\
  return a + b;\n\
}\n\
\n\
export function sub(a: number, b: number): number {\n\
  return a - b;\n\
}\n\
\n\
export const PI = 3.14;\n";

fn skim_cmd() -> Command {
    let mut cmd = common::skim();
    cmd.env_remove("SKIM_PASSTHROUGH");
    cmd.env_remove("SKIM_DEBUG");
    cmd
}

fn write_ts_fixture(dir: &TempDir) -> std::path::PathBuf {
    let path = dir.path().join("small.ts");
    std::fs::write(&path, TS_FIXTURE).unwrap();
    path
}

// ---------------------------------------------------------------------------
// RED tests — fail on current HEAD, pass once the defect is fixed
// ---------------------------------------------------------------------------

/// For every budget in [2, 3, 4, 5, 6] a `.ts` view must never emit a line
/// that starts with `# ` — that is the Python/Shell comment prefix, not the
/// TypeScript `//` prefix.
///
/// WHY RED TODAY: budgets 4, 5, 6 trigger the post-guardrail
/// `passthrough_with_truncation` path (~process.rs:782) which hardcodes `#`
/// regardless of file language.  Live evidence at HEAD:
///   `skim small.ts --max-lines 4`
///   → `# ... (6 lines truncated; use SKIM_PASSTHROUGH=1 to see all)`
#[test]
fn max_lines_marker_uses_language_comment_prefix_on_typescript() {
    let dir = TempDir::new().unwrap();
    let file = write_ts_fixture(&dir);

    for n in [2usize, 3, 4, 5, 6] {
        let output = skim_cmd()
            .arg(file.to_str().unwrap())
            .arg("--max-lines")
            .arg(n.to_string())
            .arg("--no-cache")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "skim --max-lines {n} should exit 0; stderr={}",
            String::from_utf8_lossy(&output.stderr),
        );

        let stdout = String::from_utf8(output.stdout.clone()).unwrap();
        let bad_line = stdout.lines().find(|l| l.starts_with("# "));
        assert!(
            bad_line.is_none(),
            "skim --max-lines {n} on a .ts file must not emit a `# ` elision \
             marker (TypeScript uses `//`); offending line: {:?}\nFull stdout:\n{}",
            bad_line,
            stdout,
        );
    }
}

/// With `--max-lines 4` on the 9-line TypeScript fixture, the output must
/// contain exactly one truncation line, and that line must:
///   - start with `// ...`  (the TypeScript comment prefix, A-form)
///   - contain `SKIM_PASSTHROUGH=1 for full output`  (A-form phrasing)
///   - NOT contain `use SKIM_PASSTHROUGH=1 to see all`  (C-form phrasing)
///
/// WHY RED TODAY: `passthrough_with_truncation` emits the C-form with a `#`
/// prefix.  All three sub-assertions fail:
///   current: `# ... (6 lines truncated; use SKIM_PASSTHROUGH=1 to see all)`
///   correct: `// ... (6 lines truncated) — SKIM_PASSTHROUGH=1 for full output`
#[test]
fn max_lines_marker_uses_canonical_phrasing_on_typescript() {
    let dir = TempDir::new().unwrap();
    let file = write_ts_fixture(&dir);

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--max-lines")
        .arg("4")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "skim --max-lines 4 should exit 0; stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout.clone()).unwrap();

    let truncated_lines: Vec<&str> = stdout.lines().filter(|l| l.contains("truncated")).collect();
    assert_eq!(
        truncated_lines.len(),
        1,
        "expected exactly one truncation line; found {}:\nFull stdout:\n{}",
        truncated_lines.len(),
        stdout,
    );

    let marker = truncated_lines[0];

    assert!(
        marker.starts_with("// ..."),
        "marker must start with `// ...` (TypeScript comment prefix, A-form); \
         got: {:?}\nFull stdout:\n{}",
        marker,
        stdout,
    );

    assert!(
        marker.contains("SKIM_PASSTHROUGH=1 for full output"),
        "marker must contain A-form hint `SKIM_PASSTHROUGH=1 for full output`; \
         got: {:?}\nFull stdout:\n{}",
        marker,
        stdout,
    );

    assert!(
        !marker.contains("use SKIM_PASSTHROUGH=1 to see all"),
        "marker must NOT contain C-form phrasing `use SKIM_PASSTHROUGH=1 to see all`; \
         got: {:?}\nFull stdout:\n{}",
        marker,
        stdout,
    );
}

/// With `--last-lines 4 --mode=full` on the 9-line TypeScript fixture the
/// elision marker must use the TypeScript `//` comment prefix, not `#`.
///
/// `--mode=full` is used to ensure the guardrail serves the raw 9-line view
/// (no compression savings), causing the post-guardrail
/// `passthrough_with_truncation` (~process.rs:782) to enforce the budget and
/// emit the marker.
///
/// WHY RED TODAY: `passthrough_with_truncation` (last_lines branch) hardcodes
/// `#` regardless of language:
///   current: `# ... (6 lines above; use SKIM_PASSTHROUGH=1 to see all)`
///   correct: `// ... (6 lines above) — SKIM_PASSTHROUGH=1 for full output`
#[test]
fn last_lines_marker_uses_language_comment_prefix_on_typescript() {
    let dir = TempDir::new().unwrap();
    let file = write_ts_fixture(&dir);

    let output = skim_cmd()
        .arg(file.to_str().unwrap())
        .arg("--last-lines")
        .arg("4")
        .arg("--mode=full")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "skim --last-lines 4 should exit 0; stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout.clone()).unwrap();

    // No line in a .ts view should ever carry the `#` comment prefix.
    let bad_line = stdout.lines().find(|l| l.starts_with("# "));
    assert!(
        bad_line.is_none(),
        "skim --last-lines 4 on a .ts file must not emit a `# ` elision marker; \
         offending line: {:?}\nFull stdout:\n{}",
        bad_line,
        stdout,
    );

    // The marker line (carrying SKIM_PASSTHROUGH) must use the `//` prefix.
    let marker_line = stdout.lines().find(|l| l.contains("SKIM_PASSTHROUGH"));
    assert!(
        marker_line.is_some(),
        "expected a SKIM_PASSTHROUGH hint in the output (file has 9 lines, \
         budget is 4); no such line found.\nFull stdout:\n{}",
        stdout,
    );
    let marker = marker_line.unwrap();
    assert!(
        marker.starts_with("// ..."),
        "last-lines marker must start with `// ...` for a .ts file; \
         got: {:?}\nFull stdout:\n{}",
        marker,
        stdout,
    );
}

// ---------------------------------------------------------------------------
// CONTROL — must stay GREEN before and after the fix
// ---------------------------------------------------------------------------

/// CONTROL: Python uses `#` as its comment prefix.
///
/// With `--max-lines 4 --mode=full` on a 9-line Python fixture the elision
/// marker MUST start with `# ...`.  `--mode=full` is used to ensure the raw
/// 9-line view is served and the bound enforcement marker is actually emitted.
///
/// The current `passthrough_with_truncation` happens to produce the right
/// prefix for Python (it hardcodes `#`).  After the fix makes the prefix
/// language-aware, Python must still receive `# ...`.  If this test ever
/// turns red, the fix has regressed Python comment-prefix handling.
#[test]
fn max_lines_marker_keeps_hash_prefix_on_python() {
    // Two defs and a constant — 9 lines, matching the TypeScript fixture size.
    const PY_FIXTURE: &str = "\
def add(a, b):\n\
    return a + b\n\
\n\
\n\
def sub(a, b):\n\
    return a - b\n\
\n\
\n\
PI = 3.14\n";

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("small.py");
    std::fs::write(&path, PY_FIXTURE).unwrap();

    let output = skim_cmd()
        .arg(path.to_str().unwrap())
        .arg("--max-lines")
        .arg("4")
        .arg("--mode=full")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "skim --max-lines 4 on Python should exit 0; stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout.clone()).unwrap();

    let marker_line = stdout.lines().find(|l| l.contains("SKIM_PASSTHROUGH"));
    assert!(
        marker_line.is_some(),
        "expected a SKIM_PASSTHROUGH hint (file has 9 lines, budget is 4); \
         no such line found.\nFull stdout:\n{}",
        stdout,
    );
    let marker = marker_line.unwrap();
    assert!(
        marker.starts_with("# ..."),
        "elision marker for a .py file must start with `# ...`; \
         got: {:?}\nFull stdout:\n{}",
        marker,
        stdout,
    );
}

// ---------------------------------------------------------------------------
// Markdown — the hint must stay inside the HTML comment
// ---------------------------------------------------------------------------

/// Markdown is the only language whose comment carries a closing suffix (` -->`),
/// so the `SKIM_PASSTHROUGH` remedy clause must render INSIDE the comment.
///
/// WHY RED TODAY: the marker builder closes the comment before appending the
/// hint, so the hint escapes into the rendered document as visible prose:
///   current: `<!-- ... (8 lines truncated) --> — SKIM_PASSTHROUGH=1 for full output`
///   correct: `<!-- ... (8 lines truncated) — SKIM_PASSTHROUGH=1 for full output -->`
#[test]
fn max_lines_marker_on_markdown_keeps_hint_inside_comment() {
    // 12 lines, deliberately free of fenced code blocks.
    const MD_FIXTURE: &str = "\
# Title\n\
\n\
Intro paragraph.\n\
\n\
## Alpha\n\
\n\
Alpha body text.\n\
\n\
## Beta\n\
\n\
Beta body text.\n\
Final closing sentence.\n";

    let dir = TempDir::new().unwrap();
    let path = dir.path().join("doc.md");
    std::fs::write(&path, MD_FIXTURE).unwrap();

    let output = skim_cmd()
        .arg(path.to_str().unwrap())
        .arg("--mode=full")
        .arg("--max-lines")
        .arg("5")
        .arg("--no-cache")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "skim --mode=full --max-lines 5 on markdown should exit 0; stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout.clone()).unwrap();

    let marker_line = stdout.lines().find(|l| l.contains("SKIM_PASSTHROUGH"));
    assert!(
        marker_line.is_some(),
        "expected a SKIM_PASSTHROUGH hint (fixture has 12 lines, budget is 5); \
         no such line found.\nFull stdout:\n{}",
        stdout,
    );
    let marker = marker_line.unwrap();

    assert!(
        marker.starts_with("<!-- ..."),
        "markdown marker must open an HTML comment; got: {:?}\nFull stdout:\n{}",
        marker,
        stdout,
    );

    assert!(
        marker.ends_with(" -->"),
        "markdown marker must close its HTML comment last; got: {:?}\nFull stdout:\n{}",
        marker,
        stdout,
    );

    assert!(
        marker.contains("SKIM_PASSTHROUGH=1 for full output"),
        "markdown marker must carry the A-form remedy hint; got: {:?}\nFull stdout:\n{}",
        marker,
        stdout,
    );

    assert!(
        !marker.contains("--> \u{2014}"),
        "the remedy hint must not escape the HTML comment; got: {:?}\nFull stdout:\n{}",
        marker,
        stdout,
    );
}
