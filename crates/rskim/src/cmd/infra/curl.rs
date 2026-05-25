//! curl parser with three-tier degradation and hardened response handling (#116, #169).
//!
//! Executes `curl` and parses the output into structured `InfraResult`.
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Detect JSON body in output and parse it
//! - **Tier 2 (Degraded)**: Strip verbose lines, extract HTTP status
//! - **Tier 3 (Passthrough)**: Raw stdout+stderr concatenation
//!
//! Hardening additions (#169):
//! - **6A**: Header extraction for `-i` mode (HTTP/x.x status + response headers)
//! - **6B**: Error body awareness — 4xx/5xx responses surface error messages first
//! - **6C**: Write-out format (`-w`) — trailing 3-digit status code stripped from body
//! - **6D**: Verbose stderr enhancement — extracts response headers from `< header:` lines

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::output::ParseResult;
use crate::output::canonical::{InfraItem, InfraResult};
use crate::runner::CommandOutput;

use super::combine_stdout_stderr;
use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "curl",
    env_overrides: &[],
    install_hint: "Install curl: https://curl.se/",
    family: "infra",
    skip_ansi_strip: false,
    command_type: CommandType::Infra,
};

/// Maximum number of source fields in a JSON response before a truncation notice is added.
const MAX_ITEMS: usize = 100;

/// Maximum byte length of JSON input accepted for Tier 1 parsing.
///
/// Inputs larger than this are skipped and fall through to the regex tier,
/// preventing unbounded allocation on pathological or adversarial responses.
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

// ============================================================================
// 6D: Verbose stderr regex (existing + new header extraction)
// ============================================================================

static RE_CURL_HTTP_STATUS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^< HTTP/[\d.]+ (\d{3})\s*(.*)").unwrap());

/// Matches lines that are curl verbose metadata (not response body).
/// Uses literal space instead of \s so indented body content is preserved.
static RE_CURL_VERBOSE_LINE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[*><{} ]").unwrap());

// ============================================================================
// 6A: Header extraction for -i mode
// ============================================================================

/// Matches the HTTP status line (first line in `-i` output).
static RE_HTTP_STATUS_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^HTTP/[\d.]+ (\d{3})(?:\s+(.*))?$").unwrap());

/// Matches a response header line: `Name: value`.
static RE_HEADER_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z][\w-]+):\s*(.+)$").unwrap());

/// Key headers to surface when parsing response headers (6A/6D).
const KEY_HEADERS: &[&str] = &[
    "content-type",
    "location",
    "content-length",
    "x-ratelimit-remaining",
    "x-ratelimit-limit",
];

/// Set-Cookie is counted, not echoed verbatim.
const SET_COOKIE_HEADER: &str = "set-cookie";

/// Authorization header value is always redacted.
const AUTH_HEADER: &str = "authorization";

/// Proxy-Authorization carries the same credential material as Authorization.
const PROXY_AUTH_HEADER: &str = "proxy-authorization";

/// Run `skim curl [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    // No flag injection for curl — flags are too varied
    run_tool(CONFIG, args, ctx, |_| {}, parse_impl)
}

/// Determine the effective HTTP status code string from the two possible sources.
///
/// Precedence: `-i` header items first (the parsed response headers are authoritative),
/// falling back to the verbose stderr status if the `-i` block did not carry one.
fn resolve_http_status(
    header_items_from_i: &[InfraItem],
    http_status_from_verbose: Option<&str>,
) -> Option<String> {
    header_items_from_i
        .iter()
        .find(|i| i.label == "status")
        .and_then(|i| {
            // Extract just the code from e.g. "HTTP/1.1 200 OK"
            let code = i.value.split_whitespace().nth(1).unwrap_or("");
            if code.is_empty() {
                None
            } else {
                Some(code.to_string())
            }
        })
        .or_else(|| http_status_from_verbose.map(str::to_string))
}

