//! Subcommand registry for skim CLI.
//!
//! Provides the authoritative lists of known subcommands, meta/management
//! subcommands, and wrapper targets, along with the lookup functions used by
//! the dispatch router, shell completion, and wrapper installer.

use std::sync::LazyLock;

/// Known subcommands that the pre-parse router will recognize.
///
/// IMPORTANT: Only register subcommands we will actually implement.
/// Keep this list exact — no broad patterns. See GRANITE lesson #336.
///
/// v2.8.0: Flat dispatch — tool names are top-level subcommands.
///
/// NOTE: This array is NOT used by the dispatch router. Its current purposes are:
///   1. Shell completion candidates (completions subcommand)
///   2. Sync-guard test (`test_dispatch_covers_all_known_subcommands`) — asserts
///      every registered name reaches a match arm in `dispatch()` without panicking.
///
/// INVARIANT: must remain in ascending lexicographic order so that
/// `is_known_subcommand` can use `binary_search` — O(log n) instead of O(n).
/// The `test_known_subcommands_are_sorted` test enforces this.
pub(crate) const KNOWN_SUBCOMMANDS: &[&str] = &[
    "agents",      // meta: skim management
    "aws",         // infrastructure
    "biome",       // linter
    "black",       // linter
    "cargo",       // multi-category dispatcher
    "completions", // meta: skim management
    "curl",        // infrastructure
    "cypress",     // test runner
    "df",          // file operations
    "diff",        // file operations
    "dig",         // infrastructure
    "discover",    // meta: skim management
    "docker",      // infrastructure
    "dotnet",      // test runner / passthrough
    "dprint",      // linter
    "du",          // file operations
    "env",         // file operations
    "eslint",      // linter
    "find",        // file operations
    "gh",          // infrastructure
    "git",         // multi-category dispatcher
    "go",          // multi-category dispatcher
    "gofmt",       // linter
    "golangci",    // linter
    "gradle",      // build tool
    "gradlew",     // build tool
    "grep",        // file operations
    "heatmap",     // meta: skim management
    "init",        // meta: skim management
    "jest",        // test runner
    "kubectl",     // infrastructure
    "learn",       // meta: skim management
    "log",         // meta: skim management (log compression, not a system tool)
    "ls",          // file operations
    "make",        // build tool
    "mvn",         // build tool
    "mvnw",        // build tool
    "mypy",        // linter
    "mysql",       // database
    "npm",         // package manager
    "nslookup",    // infrastructure
    "oxlint",      // linter
    "pip",         // package manager
    "playwright",  // test runner
    "pnpm",        // package manager
    "prettier",    // linter
    "printenv",    // file operations
    "proxy",       // meta: skim Layer-3 HTTP reverse proxy (#303)
    "ps",          // file operations
    "psql",        // database
    "pytest",      // test runner
    "rewrite",     // meta: skim management
    "rg",          // file operations
    "rubocop",     // linter
    "ruff",        // linter
    "rustfmt",     // linter
    "search",      // meta: skim management
    "sqlite3",     // database
    "stats",       // meta: skim management
    "swift",       // test runner / passthrough
    "swiftlint",   // linter
    "terraform",   // infrastructure
    "tree",        // file operations
    "tsc",         // build tool
    "vitest",      // test runner
    "wc",          // file operations
    "wget",        // infrastructure
    "yarn",        // package manager
];

/// Meta/management subcommands that belong to skim itself.
///
/// These should NOT be wrapper targets in `~/.skim/bin/` because:
/// 1. They manage skim, not external tools — wrapping them would create confusing
///    recursive behaviour (e.g. `~/.skim/bin/init` would invoke `skim init`).
/// 2. A sub-agent invoking `init` is almost certainly invoking skim's own init,
///    not a tool named "init" — the wrapper would intercept incorrectly.
///
/// Everything in KNOWN_SUBCOMMANDS that is NOT in META_SUBCOMMANDS is a valid
/// wrapper target (i.e. it wraps an external tool of the same name).
///
/// SYNC NOTE: if you add a new meta subcommand to KNOWN_SUBCOMMANDS, add it
/// here too. The `test_meta_subcommands_are_in_known_subcommands` sync-guard
/// test will catch any entries that are in META but not in KNOWN.
///
/// ### Classification notes
///
/// - **`log`**: skim's own log-compression subcommand (pipes stdin through a
///   structured-log filter). There is no standard system tool called `log` that
///   agents invoke, so creating a `~/.skim/bin/log` symlink would be wrong.
///   META classification is intentional — it prevents the symlink from being
///   created by `skim init --wrappers`.
pub(crate) const META_SUBCOMMANDS: &[&str] = &[
    "agents",
    "completions",
    "discover",
    "heatmap",
    "init",
    "learn",
    "log",
    "proxy", // meta: skim Layer-3 HTTP reverse proxy (server, not a tool to intercept)
    "rewrite",
    "search",
    "stats",
];

/// Check whether `name` is a registered meta/management subcommand.
///
/// Meta subcommands are skim's own management commands and are NOT valid
/// wrapper targets (see [`META_SUBCOMMANDS`]).
///
/// Uses binary search because `META_SUBCOMMANDS` is sorted — O(log n) vs O(n)
/// for `.contains()`. At 10 entries the difference is negligible per call, but
/// this runs on every invocation via `detect_argv0_for()` so correctness of the
/// invariant matters more than the raw savings.
///
/// INVARIANT: `META_SUBCOMMANDS` must remain sorted. The
/// `test_meta_subcommands_are_sorted` test enforces this.
pub(crate) fn is_meta_subcommand(name: &str) -> bool {
    META_SUBCOMMANDS.binary_search(&name).is_ok()
}

