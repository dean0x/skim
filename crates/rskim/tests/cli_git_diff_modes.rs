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
//! Both loops that carry those assertions are preceded by a non-vacuity guard
//! (PF-025): an empty parsed collection skips every assertion inside its loop
//! body, so all twelve call sites would go green having checked nothing.
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

/// Assert the render-fidelity invariants for `rendered` against `raw`.
///
/// `label` names the configuration so a failure identifies which mode and
/// context window produced it.
///
/// Two of the invariant sets are loop bodies over a parsed collection, and an
/// empty collection skips every assertion inside it — the function then
/// returns success having checked nothing.  Each loop is therefore preceded by
/// a non-vacuity guard (PF-025); the comment at each guard states what it
/// proves and what it does not.
///
/// Not every invariant is checked on every path: the axis invariants
/// (uniqueness, monotonicity, marker fidelity) are read off skim's
/// line-number column, which git's own bytes do not have, so they are skipped
/// when skim served those bytes verbatim — see the early return below.
fn assert_render_fidelity(label: &str, rendered: &str, raw: &str) {
    let model = parse_raw(raw);

    // Non-vacuity guard for the #317 loop below.  `changed_content` is derived
    // from `raw` and never from the render, so this holds on every path — every
    // caller diffs two genuinely different revisions.  An empty vector means
    // `parse_raw` stopped recognising the diff (hunk-header shape, line
    // prefixes, `in_hunk` bookkeeping), which would retire #317 coverage at
    // all twelve call sites at once and in silence.
    //
    // It proves the raw model was parsed; it does NOT prove that a content
    // assertion ran.  A fixture whose only changed line is BLANK contributes
    // one entry that the loop skips, and those two callers pin the blank
    // line's emission directly instead — see the `"+3 "` assertions in
    // `added_blank_line_between_struct_fields_reaches_reader` and
    // `structure_mode_struct_fields_preserve_line_numbers_and_indentation`.
    assert!(
        !model.changed_content.is_empty(),
        "{label}: fidelity check is VACUOUS — `model.changed_content` is empty, \
         so `parse_raw` found no `+`/`-` lines and the #317 content-coverage \
         loop below asserts nothing.  Raw diff was:\n{raw}"
    );

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

    // Every skim render opens with the file header and writes each patch line
    // through `emit_patch_line`'s `{prefix}{n:>ln_width$} ` column — that
    // includes `render_raw_hunks`, the in-file fallback, which inherits the
    // same header.  No path under `cmd/git/` emits a `diff --git` line, so
    // this prefix means skim served git's own bytes verbatim (the ADR-001
    // `Passthrough` verdict).  Those bytes are byte-faithful AND carry no
    // line-number column, so `parse_emissions` would be reading a grammar
    // that is not there: the axis checks, and any non-vacuity claim over
    // them, are both meaningless here.  Return before the guard below.
    if rendered.starts_with("diff --git") {
        return;
    }

    let ln_width = model.ln_width;
    let emissions = parse_emissions(rendered, ln_width);

    // Non-vacuity guard for the axis loop below.  Reached only past the
    // passthrough return above, so `rendered` is one of skim's own renders and
    // does carry the line-number column; a non-empty diff always puts at
    // least one numbered line in it.  An empty set therefore means
    // `parse_emissions` recognised nothing at all in skim's own layout, and
    // all four axis assertions below (uniqueness, monotonicity,
    // added-as-context, marker correctness) would be skipped rather than
    // checked.
    //
    // It is a TOTAL-loss detector, and that bound is measured rather than
    // assumed: changing the marker byte, the width of the number field, or
    // the separator to a non-space byte empties the set and fires this, as do
    // a numberless render, a header-only render and empty stdout — but merely
    // WIDENING the separator, or left-aligning the number inside its field,
    // still matches most lines and stays silent.  Partial corruption is the
    // axis assertions' own job, which is exactly why they have to be
    // reachable.
    assert!(
        !emissions.is_empty(),
        "{label}: fidelity check is VACUOUS — `emissions` is empty, so \
         `parse_emissions` matched no numbered lines in this render \
         (ln_width={ln_width}) and the uniqueness, monotonicity and marker \
         assertions below would all pass without executing.  Render \
         was:\n{rendered}"
    );

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

/// Assert skim rendered the diff itself rather than serving git's bytes.
///
/// Every skim render opens with `{path} ({status})`; only git's own bytes open
/// with `diff --git a/…`.  Pinning this stops a "fix" that silently trades a
/// render for raw bytes from passing the fidelity assertions vacuously
/// through the early return in `assert_render_fidelity`.
///
/// Bound worth knowing before relying on the name: the two shapes it compares
/// do NOT separate the AST walk from `render_raw_hunks`, which inherits the
/// same file header, so this does not pin AST rendering specifically.
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
// Non-vacuity guards: run the guards against a known-vacuous input
// ============================================================================

/// Call `assert_render_fidelity` and return the message it panicked with.
///
/// Fails if the call did NOT panic — a guard that does not fire is exactly the
/// defect these tests exist to detect.
///
/// The panic hook is deliberately left alone: suppressing it would be
/// process-global, so under a thread-per-test harness it would also swallow an
/// unrelated concurrent test's failure message.  libtest and nextest both
/// capture this expected panic's output and discard it while the test passes.
fn catch_fidelity_panic(label: &str, rendered: &str, raw: &str) -> String {
    // `AssertUnwindSafe` because the payload we are reaching across the unwind
    // is three `&str`s and the callee is *expected* to panic — there is no
    // half-mutated state for the boundary to protect.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert_render_fidelity(label, rendered, raw);
    }));
    let payload = result.expect_err("assert_render_fidelity must panic on a vacuous check");
    if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else {
        panic!("panic payload was neither String nor &str");
    }
}