/// Three-tier parse function for curl output.
fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    let mut extra_header_items: Vec<InfraItem> = Vec::new();

    // 6D: Check stderr for verbose response headers (curl -v)
    let (http_status_from_verbose, verbose_header_items) = extract_verbose_metadata(&output.stderr);
    extra_header_items.extend(verbose_header_items);

    // 6A: Try to split `-i` header/body output.
    // Use Cow to borrow when no stripping is needed, own only when headers are present.
    let (body_text, header_items_from_i): (Cow<'_, str>, Vec<InfraItem>) =
        if let Some((header_items, body)) = try_split_header_body(&output.stdout) {
            (Cow::Borrowed(body), header_items)
        } else {
            (Cow::Borrowed(&output.stdout), Vec::new())
        };

    // Determine effective HTTP status
    let http_status =
        resolve_http_status(&header_items_from_i, http_status_from_verbose.as_deref());

    // 6C: Strip write-out trailing status code from body.
    // Only allocate a new String when a write-out suffix is actually stripped.
    let (body_text, writeout_item) =
        if let Some((wo, stripped)) = try_extract_writeout(body_text.as_ref()) {
            (Cow::Owned(stripped.to_string()), Some(wo))
        } else {
            (body_text, None)
        };

    // Build initial items from -i headers (if any)
    let mut header_items: Vec<InfraItem> = header_items_from_i;
    header_items.extend(extra_header_items);
    if let Some(wo) = writeout_item {
        header_items.push(wo);
    }

    // Tier 1: Try JSON parse
    if let Some(mut result) = try_parse_json(body_text.as_ref(), http_status.as_deref()) {
        merge_header_items(header_items, &mut result);
        return ParseResult::Full(result);
    }

    let combined = combine_stdout_stderr(output);

    // Tier 2: regex fallback
    if let Some(mut result) = try_parse_regex(&combined) {
        merge_header_items(header_items, &mut result);
        return ParseResult::Degraded(
            result,
            vec!["curl: structured parse failed, using regex".to_string()],
        );
    }

    ParseResult::Passthrough(combined.into_owned())
}

/// Prepend `header_items` before `result.items`, deduplicating the `status` label
/// (the status is already present in `header_items` when `-i` mode is used, so any
/// duplicate `status` item emitted by the JSON/regex tier is dropped).
fn merge_header_items(header_items: Vec<InfraItem>, result: &mut InfraResult) {
    if header_items.is_empty() {
        return;
    }
    let tier_items: Vec<InfraItem> = result
        .items
        .drain(..)
        .filter(|i| i.label != "status")
        .collect();
    result.items = header_items;
    result.items.extend(tier_items);
}

// ============================================================================
// Shared header classification logic (6A + 6D)
// ============================================================================

/// Classify a single response header into an `InfraItem`, updating `set_cookie_count`
/// for Set-Cookie headers (which are counted rather than emitted verbatim).
///
/// Returns:
/// - `Some(item)` — emit this item into the output list
/// - `None` — header was counted (Set-Cookie) or silently dropped (unknown)
fn classify_header(name: &str, value: &str, set_cookie_count: &mut usize) -> Option<InfraItem> {
    if name.eq_ignore_ascii_case(AUTH_HEADER) || name.eq_ignore_ascii_case(PROXY_AUTH_HEADER) {
        Some(InfraItem {
            label: name.to_string(),
            value: "***".to_string(),
        })
    } else if name.eq_ignore_ascii_case(SET_COOKIE_HEADER) {
        *set_cookie_count += 1;
        None
    } else if KEY_HEADERS.iter().any(|&h| name.eq_ignore_ascii_case(h)) {
        Some(InfraItem {
            label: name.to_string(),
            value: value.to_string(),
        })
    } else {
        None
    }
}

// ============================================================================
// 6A: Header extraction for -i / -iL mode
// ============================================================================

/// Split `-i` style output into (header_items, body_text).
///
/// For redirect chains (`-iL`), only the LAST HTTP response's headers are used.
/// Returns `None` if the output does not start with an HTTP status line.
fn try_split_header_body(stdout: &str) -> Option<(Vec<InfraItem>, &str)> {
    // Must start with an HTTP status line
    let first_line = stdout.lines().next()?.trim();
    if !RE_HTTP_STATUS_LINE.is_match(first_line) {
        return None;
    }

    // For redirect chains, find the LAST HTTP/ status line and use that response.
    // We split on blank lines; each blank line separates header block from body.
    // Multiple `HTTP/` lines indicate a redirect chain.
    let last_http_pos = find_last_http_response_start(stdout);
    let from = &stdout[last_http_pos..];

    // Split at first blank line (handles \r\n and \n)
    let body_start = find_body_start(from)?;
    let headers_section = &from[..body_start.0];
    let body_text = &from[body_start.1..]; // skip the blank line itself

    let mut items: Vec<InfraItem> = Vec::new();
    let mut set_cookie_count = 0usize;

    for line in headers_section.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }

        // HTTP status line
        if RE_HTTP_STATUS_LINE.is_match(line) {
            items.push(InfraItem {
                label: "status".to_string(),
                value: line.to_string(),
            });
            continue;
        }

        // Regular header line
        if let Some(caps) = RE_HEADER_LINE.captures(line) {
            let name = &caps[1];
            let value = caps[2].trim();
            if let Some(item) = classify_header(name, value, &mut set_cookie_count) {
                items.push(item);
            }
        }
    }

    // Add Set-Cookie count if any cookies were present
    if set_cookie_count > 0 {
        items.push(InfraItem {
            label: "Set-Cookie".to_string(),
            value: format!("({set_cookie_count} values)"),
        });
    }

    Some((items, body_text))
}

