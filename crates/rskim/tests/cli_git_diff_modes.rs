//! Render-fidelity tests for `skim git diff --mode structure|full` (C1c/C1d/C1e).
//!
//! The `Default` diff mode got a single positional walk plus a post-render
//! verifier in `3fb0fd3`.  `structure` and `full` route through
//! `render_with_unchanged_context` instead and were never covered, so they kept
//! emitting container headers and closing braces with an unconditional context
//! prefix — rendering brand-new code as pre-existing — and emitting the same
//! source line from two overlapping AST nodes.
//!
//! Every test here drives the real binary against a hermetic repo and checks the
//! rendered output against the raw `git diff` for the same revision range:
//!
//! - **uniqueness**   — no `(axis, line)` pair is emitted twice
//! - **monotonicity** — new-side line numbers never jump backward
//! - **marker fidelity** — a line rendered with ` ` is a context line in the raw
//!   diff, a line rendered with `+` is an added line, a line rendered with `-`
//!   is a removed line (C1d: the dominant corruption class, invisible to the
//!   number-only checks above)
//! - **coverage**     — every `+`/`-` line's content reaches the reader (#317)
//!
//! The assertions hold whether the render is AST-based or the raw-hunk
//! fallback, so a fix that trades a lying render for a safe one still passes —
//! but `assert_ast_rendered` pins the shapes where AST rendering must survive.
//!
//! PF-009: every hermetic repo pins `-b main` and sets `user.name`/`user.email`
//! locally so the developer's global git config cannot change behaviour in CI.

mod common;

// ============================================================================
// Hermetic fixture helpers
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

/// Create a hermetic repo with two commits: `before` then `after` in `src/lib.rs`.
///
/// Returns the temp dir (caller must keep it alive) and the repo path.
fn two_commit_repo(before: &str, after: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).expect("create src dir");

    git_in(dir.path(), &["init", "-b", "main", repo.to_str().unwrap()]);
    git_in(&repo, &["config", "user.email", "test@example.com"]);
    git_in(&repo, &["config", "user.name", "Test"]);
    // Keep the diff shape independent of the host's diff config.
    git_in(&repo, &["config", "diff.algorithm", "myers"]);
    git_in(&repo, &["config", "core.autocrlf", "false"]);

    std::fs::write(repo.join("src/lib.rs"), before).expect("write before");
    git_in(&repo, &["add", "src/lib.rs"]);
    git_in(&repo, &["commit", "-m", "before"]);

    std::fs::write(repo.join("src/lib.rs"), after).expect("write after");
    git_in(&repo, &["add", "src/lib.rs"]);
    git_in(&repo, &["commit", "-m", "after"]);

    (dir, repo)
}

