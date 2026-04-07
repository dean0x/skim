//! Git fetch compression.
//!
//! Parses `git fetch` output (written to stderr by git) into a structured
//! summary: updated branches, new branches/tags, pruned refs, forced updates,
//! and submodule fetches. Progress/noise lines (`remote: ...`, `Unpacking ...`)
//! are stripped.

use std::process::ExitCode;

use crate::cmd::{extract_output_format, user_has_flag, OutputFormat};
use crate::output::canonical::GitResult;
use crate::runner::CommandRunner;

use super::{map_exit_code, run_passthrough};

/// Run `git fetch` with output compression.
///
/// Flag-aware passthrough: `--dry-run`, `-q`, `--quiet` pass through unmodified.
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

    let runner = CommandRunner::new(None);
    let arg_refs: Vec<&str> = full_args.iter().map(|s| s.as_str()).collect();
    let output = runner.run("git", &arg_refs)?;

    if output.exit_code != Some(0) {
        // On failure, pass through stderr
        if !output.stderr.is_empty() {
            eprint!("{}", output.stderr);
        }
        if !output.stdout.is_empty() {
            print!("{}", output.stdout);
        }
        return Ok(map_exit_code(output.exit_code));
    }

    // Git fetch writes its output to stderr; combine both for parsing
    let combined = format!("{}\n{}", output.stderr, output.stdout);
    let result = parse_fetch(&combined);

    let result_str = match output_format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&result)
                .map_err(|e| anyhow::anyhow!("failed to serialize result: {e}"))?;
            println!("{json}");
            json
        }
        OutputFormat::Text => {
            let s = result.to_string();
            println!("{s}");
            s
        }
    };

    if show_stats {
        let (orig, comp) = crate::process::count_token_pair(&combined, &result_str);
        crate::process::report_token_stats(orig, comp, "");
    }

    if crate::analytics::is_analytics_enabled() {
        crate::analytics::try_record_command(
            combined,
            result_str,
            format!("skim git fetch {}", args.join(" ")),
            crate::analytics::CommandType::Git,
            output.duration,
            None,
        );
    }

    Ok(ExitCode::SUCCESS)
}

// ============================================================================
// Parser
// ============================================================================

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
        return GitResult::new(
            "fetch".to_string(),
            "up to date".to_string(),
            Vec::new(),
        );
    }

    let mut remote = String::new();
    let mut new_branches: Vec<String> = Vec::new();
    let mut new_tags: Vec<String> = Vec::new();
    let mut updated: Vec<String> = Vec::new();
    let mut pruned: Vec<String> = Vec::new();
    let mut forced: Vec<String> = Vec::new();
    let mut submodule_sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut current_submodule: Option<String> = None;

    for line in &lines {
        let trimmed = line.trim();

        // Skip progress/noise lines
        if trimmed.starts_with("remote:") || trimmed.starts_with("Unpacking") || trimmed.is_empty()
        {
            continue;
        }

        // Submodule header
        if let Some(sub) = trimmed.strip_prefix("Fetching submodule ") {
            current_submodule = Some(sub.to_string());
            continue;
        }

        // From line — extract remote (only first one for primary remote)
        if let Some(rest) = trimmed.strip_prefix("From ") {
            if current_submodule.is_none() && remote.is_empty() {
                remote = rest.to_string();
            }
            continue;
        }

        // New branch
        if trimmed.contains("[new branch]") {
            if let Some(name) = extract_ref_name(trimmed) {
                if let Some(ref sub) = current_submodule {
                    add_to_submodule(
                        &mut submodule_sections,
                        sub,
                        &format!("new branch: {name}"),
                    );
                } else {
                    new_branches.push(name);
                }
            }
            continue;
        }

        // New tag
        if trimmed.contains("[new tag]") {
            if let Some(name) = extract_ref_name(trimmed) {
                new_tags.push(name);
            }
            continue;
        }

        // Deleted/pruned
        if trimmed.contains("[deleted]") {
            if let Some(name) = extract_pruned_ref(trimmed) {
                pruned.push(name);
            }
            continue;
        }

        // Forced update
        if trimmed.contains("(forced update)") {
            if let Some(name) = extract_updated_ref(trimmed) {
                forced.push(name);
            }
            continue;
        }

        // Regular update (abc..def ref -> origin/ref)
        if trimmed.contains("->") && (trimmed.contains("..") || trimmed.contains("...")) {
            if let Some(name) = extract_updated_ref(trimmed) {
                if let Some(ref sub) = current_submodule {
                    add_to_submodule(&mut submodule_sections, sub, &format!("updated: {name}"));
                } else {
                    updated.push(name);
                }
            }
            continue;
        }
    }

    // Build summary parts
    let mut parts: Vec<String> = Vec::new();
    if !updated.is_empty() {
        parts.push(format!("{} updated", updated.len()));
    }
    if !new_branches.is_empty() {
        parts.push(format!(
            "{} new branch{}",
            new_branches.len(),
            if new_branches.len() == 1 { "" } else { "es" }
        ));
    }
    if !new_tags.is_empty() {
        parts.push(format!(
            "{} new tag{}",
            new_tags.len(),
            if new_tags.len() == 1 { "" } else { "s" }
        ));
    }
    if !pruned.is_empty() {
        parts.push(format!("{} pruned", pruned.len()));
    }
    if !forced.is_empty() {
        parts.push(format!("{} forced", forced.len()));
    }

    if parts.is_empty() && submodule_sections.is_empty() {
        return GitResult::new(
            "fetch".to_string(),
            "up to date".to_string(),
            Vec::new(),
        );
    }

    // Build detail lines
    let mut details: Vec<String> = Vec::new();
    for b in &new_branches {
        details.push(format!("+ {b} (new branch)"));
    }
    for t in &new_tags {
        details.push(format!("+ {t} (new tag)"));
    }
    for u in &updated {
        details.push(format!("~ {u}"));
    }
    for f in &forced {
        details.push(format!("! {f} (forced)"));
    }
    for p in &pruned {
        details.push(format!("- {p} (pruned)"));
    }
    for (sub_name, entries) in &submodule_sections {
        details.push(format!("[submodule {sub_name}]"));
        for e in entries {
            details.push(format!("  {e}"));
        }
    }

    let display_summary = build_summary(&remote, &parts);

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

/// Add an entry to a submodule section, creating the section if it doesn't exist.
fn add_to_submodule(sections: &mut Vec<(String, Vec<String>)>, sub: &str, entry: &str) {
    if let Some(section) = sections.iter_mut().find(|(name, _)| name == sub) {
        section.1.push(entry.to_string());
    } else {
        sections.push((sub.to_string(), vec![entry.to_string()]));
    }
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
        assert!(result.summary.contains("github.com:user/repo"), "expected remote in summary");
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
        assert!(!result.summary.contains("updated"), "should not mention updated");
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
        let mut sections: Vec<(String, Vec<String>)> = Vec::new();
        add_to_submodule(&mut sections, "lib/core", "updated: main");
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, "lib/core");
        assert_eq!(sections[0].1, vec!["updated: main"]);
    }

    #[test]
    fn test_add_to_submodule_appends_to_existing() {
        let mut sections: Vec<(String, Vec<String>)> = Vec::new();
        add_to_submodule(&mut sections, "lib/core", "updated: main");
        add_to_submodule(&mut sections, "lib/core", "new branch: feature");
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].1.len(), 2);
    }
}