/// Find the byte offset of the last `HTTP/` status line in `text`.
///
/// For redirect chains, this skips earlier responses and returns the start
/// of the final response's headers.
///
/// Uses direct byte scanning for `\nHTTP/` and `\r\nHTTP/` substrings rather
/// than iterating with `str::lines()`. `str::lines()` strips `\r` from `\r\n`
/// lines, causing the reconstructed byte offset to drift by 1 per CRLF line and
/// producing incorrect body splits for redirect chains.
fn find_last_http_response_start(text: &str) -> usize {
    let mut last_pos = 0usize;

    // The first line is a candidate if the text opens with HTTP/.
    if text.starts_with("HTTP/") {
        last_pos = 0;
    }

    // Scan for \nHTTP/ — works for both \n and \r\n line endings because
    // the byte immediately after \n is the start of the next line.
    let bytes = text.as_bytes();
    for pos in memchr::memmem::find_iter(bytes, b"\nHTTP/") {
        let abs = pos + 1; // +1: skip the leading \n; abs points at 'H'
        // Match only the first line at this position (regex has $ anchor).
        let line_end = text[abs..]
            .find('\n')
            .map(|i| abs + i)
            .unwrap_or(text.len());
        let candidate = text[abs..line_end].trim_end_matches('\r');
        if RE_HTTP_STATUS_LINE.is_match(candidate) {
            last_pos = abs;
        }
    }

    last_pos
}

/// Find the byte range of the first blank line in `text`.
///
/// Returns `Some((blank_start, body_start))` where:
/// - `blank_start` is where the blank line begins
/// - `body_start` is where the body text begins (after the blank line)
///
/// Handles both `\n\n` and `\r\n\r\n`.
fn find_body_start(text: &str) -> Option<(usize, usize)> {
    // Look for \r\n\r\n first (HTTP/1.1 style)
    if let Some(pos) = text.find("\r\n\r\n") {
        return Some((pos, pos + 4));
    }
    // Fall back to \n\n
    if let Some(pos) = text.find("\n\n") {
        return Some((pos, pos + 2));
    }
    None
}

// ============================================================================
// 6B: Error message extraction for 4xx/5xx responses
// ============================================================================

/// Extract a human-readable error message from a JSON error response.
///
/// Priority order:
/// 1. `json["error"]["message"]` (nested object)
/// 2. `json["error"]` (if string)
/// 3. `json["message"]`
/// 4. `json["detail"]`
/// 5. `json["errors"]` (if array — reports count)
/// 6. `json["error_description"]`
fn extract_error_message(json: &Value) -> Option<String> {
    // 1. Nested error object with message field
    if let Some(err_obj) = json.get("error").and_then(|e| e.as_object())
        && let Some(msg) = err_obj.get("message").and_then(|m| m.as_str())
    {
        return Some(truncate_message(msg));
    }

    // 2. error as string
    if let Some(err_str) = json.get("error").and_then(|e| e.as_str()) {
        return Some(truncate_message(err_str));
    }

    // 3. message field
    if let Some(msg) = json.get("message").and_then(|m| m.as_str()) {
        return Some(truncate_message(msg));
    }

    // 4. detail field
    if let Some(detail) = json.get("detail").and_then(|d| d.as_str()) {
        return Some(truncate_message(detail));
    }

    // 5. errors array — report count
    if let Some(errs) = json.get("errors").and_then(|e| e.as_array()) {
        return Some(format!("{} errors", errs.len()));
    }

    // 6. error_description
    if let Some(desc) = json.get("error_description").and_then(|d| d.as_str()) {
        return Some(truncate_message(desc));
    }

    None
}

fn truncate_message(msg: &str) -> String {
    if msg.len() > 200 {
        // Find a char boundary at or before byte 200 to avoid panicking on multi-byte UTF-8.
        let end = msg
            .char_indices()
            .map(|(i, _)| i)
            .take_while(|&i| i <= 200)
            .last()
            .unwrap_or(0);
        format!("{}...", &msg[..end])
    } else {
        msg.to_string()
    }
}

// ============================================================================
// 6C: Write-out format detection
// ============================================================================

/// Check if the last non-empty line of `text` is a bare 3-digit HTTP status code.
///
/// If so, returns `(InfraItem { label: "write_out", value: code }, rest_of_text)`.
fn try_extract_writeout(text: &str) -> Option<(InfraItem, &str)> {
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        return None;
    }

    // Find the last non-empty line
    let last_newline = trimmed.rfind('\n').map(|p| p + 1).unwrap_or(0);
    let last_line = trimmed[last_newline..].trim();

    // Check if it's a bare 3-digit number
    if last_line.len() == 3 && last_line.chars().all(|c| c.is_ascii_digit()) {
        let body_end = trimmed[..last_newline].trim_end().len();
        // Body is everything before the last line, trimmed
        let body = &text[..body_end];
        let item = InfraItem {
            label: "write_out".to_string(),
            value: last_line.to_string(),
        };
        return Some((item, body));
    }

    None
}