/// Raw `git diff --no-color HEAD~1..HEAD -U<ctx> -- src/lib.rs` for the repo.
fn raw_diff(repo: &std::path::Path, ctx: &str) -> String {
    let out = std::process::Command::new("git")
        .args([
            "diff",
            "--no-color",
            "HEAD~1..HEAD",
            ctx,
            "--",
            "src/lib.rs",
        ])
        .current_dir(repo)
        .output()
        .expect("git diff must run");
    assert!(out.status.success(), "git diff must succeed");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// `skim git diff HEAD~1..HEAD -U<ctx> [--mode <mode>] -- src/lib.rs`.
fn skim_diff(repo: &std::path::Path, ctx: &str, mode: Option<&str>) -> String {
    let mut cmd = common::skim();
    cmd.current_dir(repo)
        .args(["git", "diff", "HEAD~1..HEAD", ctx]);
    if let Some(m) = mode {
        cmd.args(["--mode", m]);
    }
    cmd.args(["--", "src/lib.rs"]);
    let out = cmd.output().expect("skim git diff must run");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ============================================================================
// Raw-diff model — the authority every assertion is checked against
// ============================================================================

/// Line classification derived from the raw unified diff.
struct RawModel {
    /// New-side line numbers that carry a `+` prefix in the raw diff.
    added: std::collections::HashSet<usize>,
    /// Old-side line numbers that carry a `-` prefix in the raw diff.
    removed: std::collections::HashSet<usize>,
    /// Content of every `+` / `-` line, for the coverage assertion.
    changed_content: Vec<String>,
    /// Width of the right-aligned line-number column skim will use.
    ln_width: usize,
}

fn parse_raw(raw: &str) -> RawModel {
    let mut added = std::collections::HashSet::new();
    let mut removed = std::collections::HashSet::new();
    let mut changed_content = Vec::new();
    let mut max_line = 0usize;
    let (mut cur_new, mut cur_old) = (0usize, 0usize);
    let mut in_hunk = false;

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("@@ -") {
            let head = rest.split(" @@").next().unwrap_or("");
            let mut parts = head.split(" +");
            let old = parts.next().unwrap_or("");
            let new = parts.next().unwrap_or("");
            let parse = |s: &str| -> (usize, usize) {
                let mut it = s.split(',');
                let start = it.next().unwrap_or("0").parse().unwrap_or(0);
                let count = it.next().map_or(1, |c| c.parse().unwrap_or(1));
                (start, count)
            };
            let (os, oc) = parse(old);
            let (ns, nc) = parse(new);
            cur_old = os;
            cur_new = ns;
            max_line = max_line.max(os + oc).max(ns + nc);
            in_hunk = true;
            continue;
        }
        if line.starts_with("diff --git") {
            in_hunk = false;
            continue;
        }
        if !in_hunk {
            continue;
        }
        match line.as_bytes().first() {
            Some(b'+') => {
                added.insert(cur_new);
                changed_content.push(line[1..].to_string());
                cur_new += 1;
            }
            Some(b'-') => {
                removed.insert(cur_old);
                changed_content.push(line[1..].to_string());
                cur_old += 1;
            }
            Some(b'\\') => {}
            Some(b' ') | None => {
                cur_new += 1;
                cur_old += 1;
            }
            _ => in_hunk = false,
        }
    }

    let ln_width = if max_line == 0 {
        1
    } else {
        max_line.to_string().len()
    };
    RawModel {
        added,
        removed,
        changed_content,
        ln_width,
    }
}

/// One parsed emission from skim's render: `(marker, line_number)`.
///
/// Structure mode renders unchanged nodes as synthetic, NUMBERLESS text
/// (` {line}`); those lines correspond to no source position and are skipped —
/// counting them as line 0 would both crash the axis bookkeeping and
/// under-report real emissions.
fn parse_emissions(rendered: &str, ln_width: usize) -> Vec<(char, usize)> {
    let mut out = Vec::new();
    for line in rendered.lines().skip(1) {
        let Some(marker) = line.chars().next() else {
            continue;
        };
        if !matches!(marker, '+' | '-' | ' ') {
            continue; // `\ No newline at end of file`
        }
        let rest = &line[1..];
        if rest.len() <= ln_width || !rest.is_char_boundary(ln_width) {
            continue;
        }
        let (field, tail) = rest.split_at(ln_width);
        if !tail.starts_with(' ') {
            continue;
        }
        let trimmed = field.trim_start();
        if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
            continue; // structure mode's numberless synthetic text
        }
        let Ok(n) = trimmed.parse::<usize>() else {
            continue;
        };
        out.push((marker, n));
    }
    out
}

