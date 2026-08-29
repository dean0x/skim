//! Git-history corpus harness for diff compress-never-truncate (#317).
//!
//! # Purpose
//!
//! Walks the real git history of this repository and asserts that skim's
//! DEFAULT-mode diff output never drops `+` or `-` lines that appear in the
//! raw diff.  This is the operationalisation of invariant #317 ("compress,
//! never truncate") for the default-mode diff code path.
//!
//! Additionally tracks per-mode fallback rates and violation counts for the
//! `structure` and `full` modes — as informational (no assertion) because
//! those modes apply AST compression to the diff and intentionally compact
//! content: the resulting pre-B3 violations are KNOWN and are what B3 is
//! designed to cure.  Asserting on them before B3 would make the corpus test
//! permanently broken.
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
//! (not `skim git diff`) AND with `SKIM_PASSTHROUGH=1` to prevent the rewrite
//! hook from wrapping the control command itself.  A naive `git diff` call
//! inside a skim-hooked shell would produce skim-compressed output as its
//! "raw" baseline, making the comparison circular.
//!
//! # Usage
//!
//! This test is `#[ignore]` by default.  Run explicitly:
//!
//! ```text
//! SKIM_PASSTHROUGH=1 cargo nextest run -p rskim --all-targets -j 1 \
//!     -E 'binary(cli_git_diff_corpus)' --run-ignored ignored-only \
//!     --no-capture
//! ```
//!
//! The test never fails on CI in normal mode (ignored); it is intended for
//! local validation and periodic regression runs.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

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
    /// intentionally compacts content lines.  Pre-B3 violations in those modes
    /// are KNOWN and expected.  Asserting on them before B3 is implemented
    /// would make this harness permanently broken, defeating its purpose as a
    /// baseline measurement instrument.
    fn invariant_asserted(self) -> bool {
        matches!(self, Self::Default)
    }
}

// ---------------------------------------------------------------------------
// Per-mode stats accumulator
// ---------------------------------------------------------------------------

#[derive(Default, Debug)]
struct ModeStats {
    examined: usize,
    raw_fallback: usize,
    /// Lines-dropped violations.  For `Default`, this is asserted == 0.
    /// For `Structure`/`Full`, this is informational (pre-B3 expected state).
    violations: usize,
}

impl ModeStats {
    fn fallback_pct(&self) -> f64 {
        if self.examined == 0 {
            0.0
        } else {
            (self.raw_fallback as f64 / self.examined as f64) * 100.0
        }
    }

