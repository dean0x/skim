//! Default exclusion patterns and GlobSet construction for `skim heatmap`.

use globset::{Glob, GlobSet, GlobSetBuilder};

use super::metrics::without_quotes;

// ============================================================================
// Default exclusion patterns
// ============================================================================

/// Hardcoded patterns for files that should be excluded from heatmap analysis
/// by default (lock files, build artifacts, minified bundles, logs, etc.).
///
/// The patterns match paths only. A file marked "generated" only in a header
/// line, in a folder not named `generated`, stays in.
pub(crate) const DEFAULT_EXCLUDES: &[&str] = &[
    // Lock files
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "poetry.lock",
    "Pipfile.lock",
    "composer.lock",
    "Gemfile.lock",
    "go.sum",
    "*.lock",
    // Build / dist artifacts
    "dist/**",
    "build/**",
    "target/**",
    "out/**",
    ".next/**",
    ".nuxt/**",
    "__pycache__/**",
    "*.egg-info/**",
    // Minified / generated bundles
    "*.min.js",
    "*.min.css",
    "*.bundle.js",
    "*.chunk.js",
    // Log files
    "*.log",
    "logs/**",
    // Vendored dependencies
    "vendor/**",
    "node_modules/**",
    // IDE / OS metadata
    ".idea/**",
    ".vscode/**",
    ".DS_Store",
    // Coverage / test artifacts
    "coverage/**",
    ".nyc_output/**",
    // Migration generated files
    "migrations/**/*.sql",
    // Changelogs, test snapshots and generated code, at any depth
    "**/CHANGELOG.md",
    "**/*.snap",
    "**/snapshots/**",
    "**/*.pb.go",
    "**/*_pb2.py",
    "**/generated/**",
];

// ============================================================================
// GlobSet construction
// ============================================================================

/// Build a [`GlobSet`] from the default excludes plus any extra patterns.
///
/// Returns an empty set when `no_exclude` is `true` so that no files are
/// filtered out.
pub(crate) fn build_exclude_set(no_exclude: bool, extra: &[String]) -> GlobSet {
    if no_exclude {
        return GlobSet::empty();
    }

    let mut builder = GlobSetBuilder::new();

    for pattern in DEFAULT_EXCLUDES {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
        }
    }

    for pattern in extra {
        if let Ok(glob) = Glob::new(pattern) {
            builder.add(glob);
        }
    }

    builder.build().unwrap_or_else(|_| GlobSet::empty())
}

/// Return `true` when `path` matches any pattern in `set`.
///
/// `path` is in the form of git's numstat output. git writes a path with
/// unusual bytes in double quotes, with C and octal escapes; the outer
/// quotes would hide each extension and file name, and the escapes each
/// non-ASCII name, from the patterns. So a quoted path is tested as the
/// path that it names, or without its quotes when it does not decode.
pub(crate) fn should_exclude(path: &str, set: &GlobSet) -> bool {
    let (inner, quoted) = without_quotes(path);
    if !quoted {
        return set.is_match(path);
    }
    match super::disk_path(path) {
        Some(named) => set.is_match(named),
        None => set.is_match(inner),
    }
}

