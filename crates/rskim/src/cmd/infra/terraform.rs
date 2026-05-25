//! Terraform CLI parser with three-tier degradation (#117).
//!
//! Executes `terraform` and parses the output into structured `InfraResult`.
//!
//! Supports: `terraform plan`, `terraform apply`
//!
//! # SAFETY INVARIANTS
//!
//! `prepare_args` is ALWAYS a no-op for terraform. NEVER inject `-json`:
//! - `-json` for `terraform plan` bypasses interactive approval prompts
//! - `-json` for `terraform apply` bypasses safety confirmations
//! - Injecting these flags could silently trigger infrastructure changes

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::output::ParseResult;
use crate::output::canonical::{InfraItem, InfraResult};

use super::{combine_stdout_stderr, passthrough_parse};
use crate::cmd::{ToolRunConfig, run_tool};
use crate::analytics::CommandType;

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "terraform",
    env_overrides: &[],
    install_hint: "Install Terraform: https://developer.hashicorp.com/terraform/downloads",
    family: "infra",
    skip_ansi_strip: false,
    command_type: CommandType::Infra,
};

/// Matches the plan summary line in text output.
static RE_PLAN_SUMMARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Plan:\s+(\d+) to add,\s+(\d+) to change,\s+(\d+) to destroy").unwrap()
});

/// Matches the apply summary line.
static RE_APPLY_SUMMARY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Apply complete! Resources:\s+(\d+) added,\s+(\d+) changed,\s+(\d+) destroyed")
        .unwrap()
});

/// Matches resource action lines in text output: `# resource.name will be created/destroyed/updated`
static RE_RESOURCE_ACTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"#\s+(\S+)\s+will be (created|destroyed|updated in-place|replaced)").unwrap()
});

/// Run `skim terraform [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    let subcmd = args.first().map(|s| s.as_str()).unwrap_or("");

    match subcmd {
        "plan" => run_tool(CONFIG, args, ctx, prepare_args, parse_plan),
        "apply" => run_tool(CONFIG, args, ctx, prepare_args, parse_apply),
        _ => run_tool(CONFIG, args, ctx, prepare_args, passthrough_parse),
    }
}

/// No-op: NEVER inject `-json` for terraform.
///
/// # Safety invariant
/// Injecting `-json` to `terraform plan` or `terraform apply` bypasses interactive
/// safety prompts. This function MUST remain a no-op for all terraform subcommands.
pub(crate) fn prepare_args(_args: &mut Vec<String>) {
    // Intentionally empty: NEVER inject -json for terraform.
}

/// Three-tier parse for `terraform plan`.
fn parse_plan(output: &crate::runner::CommandOutput) -> ParseResult<InfraResult> {
    parse_terraform_output(output, "plan")
}

/// Three-tier parse for `terraform apply`.
fn parse_apply(output: &crate::runner::CommandOutput) -> ParseResult<InfraResult> {
    parse_terraform_output(output, "apply")
}

fn parse_terraform_output(
    output: &crate::runner::CommandOutput,
    subcmd: &str,
) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);
    let text = combined.trim();

    if text.is_empty() {
        return ParseResult::Passthrough(String::new());
    }

    // Tier 1: NDJSON (structured JSON output from `terraform plan -json`)
    if let Some(result) = try_parse_ndjson(text, subcmd) {
        return ParseResult::Full(result);
    }

    // Tier 2: regex on text output
    if let Some(result) = try_parse_text(text, subcmd) {
        return ParseResult::Degraded(
            result,
            vec![format!("terraform {subcmd}: using text parser")],
        );
    }

    // Tier 3: passthrough
    ParseResult::Passthrough(combined.into_owned())
}

fn try_parse_ndjson(text: &str, subcmd: &str) -> Option<InfraResult> {
    // Quick format check: bail early if no line looks like a JSON object.
    // The actual parse happens in the main loop below.
    if !text.lines().any(|l| l.trim().starts_with('{')) {
        return None;
    }

    let mut add = 0u64;
    let mut change = 0u64;
    let mut remove = 0u64;
    let mut resources: Vec<InfraItem> = Vec::new();
    let mut found_summary = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with('{') {
            continue;
        }
        let Ok(obj) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };

        match obj["type"].as_str() {
            Some("change_summary") => {
                found_summary = true;
                add = obj["changes"]["add"].as_u64().unwrap_or(0);
                change = obj["changes"]["change"].as_u64().unwrap_or(0);
                remove = obj["changes"]["remove"].as_u64().unwrap_or(0);
            }
            Some("planned_change") | Some("apply_start") => {
                let addr = obj["change"]["resource"]["addr"]
                    .as_str()
                    .or_else(|| obj["hook"]["resource"]["addr"].as_str())
                    .unwrap_or("unknown");
                let action = obj["change"]["action"]
                    .as_str()
                    .or_else(|| obj["hook"]["action"].as_str())
                    .unwrap_or("change");
                if resources.len() < 50 {
                    resources.push(InfraItem {
                        label: addr.to_string(),
                        value: action.to_string(),
                    });
                }
            }
            _ => {}
        }
    }

    if !found_summary && resources.is_empty() {
        return None;
    }

    let summary = format!("+{add} ~{change} -{remove}");
    Some(InfraResult::new(
        "terraform".to_string(),
        subcmd.to_string(),
        summary,
        resources,
    ))
}

