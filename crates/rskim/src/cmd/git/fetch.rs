//! Git fetch compression.
//!
//! Parses `git fetch` output (written to stderr by git) into a structured
//! summary: updated branches, new branches/tags, pruned refs, forced updates,
//! and submodule fetches. Progress/noise lines (`remote: ...`, `Unpacking ...`)
//! are stripped.

use std::collections::HashMap;
use std::process::ExitCode;

use crate::cmd::{extract_output_format, user_has_flag};
use crate::output::canonical::GitResult;

use super::{run_parsed_command, run_passthrough};

/// Run `git fetch` with output compression.
///
/// Flag-aware passthrough: `--dry-run`, `-q`, `--quiet` pass through unmodified.
///
/// Git fetch writes its ref updates to stderr, so `run_parsed_command` is
/// called with `combine_stderr: true` to merge stderr+stdout before parsing.
pub(super) fn run_fetch(
    global_flags: &[String],
    args: &[String],
    show_stats: bool,
) -> anyhow::Result<ExitCode> {
    if user_has_flag(args, &["--dry-run", "-q", "--quiet"]) {
        return run_passthrough(global_flags, "fetch", args, show_stats);
    }

    let (filtered_args, output_format) = extract_output_format(args);

    let mut full_args: Vec<String> = global_flags.to_vec();
    full_args.push("fetch".to_string());
    full_args.extend_from_slice(&filtered_args);

    run_parsed_command(&full_args, show_stats, output_format, true, parse_fetch)
}

// ============================================================================
// Parser
// ============================================================================

/// Accumulated, classified lines from a `git fetch` output pass.
///
/// Submodule sections use a `HashMap` for O(1) insertion (avoids O(n) linear
/// scan per call) and a separate ordered key list to preserve encounter order.
struct FetchCategories {
    remote: String,
    new_branches: Vec<String>,
    new_tags: Vec<String>,
    updated: Vec<String>,
    pruned: Vec<String>,
    forced: Vec<String>,
    /// Ordered submodule names (insertion order).
    submodule_order: Vec<String>,
    /// Per-submodule entries; keyed by submodule path.
    submodule_map: HashMap<String, Vec<String>>,
}

impl FetchCategories {
    fn new() -> Self {
        Self {
            remote: String::new(),
            new_branches: Vec::new(),
            new_tags: Vec::new(),
            updated: Vec::new(),
            pruned: Vec::new(),
            forced: Vec::new(),
            submodule_order: Vec::new(),
            submodule_map: HashMap::new(),
        }
    }

    /// Add an entry to a submodule section in O(1) time.
    fn add_submodule_entry(&mut self, sub: &str, entry: String) {
        let entries = self.submodule_map.entry(sub.to_string()).or_insert_with(|| {
            self.submodule_order.push(sub.to_string());
            Vec::new()
        });
        entries.push(entry);
    }

    /// Iterate submodule sections in the order they were first encountered.
    fn submodule_sections(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.submodule_order
            .iter()
            .filter_map(|name| self.submodule_map.get(name).map(|v| (name.as_str(), v.as_slice())))
    }
}

/// Classify all lines from `git fetch` output into `FetchCategories`.
///
/// Uses `next_from_is_submodule` to distinguish a submodule's `From` line
/// (which follows `Fetching submodule`) from a top-level `From` line (which
/// resets the submodule context). This prevents top-level refs from being
/// incorrectly attributed to the previously-active submodule.
fn classify_lines<'a>(lines: impl Iterator<Item = &'a str>) -> FetchCategories {
    let mut cats = FetchCategories::new();
    let mut current_submodule: Option<String> = None;
    // True immediately after a "Fetching submodule" line so the following
    // "From" is recognised as that submodule's remote, not a new top-level block.
    let mut next_from_is_submodule = false;

    for trimmed in lines.map(str::trim) {
        // Skip progress/noise lines
        if trimmed.starts_with("remote:")
            || trimmed.starts_with("Unpacking")
            || trimmed.is_empty()
        {
            continue;
        }

        // Submodule header
        if let Some(sub) = trimmed.strip_prefix("Fetching submodule ") {
            current_submodule = Some(sub.to_string());
            next_from_is_submodule = true;
            continue;
        }

        // From line
        if let Some(rest) = trimmed.strip_prefix("From ") {
            if next_from_is_submodule {
                // This "From" belongs to the submodule — preserve current_submodule.
                next_from_is_submodule = false;
            } else {
                // Top-level "From" — reset submodule context.
                current_submodule = None;
                if cats.remote.is_empty() {
                    cats.remote = rest.to_string();
                }
            }
            continue;
        }

        // New branch
        if trimmed.contains("[new branch]") {
            if let Some(name) = extract_ref_name(trimmed) {
                if let Some(ref sub) = current_submodule {
                    cats.add_submodule_entry(sub, format!("new branch: {name}"));
                } else {
                    cats.new_branches.push(name);
                }
            }
            continue;
        }

        // New tag
        if trimmed.contains("[new tag]") {
            if let Some(name) = extract_ref_name(trimmed) {
                cats.new_tags.push(name);
            }
            continue;
        }

        // Deleted/pruned
        if trimmed.contains("[deleted]") {
            if let Some(name) = extract_pruned_ref(trimmed) {
                cats.pruned.push(name);
            }
            continue;
        }

        // Forced update
        if trimmed.contains("(forced update)") {
            if let Some(name) = extract_updated_ref(trimmed) {
                cats.forced.push(name);
            }
            continue;
        }

        // Regular update (abc..def ref -> origin/ref)
        if trimmed.contains("->") && (trimmed.contains("..") || trimmed.contains("...")) {
            if let Some(name) = extract_updated_ref(trimmed) {
                if let Some(ref sub) = current_submodule {
                    cats.add_submodule_entry(sub, format!("updated: {name}"));
                } else {
                    cats.updated.push(name);
                }
            }
        }
    }

    cats
}