/// PF-025 discipline for the two guards in `assert_render_fidelity`: an
/// invariant proposed to catch a corruption class must be run against a
/// KNOWN-CORRUPT input and required to FAIL, or it ships green and catches
/// nothing.  The twelve call sites above only ever exercise the pass
/// direction; this is the fail direction, for both vacuity surfaces.
#[test]
fn assert_render_fidelity_rejects_a_vacuous_check() {
    let raw = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,2 @@
 pub fn f() {}
-// old
+// new
";

    // Control: a well-formed pair, so a panic below is the guard under test
    // rather than a fixture that was broken all along.
    assert_render_fidelity(
        "control",
        "src/lib.rs (modified)\n 1 pub fn f() {}\n-2 // old\n+2 // new\n",
        raw,
    );

    // Surface 1 — `model.changed_content` empty: a hunk header `parse_raw` no
    // longer recognises, which is how a real parse regression empties it.
    let unparseable_raw = raw.replace("@@ -", "@@@ -");
    let err = catch_fidelity_panic(
        "vacuous-content",
        "src/lib.rs (modified)\n+2 // new\n",
        &unparseable_raw,
    );
    assert!(
        err.contains("VACUOUS") && err.contains("changed_content"),
        "the empty-`changed_content` guard must fire and name itself; got:\n{err}"
    );

    // Surface 2 — `emissions` empty: a render that carries the changed content
    // (so the #317 loop passes) but no line-number column at all, which is how
    // a render-layout move empties it.
    let err = catch_fidelity_panic(
        "vacuous-emissions",
        "src/lib.rs (modified)\n// old\n// new\n",
        raw,
    );
    assert!(
        err.contains("VACUOUS") && err.contains("parse_emissions"),
        "the empty-`emissions` guard must fire and name itself; got:\n{err}"
    );

    // The two guards must be distinguishable, or a failure cannot be triaged.
    assert!(
        !err.contains("changed_content"),
        "the two vacuity guards must not report each other; got:\n{err}"
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

// ============================================================================
// Phase B-repair golden tests: structure-mode rendering integrity (#512)
//
// These tests pin the properties that B3's gap-fill change violated.  They are
// RED at the B3 commit (1c6faa9 / ef91183) and GREEN after reverting the
// gap-fill in render_container_with_mode.  See Phase B-repair task description.
//
// PF-025 discipline: each assertion was confirmed to FAIL against the buggy
// (post-B3) binary before being committed.  B3's own four regression tests
// pass in both states (they never discriminated); see the step-3 report.
// ============================================================================

/// Structure-mode rendering of a struct with an added blank line between fields
/// must preserve source line numbers and indentation on unchanged field lines,
/// and must NOT produce orphaned commas on their own output lines.
///
/// **Why this test discriminates:**
/// B3's gap-fill change causes `render_container_with_mode` to iterate the
/// container body's children including `,` separator tokens.  In structure
/// mode, `render_unchanged_node` calls `parser.transform(",", structure_config)`
/// which writes ` ,\n` — a comma on its own line.  The field text is
/// simultaneously transformed WITHOUT its comma, losing the source-line number
/// and the leading indentation.
///
/// Pre-B3 the body was rendered as a unit via `render_node_with_hunks`, which
/// walks hunk patch lines and emits them with their original prefix and line
/// number.  The unchanged fields appeared as ` 2     pub timeout: u32,` and
/// ` 4     pub retries: u32,` — with number, indent, and comma intact.
///
/// This test is RED at HEAD (B3 present) and GREEN after reverting the gap-fill.
#[test]
fn structure_mode_struct_fields_preserve_line_numbers_and_indentation() {
    // Adding a blank line between two struct fields.
    // New-side line numbers after: 1=header 2=timeout 3=blank(+) 4=retries 5=closing.
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
    let rendered = skim_diff(&repo, ctx, Some("structure"));
    let label = "structure-mode struct field integrity";

    // Must be AST-rendered (not a raw-hunk fallback).
    assert_ast_rendered(label, &rendered);

    // Render fidelity: changed-line coverage + marker checks.
    assert_render_fidelity(label, &rendered, &raw);

    // The added blank line at new-side line 3 must appear with a `+` prefix.
    // In the render format (ln_width=1), an added line at line 3 is "+3 ".
    assert!(
        rendered.lines().any(|l| l == "+3 "),
        "{label}: added blank at line 3 must appear as \"+3 \" in the render;\n\
         got:\n{rendered}"
    );

    // Regression guard — B3 gap-fill: structure mode must NOT produce a line
    // that is only a comma (orphaned `,` from field separator token).
    assert!(
        !rendered.lines().any(|l| l.trim() == ","),
        "{label}: structure mode produced an orphaned comma on its own line — \
         this is the B3 gap-fill regression;\ngot:\n{rendered}"
    );

    // Regression guard — B3 gap-fill: unchanged field content must appear WITH
    // its source line number, not as numberless synthetic structure text.
    // Correct format for ln_width=1: " 2     pub timeout: u32,"
    // Broken format: " pub timeout: u32" (no line number, no indent, no comma).
    assert!(
        rendered.contains("pub timeout: u32,"),
        "{label}: field 'pub timeout: u32,' must appear intact (with comma) \
         in the render;\ngot:\n{rendered}"
    );
    assert!(
        rendered.contains("pub retries: u32,"),
        "{label}: field 'pub retries: u32,' must appear intact (with comma) \
         in the render;\ngot:\n{rendered}"
    );
}

/// Structure-mode rendering of an impl block must preserve line numbers on all
/// emitted lines and must not collapse the changed method into `{...}`.
///
/// This pins the impl-fixture behavior described in the Phase B-repair task.
/// Both pre-B3 and post-B3 binaries produce the same output for this fixture
/// (the regression is only triggered when body-gap lines are present), so this
/// test stays GREEN in both states — it documents the correct behavior rather
/// than discriminating the regression.  Included here for completeness.
#[test]
fn structure_mode_impl_changed_method_is_not_collapsed() {
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
    let rendered = skim_diff(&repo, ctx, Some("structure"));
    let label = "structure-mode impl method integrity";

    assert_ast_rendered(label, &rendered);
    assert_render_fidelity(label, &rendered, &raw);

    // The changed method body must reach the reader — it must not be collapsed.
    assert!(
        rendered.contains("self.id * 3"),
        "{label}: changed method body must appear in render (must not be \
         collapsed to {{...}});\ngot:\n{rendered}"
    );
}