    fn ast_rendered(&self) -> usize {
        self.examined.saturating_sub(self.raw_fallback)
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

/// Return the N most-recent commit SHAs in the current branch.
fn recent_commits(root: &std::path::Path, limit: usize) -> Vec<String> {
    let out = Command::new("git")
        .args(["log", "--format=%H", &format!("-{limit}")])
        .current_dir(root)
        .output()
        .expect("git log must succeed");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Run `git diff <sha>^1 <sha>` via the REAL git binary with `SKIM_PASSTHROUGH=1`.
///
/// Returns `None` when the commit has no parent (initial commit) or the diff
/// exceeds `MAX_DIFF_BYTES`.
fn raw_diff(git: &std::path::Path, root: &std::path::Path, sha: &str) -> Option<String> {
    let out = Command::new(git)
        .args(["diff", &format!("{sha}^1"), sha])
        .current_dir(root)
        // PF-026: passthrough prevents the rewrite hook from compressing the control.
        .env("SKIM_PASSTHROUGH", "1")
        .output()
        .ok()?;

    if !out.status.success() {
        // Likely no parent (initial commit) — skip.
        return None;
    }
    if out.stdout.len() > MAX_DIFF_BYTES {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run `skim git diff <sha>^1 <sha>` in the given mode via the debug binary.
fn skim_diff_mode(root: &std::path::Path, sha: &str, mode: DiffMode) -> Option<String> {
    let skim = common::skim_bin();
    let mut cmd = Command::new(&skim);
    cmd.args(["git", "diff", &format!("{sha}^1"), sha])
        .current_dir(root)
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .env("NO_COLOR", "1");
    if let Some(flag) = mode.extra_args() {
        cmd.arg(flag);
    }
    let out = cmd.output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// Invariant checker
// ---------------------------------------------------------------------------

/// Check that every `+` / `-` content line from `raw` appears in `compressed`.
///
/// The compress-never-truncate invariant (#317) says skim may RE-ENCODE diff
/// output but must never drop information.  We test the weakest verifiable
/// form: every hunk prefix line (`+foo` / `-foo`) in the raw diff is present
/// as a substring somewhere in the compressed output OR the compressed output
/// is identical to the raw (raw-fallback path).
///
/// Returns `(ok, missing_count)`.
fn check_no_line_dropped(raw: &str, compressed: &str) -> (bool, usize) {
    if compressed.trim() == raw.trim() {
        return (true, 0); // raw fallback — invariant trivially holds
    }
    let mut missing = 0usize;
    for line in raw.lines() {
        if (line.starts_with('+') && !line.starts_with("+++"))
            || (line.starts_with('-') && !line.starts_with("---"))
        {
            let content = &line[1..];
            if !content.is_empty() && !compressed.contains(content) {
                missing += 1;
            }
        }
    }
    (missing == 0, missing)
}

// ---------------------------------------------------------------------------
// Main corpus test
// ---------------------------------------------------------------------------

#[test]
#[ignore = "corpus test: run with SKIM_PASSTHROUGH=1 --no-capture --run-ignored ignored-only; reads live git history"]
fn git_diff_corpus_compress_never_truncate() {
    let git = git_bin();
    let root = repo_root();
    let commits = recent_commits(&root, MAX_COMMITS);

    assert!(
        !commits.is_empty(),
        "git log must return at least one commit; got none (is this a git repo?)"
    );

    let modes = DiffMode::all();
    // stats[0] = default, stats[1] = structure, stats[2] = full
    let mut stats: [ModeStats; 3] = Default::default();
    let mut skipped_no_parent = 0usize;
    let skipped_too_large = 0usize;

    // Violations where the safety ASSERTION fires (default mode only).
    let mut asserted_violations: Vec<(String, DiffMode, usize)> = Vec::new();
    // Violations in structure/full — tracked informational, no assertion.
    let mut advisory_violations: Vec<(String, DiffMode, usize)> = Vec::new();

    for sha in &commits {
        let raw = match raw_diff(&git, &root, sha) {
            None => {
                skipped_no_parent += 1;
                continue;
            }
            Some(r) if r.is_empty() => {
                // Empty diff — count as examined but skip content checks.
                for s in &mut stats {
                    s.examined += 1;
                }
                continue;
            }
            Some(r) => r,
        };

        for (i, &mode) in modes.iter().enumerate() {
            let compressed = skim_diff_mode(&root, sha, mode)
                .unwrap_or_else(|| raw.clone()); // skim failure → treat as raw

            stats[i].examined += 1;
            if compressed.trim() == raw.trim() {
                stats[i].raw_fallback += 1;
            }

            let (ok, missing) = check_no_line_dropped(&raw, &compressed);
            if !ok {
                stats[i].violations += 1;
                if mode.invariant_asserted() {
                    asserted_violations.push((sha.clone(), mode, missing));
                } else {
                    advisory_violations.push((sha.clone(), mode, missing));
                }
            }
        }

        if skipped_no_parent + skipped_too_large + stats[0].examined >= MAX_COMMITS {
            break;
        }
    }

    // -----------------------------------------------------------------------
    // Report
    // -----------------------------------------------------------------------
    println!("=== cli_git_diff_corpus results ===");
    println!(
        "  commits in history  : {}",
        commits.len()
    );
    println!(
        "  skipped (no parent) : {skipped_no_parent}"
    );
    println!(
        "  skipped (>256KB)    : {skipped_too_large}"
    );
    println!();
    println!(
        "  {:<12}  {:>8}  {:>12}  {:>13}  {:>11}  {:>10}",
        "mode", "examined", "ast-rendered", "raw-fallback", "fallback %", "violations"
    );
    println!("  {}", "-".repeat(80));
    for (i, mode) in modes.iter().enumerate() {
        let s = &stats[i];
        let asserted_marker = if mode.invariant_asserted() { " [asserted]" } else { " [advisory]" };
        println!(
            "  {:<12}  {:>8}  {:>12}  {:>13}  {:>10.1}%  {:>10}{}",
            mode.label(),
            s.examined,
            s.ast_rendered(),
            s.raw_fallback,
            s.fallback_pct(),
            s.violations,
            asserted_marker,
        );
    }
    println!();
    if !advisory_violations.is_empty() {
        println!(
            "  Advisory (structure/full) violations: {} — expected pre-B3 state;",
            advisory_violations.len()
        );
        println!(
            "  these represent AST-compression dropping content lines, which B3 cures."
        );
        for (sha, mode, n) in advisory_violations.iter().take(10) {
            println!("    {} [{}]: {n} dropped lines", &sha[..8], mode.label());
        }
        if advisory_violations.len() > 10 {
            println!("    … ({} more)", advisory_violations.len() - 10);
        }
        println!();
    }
    if !asserted_violations.is_empty() {
        println!("  ASSERTED violations (default mode):");
        for (sha, mode, n) in &asserted_violations {
            println!("    {} [{}]: {n} dropped lines", &sha[..8], mode.label());
        }
        println!();
    }
    println!("===================================");

    // -----------------------------------------------------------------------
    // Assert: compress-never-truncate must hold for DEFAULT mode
    //
    // Structure/full violations are advisory — they are the pre-B3 known state
    // and do not block the corpus harness from serving as a baseline
    // instrument.  After B3 is implemented, violations in those modes will
    // decrease; if they reach zero the invariant will be extended.
    // -----------------------------------------------------------------------
    assert!(
        asserted_violations.is_empty(),
        "compress-never-truncate (#317) violated in DEFAULT mode on {} case(s).\n\
         Dropped lines were found in skim's diff output that existed in the \
         raw `git diff` output.  This is the safety assertion for the default \
         rendering path.  Violations:\n{:#?}",
        asserted_violations.len(),
        asserted_violations,
    );
}

// ---------------------------------------------------------------------------
// Smoke test (always runs): harness infrastructure is wired up
// ---------------------------------------------------------------------------

/// Verify the `git_bin()` / `repo_root()` / `recent_commits()` helpers return
/// sensible values without running the full corpus.  Runs in normal (non-ignored)
/// mode so it executes on every CI pass.
#[test]
fn git_diff_corpus_harness_sanity() {
    let git = git_bin();
    assert!(git.is_file(), "git_bin() must return an existing file: {git:?}");

    let root = repo_root();
    assert!(root.join(".git").exists(), "repo_root() must contain .git: {root:?}");

    let commits = recent_commits(&root, 1);
    assert!(!commits.is_empty(), "recent_commits must return at least one SHA");
    assert_eq!(commits[0].len(), 40, "SHA must be 40 hex chars");

    // Verify the mode label/arg/assertion plumbing.
    assert_eq!(DiffMode::Default.label(), "default");
    assert_eq!(DiffMode::Structure.extra_args(), Some("--mode=structure"));
    assert_eq!(DiffMode::Full.extra_args(), Some("--mode=full"));
    assert!(DiffMode::Default.invariant_asserted());
    assert!(!DiffMode::Structure.invariant_asserted());
    assert!(!DiffMode::Full.invariant_asserted());
}
