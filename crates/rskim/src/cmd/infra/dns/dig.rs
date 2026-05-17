//! `dig` parser with three-tier degradation.

use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::runner::CommandOutput;

use super::super::{InfraToolConfig, combine_stdout_stderr, run_infra_tool};

// ============================================================================
// Tool config
// ============================================================================

pub(crate) const CONFIG_DIG: InfraToolConfig<'static> = InfraToolConfig {
    program: "dig",
    env_overrides: &[],
    install_hint: "Install via: apt install dnsutils / brew install bind",
    // dig uses TABs as field separators in ANSWER records (e.g. `name\tTTL\tIN\tA\tip`).
    // strip_ansi_escapes treats \t as a control code and removes it, collapsing fields
    // so RE_DIG_RECORD can no longer match. Skip stripping for dig and nslookup.
    skip_ansi_strip: true,
};

// ============================================================================
// dig regex patterns (compiled once)
// ============================================================================

/// Captures the DNS status from the HEADER line: `status: NOERROR`
static RE_DIG_HEADER_STATUS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"status:\s*(\w+)").unwrap());

/// Captures an answer record: name TTL class type rdata
/// Groups: (name, ttl, class, type, rdata)
static RE_DIG_RECORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\S+)\s+(\d+)\s+(\w+)\s+(\w+)\s+(.+)$").unwrap());

/// Captures the query time in msec
static RE_DIG_QUERY_TIME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r";; Query time:\s*(\d+)\s*msec").unwrap());

// ============================================================================
// Entry point
// ============================================================================

/// Run `skim dig [args...]`.
pub(crate) fn run_dig(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    run_infra_tool(CONFIG_DIG, args, ctx, |_| {}, parse_dig_impl)
}

// ============================================================================
// dig parse chain
// ============================================================================

/// Three-tier parse function for dig output.
fn parse_dig_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_dig_structured(&combined) {
        return ParseResult::Full(result);
    }

    if let Some(result) = try_parse_dig_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["dig: structured parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

/// Tier 1: Extract dig records and metadata from structured output.
fn try_parse_dig_structured(text: &str) -> Option<InfraResult> {
    // Guard: must look like dig output
    if !text.contains(";; ANSWER SECTION:") && !text.contains(";; ->>HEADER<<-") {
        return None;
    }

    let mut items: Vec<InfraItem> = Vec::new();

    // Extract status
    let status = RE_DIG_HEADER_STATUS
        .captures(text)
        .map(|c| c[1].to_string())
        .unwrap_or_else(|| "UNKNOWN".to_string());

    items.push(InfraItem {
        label: "status".to_string(),
        value: status.clone(),
    });

    // Non-NOERROR statuses: return compressed error summary
    if status != "NOERROR" {
        // Extract the query domain from the QUESTION SECTION
        let domain = extract_dig_question_domain(text).unwrap_or_else(|| "unknown".to_string());
        let query_time = extract_dig_query_time(text);

        let summary = if let Some(ms) = query_time {
            format!("{domain} → {status} ({ms}ms)")
        } else {
            format!("{domain} → {status}")
        };

        return Some(InfraResult::new(
            "dig".to_string(),
            "query".to_string(),
            summary,
            items,
        ));
    }

    // Extract ANSWER records
    let records = extract_dig_answer_records(text);
    let record_count = records.len();
    items.extend(records);

    let domain = extract_dig_question_domain(text).unwrap_or_else(|| "unknown".to_string());
    let query_time = extract_dig_query_time(text);

    let summary = if let Some(ms) = query_time {
        format!("{domain} → NOERROR ({record_count} records, {ms}ms)")
    } else {
        format!("{domain} → NOERROR ({record_count} records)")
    };

    Some(InfraResult::new(
        "dig".to_string(),
        "query".to_string(),
        summary,
        items,
    ))
}

/// Extract the queried domain from the QUESTION SECTION.
///
/// dig output has two kinds of single-semicolon lines:
/// - The version header: `; <<>> DiG 9.10.6 <<>> example.com A` — appears before any section.
/// - The question entry: `;example.com.    IN    A` — appears after `;; QUESTION SECTION:`.
///
/// We must only match the latter. We track whether we have seen the QUESTION SECTION
/// marker and then parse the first single-semicolon line inside that section.
fn extract_dig_question_domain(text: &str) -> Option<String> {
    let mut in_question = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Detect QUESTION SECTION header
        if trimmed.starts_with(";; QUESTION SECTION:") {
            in_question = true;
            continue;
        }

        if in_question {
            // Any `;;` line ends the section
            if trimmed.starts_with(";;") {
                break;
            }
            // Empty line ends the section
            if trimmed.is_empty() {
                break;
            }
            // Single-semicolon line inside QUESTION SECTION: `;example.com.    IN    A`
            if trimmed.starts_with(';') {
                let domain = trimmed.trim_start_matches(';').split_whitespace().next()?;
                // Strip trailing dot (dig always appends a dot to FQDNs)
                let domain = domain.trim_end_matches('.');
                return Some(domain.to_string());
            }
        }
    }
    None
}

