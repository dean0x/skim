//! `gh run view` parser with three-tier degradation.
//!
//! Parses workflow run metadata from `gh run view`, focusing on job-level
//! status and surfacing step details for failed jobs.
//!
//! # Design Decision: Step detail depth for failed jobs
//!
//! When a job fails, agents need to see which specific step failed in order
//! to diagnose CI failures without fetching full logs. We include up to
//! [`MAX_STEP_DETAIL`] steps per failed job, filtered to only show non-passing
//! steps. Successful jobs show only a one-line summary to minimize context.

use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{
    inject_json_fields, three_tier_parse, try_parse_json_object, MAX_STEP_DETAIL, RE_GH_RUN_HEADER,
    RE_GH_RUN_JOB, RE_GH_VIEW_FIELD,
};

/// JSON fields to inject for `gh run view`.
const RUN_VIEW_FIELDS: &str = "name,status,conclusion,event,jobs,databaseId,createdAt,updatedAt";

/// Inject `--json` for run view if not already present.
pub(super) fn prepare_args(cmd_args: &mut Vec<String>) {
    inject_json_fields(cmd_args, RUN_VIEW_FIELDS);
}

/// Three-tier parse function for `gh run view` output.
pub(super) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    three_tier_parse(
        output,
        |trimmed| try_parse_json_object(trimmed, try_parse_json),
        |t| t.starts_with('{'),
        try_parse_text,
        false,
        "gh run view: JSON parse failed, using text regex",
    )
}

// ============================================================================
// Tier 1: JSON parsing
// ============================================================================

/// Append one [`InfraItem`] per job to `items`, plus step details for failed jobs.
///
/// Separated from [`try_parse_json`] so that run metadata extraction and job
/// iteration each have a single responsibility.
fn extract_job_items(jobs: &[serde_json::Value], items: &mut Vec<InfraItem>) {
    for job in jobs {
        let job_name = job
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let job_conclusion = job
            .get("conclusion")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| job.get("status").and_then(|v| v.as_str()).unwrap_or("?"))
            .to_lowercase();

        items.push(InfraItem {
            label: format!("job:{job_name}"),
            value: job_conclusion.clone(),
        });

        // For failed jobs, show step details (up to MAX_STEP_DETAIL non-passing steps)
        if job_conclusion == "failure" || job_conclusion == "failed" {
            let steps = job
                .get("steps")
                .and_then(|v| v.as_array())
                .map(|v| v.as_slice())
                .unwrap_or(&[]);

            let mut shown = 0;
            for step in steps {
                if shown >= MAX_STEP_DETAIL {
                    break;
                }
                let step_name = step.get("name").and_then(|v| v.as_str()).unwrap_or("step");
                let step_conclusion = step
                    .get("conclusion")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_lowercase();

                // Only show non-passing steps
                if step_conclusion != "success" && step_conclusion != "skipped" {
                    items.push(InfraItem {
                        label: format!("  step:{step_name}"),
                        value: step_conclusion,
                    });
                    shown += 1;
                }
            }
        }
    }
}

/// Parse a `gh run view --json` object into an [`InfraResult`].
///
/// Shows one item per job with status. For failed jobs, adds indented step
/// details (up to [`MAX_STEP_DETAIL`]) showing only non-passing steps.
///
/// Accepts a pre-parsed JSON `Value` so this function can also be called
/// from the auto-detect dispatcher (which uses `"jobs"` field as discriminator).
pub(super) fn try_parse_json(obj: &serde_json::Value) -> Option<InfraResult> {
    let db_id = obj.get("databaseId").and_then(|v| v.as_u64())?;
    let name = obj
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let conclusion = obj
        .get("conclusion")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            obj.get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        })
        .to_lowercase();

    let summary = format!("#{db_id}: {name} ({conclusion})");

    let mut items: Vec<InfraItem> = Vec::new();

    // Event
    if let Some(event) = obj.get("event").and_then(|v| v.as_str()) {
        items.push(InfraItem {
            label: "event".to_string(),
            value: event.to_string(),
        });
    }

    // Jobs
    let jobs = obj
        .get("jobs")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[]);

    extract_job_items(jobs, &mut items);

    Some(InfraResult::new(
        "gh".to_string(),
        "run view".to_string(),
        summary,
        items,
    ))
}

// ============================================================================
// Tier 2: text regex fallback
// ============================================================================