/// Assert every render-fidelity invariant for `rendered` against `raw`.
///
/// `label` names the configuration so a failure identifies which mode and
/// context window produced it.
fn assert_render_fidelity(label: &str, rendered: &str, raw: &str) {
    let model = parse_raw(raw);

    // #317 coverage: every changed line's content must reach the reader.
    for content in &model.changed_content {
        if content.trim().is_empty() {
            continue;
        }
        assert!(
            rendered.contains(content.trim_end()),
            "{label}: changed line {content:?} is missing from the render:\n{rendered}"
        );
    }

    if rendered.starts_with("diff --git") {
        return; // raw-hunk fallback — byte-faithful by construction
    }

    let emissions = parse_emissions(rendered, model.ln_width);
    let mut seen: std::collections::HashSet<(char, usize)> = std::collections::HashSet::new();
    let mut prev_new = 0usize;

    for &(marker, line) in &emissions {
        let axis = if marker == '-' { '-' } else { 'n' };
        assert!(
            seen.insert((axis, line)),
            "{label}: line {line} emitted twice on the {axis} axis:\n{rendered}"
        );
        if axis == 'n' {
            assert!(
                line >= prev_new,
                "{label}: new-side line numbers jumped backward ({prev_new} → {line}):\n{rendered}"
            );
            prev_new = line;
        }
        match marker {
            ' ' => assert!(
                !model.added.contains(&line),
                "{label}: added line {line} rendered as unchanged context \
                 — the render claims new code is pre-existing:\n{rendered}"
            ),
            '+' => assert!(
                model.added.contains(&line),
                "{label}: line {line} rendered as added but is not a `+` line \
                 in the raw diff:\n{rendered}"
            ),
            '-' => assert!(
                model.removed.contains(&line),
                "{label}: old line {line} rendered as removed but is not a `-` line \
                 in the raw diff:\n{rendered}"
            ),
            _ => unreachable!("marker filtered above"),
        }
    }
}

/// Assert the render came from the AST path, not the raw-hunk fallback.
///
/// AST output opens with `{path} ({status})`; the fallback opens with
/// `diff --git a/…`.  Pinning this stops a "fix" that silently disables AST
/// rendering from passing the fidelity assertions vacuously.
fn assert_ast_rendered(label: &str, rendered: &str) {
    assert!(
        !rendered.starts_with("diff --git"),
        "{label}: expected an AST render, got the raw-hunk fallback:\n{rendered}"
    );
    assert!(
        rendered.starts_with("src/lib.rs (modified)"),
        "{label}: expected the AST file header, got:\n{rendered}"
    );
}

// ============================================================================
// Fixtures reproducing the three measured evidence shapes
// ============================================================================

/// `crates/rskim/src/cmd/git/mod.rs @ 92417dc9` shape — a wholly new struct AND
/// impl block inserted between existing functions.  Structure mode rendered the
/// struct header, its closing brace and the impl header as CONTEXT, so a
/// reviewer read "only the derive was added" while all four lines were `+`.
const EVIDENCE_NEW_CONTAINER_BEFORE: &str = "\
//! Module docs.

pub fn existing_one() -> i32 {
    1
}

pub fn existing_two() -> i32 {
    2
}
";

const EVIDENCE_NEW_CONTAINER_AFTER: &str = "\
//! Module docs.

pub fn existing_one() -> i32 {
    1
}

#[derive(Default)]
pub struct ParsedCommandOptions {
    pub combine_stderr: bool,
    pub raw_override: Option<String>,
}

impl ParsedCommandOptions {
    pub fn new() -> Self {
        Self::default()
    }
}

pub fn existing_two() -> i32 {
    2
}
";

/// `crates/rskim/src/cmd/file/mod.rs @ 6f8edd82` shape — a doc comment is added
/// directly above a struct, so the comment node's changed-line walk emits the
/// struct's header line as `+` and the container header emission then emitted
/// the same line again as context.
const EVIDENCE_DUP_HEADER_BEFORE: &str = "\
//! Module docs.

pub fn helper() -> i32 {
    7
}
";

const EVIDENCE_DUP_HEADER_AFTER: &str = "\
//! Module docs.

pub fn helper() -> i32 {
    7
}

/// Spec for a passthrough invocation.
pub struct PassthroughSpec<'a> {
    pub tool: &'a str,
    pub args: &'a [String],
}
";

/// `crates/rskim/src/cmd/hooks/copilot.rs @ d7407d6c` shape — a run of `//!`
/// module-doc lines.  tree-sitter's `line_comment` token includes the trailing
/// newline, so node N spans rows [N, N+1] and adjacent comment nodes overlap by
/// exactly one line; full mode emitted every doc line from the second onward
/// twice.
const EVIDENCE_DOC_RUN_BEFORE: &str = "\
//! Copilot CLI hook protocol implementation.
//!
//! Copilot CLI uses preToolUse hooks. The hook reads JSON from stdin,
//! extracts tool_input.command, rewrites if matched, and emits a
//! deny-with-suggestion response.
//!
//! UPGRADE PATH: When Copilot ships working `allow`, change one function.

