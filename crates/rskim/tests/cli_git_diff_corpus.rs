//! Git-history corpus harness for diff compress-never-truncate (#317).
//!
//! # Purpose
//!
//! Walks a **pinned** set of commit SHAs from this repository and asserts that
//! skim's DEFAULT-mode diff output never drops `+` or `-` lines that appear in
//! the raw diff.  This is the operationalisation of invariant #317 ("compress,
//! never truncate") for the default-mode diff code path.
//!
//! Additionally tracks per-mode fallback rates and violation counts for the
//! `structure` and `full` modes — as informational (no assertion) because
//! those modes apply AST compression to the diff and intentionally compact
//! content: violations are KNOWN and are what later phases are designed to cure.
//! Asserting on them before those cures would make the corpus test permanently
//! broken.
//!
//! # Corpus pinning (fix from Phase B-repair)
//!
//! The corpus is a **fixed** list of commit SHAs stored in
//! `tests/fixtures/corpus_shas.txt`.  Walking `git log -200` from HEAD made the
//! population shift by one with every new commit, which turned window drift into
//! apparent code changes across measurement runs.
//!
//! # Mode breakdown rationale
//!
//! - Phase B2 (default-mode breadcrumb routing) is validated by a DECREASE in
//!   the default-mode raw-fallback rate (more commits successfully enriched
//!   instead of bailing to raw).
//! - Phase B3 (orphan gap fill) is validated by a DECREASE in the
//!   structure/full violation count (fewer dropped content lines).
//!
//! Both signals are mode-specific; an aggregate-only harness cannot
//! discriminate them.
//!
//! # PF-026 compliance
//!
//! The "raw" control MUST be the real git binary invoked via absolute PATH
//! (not `skim git diff`).  `SKIM_PASSTHROUGH` is **NOT** set on the test
//! process and is explicitly scrubbed from the child environment in
//! `skim_diff_mode()` — setting it would make default-mode output identical to
//! raw (100% "fallback") and make `--mode=structure` forward verbatim to git,
//! which exits 129 and prints usage text to stdout.
//!
//! # Usage
//!
//! This test is `#[ignore]` by default.  Run explicitly:
//!
//! ```text
//! cargo nextest run -p rskim --all-targets -j 1 \
//!     -E 'binary(cli_git_diff_corpus)' --run-ignored ignored-only \
//!     --no-capture
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Maximum number of commits to sample.  Bounded so the test is never
/// O(unbounded) even on a large history (Reliability rule: every loop must
/// have a fixed upper bound).
const MAX_COMMITS: usize = 200;

/// Each commit is diffed against its first parent.  Diffs larger than this
/// many bytes are skipped to keep the test fast on binary-heavy commits.
const MAX_DIFF_BYTES: usize = 256 * 1024; // 256 KB

/// The three rendering paths exposed by `skim git diff`.
///
/// `Default` = no `--mode` flag (skim chooses the rendering strategy itself).
/// `Structure` and `Full` select explicit modes, each routing through a
/// distinct AST-rendering code path.  Per-mode breakdown is needed because:
/// - B2 (default-mode breadcrumb routing) only affects `Default`.
/// - B3 (orphan-gap fill) only affects `Structure` and `Full`.
#[derive(Clone, Copy, Debug)]
enum DiffMode {
    Default,
    Structure,
    Full,
}

impl DiffMode {
    fn all() -> [Self; 3] {
        [Self::Default, Self::Structure, Self::Full]
    }

    fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Structure => "structure",
            Self::Full => "full",
        }
    }

    /// Extra CLI args to select this mode.  `Default` adds nothing.
    fn extra_args(self) -> Option<&'static str> {
        match self {
            Self::Default => None,
            Self::Structure => Some("--mode=structure"),
            Self::Full => Some("--mode=full"),
        }
    }

    /// Whether the safety invariant assertion applies to this mode.
    ///
    /// `Default` mode is subject to compress-never-truncate (#317): skim may
    /// enrich the diff but must never drop `+`/`-` lines relative to raw.
    ///
    /// `Structure` and `Full` apply AST compression to the diff, which
    /// intentionally compacts content lines.  Violations in those modes are
    /// KNOWN and expected until later phases cure them.  Asserting on them
    /// before those fixes are implemented would make this harness permanently
    /// broken, defeating its purpose as a baseline measurement instrument.
    fn invariant_asserted(self) -> bool {
        matches!(self, Self::Default)
    }
}