/// Parse `gh run view` text output using regex.
fn try_parse_text(text: &str) -> Option<InfraResult> {
    let mut items: Vec<InfraItem> = Vec::new();
    let mut summary = String::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Try run header
        if summary.is_empty() {
            if let Some(caps) = RE_GH_RUN_HEADER.captures(line) {
                summary = format!("#{}: {}", &caps[2], &caps[1]);
                continue;
            }
        }

        // Try job line
        if let Some(caps) = RE_GH_RUN_JOB.captures(line) {
            items.push(InfraItem {
                label: format!("job:{}", caps[1].trim()),
                value: caps[2].to_lowercase(),
            });
            continue;
        }

        // Try field line
        if let Some(caps) = RE_GH_VIEW_FIELD.captures(line) {
            items.push(InfraItem {
                label: caps[1].to_lowercase(),
                value: caps[2].to_string(),
            });
        }
    }

    if summary.is_empty() && items.is_empty() {
        return None;
    }

    if summary.is_empty() {
        summary = "run view".to_string();
    }

    Some(InfraResult::new(
        "gh".to_string(),
        "run view".to_string(),
        summary,
        items,
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::super::test_helpers::{load_fixture, make_output};
    use super::*;

    #[test]
    fn test_tier1_json() {
        let input = load_fixture("gh_run_view.json");
        let obj: serde_json::Value = serde_json::from_str(&input).unwrap();
        let result = try_parse_json(&obj);
        assert!(result.is_some(), "Expected JSON parse to succeed");
        let result = result.unwrap();
        assert!(
            result.as_ref().contains("INFRA: gh run view"),
            "got: {}",
            result.as_ref()
        );
        assert!(
            result.as_ref().contains("#12345"),
            "got: {}",
            result.as_ref()
        );
        assert!(
            result.as_ref().contains("CI Pipeline"),
            "got: {}",
            result.as_ref()
        );
    }

    #[test]
    fn test_tier1_failed_steps() {
        let input = load_fixture("gh_run_view.json");
        let obj: serde_json::Value = serde_json::from_str(&input).unwrap();
        let result = try_parse_json(&obj).unwrap();
        // Should include step details for the failed "test" job
        let step_items: Vec<_> = result
            .items
            .iter()
            .filter(|i| i.label.starts_with("  step:"))
            .collect();
        assert!(
            !step_items.is_empty(),
            "Expected step details for failed job, got items: {:?}",
            result.items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
        // The "Run tests" step should be shown as it failed
        assert!(
            step_items.iter().any(|i| i.label.contains("Run tests")),
            "Expected 'Run tests' step in failed details"
        );
    }

    #[test]
    fn test_tier1_success() {
        let input = load_fixture("gh_run_view_success.json");
        let obj: serde_json::Value = serde_json::from_str(&input).unwrap();
        let result = try_parse_json(&obj).unwrap();
        assert!(
            result.as_ref().contains("success"),
            "got: {}",
            result.as_ref()
        );
        // No failed steps — no step detail items
        let step_items: Vec<_> = result
            .items
            .iter()
            .filter(|i| i.label.starts_with("  step:"))
            .collect();
        assert!(
            step_items.is_empty(),
            "Expected no step details for successful run"
        );
    }

    #[test]
    fn test_tier2_text() {
        let input = load_fixture("gh_run_view_text.txt");
        let result = try_parse_text(&input);
        assert!(result.is_some(), "Expected Tier 2 text parse to succeed");
        let result = result.unwrap();
        assert!(
            result.as_ref().contains("gh run view"),
            "got: {}",
            result.as_ref()
        );
        assert!(
            result.as_ref().contains("#12345"),
            "Expected run ID in text output, got: {}",
            result.as_ref()
        );
        // Should have parsed job lines
        let job_items: Vec<_> = result
            .items
            .iter()
            .filter(|i| i.label.starts_with("job:"))
            .collect();
        assert!(
            !job_items.is_empty(),
            "Expected job items from text parsing, got items: {:?}",
            result.items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_passthrough_garbage() {
        let output = make_output("not a run view response");
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for garbage, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_json_produces_full() {
        let input = load_fixture("gh_run_view.json");
        let output = make_output(&input);
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full parse result, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_tier1_empty_jobs_array() {
        // A run view JSON with an empty jobs array should produce a result with
        // run-level summary but no job items.
        let json = r#"{
            "databaseId": 99,
            "name": "Empty Run",
            "status": "completed",
            "conclusion": "success",
            "event": "push",
            "jobs": []
        }"#;
        let obj: serde_json::Value = serde_json::from_str(json).unwrap();
        let result = try_parse_json(&obj);
        assert!(
            result.is_some(),
            "Expected parse to succeed for empty jobs array"
        );
        let result = result.unwrap();
        assert!(result.as_ref().contains("#99"), "got: {}", result.as_ref());
        assert!(
            result.as_ref().contains("Empty Run"),
            "got: {}",
            result.as_ref()
        );
        let job_items: Vec<_> = result
            .items
            .iter()
            .filter(|i| i.label.starts_with("job:"))
            .collect();
        assert!(
            job_items.is_empty(),
            "Expected no job items for empty jobs array"
        );
    }

    #[test]
    fn test_tier1_user_json_fields_not_overridden() {
        let mut args = vec![
            "run".to_string(),
            "view".to_string(),
            "12345".to_string(),
            "--json".to_string(),
            "name,status".to_string(),
        ];
        let original_len = args.len();
        prepare_args(&mut args);
        assert_eq!(
            args.len(),
            original_len,
            "Should not inject when --json present"
        );
    }
}
