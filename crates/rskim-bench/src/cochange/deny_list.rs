//! Deny-list for files that should be excluded from co-change analysis.
//!
//! Lock files, vendored directories, and machine-generated outputs are excluded
//! because they produce false co-change signal: they change alongside many
//! unrelated commits and do not reflect meaningful coupling between source files.

use rskim_search::FileChangeInfo;

// ============================================================================
// Deny-list constants
// ============================================================================

/// File names that are always excluded, regardless of directory.
const DENIED_FILENAMES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "go.sum",
    "poetry.lock",
    "pnpm-lock.yaml",
    "Pipfile.lock",
    "Gemfile.lock",
    "composer.lock",
    "flake.lock",
];

/// Directory path components that trigger exclusion.
///
/// A file is excluded if any component of its path starts with one of these
/// prefixes (trailing `/` is stripped before comparison).
const DENIED_DIRS: &[&str] = &[
    "vendor",
    "node_modules",
    "dist",
    "build",
    "target",
    ".git",
    "__pycache__",
    ".tox",
];

/// File extension suffixes that indicate machine-generated content.
///
/// Checked against the full filename so `pb.go` matches but `pub.go` does not.
const DENIED_EXTENSIONS: &[&str] = &["min.js", "min.css", "pb.go", "generated.go"];

// ============================================================================
// Public API
// ============================================================================

/// Return `true` when `path` matches any deny-list rule.
///
/// Checks (in order):
/// 1. File name matches a [`DENIED_FILENAMES`] entry (exact match).
/// 2. Any path component matches a [`DENIED_DIRS`] entry.
/// 3. File name ends with a [`DENIED_EXTENSIONS`] suffix.
#[must_use]
pub fn is_denied(path: &str) -> bool {
    // Normalise to forward slashes so Windows paths also work.
    let normalised = path.replace('\\', "/");

    // Extract the file name (last segment after the final `/`).
    let filename = normalised.rsplit('/').next().unwrap_or(&normalised);

    // 1. Exact filename match.
    if DENIED_FILENAMES.contains(&filename) {
        return true;
    }

    // 2. Any directory component on the deny-list.
    //    Split on '/' and check every component except the last (filename).
    let components: Vec<&str> = normalised.split('/').collect();
    let dir_components = if components.len() > 1 {
        &components[..components.len() - 1]
    } else {
        &[][..]
    };
    for component in dir_components {
        let trimmed = component.trim_end_matches('/');
        if DENIED_DIRS.contains(&trimmed) {
            return true;
        }
    }

    // 3. Extension suffix match against the file name.
    for suffix in DENIED_EXTENSIONS {
        if filename.ends_with(suffix) {
            return true;
        }
    }

    false
}