pub fn agent_kind() -> u8 {
    1
}
";

const EVIDENCE_DOC_RUN_AFTER: &str = "\
//! Copilot CLI hook protocol implementation.
//!
//! Copilot CLI uses preToolUse hooks. The hook reads JSON from stdin,
//! extracts tool_input.command, rewrites if matched, and emits a
//! deny-with-suggestion response.
//!
//! UPGRADE PATH: When Copilot ships working `allow`, change one function.

pub fn agent_kind() -> u8 {
    2
}
";

// ============================================================================
// Evidence-case regression tests
// ============================================================================

#[test]
fn new_struct_and_impl_are_never_rendered_as_pre_existing_context() {
    let (_dir, repo) = two_commit_repo(EVIDENCE_NEW_CONTAINER_BEFORE, EVIDENCE_NEW_CONTAINER_AFTER);
    for ctx in ["-U3", "-U100", "-U100000"] {
        for mode in ["structure", "full"] {
            let raw = raw_diff(&repo, ctx);
            let rendered = skim_diff(&repo, ctx, Some(mode));
            assert_render_fidelity(&format!("92417dc9-shape {mode} {ctx}"), &rendered, &raw);
        }
    }
}

#[test]
fn struct_header_added_above_container_is_not_emitted_twice() {
    let (_dir, repo) = two_commit_repo(EVIDENCE_DUP_HEADER_BEFORE, EVIDENCE_DUP_HEADER_AFTER);
    for ctx in ["-U3", "-U100", "-U100000"] {
        for mode in ["structure", "full"] {
            let raw = raw_diff(&repo, ctx);
            let rendered = skim_diff(&repo, ctx, Some(mode));
            assert_render_fidelity(&format!("6f8edd82-shape {mode} {ctx}"), &rendered, &raw);
        }
    }
}

#[test]
fn module_doc_comment_run_emits_each_line_once_in_full_mode() {
    let (_dir, repo) = two_commit_repo(EVIDENCE_DOC_RUN_BEFORE, EVIDENCE_DOC_RUN_AFTER);
    for ctx in ["-U3", "-U100", "-U100000"] {
        let raw = raw_diff(&repo, ctx);
        let rendered = skim_diff(&repo, ctx, Some("full"));
        assert_render_fidelity(&format!("d7407d6c-shape full {ctx}"), &rendered, &raw);

        // Sharpen the generic uniqueness check into the observed symptom: each
        // doc line's text must appear exactly once in the render.
        if !rendered.starts_with("diff --git") {
            for doc in [
                "//! Copilot CLI uses preToolUse hooks.",
                "//! deny-with-suggestion response.",
            ] {
                let hits = rendered.lines().filter(|l| l.contains(doc)).count();
                assert_eq!(
                    hits, 1,
                    "full {ctx}: {doc:?} must appear exactly once, got {hits}:\n{rendered}"
                );
            }
        }
    }
}

// ============================================================================
// Mode / context matrix
// ============================================================================

/// A changed container body must still reach the reader in structure and full
/// mode.  `render_container_with_mode` skipped every child that began on the
/// container's header line — which is the body node itself — so a changed
/// `impl` block rendered as nothing but its header and closing brace.
#[test]
fn changed_container_body_reaches_the_reader() {
    let before = "\
//! Docs.

pub struct Widget {
    pub id: u32,
}

impl Widget {
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn double(&self) -> u32 {
        self.id * 2
    }
}
";
    let after = "\
//! Docs.

pub struct Widget {
    pub id: u32,
}

impl Widget {
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn triple(&self) -> u32 {
        self.id * 3
    }
}
";
    let (_dir, repo) = two_commit_repo(before, after);
    for ctx in ["-U3", "-U100", "-U100000"] {
        for mode in ["structure", "full"] {
            let raw = raw_diff(&repo, ctx);
            let rendered = skim_diff(&repo, ctx, Some(mode));
            let label = format!("container-body {mode} {ctx}");
            assert_render_fidelity(&label, &rendered, &raw);
            assert!(
                rendered.contains("self.id * 3"),
                "{label}: the changed method body must reach the reader:\n{rendered}"
            );
        }
    }
}

