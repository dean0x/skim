//! `nslookup` parser with three-tier degradation.

use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::runner::CommandOutput;

use super::super::{InfraToolConfig, combine_stdout_stderr, run_infra_tool};

// ============================================================================
// Tool config
// ============================================================================

pub(crate) const CONFIG_NSLOOKUP: InfraToolConfig<'static> = InfraToolConfig {
    program: "nslookup",
    env_overrides: &[],
    install_hint: "Install via: apt install dnsutils / brew install bind",
    // nslookup also uses TABs as field separators (Server:\t<ip>, Address:\t<ip>#port).
    // See CONFIG_DIG comment for the full rationale.
    skip_ansi_strip: true,
};

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
// Entry point
// ============================================================================

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
    use super::super::test_helpers::{load_fixture, make_output};
    use super::*;

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