/// Return `true` when a numstat line represents a binary file.
///
/// git numstat uses `-` for both additions and deletions when the file is binary.
#[allow(dead_code)]
pub(crate) fn is_binary_marker(additions: &str, deletions: &str) -> bool {
    additions == "-" && deletions == "-"
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_file_excluded_by_default() {
        let set = build_exclude_set(false, &[]);
        assert!(should_exclude("Cargo.lock", &set));
        assert!(should_exclude("package-lock.json", &set));
        assert!(should_exclude("yarn.lock", &set));
    }

    #[test]
    fn test_build_dir_excluded_by_default() {
        let set = build_exclude_set(false, &[]);
        assert!(should_exclude("dist/main.js", &set));
        assert!(should_exclude("target/debug/foo", &set));
    }

    #[test]
    fn test_minified_excluded() {
        let set = build_exclude_set(false, &[]);
        assert!(should_exclude("app.min.js", &set));
        assert!(should_exclude("styles.min.css", &set));
    }

    #[test]
    fn test_source_file_not_excluded() {
        let set = build_exclude_set(false, &[]);
        assert!(!should_exclude("src/main.rs", &set));
        assert!(!should_exclude("lib/utils.ts", &set));
    }

    #[test]
    fn test_no_exclude_skips_all_patterns() {
        let set = build_exclude_set(true, &[]);
        assert!(!should_exclude("Cargo.lock", &set));
        assert!(!should_exclude("dist/main.js", &set));
    }

    /// Paths of changelogs, snapshots and generated code, at the root and nested.
    const CHANGELOG_SNAPSHOT_GENERATED: &[&str] = &[
        "CHANGELOG.md",
        "crates/rskim/CHANGELOG.md",
        "view.snap",
        "src/__snapshots__/view.snap",
        "snapshots/case.txt",
        "tests/snapshots/case.txt",
        "foo.pb.go",
        "api/v1/foo.pb.go",
        "foo_pb2.py",
        "py/pkg/foo_pb2.py",
        "generated/types.rs",
        "src/generated/deep/types.rs",
    ];

    #[test]
    fn test_changelog_snapshot_and_generated_paths_excluded_by_default() {
        let set = build_exclude_set(false, &[]);
        let kept: Vec<&str> = CHANGELOG_SNAPSHOT_GENERATED
            .iter()
            .copied()
            .filter(|p| !should_exclude(p, &set))
            .collect();
        assert!(kept.is_empty(), "not excluded: {kept:?}");
    }

    #[test]
    fn test_no_exclude_keeps_changelog_snapshot_and_generated_paths() {
        let set = build_exclude_set(true, &[]);
        let dropped: Vec<&str> = CHANGELOG_SNAPSHOT_GENERATED
            .iter()
            .copied()
            .filter(|p| should_exclude(p, &set))
            .collect();
        assert!(dropped.is_empty(), "excluded: {dropped:?}");
    }

    /// Excludes match paths only. A file with a "generated" marker only in
    /// its header, or with "generated" only in its file name, stays in.
    #[test]
    fn test_generated_marker_or_name_outside_generated_folder_not_excluded() {
        let set = build_exclude_set(false, &[]);
        for path in [
            "src/schema.rs",
            "src/generated.rs",
            "src/generated_types.go",
        ] {
            assert!(!should_exclude(path, &set), "{path} must stay in");
        }
    }

    #[test]
    fn test_extra_excludes_applied() {
        let extra = vec!["*.generated.ts".to_string()];
        let set = build_exclude_set(false, &extra);
        assert!(should_exclude("foo.generated.ts", &set));
        assert!(!should_exclude("foo.ts", &set));
    }

    /// Paths that git writes in quotes, with octal escapes, for a non-ASCII
    /// name: one or more for each name, extension and folder pattern of the
    /// list, nested and at the root.
    const QUOTED_EXCLUDED: &[&str] = &[
        r#""pkg/caf\303\251/CHANGELOG.md""#,
        r#""src/caf\303\251.snap""#,
        r#""caf\303\251/snapshots/case.txt""#,
        r#""snapshots/caf\303\251.txt""#,
        r#""api/caf\303\251.pb.go""#,
        r#""py/caf\303\251_pb2.py""#,
        r#""src/caf\303\251/generated/types.rs""#,
        r#""generated/caf\303\251.rs""#,
        r#""caf\303\251.lock""#,
        r#""web/caf\303\251.min.js""#,
        r#""dist/caf\303\251.js""#,
    ];

    #[test]
    fn test_default_excludes_match_paths_that_git_quotes() {
        let set = build_exclude_set(false, &[]);
        let kept: Vec<&str> = QUOTED_EXCLUDED
            .iter()
            .copied()
            .filter(|p| !should_exclude(p, &set))
            .collect();
        assert!(kept.is_empty(), "not excluded: {kept:?}");
        assert!(!should_exclude(r#""src/caf\303\251.rs""#, &set));
    }

    #[test]
    fn test_extra_excludes_match_paths_that_git_quotes() {
        let extra = vec!["*.generated.ts".to_string(), "pkg/café/**".to_string()];
        let set = build_exclude_set(false, &extra);
        assert!(should_exclude(r#""src/caf\303\251.generated.ts""#, &set));
        assert!(should_exclude(r#""pkg/caf\303\251/main.rs""#, &set));
        assert!(!should_exclude(r#""src/caf\303\251.ts""#, &set));
    }

    #[test]
    fn test_is_binary_marker_true() {
        assert!(is_binary_marker("-", "-"));
    }

    #[test]
    fn test_is_binary_marker_false() {
        assert!(!is_binary_marker("0", "0"));
        assert!(!is_binary_marker("-", "0"));
        assert!(!is_binary_marker("10", "-"));
    }
}
