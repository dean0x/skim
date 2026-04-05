//! GitHub CLI (`gh`) parser with three-tier degradation (#116).
//!
//! Executes `gh` and parses the output into structured `InfraResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: JSON parsing (inject `--json` for list commands)
//! - **Tier 2 (Degraded)**: Regex on tabular text output
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation

use std::sync::LazyLock;

use regex::Regex;

use crate::cmd::user_has_flag;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, run_infra_tool, InfraToolConfig};

const CONFIG: InfraToolConfig<'static> = InfraToolConfig {
    program: "gh",
    env_overrides: &[],
    install_hint: "Install gh: https://cli.github.com/",
};

static RE_GH_TAB_ROW: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+)\t(.+)").unwrap());

/// Run `skim infra gh [args...]`.
pub(crate) fn run(
    args: &[String],
    show_stats: bool,
    json_output: bool,
) -> anyhow::Result<std::process::ExitCode> {
    run_infra_tool(CONFIG, args, show_stats, json_output, prepare_args, parse_impl)
}

/// Inject `--json` fields for list commands if not already present.
fn prepare_args(cmd_args: &mut Vec<String>) {
    if user_has_flag(cmd_args, &["--json"]) {
        return;
    }

    // Detect subcommand pattern: gh pr list, gh issue list, gh run list
    let subcmd = cmd_args.first().map(|s| s.as_str()).unwrap_or("");
    let action = cmd_args.get(1).map(|s| s.as_str()).unwrap_or("");

    match (subcmd, action) {
        ("pr", "list") => {
            cmd_args.push("--json".to_string());
            cmd_args.push("number,title,state,author".to_string());
        }
        ("issue", "list") => {
            cmd_args.push("--json".to_string());
            cmd_args.push("number,title,state,labels".to_string());
        }
        ("run", "list") => {
            cmd_args.push("--json".to_string());
            cmd_args.push("databaseId,displayTitle,status,conclusion".to_string());
        }
        // release list and other commands: no injection
        _ => {}
    }
}

/// Three-tier parse function for gh output.
fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    if let Some(result) = try_parse_json(&output.stdout) {
        return ParseResult::Full(result);
    }

    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_regex(&combined) {
        return ParseResult::Degraded(result, vec!["regex fallback".to_string()]);
    }

    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// Tier 1: JSON parsing
// ============================================================================

/// Maximum number of items to include in a parsed result.
///
/// Prevents unbounded memory growth when `gh` returns very large JSON arrays
/// (e.g., repositories with thousands of open issues or PRs).
const MAX_ITEMS: usize = 100;

/// Parse gh JSON array output.
fn try_parse_json(stdout: &str) -> Option<InfraResult> {
    let trimmed = stdout.trim();
    if !trimmed.starts_with('[') {
        return None;
    }

    let arr: Vec<serde_json::Value> = serde_json::from_str(trimmed).ok()?;
    let total = arr.len();
    let truncated = total > MAX_ITEMS;

    let items: Vec<InfraItem> = arr
        .into_iter()
        .take(MAX_ITEMS)
        .map(|entry| {
            let label = entry
                .get("number")
                .and_then(|v| v.as_u64())
                .or_else(|| entry.get("databaseId").and_then(|v| v.as_u64()))
                .map(|n| format!("#{n}"))
                .unwrap_or_else(|| "item".to_string());

            let title = entry
                .get("title")
                .and_then(|v| v.as_str())
                .or_else(|| entry.get("displayTitle").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();

            let state = entry
                .get("state")
                .and_then(|v| v.as_str())
                .or_else(|| entry.get("status").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_lowercase();

            let value = if state.is_empty() {
                title
            } else {
                format!("{title} ({state})")
            };

            InfraItem { label, value }
        })
        .collect();

    let count = items.len();
    let summary = if truncated {
        format!("showing first {MAX_ITEMS} of {total} items")
    } else {
        format!("{count} item{}", if count == 1 { "" } else { "s" })
    };
    Some(InfraResult::new("gh".to_string(), "list".to_string(), summary, items))
}

// ============================================================================
// Tier 2: regex fallback
// ============================================================================

/// Parse tab-separated gh text output.
fn try_parse_regex(text: &str) -> Option<InfraResult> {
    let mut items: Vec<InfraItem> = Vec::new();

    for line in text.lines() {
        if let Some(caps) = RE_GH_TAB_ROW.captures(line) {
            let num = caps[1].to_string();
            let rest = caps[2].trim().to_string();
            items.push(InfraItem {
                label: format!("#{num}"),
                value: rest,
            });
        }
    }

    if items.is_empty() {
        return None;
    }

    let count = items.len();
    let summary = format!("{count} item{}", if count == 1 { "" } else { "s" });
    Some(InfraResult::new("gh".to_string(), "list".to_string(), summary, items))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn load_fixture(name: &str) -> String {
        let mut path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests/fixtures/cmd/infra");
        path.push(name);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("Failed to load fixture '{name}': {e}"))
    }

    #[test]
    fn test_tier1_gh_pass() {
        let input = load_fixture("gh_pr_list.json");
        let result = try_parse_json(&input);
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert!(result.as_ref().contains("INFRA: gh list"));
        assert_eq!(result.items.len(), 3);
    }

    #[test]
    fn test_tier1_gh_fail_non_json() {
        let result = try_parse_json("not json");
        assert!(result.is_none());
    }

    #[test]
    fn test_tier2_gh_regex() {
        let input = load_fixture("gh_pr_list_text.txt");
        let result = try_parse_regex(&input);
        assert!(result.is_some(), "Expected Tier 2 regex parse to succeed");
        let result = result.unwrap();
        assert_eq!(result.items.len(), 3);
        assert!(result.items.iter().any(|i| i.label == "#42"));
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("gh_pr_list.json");
        let output = CommandOutput {
            stdout: input,
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full parse result, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_garbage_produces_passthrough() {
        let output = CommandOutput {
            stdout: "completely unparseable output\nno json, no regex match".to_string(),
            stderr: String::new(),
            exit_code: Some(1),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_text_produces_degraded() {
        // Tier 2 input: tab-separated tabular text output (not JSON) that matches
        // the `^\d+\t.+` regex. This is what `gh pr list` emits without `--json`.
        let output = CommandOutput {
            stdout: "42\tFix login bug\tOPEN\n57\tAdd dark mode\tOPEN\n".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        };
        let result = parse_impl(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded parse result, got {}",
            result.tier_name()
        );
    }
}