fn try_parse_text(text: &str, subcmd: &str) -> Option<InfraResult> {
    // Handle "No changes." case
    if text.contains("No changes.") {
        return Some(InfraResult::new(
            "terraform".to_string(),
            subcmd.to_string(),
            "no changes".to_string(),
            vec![],
        ));
    }

    // Look for summary line (plan or apply)
    let summary_caps = RE_PLAN_SUMMARY
        .captures(text)
        .or_else(|| RE_APPLY_SUMMARY.captures(text));

    let caps = summary_caps?;

    let add: u64 = caps[1].parse().unwrap_or(0);
    let change: u64 = caps[2].parse().unwrap_or(0);
    let remove: u64 = caps[3].parse().unwrap_or(0);
    let summary = format!("+{add} ~{change} -{remove}");

    // Extract individual resource actions
    let resources: Vec<InfraItem> = RE_RESOURCE_ACTION
        .captures_iter(text)
        .take(50)
        .map(|caps| InfraItem {
            label: caps[1].to_string(),
            value: caps[2].to_string(),
        })
        .collect();

    Some(InfraResult::new(
        "terraform".to_string(),
        subcmd.to_string(),
        summary,
        resources,
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_support::{load_fixture as _load_fixture, make_output};

    fn load_fixture(name: &str) -> String {
        _load_fixture("infra", name)
    }

    #[test]
    fn test_tier1_plan_ndjson_full_result() {
        let fixture = load_fixture("terraform_plan_ndjson.json");
        let output = make_output(&fixture);
        let result = parse_plan(&output);
        assert!(
            matches!(result, ParseResult::Full(_)),
            "expected Full, got {result:?}"
        );
        if let ParseResult::Full(r) = result {
            let display = r.to_string();
            assert!(display.contains("+2"), "should contain add count");
            assert!(display.contains("~1"), "should contain change count");
        }
    }

    #[test]
    fn test_tier1_apply_ndjson_full_result() {
        let fixture = load_fixture("terraform_apply_ndjson.json");
        let output = make_output(&fixture);
        let result = parse_apply(&output);
        assert!(
            matches!(result, ParseResult::Full(_)),
            "expected Full, got {result:?}"
        );
        if let ParseResult::Full(r) = result {
            assert!(r.to_string().contains("+2"));
        }
    }

    #[test]
    fn test_tier2_plan_text_degraded() {
        let fixture = load_fixture("terraform_plan_text.txt");
        let output = make_output(&fixture);
        let result = parse_plan(&output);
        assert!(
            matches!(result, ParseResult::Degraded(_, _)),
            "expected Degraded, got {result:?}"
        );
        if let ParseResult::Degraded(r, _) = result {
            let display = r.to_string();
            assert!(display.contains("+2"), "should contain add count");
            assert!(display.contains("aws_instance"), "should contain resource");
        }
    }

    #[test]
    fn test_tier2_no_changes_edge_case() {
        let fixture = load_fixture("terraform_plan_no_changes.txt");
        let output = make_output(&fixture);
        let result = parse_plan(&output);
        assert!(matches!(result, ParseResult::Degraded(_, _)));
        if let ParseResult::Degraded(r, _) = result {
            assert!(r.to_string().contains("no changes"));
        }
    }

    #[test]
    fn test_tier3_passthrough_on_garbage() {
        let output = make_output("Error: error configuring Terraform AWS Provider");
        let result = parse_plan(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    #[test]
    fn test_empty_passthrough() {
        let output = make_output("");
        let result = parse_plan(&output);
        assert!(matches!(result, ParseResult::Passthrough(_)));
    }

    /// Safety invariant: prepare_args MUST NEVER inject -json for terraform.
    #[test]
    fn test_prepare_args_is_noop() {
        let mut args = vec!["plan".to_string(), "-var-file=prod.tfvars".to_string()];
        let original = args.clone();
        prepare_args(&mut args);
        assert_eq!(
            args, original,
            "prepare_args MUST NOT inject -json for terraform"
        );
    }

    /// Safety invariant: verify no -json flag appears in args after prepare_args.
    #[test]
    fn test_prepare_args_never_injects_json_flag() {
        for subcmd in &["plan", "apply", "destroy"] {
            let mut args = vec![subcmd.to_string()];
            prepare_args(&mut args);
            assert!(
                !args.contains(&"-json".to_string()),
                "prepare_args must never inject -json for terraform {subcmd}"
            );
        }
    }
}