/// Extract query time in milliseconds from dig output.
fn extract_dig_query_time(text: &str) -> Option<u64> {
    RE_DIG_QUERY_TIME
        .captures(text)
        .and_then(|c| c[1].parse().ok())
}

/// Extract answer records from all ANSWER SECTION blocks.
///
/// Handles both single-query and multi-query output by iterating all
/// `;; ANSWER SECTION:` markers.
fn extract_dig_answer_records(text: &str) -> Vec<InfraItem> {
    let mut items = Vec::new();
    let mut in_answer = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Detect ANSWER SECTION start
        if trimmed.starts_with(";; ANSWER SECTION:") {
            in_answer = true;
            continue;
        }

        // Detect section end: empty line or another ;; section
        if in_answer {
            if trimmed.is_empty() || trimmed.starts_with(";;") {
                in_answer = false;
                continue;
            }

            // Skip comment lines (but not section headers — handled above)
            if trimmed.starts_with(';') {
                continue;
            }

            // Parse record line: name TTL class type rdata
            if let Some(caps) = RE_DIG_RECORD.captures(trimmed) {
                let rtype = caps[4].to_string();
                let rdata = caps[5].trim().to_string();
                let name = caps[1].trim_end_matches('.').to_string();
                items.push(InfraItem {
                    label: rtype,
                    value: format!("{name} → {rdata}"),
                });
            }
        }
    }

    items
}