// ============================================================================
// 6D: Verbose stderr header extraction (replaces extract_http_status)
// ============================================================================

/// Extract HTTP status and key response headers from curl verbose stderr output.
///
/// Lines matching `^< HTTP/` give the status.
/// Lines matching `^< Header: value` give response headers.
///
/// Returns `(http_status, header_items)`.
fn extract_verbose_metadata(stderr: &str) -> (Option<String>, Vec<InfraItem>) {
    let mut http_status: Option<String> = None;
    let mut header_items: Vec<InfraItem> = Vec::new();
    let mut set_cookie_count = 0usize;

    for line in stderr.lines() {
        // HTTP status line: `< HTTP/2 200` or `< HTTP/1.1 200 OK`
        if let Some(caps) = RE_CURL_HTTP_STATUS.captures(line) {
            let code = &caps[1];
            let reason = caps[2].trim();
            http_status = Some(if reason.is_empty() {
                code.to_string()
            } else {
                format!("{code} {reason}")
            });
            // Reset headers for each new HTTP response (redirect chains)
            header_items.clear();
            set_cookie_count = 0;
            continue;
        }

        // Response header lines: `< Header-Name: value`
        if let Some(rest) = line.strip_prefix("< ")
            && let Some(caps) = RE_HEADER_LINE.captures(rest)
        {
            let name = &caps[1];
            let value = caps[2].trim();
            if let Some(item) = classify_header(name, value, &mut set_cookie_count) {
                header_items.push(item);
            }
        }
    }

    if set_cookie_count > 0 {
        header_items.push(InfraItem {
            label: "Set-Cookie".to_string(),
            value: format!("({set_cookie_count} values)"),
        });
    }

    (http_status, header_items)
}

// ============================================================================
// Tier 1: JSON body detection
// ============================================================================

/// Maximum number of top-level keys shown per JSON object in Tier 1.
///
/// 20 keys gives agents enough context to understand the response schema
/// without requiring a follow-up request for most REST API responses.
/// Keys beyond this cap produce a `truncated` notice item rather than
/// the blanket 5-key cut that was masking useful fields.
const MAX_OBJECT_KEYS: usize = 20;

/// Summarize a JSON object: collect top-level key-value items (up to [`MAX_OBJECT_KEYS`]).
///
/// Returns a human-readable summary string and the collected items.
/// When the object has more than [`MAX_OBJECT_KEYS`] keys, a `truncated`
/// notice item is appended by the caller via the existing truncation path.
fn summarize_json_object(map: &serde_json::Map<String, Value>) -> (String, Vec<InfraItem>) {
    let count = map.len();
    let items: Vec<InfraItem> = map
        .iter()
        .take(MAX_OBJECT_KEYS)
        .map(|(k, v)| InfraItem {
            label: k.clone(),
            value: json_value_to_string(v),
        })
        .collect();
    let summary = format!(
        "object with {count} key{}",
        if count == 1 { "" } else { "s" }
    );
    (summary, items)
}

/// Summarize a JSON array: record element count.
fn summarize_json_array(arr: &[Value]) -> (String, Vec<InfraItem>) {
    let count = arr.len();
    let items = vec![InfraItem {
        label: "count".to_string(),
        value: count.to_string(),
    }];
    let summary = format!(
        "array with {count} element{}",
        if count == 1 { "" } else { "s" }
    );
    (summary, items)
}

/// Convert a JSON value to a compact display string.
fn json_value_to_string(val: &Value) -> String {
    val.as_str()
        .map(|s| s.to_string())
        .or_else(|| val.as_u64().map(|n| n.to_string()))
        .unwrap_or_else(|| val.to_string())
}

