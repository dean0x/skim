//! Noise rules for `skim heatmap`: support minimums, the fix-risk lift gate,
//! the bus-factor gate, the test and doc path classes, and the insight
//! thresholds.
//!
//! Pure functions over counts and path strings, std only. The views and the
//! insight engine call these rules, so no other module keeps its own copy of
//! a gate or a threshold. Exclude patterns are not noise rules: they stay in
//! `excludes.rs`.

use std::path::Path;

// ============================================================================
// Support minimums and caps
// ============================================================================

/// Minimum commits that touch a file before its fix-risk or bus-factor
/// signal shows. Below it, `files[]` still carries the raw rates.
pub(crate) const MIN_FILE_SUPPORT: usize = 5;

/// Minimum commits that a pair of files shares before the pair shows as
/// coupled. Prevents noise from one-off coincidences.
pub(crate) const MIN_PAIR_SUPPORT: usize = 3;

/// Minimum commits that touch a module before its encapsulation shows, in
/// the module section and as an insight. It equals the file minimum: with
/// fewer commits, one cross-boundary commit moves the share by 25 points or
/// more.
pub(crate) const MODULE_MIN_COMMITS: usize = 5;

/// Maximum number of files in a commit that coupling pairs.
///
/// A commit with more files (for example a large reformat) adds to neither
/// the pair counts nor the weighted totals, so the same commits make the
/// numerator and the denominator of every confidence. The cap also bounds
/// the pair work at 50 * 49 = 2450 ordered pairs per commit.
pub(crate) const COUPLING_MAX_FILES: usize = 50;

/// Minimum distinct authors in the window before any bus-factor row shows.
/// With one author, every file has a 100% top share, which tells nothing.
pub(crate) const MIN_WINDOW_AUTHORS: usize = 2;

/// Top-author share of the commits of a file, in percent, above which the
/// file is a bus-factor risk.
pub(crate) const BUS_FACTOR_TOP_SHARE_PCT: f64 = 80.0;

/// Normal quantile for a two-sided 95% interval.
const WILSON_Z: f64 = 1.96;

/// File extensions (case-insensitive) that mark a doc file.
const DOC_EXTENSIONS: &[&str] = &["md", "mdx", "markdown", "rst", "txt", "adoc"];

// ============================================================================
// Insight thresholds
// ============================================================================

/// Stability score below which a file is CRITICAL.
pub(crate) const STABILITY_CRITICAL: u8 = 40;
/// Stability score below which a file is a WARNING (but above critical).
pub(crate) const STABILITY_WARNING: u8 = 70;

/// Fix-risk combined_pct above which a file is CRITICAL.
pub(crate) const FIX_RISK_CRITICAL: f64 = 50.0;
/// Fix-risk combined_pct above which a file is a WARNING (but below critical).
pub(crate) const FIX_RISK_WARNING: f64 = 30.0;

/// Encapsulation_pct below which a module is CRITICAL.
pub(crate) const ENCAPSULATION_CRITICAL: f64 = 40.0;
/// Encapsulation_pct below which a module is a WARNING (but above critical).
pub(crate) const ENCAPSULATION_WARNING: f64 = 60.0;

/// Coupling confidence above which a coupling pair is a WARNING.
pub(crate) const COUPLING_WARNING: f64 = 0.8;

// ============================================================================
// Fix-risk lift gate
// ============================================================================

/// Lower bound of the Wilson score interval (z = 1.96) for `successes` of
/// `trials`, in `0.0..=1.0`.
///
/// Zero trials give 0.0. A `successes` above `trials` is clamped to `trials`.
#[must_use]
pub(crate) fn wilson_lower_bound(successes: usize, trials: usize) -> f64 {
    debug_assert!(
        successes <= trials,
        "successes {successes} > trials {trials}"
    );
    if trials == 0 {
        return 0.0;
    }
    let n = trials as f64;
    let p = successes.min(trials) as f64 / n;
    let z2 = WILSON_Z * WILSON_Z;
    let centre = p + z2 / (2.0 * n);
    let spread = WILSON_Z * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt();
    ((centre - spread) / (1.0 + z2 / n)).clamp(0.0, 1.0)
}

/// Fix commits divided by all commits in the window after exclusion, in
/// `0.0..=1.0`. An empty window gives 0.0.
#[must_use]
pub(crate) fn base_rate(fix_commits: usize, commits: usize) -> f64 {
    debug_assert!(
        fix_commits <= commits,
        "fix commits {fix_commits} > commits {commits}"
    );
    if commits == 0 {
        return 0.0;
    }
    fix_commits.min(commits) as f64 / commits as f64
}