/// Tier 2: Look for a status keyword anywhere in the output.
fn try_parse_dig_regex(text: &str) -> Option<InfraResult> {
    if let Some(caps) = RE_DIG_HEADER_STATUS.captures(text) {
        let status = caps[1].to_string();
        let items = vec![InfraItem {
            label: "status".to_string(),
            value: status.clone(),
        }];
        return Some(InfraResult::new(
            "dig".to_string(),
            "query".to_string(),
            format!("DNS {status}"),
            items,
        ));
    }
    None
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::test_helpers::{load_fixture, make_output};

    // ========================================================================
    // dig: Tier 1
    // ========================================================================

    #[test]
    fn test_dig_tier1_a_record() {
        let input = load_fixture("dig_a_record.txt");
        let result = try_parse_dig_structured(&input);
        assert!(result.is_some(), "Expected Tier 1 parse to succeed");
        let result = result.unwrap();
        assert!(
            result.as_ref().contains("NOERROR"),
            "Summary should contain NOERROR: {}",
            result.as_ref()
        );
        assert!(
            result.items.iter().any(|i| i.label == "A"),
            "Expected at least one A record"
        );
    }

    #[test]
    fn test_dig_tier1_mx_record() {
        let input = load_fixture("dig_mx_record.txt");
        let result = try_parse_dig_structured(&input);
        assert!(result.is_some(), "Expected Tier 1 parse for MX");
        let result = result.unwrap();
        assert!(
            result.items.iter().any(|i| i.label == "MX"),
            "Expected MX record item"
        );
    }

    #[test]
    fn test_dig_tier1_nxdomain() {
        let input = load_fixture("dig_nxdomain.txt");
        let result = try_parse_dig_structured(&input);
        assert!(result.is_some(), "Expected Tier 1 parse for NXDOMAIN");
        let result = result.unwrap();
        assert!(
            result.as_ref().contains("NXDOMAIN"),
            "Summary should contain NXDOMAIN: {}",
            result.as_ref()
        );
        assert!(
            result
                .items
                .iter()
                .any(|i| i.label == "status" && i.value == "NXDOMAIN"),
            "Expected status=NXDOMAIN item"
        );
    }

    #[test]
    fn test_dig_tier1_multi_answer() {
        let input = load_fixture("dig_multi_answer.txt");
        let result = try_parse_dig_structured(&input);
        assert!(result.is_some(), "Expected Tier 1 parse for multi-answer");
        let result = result.unwrap();
        assert!(
            result.as_ref().contains("NOERROR"),
            "Summary should contain NOERROR: {}",
            result.as_ref()
        );
        // AC-DIG-5: multi-record fixture must yield at least 2 A record items
        let a_records: Vec<_> = result.items.iter().filter(|i| i.label == "A").collect();
        assert!(
            a_records.len() >= 2,
            "Expected at least 2 A records, got {}: {:?}",
            a_records.len(),
            a_records
        );
    }

    #[test]
    fn test_dig_tier1_query_time_in_summary() {
        let input = load_fixture("dig_a_record.txt");
        let result = try_parse_dig_structured(&input).unwrap();
        // Should include ms timing
        assert!(
            result.as_ref().contains("ms"),
            "Summary should contain timing: {}",
            result.as_ref()
        );
    }

    #[test]
    fn test_dig_tier1_aaaa_record() {
        // AC-DIG-10: AAAA/IPv6 records must be extracted as Tier 1 Full
        let input = load_fixture("dig_aaaa_record.txt");
        let result = try_parse_dig_structured(&input);
        assert!(result.is_some(), "Expected Tier 1 parse for AAAA record");
        let result = result.unwrap();
        assert!(
            result.as_ref().contains("NOERROR"),
            "Summary should contain NOERROR: {}",
            result.as_ref()
        );
        // Must have at least one AAAA item containing an IPv6 address
        let aaaa_items: Vec<_> = result.items.iter().filter(|i| i.label == "AAAA").collect();
        assert!(
            !aaaa_items.is_empty(),
            "Expected at least one AAAA record item"
        );
        assert!(
            aaaa_items.iter().any(|i| i.value.contains("2606:2800")),
            "Expected IPv6 address 2606:2800:... in AAAA items, got: {:?}",
            aaaa_items
        );
    }

    // ========================================================================
    // dig: Tier 2
    // ========================================================================

    #[test]
    fn test_dig_tier2_status_in_partial_output() {
        let input = "status: NOERROR\n; some partial dig output\n";
        let result = try_parse_dig_regex(input);
        assert!(result.is_some(), "Tier 2 should match status: NOERROR");
        let result = result.unwrap();
        assert!(result.items.iter().any(|i| i.label == "status"));
    }

    #[test]
    fn test_dig_tier2_no_false_positive() {
        let input = "some random output without DNS content\n";
        let result = try_parse_dig_regex(input);
        assert!(result.is_none(), "Tier 2 should not match random output");
    }

    // ========================================================================
    // dig: parse_dig_impl
    // ========================================================================

    #[test]
    fn test_dig_parse_impl_produces_full_for_a_record() {
        let input = load_fixture("dig_a_record.txt");
        let output = make_output(&input);
        let result = parse_dig_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_dig_parse_impl_passthrough_for_short_output() {
        // dig +short output has no HEADER or ANSWER SECTION markers
        let input = load_fixture("dig_short.txt");
        let output = make_output(&input);
        let result = parse_dig_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough for short output, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_dig_parse_impl_passthrough_for_garbage() {
        let output = make_output("random garbage not DNS output\n");
        let result = parse_dig_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_dig_parse_impl_degraded_for_partial() {
        // Partial output with status: but no HEADER/ANSWER markers
        let output = make_output("status: SERVFAIL\n");
        let result = parse_dig_impl(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded for partial status output, got {}",
            result.tier_name()
        );
    }
}