/// Parse JSON body from curl response and summarize (slim dispatcher).
///
/// 6B: When http_status indicates 4xx/5xx, extract the error message and
/// use it as the summary.
fn try_parse_json(stdout: &str, http_status: Option<&str>) -> Option<InfraResult> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() > MAX_JSON_BYTES {
        return None;
    }

    let json_val: serde_json::Value = serde_json::from_str(trimmed).ok()?;

    let mut items: Vec<InfraItem> = Vec::new();
    if let Some(status) = http_status {
        items.push(InfraItem {
            label: "status".to_string(),
            value: status.to_string(),
        });
    }

    // 6B: error awareness for 4xx/5xx
    let is_error = http_status
        .map(|s| s.starts_with('4') || s.starts_with('5'))
        .unwrap_or(false);

    let summary_str = match &json_val {
        Value::Array(arr) => {
            let (summary, extra) = summarize_json_array(arr);
            items.extend(extra);
            // Truncation notice when source array exceeds cap
            if arr.len() > MAX_ITEMS {
                items.push(InfraItem {
                    label: "truncated".to_string(),
                    value: format!("output capped at {MAX_ITEMS} items"),
                });
            }
            summary
        }
        Value::Object(map) => {
            let error_msg = if is_error {
                extract_error_message(&json_val)
            } else {
                None
            };

            let (summary, extra) = summarize_json_object(map);
            items.extend(extra);
            // Truncation notice when source object exceeds MAX_OBJECT_KEYS
            if map.len() > MAX_OBJECT_KEYS {
                items.push(InfraItem {
                    label: "truncated".to_string(),
                    value: format!("showing {MAX_OBJECT_KEYS} of {} keys", map.len()),
                });
            }

            // 6B: override summary with error message for error responses
            if let Some(err) = error_msg {
                let status_prefix = http_status.unwrap_or("ERROR");
                format!("ERROR {status_prefix}: {err}")
            } else {
                summary
            }
        }
        _ => return None,
    };

    Some(InfraResult::new(
        "curl".to_string(),
        "response".to_string(),
        summary_str,
        items,
    ))
}

// ============================================================================
// Tier 2: verbose output fallback
// ============================================================================