/// Precomputed list of wrapper targets — computed once at first use via [`LazyLock`].
///
/// Filtering `KNOWN_SUBCOMMANDS` against `META_SUBCOMMANDS` on every
/// `wrapper_targets()` call allocates a new `Vec`. Since the result is
/// deterministic (both arrays are `'static` and never mutated), we pay the
/// allocation cost once and return a reference to it on every subsequent call.
static WRAPPER_TARGETS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    KNOWN_SUBCOMMANDS
        .iter()
        .filter(|&&name| !is_meta_subcommand(name))
        .copied()
        .collect()
});

/// Return the list of subcommand names that are valid wrapper targets.
///
/// These are all [`KNOWN_SUBCOMMANDS`] that are NOT in [`META_SUBCOMMANDS`].
/// Each name corresponds to an external tool that skim can compress output for.
///
/// Used by the wrapper installer to determine which symlinks to create in
/// `~/.skim/bin/`.
///
/// Returns a reference to a precomputed static list — no allocation on repeated
/// calls (see [`WRAPPER_TARGETS`]).
pub(crate) fn wrapper_targets() -> &'static [&'static str] {
    &WRAPPER_TARGETS
}

/// Check whether `name` is a registered subcommand.
///
/// Uses binary search because [`KNOWN_SUBCOMMANDS`] is sorted — O(log n) vs O(n)
/// for `.contains()`. Consistent with [`is_meta_subcommand`] which applies the
/// same pattern to [`META_SUBCOMMANDS`].
///
/// INVARIANT: `KNOWN_SUBCOMMANDS` must remain sorted. The
/// `test_known_subcommands_are_sorted` test enforces this.
pub(crate) fn is_known_subcommand(name: &str) -> bool {
    KNOWN_SUBCOMMANDS.binary_search(&name).is_ok()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // META_SUBCOMMANDS sync-guard tests
    // ========================================================================

    /// Every entry in META_SUBCOMMANDS must also exist in KNOWN_SUBCOMMANDS.
    #[test]
    fn test_meta_subcommands_are_in_known_subcommands() {
        for &meta in META_SUBCOMMANDS {
            assert!(
                is_known_subcommand(meta),
                "META_SUBCOMMANDS entry '{meta}' is not in KNOWN_SUBCOMMANDS — \
                 every meta subcommand must also be registered in KNOWN_SUBCOMMANDS"
            );
        }
    }

    /// wrapper_targets() must not contain any meta subcommands.
    #[test]
    fn test_wrapper_targets_contains_no_meta_subcommands() {
        let targets = wrapper_targets();
        for &meta in META_SUBCOMMANDS {
            assert!(
                !targets.contains(&meta),
                "wrapper_targets() returned meta subcommand '{meta}' — \
                 meta subcommands must not be wrapper targets"
            );
        }
    }

    /// wrapper_targets() length must equal KNOWN minus META.
    #[test]
    fn test_wrapper_targets_count_equals_known_minus_meta() {
        let targets = wrapper_targets();
        let expected_len = KNOWN_SUBCOMMANDS.len() - META_SUBCOMMANDS.len();
        assert_eq!(
            targets.len(),
            expected_len,
            "wrapper_targets() has {} entries but expected {} \
             (KNOWN={} minus META={})",
            targets.len(),
            expected_len,
            KNOWN_SUBCOMMANDS.len(),
            META_SUBCOMMANDS.len()
        );
    }

    /// is_meta_subcommand() returns true for every meta subcommand.
    #[test]
    fn test_is_meta_subcommand_for_all_meta() {
        for &meta in META_SUBCOMMANDS {
            assert!(
                is_meta_subcommand(meta),
                "is_meta_subcommand('{meta}') returned false — must return true"
            );
        }
    }

    /// is_meta_subcommand() returns false for known tool wrappers.
    #[test]
    fn test_is_meta_subcommand_false_for_tool_wrappers() {
        for &name in &["git", "cargo", "npm", "grep", "find"] {
            assert!(
                !is_meta_subcommand(name),
                "is_meta_subcommand('{name}') returned true — tool wrappers must return false"
            );
        }
    }

    /// META_SUBCOMMANDS must remain sorted so that `is_meta_subcommand` can use
    /// binary search instead of a linear scan.
    #[test]
    fn test_meta_subcommands_are_sorted() {
        let mut sorted = META_SUBCOMMANDS.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            META_SUBCOMMANDS,
            sorted.as_slice(),
            "META_SUBCOMMANDS is not sorted — binary_search in is_meta_subcommand() requires \
             the array to be in ascending lexicographic order"
        );
    }

    /// KNOWN_SUBCOMMANDS must remain sorted so that `is_known_subcommand` can use
    /// binary search instead of a linear scan.
    #[test]
    fn test_known_subcommands_are_sorted() {
        let mut sorted = KNOWN_SUBCOMMANDS.to_vec();
        sorted.sort_unstable();
        assert_eq!(
            KNOWN_SUBCOMMANDS,
            sorted.as_slice(),
            "KNOWN_SUBCOMMANDS is not sorted — binary_search in is_known_subcommand() requires \
             the array to be in ascending lexicographic order"
        );
    }
}