/// Full mode over a file with no container at all must render through the AST
/// path and stay faithful — the shape that pins the fix rather than letting the
/// verifier trade every render for the raw fallback.
#[test]
fn full_mode_over_free_functions_stays_ast_rendered_and_faithful() {
    let before = "\
//! Docs.

pub fn alpha() -> i32 {
    1
}

pub fn beta() -> i32 {
    2
}
";
    let after = "\
//! Docs.

pub fn alpha() -> i32 {
    11
}

pub fn beta() -> i32 {
    2
}

pub fn gamma() -> i32 {
    3
}
";
    let (_dir, repo) = two_commit_repo(before, after);
    let ctx = "-U100000";
    let raw = raw_diff(&repo, ctx);
    let rendered = skim_diff(&repo, ctx, Some("full"));
    assert_ast_rendered("free-functions full -U100000", &rendered);
    assert_render_fidelity("free-functions full -U100000", &rendered, &raw);
}

/// Default mode must not regress: it is the only mode that survives the
/// ADR-001 net-savings guard at real context windows, and marker fidelity is
/// being added to its verifier too.
#[test]
fn default_mode_render_stays_faithful() {
    let (_dir, repo) = two_commit_repo(EVIDENCE_NEW_CONTAINER_BEFORE, EVIDENCE_NEW_CONTAINER_AFTER);
    for ctx in ["-U3", "-U100", "-U100000"] {
        let raw = raw_diff(&repo, ctx);
        let rendered = skim_diff(&repo, ctx, None);
        assert_render_fidelity(&format!("default {ctx}"), &rendered, &raw);
    }
}

// ============================================================================
// B3 gap-fill regression tests (#512)
// ============================================================================

/// A blank line added between struct fields must appear in the rendered output.
///
/// Without B3, `render_container_with_mode` iterated body members with no gap
/// fill between them.  A blank `+` line between two field declarations is an
/// orphan (no AST node covers it), so the verifier's coverage check detected
/// the missing added line and bailed to the raw-hunk fallback.  After B3 the
/// gap fill serves the blank line and the verifier passes.
///
/// The direct assertion on `"+3 "` is load-bearing: the coverage check in
/// `assert_render_fidelity` skips blank-line content, so only this assertion
/// catches the absence of the orphan line.
#[test]
fn added_blank_line_between_struct_fields_reaches_reader() {
    // After: blank added between the two field lines.
    // New-side line numbers: 1=header 2=timeout 3=blank(+) 4=retries 5=closing
    let before = "\
pub struct Config {
    pub timeout: u32,
    pub retries: u32,
}
";
    let after = "\
pub struct Config {
    pub timeout: u32,

    pub retries: u32,
}
";
    let (_dir, repo) = two_commit_repo(before, after);
    let ctx = "-U100000";
    let raw = raw_diff(&repo, ctx);
    let rendered = skim_diff(&repo, ctx, Some("full"));
    let label = "blank-line-gap full -U100000";
    assert_ast_rendered(label, &rendered);
    assert_render_fidelity(label, &rendered, &raw);
    // The added blank line at new-side line 3 must appear with a `+` marker.
    // In the render format, an added line at line N (ln_width=1) is "+N ".
    assert!(
        rendered.lines().any(|l| l == "+3 "),
        "{label}: added blank at line 3 must appear as \"+3 \" in the render:\n{rendered}",
    );
}

/// Blank lines between impl methods (not just struct fields) must also reach
/// the reader.  An impl block's body is a `declaration_list` with method
/// declarations as members; inter-method blank lines are orphans.
///
/// This fixture verifies the gap fill path for function-body containers, which
/// have a deeper tree shape than struct field lists.
#[test]
fn blank_lines_between_impl_methods_reach_reader() {
    let before = "\
pub struct Widget {
    id: u32,
}