/// Remove all denied files from `files` in-place.
///
/// Uses [`Vec::retain`] so the operation is O(n) and avoids a temporary
/// allocation.
pub fn filter_denied(files: &mut Vec<FileChangeInfo>) {
    files.retain(|f| {
        let path_str = f.path.to_string_lossy();
        !is_denied(&path_str)
    });
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fci(path: &str) -> FileChangeInfo {
        FileChangeInfo {
            path: PathBuf::from(path),
            additions: 1,
            deletions: 0,
        }
    }

    // --- Filename deny-list ---

    #[test]
    fn cargo_lock_is_denied() {
        assert!(is_denied("Cargo.lock"));
        assert!(is_denied("src/Cargo.lock"));
    }

    #[test]
    fn package_lock_json_is_denied() {
        assert!(is_denied("package-lock.json"));
        assert!(is_denied("frontend/package-lock.json"));
    }

    #[test]
    fn yarn_lock_is_denied() {
        assert!(is_denied("yarn.lock"));
    }

    #[test]
    fn go_sum_is_denied() {
        assert!(is_denied("go.sum"));
        assert!(is_denied("cmd/go.sum"));
    }

    #[test]
    fn poetry_lock_is_denied() {
        assert!(is_denied("poetry.lock"));
    }

    #[test]
    fn pnpm_lock_is_denied() {
        assert!(is_denied("pnpm-lock.yaml"));
    }

    #[test]
    fn pipfile_lock_is_denied() {
        assert!(is_denied("Pipfile.lock"));
    }

    #[test]
    fn gemfile_lock_is_denied() {
        assert!(is_denied("Gemfile.lock"));
    }

    #[test]
    fn composer_lock_is_denied() {
        assert!(is_denied("composer.lock"));
    }

    #[test]
    fn flake_lock_is_denied() {
        assert!(is_denied("flake.lock"));
    }

    // --- Directory deny-list ---

    #[test]
    fn vendor_dir_is_denied() {
        assert!(is_denied("vendor/github.com/pkg/errors/errors.go"));
    }

    #[test]
    fn node_modules_is_denied() {
        assert!(is_denied("node_modules/react/index.js"));
    }

    #[test]
    fn dist_dir_is_denied() {
        assert!(is_denied("dist/bundle.js"));
    }

    #[test]
    fn build_dir_is_denied() {
        assert!(is_denied("build/release/main.o"));
    }

    #[test]
    fn target_dir_is_denied() {
        assert!(is_denied("target/debug/rskim"));
    }

    #[test]
    fn git_dir_is_denied() {
        assert!(is_denied(".git/COMMIT_EDITMSG"));
    }

    #[test]
    fn pycache_is_denied() {
        assert!(is_denied("src/__pycache__/module.pyc"));
    }

    #[test]
    fn tox_dir_is_denied() {
        assert!(is_denied(".tox/py39/bin/pytest"));
    }

    // --- Extension deny-list ---

    #[test]
    fn min_js_is_denied() {
        assert!(is_denied("static/bundle.min.js"));
    }

    #[test]
    fn min_css_is_denied() {
        assert!(is_denied("static/style.min.css"));
    }

    #[test]
    fn pb_go_is_denied() {
        assert!(is_denied("proto/message.pb.go"));
    }

    #[test]
    fn generated_go_is_denied() {
        assert!(is_denied("gen/client.generated.go"));
    }

    // --- False-positive resistance ---

    #[test]
    fn clockwork_rs_is_not_denied() {
        assert!(!is_denied("src/clockwork.rs"));
    }

    #[test]
    fn src_lock_rs_is_not_denied() {
        assert!(!is_denied("src/lock.rs"));
    }

    #[test]
    fn normal_go_file_is_not_denied() {
        assert!(!is_denied("cmd/main.go"));
        assert!(!is_denied("internal/pub.go"));
    }

    #[test]
    fn cargo_toml_is_not_denied() {
        assert!(!is_denied("Cargo.toml"));
    }

    #[test]
    fn source_in_vendor_like_name_is_not_denied() {
        // "vendors" is NOT "vendor" — must be exact component match.
        assert!(!is_denied("vendors/utils.go"));
    }

    #[test]
    fn dist_in_name_is_not_denied() {
        // A file named "distribution.rs" in src/ should not be denied.
        assert!(!is_denied("src/distribution.rs"));
    }

    // --- filter_denied ---

    #[test]
    fn filter_denied_removes_lock_files() {
        let mut files = vec![
            fci("src/main.rs"),
            fci("Cargo.lock"),
            fci("src/lib.rs"),
            fci("package-lock.json"),
        ];
        filter_denied(&mut files);
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.path == PathBuf::from("src/main.rs")));
        assert!(files.iter().any(|f| f.path == PathBuf::from("src/lib.rs")));
    }

    #[test]
    fn filter_denied_keeps_all_allowed_files() {
        let mut files = vec![
            fci("src/auth.rs"),
            fci("tests/integration.rs"),
            fci("crates/core/src/lib.rs"),
        ];
        filter_denied(&mut files);
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn filter_denied_empty_input() {
        let mut files: Vec<FileChangeInfo> = vec![];
        filter_denied(&mut files);
        assert!(files.is_empty());
    }
}