/// Build the detail lines from classified categories.
fn build_details(cats: &FetchCategories) -> Vec<String> {
    let mut details: Vec<String> = Vec::new();
    for b in &cats.new_branches {
        details.push(format!("+ {b} (new branch)"));
    }
    for t in &cats.new_tags {
        details.push(format!("+ {t} (new tag)"));
    }
    for u in &cats.updated {
        details.push(format!("~ {u}"));
    }
    for f in &cats.forced {
        details.push(format!("! {f} (forced)"));
    }
    for p in &cats.pruned {
        details.push(format!("- {p} (pruned)"));
    }
    for (sub_name, entries) in cats.submodule_sections() {
        details.push(format!("[submodule {sub_name}]"));
        for e in entries {
            details.push(format!("  {e}"));
        }
    }
    details
}

/// Parse combined stdout+stderr from `git fetch` into a compressed GitResult.
///
/// Git fetch writes its output to stderr. The parser handles:
/// - Updated branches/refs (`abc..def branch -> origin/branch`)
/// - New branches (`* [new branch] name -> origin/name`)
/// - New tags (`* [new tag] v1.0 -> v1.0`)
/// - Pruned refs (`- [deleted] (none) -> origin/old-branch`)
/// - Forced updates (`+ abc...def branch -> origin/branch (forced update)`)
/// - Submodule fetches (`Fetching submodule lib/core`)
/// - Progress/noise lines stripped (`remote: ...`, `Unpacking ...`)
fn parse_fetch(input: &str) -> GitResult {
    let lines: Vec<&str> = input.lines().collect();

    if lines.iter().all(|l| l.trim().is_empty()) {
        return GitResult::new("fetch".to_string(), "up to date".to_string(), Vec::new());
    }

    let cats = classify_lines(lines.iter().copied());

    // Build summary parts
    let mut parts: Vec<String> = Vec::new();
    if !cats.updated.is_empty() {
        parts.push(format!("{} updated", cats.updated.len()));
    }
    if !cats.new_branches.is_empty() {
        parts.push(format!(
            "{} new branch{}",
            cats.new_branches.len(),
            if cats.new_branches.len() == 1 { "" } else { "es" }
        ));
    }
    if !cats.new_tags.is_empty() {
        parts.push(format!(
            "{} new tag{}",
            cats.new_tags.len(),
            if cats.new_tags.len() == 1 { "" } else { "s" }
        ));
    }
    if !cats.pruned.is_empty() {
        parts.push(format!("{} pruned", cats.pruned.len()));
    }
    if !cats.forced.is_empty() {
        parts.push(format!("{} forced", cats.forced.len()));
    }

    if parts.is_empty() && cats.submodule_map.is_empty() {
        return GitResult::new("fetch".to_string(), "up to date".to_string(), Vec::new());
    }

    let details = build_details(&cats);
    let display_summary = build_summary(&cats.remote, &parts);

    GitResult::new("fetch".to_string(), display_summary, details)
}

fn build_summary(remote: &str, parts: &[String]) -> String {
    if parts.is_empty() {
        String::new()
    } else if remote.is_empty() {
        parts.join(", ")
    } else {
        format!("from {remote}: {}", parts.join(", "))
    }
}

// ============================================================================
// Ref extraction helpers
// ============================================================================