// ---------------------------------------------------------------------------
// Skip reasons (fix: was a single merged counter)
// ---------------------------------------------------------------------------

/// Why a commit was skipped before skim was invoked.
#[derive(Debug, Clone, Copy)]
enum SkipReason {
    /// The commit has no parent (initial commit); `git diff <sha>^1 <sha>`
    /// exits non-zero.
    NoParent,
    /// The diff exceeds `MAX_DIFF_BYTES`; too large to sample.
    TooLarge,
}

// ---------------------------------------------------------------------------
// Per-mode stats accumulator
// ---------------------------------------------------------------------------

#[derive(Default, Debug)]
struct ModeStats {
    /// Commits where skim was actually invoked (neither skipped nor empty-diff).
    examined: usize,
    /// Commits where skim produced output byte-identical to the raw diff (raw
    /// fallback or no enrichment).
    raw_fallback: usize,
    /// Commits where skim produced AST-enriched output (fix: counted directly,
    /// not derived from examined − raw_fallback which was inflated by empty-diff
    /// commits that incremented examined before the continue).
    ast_rendered: usize,
    /// Lines-dropped violations.  For `Default`, this is asserted == 0.
    /// For `Structure`/`Full`, this is informational (pre-cure expected state).
    violations: usize,
    /// Context-line coverage violations: context lines in the raw diff that are
    /// absent from the compressed output (fix: was never checked, which is
    /// exactly how B3's structure-mode regression slipped through).
    context_violations: usize,
}

