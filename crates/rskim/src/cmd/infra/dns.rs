//! DNS tool parsers for `dig` and `nslookup` with three-tier degradation (#168).
//!
//! Executes `dig` or `nslookup` and parses the output into structured `InfraResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Regex for DNS records, status, and query metadata
//! - **Tier 2 (Degraded)**: Simpler regex fallback for partial output
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation
//!
//! Both tools use `combine_stdout_stderr: true` because DNS errors may appear
//! on stderr (e.g., SERVFAIL, connection refused).
//!
//! # Design
//!
//! Two independent entry points (`run_dig`, `run_nslookup`) with two separate
//! configs and two separate parse chains. They share regex utilities but have
//! independent tier logic because dig and nslookup have fundamentally different
//! output formats.
//!
//! The no-args guard on `run_nslookup` prevents interactive mode — nslookup
//! with no arguments drops into an interactive resolver shell, which hangs
//! indefinitely in agent contexts.

use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::runner::CommandOutput;

use super::{InfraToolConfig, combine_stdout_stderr, run_infra_tool};

// ============================================================================
// Tool configs
// ============================================================================

const CONFIG_DIG: InfraToolConfig<'static> = InfraToolConfig {
    program: "dig",
    env_overrides: &[],
    install_hint: "Install via: apt install dnsutils / brew install bind",
    // dig uses TABs as field separators in ANSWER records (e.g. `name\tTTL\tIN\tA\tip`).
    // strip_ansi_escapes treats \t as a control code and removes it, collapsing fields
    // so RE_DIG_RECORD can no longer match. Skip stripping for dig and nslookup.
    skip_ansi_strip: true,
};

const CONFIG_NSLOOKUP: InfraToolConfig<'static> = InfraToolConfig {
    program: "nslookup",
    env_overrides: &[],
    install_hint: "Install via: apt install dnsutils / brew install bind",
    // nslookup also uses TABs as field separators (Server:\t<ip>, Address:\t<ip>#port).
    // See CONFIG_DIG comment for the full rationale.
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
// nslookup regex patterns (compiled once)
// ============================================================================

/// Captures the DNS server name from `Server: <name>`
static RE_NSL_SERVER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^Server:\s+(.+)$").unwrap());

/// Captures the DNS server address+port from `Address: <ip>#<port>`
static RE_NSL_SERVER_ADDR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^Address:\s+(\S+)#(\d+)$").unwrap());

/// Captures a result Name line
static RE_NSL_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^Name:\s+(.+)$").unwrap());

/// Captures a result Address line (no #port = answer, not server)
static RE_NSL_ADDRESS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^Address:\s+(\S+)$").unwrap());

/// Detects NXDOMAIN: `** server can't find <domain>`
static RE_NSL_CANT_FIND: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\*\*\s+server can't find\s+(\S+)").unwrap());

/// Captures MX records: `mail exchanger = <prio> <host>`
static RE_NSL_MX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"mail exchanger\s*=\s*(\d+)\s+(\S+)").unwrap());

/// Captures CNAME: `canonical name = <target>`
static RE_NSL_CNAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"canonical name\s*=\s*(\S+)").unwrap());

/// Captures TXT records: `text = "<content>"`
static RE_NSL_TXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"text\s*=\s*"(.+)""#).unwrap());

/// macOS format: `address = <ip>` (without #port)
static RE_NSL_ADDRESS_MAC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^address\s*=\s*(\S+)$").unwrap());

// ============================================================================
// Entry points
// ============================================================================

/// Run `skim dig [args...]`.
pub(crate) fn run_dig(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    run_infra_tool(CONFIG_DIG, args, ctx, |_| {}, parse_dig_impl)
}