/// Extract the local ref name from a line like:
///   `* [new branch] feature/x -> origin/feature/x`
///   `* [new tag]    v2.3.0    -> v2.3.0`
fn extract_ref_name(line: &str) -> Option<String> {
    let arrow_pos = line.find("->")?;
    let target = line[arrow_pos + 2..].trim();
    // Strip "origin/" prefix if present
    let name = target.strip_prefix("origin/").unwrap_or(target);
    Some(name.to_string())
}

/// Extract the local ref name from an updated ref line like:
///   `abc1234..def5678 main -> origin/main`
///   `+ ccc3333...ddd4444 feature/z -> origin/feature/z  (forced update)`
fn extract_updated_ref(line: &str) -> Option<String> {
    let arrow_pos = line.find("->")?;
    let target = line[arrow_pos + 2..].trim();
    let target = match target.rfind('(') {
        Some(pos) => target[..pos].trim(),
        None => target,
    };
    let name = target.strip_prefix("origin/").unwrap_or(target);
    Some(name.to_string())
}

/// Extract the pruned ref name from a deleted line like:
///   `- [deleted] (none) -> origin/old-branch`
fn extract_pruned_ref(line: &str) -> Option<String> {
    let arrow_pos = line.find("->")?;
    let target = line[arrow_pos + 2..].trim();
    let name = target.strip_prefix("origin/").unwrap_or(target);
    Some(name.to_string())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let path = format!(
            "{}/tests/fixtures/cmd/git/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read fixture {path}: {e}"))
    }

    // ========================================================================
    // parse_fetch tests
    // ========================================================================

    #[test]
    fn test_parse_fetch_empty() {
        let result = parse_fetch("");
        assert_eq!(result.summary, "up to date");
        assert!(result.details.is_empty());
    }

    #[test]
    fn test_parse_fetch_whitespace_only() {
        let result = parse_fetch("   \n\n  \n");
        assert_eq!(result.summary, "up to date");
        assert!(result.details.is_empty());
    }

    #[test]
    fn test_parse_fetch_up_to_date_fixture() {
        let input = fixture("fetch_up_to_date.txt");
        let result = parse_fetch(&input);
        assert_eq!(result.summary, "up to date");
    }

    #[test]
    fn test_parse_fetch_with_updates() {
        let input = fixture("fetch_refs.txt");
        let result = parse_fetch(&input);
        // Should have: 2 updated, 2 new branches, 1 new tag
        assert!(
            result.summary.contains("2 updated"),
            "expected '2 updated' in summary, got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("2 new branches"),
            "expected '2 new branches' in summary, got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("1 new tag"),
            "expected '1 new tag' in summary, got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("github.com:user/repo"),
            "expected remote in summary"
        );
    }

    #[test]
    fn test_parse_fetch_progress_stripped() {
        let input = fixture("fetch_refs.txt");
        let result = parse_fetch(&input);
        // Progress lines must not appear in output
        let rendered = result.to_string();
        assert!(
            !rendered.contains("remote:"),
            "remote: progress lines must be stripped"
        );
        assert!(
            !rendered.contains("Unpacking"),
            "Unpacking lines must be stripped"
        );
    }

    #[test]
    fn test_parse_fetch_new_branches_in_details() {
        let input = fixture("fetch_refs.txt");
        let result = parse_fetch(&input);
        let details_str = result.details.join("\n");
        assert!(
            details_str.contains("feature/x") || details_str.contains("feature/y"),
            "expected new branch names in details, got: {details_str}"
        );
    }

    #[test]
    fn test_parse_fetch_new_branches_only() {
        let input = "From github.com:user/repo\n * [new branch]      feat/a     -> origin/feat/a\n * [new branch]      feat/b     -> origin/feat/b\n";
        let result = parse_fetch(input);
        assert!(
            result.summary.contains("2 new branches"),
            "expected '2 new branches', got: {}",
            result.summary
        );
        assert!(
            !result.summary.contains("updated"),
            "should not mention updated"
        );
    }

    #[test]
    fn test_parse_fetch_with_prune() {
        let input = fixture("fetch_with_prune.txt");
        let result = parse_fetch(&input);
        assert!(
            result.summary.contains("1 updated"),
            "expected '1 updated', got: {}",
            result.summary
        );
        assert!(
            result.summary.contains("2 pruned"),
            "expected '2 pruned', got: {}",
            result.summary
        );
        let details_str = result.details.join("\n");
        assert!(
            details_str.contains("old-branch") || details_str.contains("stale-feature"),
            "expected pruned branch names in details"
        );
    }

    #[test]
    fn test_parse_fetch_forced_update() {
        let input = fixture("fetch_forced.txt");
        let result = parse_fetch(&input);
        assert!(
            result.summary.contains("1 forced"),
            "expected '1 forced', got: {}",
            result.summary
        );
        let details_str = result.details.join("\n");
        assert!(
            details_str.contains("feature/z"),
            "expected forced branch name in details, got: {details_str}"
        );
        assert!(
            details_str.contains("forced"),
            "expected 'forced' label in details"
        );
    }

    #[test]
    fn test_parse_fetch_submodules() {
        let input = fixture("fetch_submodules.txt");
        let result = parse_fetch(&input);
        let details_str = result.details.join("\n");
        assert!(
            details_str.contains("lib/core") || details_str.contains("lib/utils"),
            "expected submodule names in details, got: {details_str}"
        );
    }

    #[test]
    fn test_parse_fetch_multiple_remotes() {
        // git fetch --all produces multiple From blocks
        let input = "\
From github.com:user/repo
   abc1234..def5678  main       -> origin/main
From github.com:upstream/repo
 * [new branch]      release    -> upstream/release
";
        let result = parse_fetch(input);
        // First remote captured in summary
        assert!(
            result.summary.contains("github.com:user/repo"),
            "expected first remote in summary, got: {}",
            result.summary
        );
        // Both updates tracked
        let details_str = result.details.join("\n");
        assert!(
            details_str.contains("main"),
            "expected main in details, got: {details_str}"
        );
        assert!(
            details_str.contains("release"),
            "expected release in details, got: {details_str}"
        );
    }

    /// Regression: refs following a submodule block must not be attributed to
    /// the previous submodule when a top-level `From` line resets context.
    #[test]
    fn test_parse_fetch_submodule_then_toplevel() {
        let input = "\
Fetching submodule lib/core
From github.com:user/core
   aaa1111..bbb2222  main       -> origin/main
From github.com:user/repo
   ccc3333..ddd4444  release    -> origin/release
";
        let result = parse_fetch(input);
        // Top-level "release" update counted in summary (submodule updates go in details only)
        assert!(
            result.summary.contains("1 updated"),
            "expected '1 updated' in summary (only top-level counted), got: {}",
            result.summary
        );
        // "release" must appear at the top level with "~ " prefix
        let details_str = result.details.join("\n");
        assert!(
            details_str.contains("~ release"),
            "expected top-level '~ release' in details, got: {details_str}"
        );
        // The top-level update must NOT be inside a submodule section
        let mut in_submodule_core = false;
        for line in result.details.iter() {
            if line.contains("[submodule lib/core]") {
                in_submodule_core = true;
            } else if line.starts_with('[') {
                in_submodule_core = false;
            }
            if in_submodule_core {
                assert!(
                    !line.contains("release"),
                    "top-level 'release' ref incorrectly attributed to submodule: {line}"
                );
            }
        }
    }

    #[test]
    fn test_extract_ref_name_new_branch() {
        let line = " * [new branch]      feature/x  -> origin/feature/x";
        let result = extract_ref_name(line);
        assert_eq!(result, Some("feature/x".to_string()));
    }

    #[test]
    fn test_extract_ref_name_new_tag() {
        let line = " * [new tag]         v2.3.0     -> v2.3.0";
        let result = extract_ref_name(line);
        assert_eq!(result, Some("v2.3.0".to_string()));
    }

    #[test]
    fn test_extract_updated_ref_normal() {
        let line = "   abc1234..def5678  main       -> origin/main";
        let result = extract_updated_ref(line);
        assert_eq!(result, Some("main".to_string()));
    }

    #[test]
    fn test_extract_updated_ref_forced() {
        let line = " + ccc3333...ddd4444 feature/z  -> origin/feature/z  (forced update)";
        let result = extract_updated_ref(line);
        assert_eq!(result, Some("feature/z".to_string()));
    }

    #[test]
    fn test_extract_pruned_ref() {
        let line = " - [deleted]         (none)     -> origin/old-branch";
        let result = extract_pruned_ref(line);
        assert_eq!(result, Some("old-branch".to_string()));
    }

    #[test]
    fn test_add_to_submodule_creates_section() {
        let mut cats = FetchCategories::new();
        cats.add_submodule_entry("lib/core", "updated: main".to_string());
        assert_eq!(cats.submodule_order.len(), 1);
        assert_eq!(cats.submodule_order[0], "lib/core");
        assert_eq!(
            cats.submodule_map["lib/core"],
            vec!["updated: main".to_string()]
        );
    }

    #[test]
    fn test_add_to_submodule_appends_to_existing() {
        let mut cats = FetchCategories::new();
        cats.add_submodule_entry("lib/core", "updated: main".to_string());
        cats.add_submodule_entry("lib/core", "new branch: feature".to_string());
        assert_eq!(cats.submodule_order.len(), 1);
        assert_eq!(cats.submodule_map["lib/core"].len(), 2);
    }
}