/// True when the base rate is 0% or 100% (or not a number). No file can then
/// rise above the base rate in a way that tells something, so the fix-risk
/// section shows one reason line and no rows.
#[must_use]
pub(crate) fn base_rate_is_degenerate(base_rate: f64) -> bool {
    !(base_rate > 0.0 && base_rate < 1.0)
}

/// The fix-risk gate. True only when the file has [`MIN_FILE_SUPPORT`] or
/// more commits, the base rate is not degenerate, and the Wilson lower bound
/// of the fix rate of the file is above the base rate.
#[must_use]
pub(crate) fn passes_lift(fix_commits: usize, support: usize, base_rate: f64) -> bool {
    support >= MIN_FILE_SUPPORT
        && !base_rate_is_degenerate(base_rate)
        && wilson_lower_bound(fix_commits, support) > base_rate
}

/// The fix rate of the file divided by the base rate. Zero support or a base
/// rate that is not above zero gives 0.0.
#[must_use]
pub(crate) fn lift(fix_commits: usize, support: usize, base_rate: f64) -> f64 {
    if support == 0 || base_rate <= 0.0 || base_rate.is_nan() {
        return 0.0;
    }
    (fix_commits.min(support) as f64 / support as f64) / base_rate
}

// ============================================================================
// Bus-factor gate
// ============================================================================

/// The bus-factor gate. True only when the window has [`MIN_WINDOW_AUTHORS`]
/// or more distinct authors, the file has [`MIN_FILE_SUPPORT`] or more
/// commits, and the top author holds more than [`BUS_FACTOR_TOP_SHARE_PCT`]
/// percent of them.
#[must_use]
pub(crate) fn passes_bus_gate(
    window_authors: usize,
    file_support: usize,
    top_share_pct: f64,
) -> bool {
    window_authors >= MIN_WINDOW_AUTHORS
        && file_support >= MIN_FILE_SUPPORT
        && top_share_pct > BUS_FACTOR_TOP_SHARE_PCT
}

// ============================================================================
// Path classes
// ============================================================================

/// True when `path` is a test file: a `tests/`, `test/` or `__tests__/`
/// directory at any depth, or a file name of the form `*_test.*`, `*.test.*`,
/// `*.spec.*` or `test_*.py`. Paths are repo-relative with `/` separators.
#[must_use]
pub(crate) fn is_test_path(path: &str) -> bool {
    let (dirs, name) = path.rsplit_once('/').unwrap_or(("", path));
    dirs.split('/')
        .any(|d| matches!(d, "tests" | "test" | "__tests__"))
        || name.contains("_test.")
        || name.contains(".test.")
        || name.contains(".spec.")
        || (name.starts_with("test_") && name.ends_with(".py"))
}