/// Run `skim nslookup [args...]`.
///
/// # No-args guard
///
/// `nslookup` with no arguments drops into an interactive resolver shell.
/// In agent contexts (piped stdin, non-TTY) this hangs or exits immediately
/// with no useful output. Guard triggers when args are empty AND stdin is a
/// terminal — matching exactly the interactive-mode case.
///
/// The piped-stdin case (`echo '' | skim nslookup`) is safe: `should_read_stdin`
/// in `run_infra_tool` detects the non-TTY pipe and reads stdin directly,
/// never spawning the nslookup binary at all.
pub(crate) fn run_nslookup(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    if args.is_empty() {
        use std::io::IsTerminal as _;
        if std::io::stdin().is_terminal() {
            eprintln!("skim nslookup: missing domain argument");
            eprintln!("usage: skim nslookup <domain> [options]");
            return Ok(std::process::ExitCode::FAILURE);
        }
    }
    run_infra_tool(CONFIG_NSLOOKUP, args, ctx, |_| {}, parse_nslookup_impl)
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
// nslookup parse chain
// ============================================================================

/// Three-tier parse function for nslookup output.
fn parse_nslookup_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);

    if let Some(result) = try_parse_nslookup_structured(&combined) {
        return ParseResult::Full(result);
    }

    if let Some(result) = try_parse_nslookup_regex(&combined) {
        return ParseResult::Degraded(
            result,
            vec!["nslookup: structured parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

/// Tier 1: Extract nslookup records from structured output.
fn try_parse_nslookup_structured(text: &str) -> Option<InfraResult> {
    // Guard: must look like nslookup output
    if !text.contains("Server:") && !text.contains("server can't find") {
        return None;
    }

    let mut items: Vec<InfraItem> = Vec::new();

    // NXDOMAIN / error path
    if let Some(caps) = RE_NSL_CANT_FIND.captures(text) {
        let domain = caps[1].trim_end_matches(':').to_string();
        let server = extract_nslookup_server(text).unwrap_or_else(|| "unknown".to_string());

        // No separate error item: the summary line already carries the full
        // NXDOMAIN signal. Adding an item repeats the domain a third time and
        // makes the compressed output larger than the raw input (AC-NSL-10).
        let summary = format!("{domain} → NXDOMAIN (via {server})");
        return Some(InfraResult::new(
            "nslookup".to_string(),
            "query".to_string(),
            summary,
            items,
        ));
    }

    // Extract DNS server
    let server = extract_nslookup_server(text).unwrap_or_else(|| "unknown".to_string());

    // Extract records
    let records = extract_nslookup_records(text);
    let record_count = records.len();
    items.extend(records);

    if items.is_empty() && server == "unknown" {
        return None;
    }

    // Try to find the queried domain from Name: lines (A/AAAA queries),
    // or from MX record lines (MX queries, which have no Name: line).
    // MX format: `example.com\tmail exchanger = 0 .` — domain is first field.
    let domain = RE_NSL_NAME
        .captures(text)
        .map(|c| c[1].trim().to_string())
        .or_else(|| extract_nslookup_mx_domain(text))
        .unwrap_or_else(|| "unknown".to_string());

    let summary = format!("{domain} → {record_count} records (via {server})");

    Some(InfraResult::new(
        "nslookup".to_string(),
        "query".to_string(),
        summary,
        items,
    ))
}

/// Extract the DNS server IP/name used for the query.
fn extract_nslookup_server(text: &str) -> Option<String> {
    // Prefer the Address line with #port (= DNS server, not result)
    if let Some(caps) = RE_NSL_SERVER_ADDR.captures(text) {
        return Some(caps[1].to_string());
    }
    // Fallback: Server: name line
    RE_NSL_SERVER
        .captures(text)
        .map(|c| c[1].trim().to_string())
}

/// Extract the queried domain from an MX record line.
///
/// nslookup MX output has no `Name:` line. The domain appears as the first
/// whitespace-delimited token on a line that contains "mail exchanger":
///
/// ```text
/// example.com    mail exchanger = 0 .
/// ```
fn extract_nslookup_mx_domain(text: &str) -> Option<String> {
    for line in text.lines() {
        if line.contains("mail exchanger") {
            let domain = line.split_whitespace().next()?;
            if !domain.is_empty() {
                return Some(domain.trim_end_matches('.').to_string());
            }
        }
    }
    None
}

/// Extract answer records from nslookup output.
///
/// Skips the DNS server header block (Server:/Address:#port lines) and parses
/// the answer section for A, MX, CNAME, and TXT records. Uses the presence of
/// `#port` to distinguish server Address lines from answer Address lines.
fn extract_nslookup_records(text: &str) -> Vec<InfraItem> {
    let mut items = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();

        // Skip header/comment lines
        if trimmed.is_empty()
            || trimmed.starts_with("Server:")
            || trimmed.starts_with("Authoritative")
            || trimmed.starts_with("Non-authoritative")
            || trimmed.starts_with("**")
        {
            continue;
        }

        // Skip the server Address line (has #port)
        if trimmed.starts_with("Address:") && trimmed.contains('#') {
            continue;
        }

        if let Some(item) = try_parse_nslookup_record_line(trimmed) {
            items.push(item);
        }
    }

    items
}

/// Try to parse a single trimmed nslookup answer line into an `InfraItem`.
///
/// Returns `Some` for MX, CNAME, TXT, A/AAAA (standard Address:), and macOS
/// `address = <ip>` formats. Returns `None` for lines that carry no answer record.
fn try_parse_nslookup_record_line(trimmed: &str) -> Option<InfraItem> {
    // MX records: `example.com  mail exchanger = 0 host.`
    if let Some(caps) = RE_NSL_MX.captures(trimmed) {
        return Some(InfraItem {
            label: "MX".to_string(),
            value: format!("{} (priority {})", &caps[2], &caps[1]),
        });
    }

    // CNAME records: `canonical name = target.`
    if let Some(caps) = RE_NSL_CNAME.captures(trimmed) {
        return Some(InfraItem {
            label: "CNAME".to_string(),
            value: caps[1].trim_end_matches('.').to_string(),
        });
    }

    // TXT records: `text = "content"`
    if let Some(caps) = RE_NSL_TXT.captures(trimmed) {
        return Some(InfraItem {
            label: "TXT".to_string(),
            value: caps[1].to_string(),
        });
    }

    // A/AAAA answer Address line (no #port — server Address lines already skipped above)
    if let Some(caps) = RE_NSL_ADDRESS.captures(trimmed) {
        return Some(InfraItem {
            label: "A".to_string(),
            value: caps[1].to_string(),
        });
    }

    // macOS format: `address = <ip>`
    if let Some(caps) = RE_NSL_ADDRESS_MAC.captures(trimmed) {
        return Some(InfraItem {
            label: "A".to_string(),
            value: caps[1].to_string(),
        });
    }

    None
}

/// Tier 2: Simple fallback — look for Server: line indicating DNS output.
fn try_parse_nslookup_regex(text: &str) -> Option<InfraResult> {
    if let Some(caps) = RE_NSL_SERVER.captures(text) {
        let server = caps[1].trim().to_string();
        let items = vec![InfraItem {
            label: "server".to_string(),
            value: server.clone(),
        }];
        return Some(InfraResult::new(
            "nslookup".to_string(),
            "query".to_string(),
            format!("DNS via {server}"),
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
    use crate::cmd::test_support::{load_fixture as _load_fixture, make_output};

    fn load_fixture(name: &str) -> String {
        _load_fixture("infra", name)
    }

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

    // ========================================================================
    // nslookup: Tier 1
    // ========================================================================

    #[test]
    fn test_nslookup_tier1_a_record() {
        let input = load_fixture("nslookup_a_record.txt");
        let result = try_parse_nslookup_structured(&input);
        assert!(result.is_some(), "Expected Tier 1 parse for A record");
        let result = result.unwrap();
        assert!(
            result.as_ref().contains("records"),
            "Summary should contain record count: {}",
            result.as_ref()
        );
    }

    #[test]
    fn test_nslookup_tier1_mx_record() {
        let input = load_fixture("nslookup_mx_record.txt");
        let result = try_parse_nslookup_structured(&input);
        assert!(result.is_some(), "Expected Tier 1 parse for MX record");
        let result = result.unwrap();
        assert!(
            result.items.iter().any(|i| i.label == "MX"),
            "Expected MX item"
        );
    }

    #[test]
    fn test_nslookup_tier1_nxdomain() {
        let input = load_fixture("nslookup_nxdomain.txt");
        let result = try_parse_nslookup_structured(&input);
        assert!(result.is_some(), "Expected Tier 1 parse for NXDOMAIN");
        let result = result.unwrap();
        assert!(
            result.as_ref().contains("NXDOMAIN"),
            "Summary should contain NXDOMAIN: {}",
            result.as_ref()
        );
        // No separate error item — the summary line carries the full NXDOMAIN
        // signal. A separate item would repeat the domain and bloat the output
        // past the raw input size (AC-NSL-10).
        assert!(
            result.items.is_empty(),
            "NXDOMAIN result must have no items (summary line is sufficient): {:?}",
            result.items
        );
    }

    #[test]
    fn test_nslookup_tier1_nxdomain_compression() {
        // AC-NSL-10: NXDOMAIN compressed output must be strictly smaller than raw input.
        let input = load_fixture("nslookup_nxdomain.txt");
        let output = make_output(&input);
        let result = parse_nslookup_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full parse for NXDOMAIN, got {}",
            result.tier_name()
        );
        // The rendered output (as_ref() + newline) must be smaller than the raw fixture.
        // parse_nslookup_impl returns a ParseResult<InfraResult>; the InfraResult is
        // accessible via the Full variant. We compare the rendered string length plus 1
        // (for the trailing newline that the CLI emits) against the raw input length.
        if let ParseResult::Full(ref r) = result {
            let rendered_len = r.as_ref().len() + 1; // +1 for trailing newline
            assert!(
                rendered_len < input.len(),
                "NXDOMAIN compressed output ({} bytes) must be smaller than raw input ({} bytes):\n{}",
                rendered_len,
                input.len(),
                r.as_ref()
            );
        } else {
            panic!("Expected Full parse result");
        }
    }

    #[test]
    fn test_nslookup_tier1_mx_domain_extracted() {
        // AC-NSL-MX: MX queries must show the queried domain, not "unknown".
        let input = load_fixture("nslookup_mx_record.txt");
        let result = try_parse_nslookup_structured(&input);
        assert!(result.is_some(), "Expected Tier 1 parse for MX");
        let result = result.unwrap();
        let summary = result.as_ref();
        assert!(
            !summary.contains("unknown"),
            "MX summary must not contain 'unknown' domain: {}",
            summary
        );
        assert!(
            summary.contains("example.com"),
            "MX summary must contain queried domain 'example.com': {}",
            summary
        );
    }

    #[test]
    fn test_nslookup_tier1_server_in_summary() {
        let input = load_fixture("nslookup_a_record.txt");
        let result = try_parse_nslookup_structured(&input).unwrap();
        assert!(
            result.as_ref().contains("via"),
            "Summary should contain 'via <server>': {}",
            result.as_ref()
        );
    }

    #[test]
    fn test_nslookup_tier1_macos_address_format() {
        // AC-NSL-8: macOS nslookup uses `address = x.x.x.x` (lowercase, equals sign).
        // This must be extracted as Tier 1 Full with the address in items.
        let input = "Server:\t8.8.8.8\nAddress:\t8.8.8.8#53\n\nNon-authoritative answer:\nName:\texample.com\naddress = 93.184.216.34\n";
        let result = try_parse_nslookup_structured(input);
        assert!(
            result.is_some(),
            "Expected Tier 1 parse for macOS nslookup format"
        );
        let result = result.unwrap();
        let a_items: Vec<_> = result.items.iter().filter(|i| i.label == "A").collect();
        assert!(
            !a_items.is_empty(),
            "Expected at least one A item from macOS address = format, got items: {:?}",
            result.items
        );
        assert!(
            a_items.iter().any(|i| i.value.contains("93.184.216.34")),
            "Expected 93.184.216.34 in A items, got: {:?}",
            a_items
        );
    }

    #[test]
    fn test_nslookup_tier1_cname_record() {
        // Synthetic nslookup CNAME output (Linux format).
        let input = "Server:\t8.8.8.8\nAddress:\t8.8.8.8#53\n\nNon-authoritative answer:\nwww.example.com\tcanonical name = example.com.\nName:\texample.com\nAddress: 93.184.216.34\n";
        let result = try_parse_nslookup_structured(input);
        assert!(
            result.is_some(),
            "Expected Tier 1 parse for CNAME record output"
        );
        let result = result.unwrap();
        let cname_items: Vec<_> = result.items.iter().filter(|i| i.label == "CNAME").collect();
        assert!(
            !cname_items.is_empty(),
            "Expected at least one CNAME item, got items: {:?}",
            result.items
        );
        assert!(
            cname_items.iter().any(|i| i.value.contains("example.com")),
            "Expected 'example.com' in CNAME value, got: {:?}",
            cname_items
        );
    }

    #[test]
    fn test_nslookup_tier1_txt_record() {
        // Synthetic nslookup TXT output (Linux format).
        let input = "Server:\t8.8.8.8\nAddress:\t8.8.8.8#53\n\nNon-authoritative answer:\nexample.com\ttext = \"v=spf1 include:_spf.example.com ~all\"\n";
        let result = try_parse_nslookup_structured(input);
        assert!(
            result.is_some(),
            "Expected Tier 1 parse for TXT record output"
        );
        let result = result.unwrap();
        let txt_items: Vec<_> = result.items.iter().filter(|i| i.label == "TXT").collect();
        assert!(
            !txt_items.is_empty(),
            "Expected at least one TXT item, got items: {:?}",
            result.items
        );
        assert!(
            txt_items.iter().any(|i| i.value.contains("v=spf1")),
            "Expected SPF content in TXT value, got: {:?}",
            txt_items
        );
    }

    // ========================================================================
    // nslookup: try_parse_nslookup_record_line
    // ========================================================================

    #[test]
    fn test_try_parse_nslookup_record_line_mx() {
        let item =
            try_parse_nslookup_record_line("example.com\tmail exchanger = 10 mail.example.com.");
        assert!(item.is_some());
        let item = item.unwrap();
        assert_eq!(item.label, "MX");
        assert!(
            item.value.contains("mail.example.com"),
            "got: {}",
            item.value
        );
        assert!(item.value.contains("priority 10"), "got: {}", item.value);
    }

    #[test]
    fn test_try_parse_nslookup_record_line_cname() {
        let item = try_parse_nslookup_record_line("www.example.com\tcanonical name = example.com.");
        assert!(item.is_some());
        let item = item.unwrap();
        assert_eq!(item.label, "CNAME");
        assert_eq!(item.value, "example.com");
    }

    #[test]
    fn test_try_parse_nslookup_record_line_txt() {
        let item = try_parse_nslookup_record_line(r#"example.com	text = "v=spf1 ~all""#);
        assert!(item.is_some());
        let item = item.unwrap();
        assert_eq!(item.label, "TXT");
        assert_eq!(item.value, "v=spf1 ~all");
    }

    #[test]
    fn test_try_parse_nslookup_record_line_no_match() {
        // Non-answer lines must return None
        assert!(try_parse_nslookup_record_line("").is_none());
        assert!(try_parse_nslookup_record_line("Server:\t8.8.8.8").is_none());
        assert!(try_parse_nslookup_record_line("Non-authoritative answer:").is_none());
    }

    // ========================================================================
    // nslookup: Tier 2
    // ========================================================================

    #[test]
    fn test_nslookup_tier2_server_line() {
        let input = "Server:\t8.8.8.8\n";
        let result = try_parse_nslookup_regex(input);
        assert!(result.is_some(), "Tier 2 should match Server: line");
    }

    #[test]
    fn test_nslookup_tier2_no_false_positive() {
        let input = "garbage output without Server line\n";
        let result = try_parse_nslookup_regex(input);
        assert!(result.is_none(), "Tier 2 should not match random output");
    }

    // ========================================================================
    // nslookup: parse_nslookup_impl
    // ========================================================================

    #[test]
    fn test_nslookup_parse_impl_produces_full() {
        let input = load_fixture("nslookup_a_record.txt");
        let output = make_output(&input);
        let result = parse_nslookup_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_nslookup_parse_impl_passthrough_for_garbage() {
        let output = make_output("some random output\n");
        let result = parse_nslookup_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }
}
