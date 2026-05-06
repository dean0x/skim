//! Default exclusion patterns and GlobSet construction for `skim heatmap`.

use globset::{Glob, GlobSet, GlobSetBuilder};

// ============================================================================
// Default exclusion patterns
// ============================================================================

/// Hardcoded patterns for files that should be excluded from heatmap analysis
/// by default (lock files, build artifacts, minified bundles, logs, etc.).
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
pub(crate) fn should_exclude(path: &str, set: &GlobSet) -> bool {
    set.is_match(path)
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

    #[test]
    fn test_extra_excludes_applied() {
        let extra = vec!["*.generated.ts".to_string()];
        let set = build_exclude_set(false, &extra);
        assert!(should_exclude("foo.generated.ts", &set));
        assert!(!should_exclude("foo.ts", &set));
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