/// True when `path` is a doc file: its extension is `md`, `mdx`, `markdown`,
/// `rst`, `txt` or `adoc`, in any letter case.
///
/// git writes a path with unusual characters in quotes (`"docs/caf\303\251.md"`).
/// Such a path ends in `"`, so the extension is read before that quote. git
/// quotes every path that holds a `"`, so an unquoted path never ends in one.
#[must_use]
pub(crate) fn is_doc_path(path: &str) -> bool {
    Path::new(path.strip_suffix('"').unwrap_or(path))
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| DOC_EXTENSIONS.iter().any(|d| ext.eq_ignore_ascii_case(d)))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-12;

    // -----------------------------------------------------------------------
    // wilson_lower_bound
    // -----------------------------------------------------------------------

    #[test]
    fn test_wilson_lower_bound_zero_trials_is_zero() {
        assert_eq!(wilson_lower_bound(0, 0), 0.0);
    }

    #[test]
    fn test_wilson_lower_bound_five_of_five_matches_hand_value() {
        // At p = 1 the bound reduces to n / (n + z^2) = 5 / (5 + 1.96^2).
        let expected = 5.0 / (5.0 + 1.96 * 1.96);
        let got = wilson_lower_bound(5, 5);
        assert!((got - 0.565_508_505_247_919).abs() < EPS, "got {got}");
        assert!(
            (got - expected).abs() < EPS,
            "got {got}, expected {expected}"
        );
    }

    #[test]
    fn test_wilson_lower_bound_zero_of_five_is_zero_not_negative() {
        let got = wilson_lower_bound(0, 5);
        assert!((0.0..EPS).contains(&got), "got {got}");
    }

    // -----------------------------------------------------------------------
    // base_rate, passes_lift, lift
    // -----------------------------------------------------------------------

    #[test]
    fn test_base_rate_is_fix_commits_over_commits() {
        assert!((base_rate(4, 20) - 0.2).abs() < EPS);
        assert_eq!(base_rate(0, 0), 0.0);
    }

    #[test]
    fn test_passes_lift_needs_five_commits() {
        // Base rate 20%. A file with 4 of 4 fix commits has a Wilson bound of
        // 4 / (4 + 1.96^2) = 0.5101, above the base rate, but support 4 is
        // below the minimum: no row. The same file with 5 of 5 gets a row.
        let base = 0.2;
        assert!(wilson_lower_bound(4, 4) > base);
        assert!(!passes_lift(4, 4, base), "support 4 must not pass");
        assert!(passes_lift(5, 5, base), "support 5 above base must pass");
    }

    #[test]
    fn test_passes_lift_needs_bound_above_base_rate() {
        // 1 of 5 has a Wilson bound of about 0.036: below a 20% base rate.
        assert!(!passes_lift(1, 5, 0.2));
    }

    #[test]
    fn test_passes_lift_false_at_degenerate_base_rate() {
        assert!(base_rate_is_degenerate(0.0));
        assert!(base_rate_is_degenerate(1.0));
        assert!(base_rate_is_degenerate(f64::NAN));
        assert!(!base_rate_is_degenerate(0.2));
        assert!(!passes_lift(5, 5, 0.0), "0% base rate gives no rows");
        assert!(!passes_lift(5, 5, 1.0), "100% base rate gives no rows");
    }

    #[test]
    fn test_lift_row_values_match_hand_computed() {
        // Known history: 20 commits after exclusion, 4 of them fix commits.
        // One file: 5 commits, 3 of them fix commits.
        let base = base_rate(4, 20);
        assert!((base - 0.2).abs() < EPS, "base rate {base}");
        // Wilson bound for 3 of 5, worked by hand: 0.230720 > 0.2.
        let bound = wilson_lower_bound(3, 5);
        assert!(
            (bound - 0.230_719_932_208_837).abs() < 1e-9,
            "bound {bound}"
        );
        assert!(passes_lift(3, 5, base));
        // Lift = (3 / 5) / 0.2 = 3.0.
        let got = lift(3, 5, base);
        assert!((got - 3.0).abs() < 1e-9, "lift {got}");
    }

    #[test]
    fn test_lift_zero_support_or_zero_base_is_zero() {
        assert_eq!(lift(0, 0, 0.2), 0.0);
        assert_eq!(lift(3, 5, 0.0), 0.0);
    }

    // -----------------------------------------------------------------------
    // passes_bus_gate
    // -----------------------------------------------------------------------

    #[test]
    fn test_passes_bus_gate_needs_two_authors_five_commits_and_top_share() {
        assert!(!passes_bus_gate(1, 10, 100.0), "one-author window");
        assert!(!passes_bus_gate(2, 4, 100.0), "support below minimum");
        assert!(!passes_bus_gate(2, 5, 80.0), "share at 80% is not above it");
        assert!(passes_bus_gate(2, 5, 80.1));
    }

    // -----------------------------------------------------------------------
    // is_test_path, is_doc_path
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_test_path_matches_test_dirs_and_names() {
        let tests = [
            "tests/cli.rs",
            "crates/rskim/tests/cli_heatmap.rs",
            "test/a.c",
            "src/__tests__/a.ts",
            "pkg/foo_test.go",
            "web/a.test.ts",
            "web/a.spec.js",
            "py/test_util.py",
            "test_util.py",
        ];
        for p in tests {
            assert!(is_test_path(p), "{p} must be a test path");
        }
    }

    #[test]
    fn test_is_test_path_rejects_other_paths() {
        let others = [
            "src/main.rs",
            "src/testing.rs",
            "src/contest.rs",
            "src/latest/a.rs",
            "tests",
            "test_util.rs",
            "src/attest_x.py",
            "src/test.rs",
        ];
        for p in others {
            assert!(!is_test_path(p), "{p} must not be a test path");
        }
    }

    #[test]
    fn test_is_doc_path_matches_doc_extensions() {
        let docs = [
            "README.md",
            "docs/guide.mdx",
            "a.markdown",
            "b.rst",
            "notes.txt",
            "c.adoc",
            "UPPER.MD",
        ];
        for p in docs {
            assert!(is_doc_path(p), "{p} must be a doc path");
        }
        let code = ["src/main.rs", "Cargo.toml", "md", "docs/md/file.rs"];
        for p in code {
            assert!(!is_doc_path(p), "{p} must not be a doc path");
        }
    }

    #[test]
    fn test_is_doc_path_reads_a_git_quoted_path() {
        assert!(is_doc_path(r#""docs/caf\303\251.md""#));
        assert!(is_doc_path(r#""docs/a\tb.RST""#));
        assert!(!is_doc_path(r#""src/caf\303\251.rs""#));
        assert!(!is_doc_path(r#""docs/caf\303\251.md/x.rs""#));
    }
}