impl Widget {
    pub fn id(&self) -> u32 {
        self.id
    }
    pub fn doubled(&self) -> u32 {
        self.id * 2
    }
}
";
    let after = "\
pub struct Widget {
    id: u32,
}

impl Widget {
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn tripled(&self) -> u32 {
        self.id * 3
    }
}
";
    let (_dir, repo) = two_commit_repo(before, after);
    let ctx = "-U100000";
    let raw = raw_diff(&repo, ctx);
    let rendered = skim_diff(&repo, ctx, Some("full"));
    let label = "impl-method-blank-gap full -U100000";
    assert_ast_rendered(label, &rendered);
    assert_render_fidelity(label, &rendered, &raw);
    // The changed method body must reach the reader.
    assert!(
        rendered.contains("self.id * 3"),
        "{label}: changed method body must appear in render:\n{rendered}"
    );
}

/// A struct where a new field is added after existing fields, with blank
/// separator lines that are themselves added.  Pins that structure mode also
/// produces an AST render (not just full mode).
#[test]
fn struct_new_field_with_blank_separator_structure_mode() {
    let before = "\
//! Config types.

pub struct Config {
    pub timeout: u32,
    pub retries: u32,
}
";
    let after = "\
//! Config types.

pub struct Config {
    pub timeout: u32,
    pub retries: u32,

    pub max_connections: u32,
}
";
    let (_dir, repo) = two_commit_repo(before, after);
    let ctx = "-U100000";
    for mode in ["structure", "full"] {
        let raw = raw_diff(&repo, ctx);
        let rendered = skim_diff(&repo, ctx, Some(mode));
        let label = format!("struct-new-field-gap {mode} {ctx}");
        assert_ast_rendered(&label, &rendered);
        assert_render_fidelity(&label, &rendered, &raw);
        assert!(
            rendered.contains("max_connections"),
            "{label}: the new field must reach the reader:\n{rendered}"
        );
    }
}

/// Guard: unchanged inter-member blank lines must NOT be emitted by the gap
/// fill.  Only blank lines that fall in a changed hunk (`+` in the raw diff)
/// should appear.  Without this guard a refactoring with no content change
/// could emit extra blank lines that aren't in the diff.
#[test]
fn unchanged_blank_lines_between_members_are_not_emitted() {
    // The blank line between the two methods exists in BOTH before and after,
    // so it appears as a context line (`' '`) in the raw diff, not as `+`.
    let before = "\
pub struct Thing {
    pub a: u32,

    pub b: u32,
}
";
    let after = "\
pub struct Thing {
    pub a: u32,

    pub b: u32,
    pub c: u32,
}
";
    let (_dir, repo) = two_commit_repo(before, after);
    let ctx = "-U100000";
    let raw = raw_diff(&repo, ctx);
    let rendered = skim_diff(&repo, ctx, Some("full"));
    let label = "unchanged-blank-gap full -U100000";
    assert_ast_rendered(label, &rendered);
    assert_render_fidelity(label, &rendered, &raw);
    assert!(
        rendered.contains("pub c: u32"),
        "{label}: the new field must appear:\n{rendered}"
    );
}

/// ADR-011 class-2 pin: when the verifier rejects a render, skim serves the raw
/// hunks — a no-loss fallback — so the banner must cost ZERO stderr bytes
/// without `SKIM_DEBUG`, and must appear with it.
#[test]
fn verifier_raw_fallback_banner_is_debug_gated() {
    let (_dir, repo) = two_commit_repo(EVIDENCE_DOC_RUN_BEFORE, EVIDENCE_DOC_RUN_AFTER);

    let quiet = common::skim()
        .current_dir(&repo)
        .args([
            "git",
            "diff",
            "HEAD~1..HEAD",
            "-U100000",
            "--mode",
            "full",
            "--",
            "src/lib.rs",
        ])
        .output()
        .expect("skim git diff must run");
    let quiet_err = String::from_utf8_lossy(&quiet.stderr);
    assert!(
        !quiet_err.contains("verifier"),
        "ADR-011 class-2: the no-loss verifier fallback must be silent without \
         SKIM_DEBUG; got stderr:\n{quiet_err}"
    );
}