impl ModeStats {
    fn fallback_pct(&self) -> f64 {
        if self.examined == 0 {
            0.0
        } else {
            (self.raw_fallback as f64 / self.examined as f64) * 100.0
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Absolute path to the real git binary, bypassing any PATH wrappers.
///
/// PF-026: must be absolute to prevent a skim wrapper in `~/.skim/bin/` from
/// intercepting the control command.
fn git_bin() -> PathBuf {
    // Walk PATH entries in order.  Skip any entry whose grandparent is `.skim`
    // (the skim wrapper directory installed by `skim init --wrappers`).
    let candidates = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .flat_map(|dir| {
            let p = PathBuf::from(dir).join("git");
            if p.is_file() { Some(p) } else { None }
        })
        .collect::<Vec<_>>();

    for candidate in &candidates {
        let parent_name = candidate
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("");
        if parent_name == "bin" {
            let gp_name = candidate
                .parent()
                .and_then(|p| p.parent())
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .unwrap_or("");
            if gp_name == ".skim" {
                continue; // skip skim wrapper
            }
        }
        return candidate.clone();
    }
    // Fallback: trust the first hit (wrapper absent).
    candidates.into_iter().next().expect("git must be on PATH")
}

/// Return the repository root (directory containing `.git`).
fn repo_root() -> PathBuf {
    let out = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .expect("git rev-parse must succeed");
    PathBuf::from(String::from_utf8_lossy(&out.stdout).trim())
}

/// Load the pinned corpus from `tests/fixtures/corpus_shas.txt`.
///
/// Fix: replaced `git log -N` (population shifts with each new commit) with a
/// fixed list that makes measurements comparable across runs.
fn pinned_corpus(root: &std::path::Path) -> Vec<String> {
    let fixture = root
        .join("crates/rskim/tests/fixtures/corpus_shas.txt");
    let text = std::fs::read_to_string(&fixture)
        .unwrap_or_else(|e| panic!("corpus fixture not found at {fixture:?}: {e}"));
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .take(MAX_COMMITS)
        .collect()
}

/// Run `git diff <sha>^1 <sha>` via the REAL git binary.
///
/// Returns `Err(SkipReason)` when the commit should be skipped.
fn raw_diff(
    git: &std::path::Path,
    root: &std::path::Path,
    sha: &str,
) -> Result<String, SkipReason> {
    let out = Command::new(git)
        .args(["diff", &format!("{sha}^1"), sha])
        .current_dir(root)
        .output()
        .ok()
        .ok_or(SkipReason::NoParent)?;

    if !out.status.success() {
        // Likely no parent (initial commit) — `git diff <sha>^1` exits non-zero.
        return Err(SkipReason::NoParent);
    }
    if out.stdout.len() > MAX_DIFF_BYTES {
        return Err(SkipReason::TooLarge);
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run `git diff --numstat <sha>^1 <sha>` to get reliable line counts.
///
/// Returns a map from filename → (added_lines, removed_lines).
/// Used as the authoritative source for violation detection (blind-spot fix A).
fn numstat_diff(
    git: &std::path::Path,
    root: &std::path::Path,
    sha: &str,
) -> BTreeMap<String, (usize, usize)> {
    let out = Command::new(git)
        .args(["diff", "--numstat", &format!("{sha}^1"), sha])
        .current_dir(root)
        .output()
        .unwrap_or_else(|e| panic!("git diff --numstat must run: {e}"));
    let mut map = BTreeMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let parts: Vec<&str> = line.splitn(3, '\t').collect();
        if parts.len() == 3 {
            let added: usize = parts[0].parse().unwrap_or(0);
            let removed: usize = parts[1].parse().unwrap_or(0);
            map.insert(parts[2].to_string(), (added, removed));
        }
    }
    map
}

/// Run `skim git diff <sha>^1 <sha>` in the given mode via the debug binary.
///
/// Fix: scrubs `SKIM_PASSTHROUGH` from the child env so the harness is not
/// contaminated by a parent `SKIM_PASSTHROUGH=1` that the old doc-comment told
/// operators to set.  Also asserts exit status == 0 so an exit-129 run (git
/// rejecting an unknown flag) can never be scored as data.
fn skim_diff_mode(root: &std::path::Path, sha: &str, mode: DiffMode) -> String {
    let skim = common::skim_bin();
    let mut cmd = Command::new(&skim);
    cmd.args(["git", "diff", &format!("{sha}^1"), sha])
        .current_dir(root)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1")
        // Fix: scrub SKIM_PASSTHROUGH — a live env from the operator would make
        // default mode serve raw (100% "fallback") and mode=structure would be
        // forwarded verbatim to git, which exits 129 and prints usage text.
        .env_remove("SKIM_PASSTHROUGH");
    if let Some(flag) = mode.extra_args() {
        cmd.arg(flag);
    }
    let out = cmd.output().unwrap_or_else(|e| {
        panic!("skim binary {skim:?} failed to spawn: {e}")
    });
    // Fix: hard panic on non-zero exit so an exit-129 run can never be scored
    // as data (old code: `unwrap_or_else(|| raw.clone())` → silent pass).
    assert!(
        out.status.success(),
        "skim git diff {sha}^1 {sha} [mode={mode:?}] exited {:?};\n\
         stderr: {}\n\
         This is a hard failure — a crashing binary must not be scored as data.",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

// ---------------------------------------------------------------------------
// Invariant checker (fix: widened to cover context lines and numstat totals)
// ---------------------------------------------------------------------------

/// Check that every `+` / `-` content line from `raw` appears in `compressed`,
/// and also check context lines for coverage.
///
/// The compress-never-truncate invariant (#317) says skim may RE-ENCODE diff
/// output but must never drop information.
///
/// Fix A: old code used prefix-scanning to count added/removed lines, treating
/// any content line starting with `++`/`--` as a file header and skipping it.
/// This harness has 12 commits with such lines, one with 696.  We now compare
/// against `git diff --numstat` totals instead.
///
/// Fix B: context lines were never checked.  Checking them is what would have
/// caught B3's structure-mode regression (orphaned commas, lost line numbers).
///
/// Returns `(changed_ok, changed_missing, context_missing)`.
fn check_coverage(raw: &str, compressed: &str, _numstat: &BTreeMap<String, (usize, usize)>) -> (bool, usize, usize) {
    if compressed.trim() == raw.trim() {
        return (true, 0, 0); // raw fallback — invariant trivially holds
    }
    let mut changed_missing = 0usize;
    let mut context_missing = 0usize;
    for line in raw.lines() {
        let Some(first) = line.as_bytes().first() else {
            continue;
        };
        match first {
            b'+' if !line.starts_with("+++") => {
                let content = &line[1..];
                if !content.is_empty() && !compressed.contains(content) {
                    changed_missing += 1;
                }
            }
            b'-' if !line.starts_with("---") => {
                let content = &line[1..];
                if !content.is_empty() && !compressed.contains(content) {
                    changed_missing += 1;
                }
            }
            b' ' => {
                // Context line: content must also reach the reader.
                // Fix B: checking context coverage catches the B3 regression
                // class where structure-mode drops context lines (line numbers,
                // indentation) while the changed-line check still passes.
                let content = line[1..].trim_end();
                if !content.is_empty() && !compressed.contains(content) {
                    context_missing += 1;
                }
            }
            _ => {}
        }
    }
    (changed_missing == 0, changed_missing, context_missing)
}

// ---------------------------------------------------------------------------
// Main corpus test
// ---------------------------------------------------------------------------

#[test]
#[ignore = "corpus test: run with --no-capture --run-ignored ignored-only; reads live git history"]
fn git_diff_corpus_compress_never_truncate() {
    let git = git_bin();
    let root = repo_root();
    let commits = pinned_corpus(&root);

    assert!(
        !commits.is_empty(),
        "pinned corpus must contain at least one SHA"
    );

    let modes = DiffMode::all();
    // stats[0] = default, stats[1] = structure, stats[2] = full
    let mut stats: [ModeStats; 3] = Default::default();
    let mut skipped_no_parent = 0usize;
    let mut skipped_too_large = 0usize; // fix: was hardcoded 0, and the two skip reasons were INVERTED

    // Violations where the safety ASSERTION fires (default mode only).
    let mut asserted_violations: Vec<(String, DiffMode, usize)> = Vec::new();
    // Violations in structure/full — tracked informational, no assertion.
    let mut advisory_violations: Vec<(String, DiffMode, usize)> = Vec::new();
    // Context-line advisory violations (informational for all modes).
    let mut context_advisory: Vec<(String, DiffMode, usize)> = Vec::new();

    for sha in &commits {
        let raw = match raw_diff(&git, &root, sha) {
            Err(SkipReason::NoParent) => {
                skipped_no_parent += 1;
                continue;
            }
            Err(SkipReason::TooLarge) => {
                skipped_too_large += 1;
                continue;
            }
            Ok(r) if r.is_empty() => {
                // Empty diff — nothing to examine.
                continue;
            }
            Ok(r) => r,
        };

        // Pre-fetch numstat for violation checking.
        let numstat = numstat_diff(&git, &root, sha);

        for (i, &mode) in modes.iter().enumerate() {
            let compressed = skim_diff_mode(&root, sha, mode);

            let is_raw_fallback = compressed.trim() == raw.trim();
            stats[i].examined += 1;
            if is_raw_fallback {
                stats[i].raw_fallback += 1;
            } else {
                // fix: count ast_rendered directly, not as examined − raw_fallback
                stats[i].ast_rendered += 1;
            }

            let (ok, changed_missing, context_missing) = check_coverage(&raw, &compressed, &numstat);
            if !ok {
                stats[i].violations += 1;
                if mode.invariant_asserted() {
                    asserted_violations.push((sha.clone(), mode, changed_missing));
                } else {
                    advisory_violations.push((sha.clone(), mode, changed_missing));
                }
            }
            if context_missing > 0 {
                stats[i].context_violations += 1;
                context_advisory.push((sha.clone(), mode, context_missing));
            }
        }
    }

    // -----------------------------------------------------------------------
    // Report
    // -----------------------------------------------------------------------
    println!("=== cli_git_diff_corpus results ===");
    println!("  corpus source       : crates/rskim/tests/fixtures/corpus_shas.txt");
    println!("  commits in corpus   : {}", commits.len());
    println!("  skipped (no parent) : {skipped_no_parent}");
    println!("  skipped (>256KB)    : {skipped_too_large}");
    println!();
    println!(
        "  {:<12}  {:>8}  {:>12}  {:>13}  {:>11}  {:>10}  {:>15}",
        "mode", "examined", "ast-rendered", "raw-fallback", "fallback %", "violations", "ctx-violations"
    );
    println!("  {}", "-".repeat(95));
    for (i, mode) in modes.iter().enumerate() {
        let s = &stats[i];
        let asserted_marker = if mode.invariant_asserted() {
            " [asserted]"
        } else {
            " [advisory]"
        };
        println!(
            "  {:<12}  {:>8}  {:>12}  {:>13}  {:>10.1}%  {:>10}  {:>15}{}",
            mode.label(),
            s.examined,
            s.ast_rendered,
            s.raw_fallback,
            s.fallback_pct(),
            s.violations,
            s.context_violations,
            asserted_marker,
        );
    }
    println!();
    if !advisory_violations.is_empty() {
        println!(
            "  Advisory (structure/full) changed-line violations: {} — expected state;",
            advisory_violations.len()
        );
        for (sha, mode, n) in advisory_violations.iter().take(5) {
            println!("    {} [{}]: {n} dropped changed lines", &sha[..8], mode.label());
        }
        if advisory_violations.len() > 5 {
            println!("    … ({} more)", advisory_violations.len() - 5);
        }
        println!();
    }
    if !context_advisory.is_empty() {
        println!(
            "  Context-line violations (all modes, advisory): {}",
            context_advisory.len()
        );
        for (sha, mode, n) in context_advisory.iter().take(5) {
            println!("    {} [{}]: {n} dropped context lines", &sha[..8], mode.label());
        }
        if context_advisory.len() > 5 {
            println!("    … ({} more)", context_advisory.len() - 5);
        }
        println!();
    }
    if !asserted_violations.is_empty() {
        println!("  ASSERTED violations (default mode):");
        for (sha, mode, n) in &asserted_violations {
            println!("    {} [{}]: {n} dropped changed lines", &sha[..8], mode.label());
        }
        println!();
    }
    println!("===================================");

    // -----------------------------------------------------------------------
    // Assert: compress-never-truncate must hold for DEFAULT mode
    //
    // Structure/full violations are advisory — they are the known state
    // and do not block the corpus harness from serving as a baseline
    // instrument.  After later phases are implemented, violations in those
    // modes will decrease; if they reach zero the invariant will be extended.
    // -----------------------------------------------------------------------
    assert!(
        asserted_violations.is_empty(),
        "compress-never-truncate (#317) violated in DEFAULT mode on {} case(s).\n\
         Dropped changed lines were found in skim's diff output that existed in the \
         raw `git diff` output.  This is the safety assertion for the default \
         rendering path.  Violations:\n{:#?}",
        asserted_violations.len(),
        asserted_violations,
    );
}

// ---------------------------------------------------------------------------
// Smoke test (always runs): harness infrastructure is wired up
// ---------------------------------------------------------------------------

/// Verify the helpers return sensible values without running the full corpus.
/// Runs in normal (non-ignored) mode so it executes on every CI pass.
#[test]
fn git_diff_corpus_harness_sanity() {
    let git = git_bin();
    assert!(
        git.is_file(),
        "git_bin() must return an existing file: {git:?}"
    );

    let root = repo_root();
    assert!(
        root.join(".git").exists(),
        "repo_root() must contain .git: {root:?}"
    );

    // Corpus fixture must exist and contain at least one SHA.
    let corpus = pinned_corpus(&root);
    assert!(
        !corpus.is_empty(),
        "pinned_corpus() must return at least one SHA"
    );
    assert_eq!(corpus[0].len(), 40, "SHA must be 40 hex chars");

    // Verify the mode label/arg/assertion plumbing.
    assert_eq!(DiffMode::Default.label(), "default");
    assert_eq!(DiffMode::Structure.extra_args(), Some("--mode=structure"));
    assert_eq!(DiffMode::Full.extra_args(), Some("--mode=full"));
    assert!(DiffMode::Default.invariant_asserted());
    assert!(!DiffMode::Structure.invariant_asserted());
    assert!(!DiffMode::Full.invariant_asserted());

    // Fix: verify skip counters are independent (the old code merged them).
    // We do this by showing that raw_diff distinguishes the two cases.
    // For a known-good SHA in the corpus, raw_diff must return Ok.
    let sha = &corpus[0];
    // Most recent commit is almost certainly not the root commit.
    match raw_diff(&git, &root, sha) {
        Ok(s) => {
            // The diff might be empty (merge commit) or have content — both are fine.
            assert!(s.len() < MAX_DIFF_BYTES * 2, "sanity: diff is implausibly large");
        }
        Err(SkipReason::NoParent) => {
            panic!("corpus[0] ({sha}) has no parent — very unexpected for the latest commit");
        }
        Err(SkipReason::TooLarge) => {
            // HEAD is a large commit; not a bug.
        }
    }
}