/// Parse curl verbose output by extracting non-verbose lines.
fn try_parse_regex(text: &str) -> Option<InfraResult> {
    let mut items: Vec<InfraItem> = Vec::new();

    // Extract HTTP status
    for line in text.lines() {
        if let Some(caps) = RE_CURL_HTTP_STATUS.captures(line) {
            items.push(InfraItem {
                label: "status".to_string(),
                value: format!("{} {}", &caps[1], caps[2].trim()),
            });
        }
    }

    // Extract body lines (non-verbose, non-header lines)
    let body_lines: Vec<&str> = text
        .lines()
        .filter(|line| !RE_CURL_VERBOSE_LINE.is_match(line) && !line.is_empty())
        .take(3)
        .collect();

    if !body_lines.is_empty() {
        items.push(InfraItem {
            label: "body_preview".to_string(),
            value: body_lines.join(" ").chars().take(200).collect(),
        });
    }

    if items.is_empty() {
        return None;
    }

    let summary = items
        .iter()
        .find(|i| i.label == "status")
        .map(|i| i.value.clone())
        .unwrap_or_else(|| "response received".to_string());

    Some(InfraResult::new(
        "curl".to_string(),
        "response".to_string(),
        summary,
        items,
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_support::{load_fixture, make_output_full};

    // ---- Pre-existing tests (must remain passing) ----

    #[test]
    fn test_tier1_curl_json_response() {
        let input = load_fixture("infra", "curl_json_response.txt");
        let result = try_parse_json(&input, Some("200 OK"));
        assert!(result.is_some(), "Expected Tier 1 JSON parse to succeed");
        let result = result.unwrap();
        assert!(result.as_ref().contains("curl "));
    }

    #[test]
    fn test_tier1_curl_non_json_fails() {
        let result = try_parse_json("not json at all", None);
        assert!(result.is_none());
    }

    #[test]
    fn test_tier2_curl_regex() {
        let input = load_fixture("infra", "curl_verbose.txt");
        let result = try_parse_regex(&input);
        assert!(result.is_some(), "Expected Tier 2 parse to succeed");
        let result = result.unwrap();
        assert!(result.items.iter().any(|i| i.label == "status"));
    }

    #[test]
    fn test_parse_impl_produces_full() {
        let input = load_fixture("infra", "curl_json_response.txt");
        let output = make_output_full(&input, "", Some(0));
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "Expected Full parse result, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_parse_impl_garbage_produces_passthrough() {
        let output = make_output_full("", "", Some(7));
        let result = parse_impl(&output);
        assert!(
            result.is_passthrough(),
            "Expected Passthrough, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_tier2_preserves_indented_body_content() {
        // Regression: RE_CURL_VERBOSE_LINE must not drop tab-indented lines.
        let text = "< HTTP/1.1 200 OK\n<\n\t<html>\n\t<body>hello</body>\n";
        let result = try_parse_regex(text);
        let result = result.unwrap();
        let body = result.items.iter().find(|i| i.label == "body_preview");
        assert!(
            body.is_some(),
            "Expected indented body lines to be captured"
        );
        let val = &body.unwrap().value;
        assert!(
            val.contains("<html>") || val.contains("<body>"),
            "Indented body content should be preserved, got: {val}"
        );
    }

    #[test]
    fn test_tier1_max_items_cap() {
        let fields: String = (0..200)
            .map(|i| format!("\"key{i}\": \"val{i}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let json = format!("{{{fields}}}");
        let result = try_parse_json(&json, None);
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(
            result.items.iter().any(|i| i.label == "truncated"),
            "Expected truncation notice item when JSON has > {MAX_ITEMS} fields"
        );
    }

    #[test]
    fn test_summarize_json_object() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"a":1,"b":"two","c":true}"#).unwrap();
        let map = json.as_object().unwrap();
        let (summary, items) = summarize_json_object(map);
        assert!(summary.contains("3 key"), "Got: {summary}");
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn test_summarize_json_array() {
        let json: serde_json::Value = serde_json::from_str(r#"[1,2,3]"#).unwrap();
        let arr = json.as_array().unwrap();
        let (summary, items) = summarize_json_array(arr);
        assert!(summary.contains("3 element"), "Got: {summary}");
        assert_eq!(items[0].value, "3");
    }

    #[test]
    fn test_json_value_to_string() {
        assert_eq!(
            json_value_to_string(&Value::String("hello".into())),
            "hello"
        );
        assert_eq!(json_value_to_string(&Value::Number(42u64.into())), "42");
        assert_eq!(json_value_to_string(&Value::Bool(true)), "true");
    }

    #[test]
    fn test_parse_impl_text_produces_degraded() {
        let output = make_output_full(
            "",
            "* Connected to api.example.com\n> GET / HTTP/1.1\n< HTTP/1.1 200 OK\n<\n",
            Some(0),
        );
        let result = parse_impl(&output);
        assert!(
            result.is_degraded(),
            "Expected Degraded parse result, got {}",
            result.tier_name()
        );
    }

    #[test]
    fn test_summarize_json_object_shows_up_to_max_keys() {
        let json: serde_json::Value =
            serde_json::from_str(r#"{"a":1,"b":2,"c":3,"d":4,"e":5,"f":6}"#).unwrap();
        let map = json.as_object().unwrap();
        let (_, items) = summarize_json_object(map);
        assert_eq!(
            items.len(),
            6,
            "All 6 keys must be shown (cap is {MAX_OBJECT_KEYS}), got {} items",
            items.len()
        );
    }

    #[test]
    fn test_summarize_json_object_truncation_notice() {
        let fields: String = (0..=MAX_OBJECT_KEYS)
            .map(|i| format!("\"k{i}\": {i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let json_str = format!("{{{fields}}}");
        let result = try_parse_json(&json_str, None).expect("must parse");
        assert!(
            result.items.iter().any(|i| i.label == "truncated"),
            "Must produce truncated notice for >{MAX_OBJECT_KEYS} keys: {:?}",
            result.items.iter().map(|i| &i.label).collect::<Vec<_>>()
        );
    }

    // ---- resolve_http_status tests ----

    #[test]
    fn test_resolve_http_status_prefers_i_headers() {
        let items = vec![InfraItem {
            label: "status".to_string(),
            value: "HTTP/1.1 200 OK".to_string(),
        }];
        let result = resolve_http_status(&items, Some("404"));
        assert_eq!(result.as_deref(), Some("200"));
    }

    #[test]
    fn test_resolve_http_status_falls_back_to_verbose() {
        // Empty header_items_from_i → fall back to verbose status
        let result = resolve_http_status(&[], Some("201"));
        assert_eq!(result.as_deref(), Some("201"));
    }

    #[test]
    fn test_resolve_http_status_none_when_both_absent() {
        let result = resolve_http_status(&[], None);
        assert!(result.is_none());
    }

    #[test]
    fn test_resolve_http_status_i_without_status_item_falls_back() {
        // header_items_from_i is non-empty but contains no "status" label
        let items = vec![InfraItem {
            label: "content-type".to_string(),
            value: "application/json".to_string(),
        }];
        let result = resolve_http_status(&items, Some("302"));
        assert_eq!(result.as_deref(), Some("302"));
    }

    // ---- 6A: Header extraction tests ----

    #[test]
    fn test_header_extraction_i_mode() {
        let input = load_fixture("infra", "curl_headers_body.txt");
        let result = try_split_header_body(&input);
        assert!(result.is_some(), "Expected header/body split to succeed");
        let (items, body) = result.unwrap();
        assert!(
            items.iter().any(|i| i.label == "status"),
            "Should have status item"
        );
        assert!(
            items.iter().any(|i| i.label == "Content-Type"),
            "Should have Content-Type item"
        );
        assert!(
            body.trim().starts_with('{'),
            "Body should be the JSON part, got: {body}"
        );
    }

    #[test]
    fn test_header_extraction_http2() {
        // HTTP/2 status line (no reason phrase)
        let input = "HTTP/2 200\ncontent-type: application/json\n\n{\"ok\":true}\n";
        let result = try_split_header_body(input);
        assert!(result.is_some(), "HTTP/2 status should be recognized");
        let (items, _body) = result.unwrap();
        assert!(items.iter().any(|i| i.label == "status"));
    }

    #[test]
    fn test_header_extraction_redacts_auth() {
        let input = "HTTP/1.1 200 OK\nAuthorization: Bearer secret-token\n\n{\"ok\":true}\n";
        let result = try_split_header_body(input);
        assert!(result.is_some());
        let (items, _) = result.unwrap();
        let auth = items.iter().find(|i| i.label == "Authorization");
        // Note: Authorization is a response header (unusual but possible)
        if let Some(auth_item) = auth {
            assert_eq!(
                auth_item.value, "***",
                "Authorization value should be redacted"
            );
        }
    }

    #[test]
    fn test_header_extraction_redacts_proxy_auth() {
        // Proxy-Authorization carries identical credential material to Authorization
        // and must always be redacted.
        let input = "HTTP/1.1 200 OK\nProxy-Authorization: Basic dXNlcjpwYXNz\n\n{\"ok\":true}\n";
        let result = try_split_header_body(input);
        assert!(result.is_some());
        let (items, _) = result.unwrap();
        let proxy_auth = items
            .iter()
            .find(|i| i.label.eq_ignore_ascii_case("Proxy-Authorization"));
        assert!(
            proxy_auth.is_some(),
            "Proxy-Authorization header should be present in output (as redacted)"
        );
        assert_eq!(
            proxy_auth.unwrap().value,
            "***",
            "Proxy-Authorization value must be redacted"
        );
    }

    #[test]
    fn test_header_extraction_set_cookie_count() {
        let input = load_fixture("infra", "curl_headers_body.txt");
        let (items, _) = try_split_header_body(&input).unwrap();
        let cookie = items.iter().find(|i| i.label == "Set-Cookie");
        assert!(cookie.is_some(), "Set-Cookie should be counted");
        let val = &cookie.unwrap().value;
        assert!(
            val.contains("2"),
            "There are 2 Set-Cookie headers in the fixture, got: {val}"
        );
    }

    #[test]
    fn test_redirect_chain_takes_last() {
        let input = load_fixture("infra", "curl_redirect_chain.txt");
        let result = try_split_header_body(&input);
        assert!(result.is_some(), "Redirect chain should parse");
        let (items, body) = result.unwrap();
        // Should take the LAST HTTP response (200 OK)
        let status = items.iter().find(|i| i.label == "status").unwrap();
        assert!(
            status.value.contains("200"),
            "Should use last response status (200), got: {}",
            status.value
        );
        assert!(
            body.trim().contains('{'),
            "Body should be from the 200 response"
        );
    }

    /// Regression guard for the CRLF byte-offset drift bug.
    ///
    /// The old `find_last_http_response_start` iterated with `str::lines()`, which
    /// strips `\r` from `\r\n` lines. Each CRLF line caused the reconstructed byte
    /// offset to drift by 1, producing an incorrect body split on redirect chains
    /// with CRLF line endings. The current implementation scans bytes directly and
    /// must handle CRLF cleanly.
    #[test]
    fn test_redirect_chain_crlf_takes_last() {
        // Redirect chain with CRLF line endings throughout.
        // Under the old str::lines()-based implementation each header line would cause
        // a +1 byte drift, pushing the split point into the body JSON and corrupting it.
        let input = concat!(
            "HTTP/1.1 301 Moved Permanently\r\n",
            "Location: https://example.com/new-path\r\n",
            "Content-Length: 0\r\n",
            "\r\n",
            "HTTP/1.1 200 OK\r\n",
            "Content-Type: application/json\r\n",
            "Content-Length: 27\r\n",
            "\r\n",
            "{\"status\":\"ok\",\"moved\":true}\r\n",
        );

        let result = try_split_header_body(input);
        assert!(result.is_some(), "CRLF redirect chain should parse");
        let (items, body) = result.unwrap();

        let status = items.iter().find(|i| i.label == "status").unwrap();
        assert!(
            status.value.contains("200"),
            "Should use last response status (200) with CRLF endings, got: {}",
            status.value
        );
        assert!(
            body.trim_start_matches("\r\n").starts_with('{'),
            "Body should be the JSON from the 200 response (not corrupted by offset drift), got: {body:?}"
        );
        assert!(
            body.contains("ok"),
            "Body JSON content should be intact, got: {body:?}"
        );
    }

    // ---- 6B: Error body awareness tests ----

    #[test]
    fn test_tier1_curl_error_4xx_json() {
        let input = load_fixture("infra", "curl_error_4xx.txt");
        // Parse with 404 status
        let result = try_parse_json(&input, Some("404"));
        assert!(result.is_some(), "Should parse 4xx JSON response");
        let result = result.unwrap();
        // Summary should be error-oriented
        assert!(
            result.summary.contains("ERROR"),
            "4xx summary should contain ERROR, got: {}",
            result.summary
        );
    }

    #[test]
    fn test_tier1_curl_error_5xx_json() {
        let input = load_fixture("infra", "curl_error_5xx.txt");
        let result = try_parse_json(&input, Some("500"));
        assert!(result.is_some(), "Should parse 5xx JSON response");
        let result = result.unwrap();
        assert!(
            result.summary.contains("ERROR"),
            "5xx summary should contain ERROR, got: {}",
            result.summary
        );
    }

    #[test]
    fn test_error_message_extraction_priority() {
        // Priority 1: error.message (nested)
        let j1: Value =
            serde_json::from_str(r#"{"error":{"message":"nested msg","code":404}}"#).unwrap();
        assert_eq!(extract_error_message(&j1).unwrap(), "nested msg");

        // Priority 2: error as string (when error is not an object)
        let j2: Value = serde_json::from_str(r#"{"error":"simple error"}"#).unwrap();
        assert_eq!(extract_error_message(&j2).unwrap(), "simple error");

        // Priority 3: message
        let j3: Value = serde_json::from_str(r#"{"message":"top-level msg"}"#).unwrap();
        assert_eq!(extract_error_message(&j3).unwrap(), "top-level msg");

        // Priority 4: detail
        let j4: Value = serde_json::from_str(r#"{"detail":"detail msg"}"#).unwrap();
        assert_eq!(extract_error_message(&j4).unwrap(), "detail msg");

        // Priority 5: errors array count
        let j5: Value = serde_json::from_str(r#"{"errors":["a","b","c"]}"#).unwrap();
        assert_eq!(extract_error_message(&j5).unwrap(), "3 errors");

        // Priority 6: error_description
        let j6: Value = serde_json::from_str(r#"{"error_description":"oauth error"}"#).unwrap();
        assert_eq!(extract_error_message(&j6).unwrap(), "oauth error");
    }

    // ---- 6C: Write-out tests ----

    #[test]
    fn test_writeout_extraction() {
        let input = load_fixture("infra", "curl_writeout.txt");
        let result = try_extract_writeout(&input);
        assert!(result.is_some(), "Should detect write-out status code");
        let (item, body) = result.unwrap();
        assert_eq!(item.label, "write_out");
        assert_eq!(item.value, "200");
        assert!(
            body.trim().contains('{'),
            "Body should be the JSON part, got: {body}"
        );
    }

    // ---- 6D: Verbose stderr tests ----

    #[test]
    fn test_verbose_response_headers() {
        let stderr = load_fixture("infra", "curl_verbose_headers.txt");
        let (status, items) = extract_verbose_metadata(&stderr);
        assert!(
            status.is_some(),
            "Should extract HTTP status from verbose output"
        );
        assert!(
            status.as_deref().unwrap().contains("200"),
            "Status should be 200, got: {:?}",
            status
        );
        assert!(
            items.iter().any(|i| i.label == "content-type"),
            "Should extract content-type header"
        );
        // set-cookie should be counted
        let cookie = items.iter().find(|i| i.label == "Set-Cookie");
        assert!(cookie.is_some(), "Set-Cookie should be counted");
        let val = &cookie.unwrap().value;
        assert!(val.contains("2"), "There are 2 set-cookie headers");
    }

    // ---- Additional coverage ----

    #[test]
    fn test_tier1_curl_json_array() {
        let input = load_fixture("infra", "curl_json_array.txt");
        let result = try_parse_json(&input, None);
        assert!(result.is_some(), "Should parse JSON array response");
        let result = result.unwrap();
        assert!(
            result.summary.contains("array"),
            "Summary should mention array, got: {}",
            result.summary
        );
        assert!(
            result.items.iter().any(|i| i.label == "count"),
            "Should have count item for array"
        );
    }

    #[test]
    fn test_html_body_falls_to_tier2() {
        let input = load_fixture("infra", "curl_html_response.txt");
        let output = make_output_full(&input, "", Some(0));
        let result = parse_impl(&output);
        // HTML is not valid JSON, so Tier 1 fails.
        // try_parse_regex would find no HTTP status lines either, so falls to passthrough.
        // Either Degraded or Passthrough is acceptable for HTML input.
        assert!(
            !result.is_full(),
            "HTML body should not produce Full JSON result, got {}",
            result.tier_name()
        );
    }
}
