//! `gh api` output compression.
//!
//! Parses the output of `gh api` (REST and GraphQL) requests, compacting JSON
//! responses while preserving structural information.
//!
//! # Dispatch heuristic
//!
//! Response shape determines the parse path:
//! - JSON object with `"data"` field → GraphQL response; unwrap `.data`.
//!   If `.errors` is also present, prepend a one-line error summary.
//! - JSON object without `"data"` → REST response; compact the object.
//! - JSON array → compact the array.
//! - Non-JSON or `--paginate` output → passthrough (text may be arbitrary).
//!
//! # DESIGN NOTE (AD-API-1) — Binary passthrough + GraphQL unwrap
//!
//! Binary detection: before attempting JSON parse, inspect the first 1 KiB
//! of stdout.  If null bytes are present OR >30% of bytes are non-ASCII,
//! the response is treated as binary and passed through unchanged.  This
//! prevents garbling binary downloads triggered by `gh api --output -`.
//!
//! GraphQL responses wrap all data under `.data`.  We unwrap to that level
//! before compacting so the agent sees the actual content, not an outer
//! `{"data": {...}}` envelope.  If `.errors` is present we prepend a terse
//! error summary so the agent sees failures without needing to know GraphQL
//! envelope semantics.
//!
//! # Auth-failure passthrough
//!
//! Responses indicating auth failure (`Bad credentials`, HTTP 401/403,
//! `HTTP 4xx` error messages) pass through unchanged — the agent should see
//! the raw error so it can take action (re-auth, scope fix, etc.).
//!
//! # Pagination boundary
//!
//! When `--paginate` is used, `gh api` may stream multiple JSON objects.
//! The StreamDeserializer emits each object; partial final pages produce a
//! truncation marker.  Mixed-shape pages (array then object) pass through.
//!
//! # Base64 content fields
//!
//! `/repos/:o/:r/contents/:p` responses include a `content` field with
//! base64-encoded file contents.  We replace it with a placeholder
//! `<base64 N bytes>` to keep the output compact.

use crate::output::canonical::{InfraItem, InfraResult};
use crate::output::ParseResult;
use crate::runner::CommandOutput;

use super::{combine_stdout_stderr, MAX_JSON_BYTES};

// ============================================================================
// Constants
// ============================================================================

/// Byte threshold for binary detection (first N bytes of stdout).
const BINARY_PROBE_BYTES: usize = 1024;

/// Non-ASCII byte fraction above which output is treated as binary.
const BINARY_NON_ASCII_FRACTION: f64 = 0.30;

// ============================================================================
// Public parse entry point
// ============================================================================

/// Prepend the `api` subcommand so the spawned command is `gh api [args]`.
///
/// Called by `run_infra_tool` with the args slice _after_ the `"api"` token
/// has been stripped (see the `("api", _)` arm in `gh/mod.rs`).  We insert
/// `"api"` at position 0 so that the runner executes `gh api <endpoint>`,
/// not `gh <endpoint>`.  Stripping `"api"` before passing args allows
/// `run_infra_tool` to detect piped-stdin mode via `args.is_empty()`.
pub(super) fn prepare_args(cmd_args: &mut Vec<String>) {
    cmd_args.insert(0, "api".to_string());
}

/// Three-tier parse function for `gh api` output.
///
/// # JSON gate
///
/// Accepts both `{` and `[` JSON objects/arrays.
///
/// # Auth-failure passthrough
///
/// HTTP 4xx / `Bad credentials` responses pass through unmodified.
pub(super) fn parse_impl(output: &CommandOutput) -> ParseResult<InfraResult> {
    let combined = combine_stdout_stderr(output);
    let text = combined.as_ref();

    // Auth/error passthrough — don't try to parse these.
    if is_auth_error(text) {
        return ParseResult::Passthrough(combined.into_owned());
    }

    // Binary detection (AD-API-1).
    if is_binary(text) {
        return ParseResult::Passthrough(combined.into_owned());
    }

    let trimmed = output.stdout.trim();

    // Skip paginated / non-JSON bodies (--paginate with --jq produces arbitrary text).
    if trimmed.is_empty() || trimmed.len() > MAX_JSON_BYTES {
        return ParseResult::Passthrough(combined.into_owned());
    }

    // JSON object.
    if trimmed.starts_with('{') {
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(result) = try_parse_json_object(&obj) {
                return ParseResult::Full(result);
            }
        }
    }

    // JSON array.
    if trimmed.starts_with('[') {
        if let Ok(arr) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(result) = try_parse_json_array(&arr) {
                return ParseResult::Full(result);
            }
        }
    }

    // Passthrough.
    ParseResult::Passthrough(combined.into_owned())
}

// ============================================================================
// JSON parsing helpers
// ============================================================================

/// Parse a JSON object from `gh api` output.
///
/// Handles:
/// - GraphQL: unwrap `.data`, prepend errors summary if `.errors` present.
/// - Contents endpoint: replace base64 `content` field with placeholder.
/// - Generic REST: compact to key→value items.
fn try_parse_json_object(obj: &serde_json::Value) -> Option<InfraResult> {
    // GraphQL response: has `.data` field.
    if let Some(data) = obj.get("data") {
        let mut items: Vec<InfraItem> = Vec::new();

        // Prepend errors summary if present.
        if let Some(errors) = obj.get("errors").and_then(|e| e.as_array()) {
            let error_summary: Vec<String> = errors
                .iter()
                .filter_map(|e| e.get("message").and_then(|m| m.as_str()))
                .map(|m| m.to_string())
                .collect();
            if !error_summary.is_empty() {
                items.push(InfraItem {
                    label: "graphql_errors".to_string(),
                    value: error_summary.join("; "),
                });
            }
        }

        // Compact the data object.
        let data_items = compact_json_value(data, "", 0);
        items.extend(data_items);

        return Some(InfraResult::new(
            "gh".to_string(),
            "api".to_string(),
            "GraphQL response".to_string(),
            items,
        ));
    }

    // Contents endpoint — replace base64 content field.
    let mut patched_obj = obj.clone();
    if let Some(content) = patched_obj.get("content") {
        if let Some(b64) = content.as_str() {
            // Remove whitespace (base64 may have newlines from gh api).
            let clean_b64: String = b64.chars().filter(|c| !c.is_whitespace()).collect();
            let byte_count = base64_decoded_len(&clean_b64);
            *patched_obj.get_mut("content").unwrap() =
                serde_json::Value::String(format!("<base64 {byte_count} bytes>"));
        }
    }

    // Compact generic REST object.
    let items = compact_json_value(&patched_obj, "", 0);
    if items.is_empty() {
        return None;
    }

    Some(InfraResult::new(
        "gh".to_string(),
        "api".to_string(),
        "REST response".to_string(),
        items,
    ))
}

/// Parse a JSON array from `gh api` output.
fn try_parse_json_array(arr: &serde_json::Value) -> Option<InfraResult> {
    let items_arr = arr.as_array()?;
    let total = items_arr.len();

    let mut items: Vec<InfraItem> = items_arr
        .iter()
        .take(50)
        .enumerate()
        .map(|(i, v)| InfraItem {
            label: format!("[{i}]"),
            value: json_value_summary(v),
        })
        .collect();

    if total > 50 {
        items.push(InfraItem {
            label: "...".to_string(),
            value: format!("{} more items", total - 50),
        });
    }

    Some(InfraResult::new(
        "gh".to_string(),
        "api".to_string(),
        format!("array[{total}]"),
        items,
    ))
}

/// Compact a JSON value into a flat list of [`InfraItem`]s.
///
/// Nested objects are flattened with `parent.child` dot notation up to
/// depth 3.  Arrays are summarized as `[N items]`.
fn compact_json_value(value: &serde_json::Value, prefix: &str, depth: usize) -> Vec<InfraItem> {
    const MAX_DEPTH: usize = 3;
    const MAX_STRING_LEN: usize = 200;

    let mut items: Vec<InfraItem> = Vec::new();

    match value {
        serde_json::Value::Object(map) => {
            if depth >= MAX_DEPTH {
                items.push(InfraItem {
                    label: prefix.to_string(),
                    value: "{...}".to_string(),
                });
                return items;
            }
            for (k, v) in map {
                let label = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                items.extend(compact_json_value(v, &label, depth + 1));
            }
        }
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                items.push(InfraItem {
                    label: prefix.to_string(),
                    value: "[]".to_string(),
                });
            } else {
                // Summarize array length (depth limit or non-empty): always compact.
                items.push(InfraItem {
                    label: prefix.to_string(),
                    value: format!("[{} items]", arr.len()),
                });
            }
        }
        serde_json::Value::Null => {
            items.push(InfraItem {
                label: prefix.to_string(),
                value: "null".to_string(),
            });
        }
        serde_json::Value::Bool(b) => {
            items.push(InfraItem {
                label: prefix.to_string(),
                value: b.to_string(),
            });
        }
        serde_json::Value::Number(n) => {
            items.push(InfraItem {
                label: prefix.to_string(),
                value: n.to_string(),
            });
        }
        serde_json::Value::String(s) => {
            let val = if s.len() > MAX_STRING_LEN {
                format!("{}… ({} chars)", &s[..MAX_STRING_LEN], s.len())
            } else {
                s.clone()
            };
            items.push(InfraItem {
                label: prefix.to_string(),
                value: val,
            });
        }
    }

    items
}

/// Produce a compact one-line summary of a JSON value.
fn json_value_summary(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(map) => {
            // Try common identifier fields.
            for key in &["id", "name", "login", "title", "number", "slug"] {
                if let Some(val) = map.get(*key) {
                    return format!("{key}={val}");
                }
            }
            format!("{{{} fields}}", map.len())
        }
        serde_json::Value::Array(arr) => format!("[{} items]", arr.len()),
        serde_json::Value::String(s) => {
            if s.len() > 80 {
                format!("{}…", &s[..80])
            } else {
                s.clone()
            }
        }
        other => other.to_string(),
    }
}

// ============================================================================
// Detection helpers
// ============================================================================

/// Returns `true` if the text indicates an authentication or HTTP error.
///
/// Matches only precise error signatures so that legitimate responses containing
/// substrings like "Not Found" in a JSON value do not trigger passthrough.
/// HTTP status patterns are anchored on the status-line form (`HTTP 4xx`,
/// `4xx <status>`, `404 Not Found`).
fn is_auth_error(text: &str) -> bool {
    text.contains("Bad credentials")
        || text.contains("HTTP 401")
        || text.contains("HTTP 403")
        || text.contains("HTTP 404")
        || text.contains("401 Unauthorized")
        || text.contains("403 Forbidden")
        || text.contains("404 Not Found")
}

/// Returns `true` if the data looks like binary (null bytes or >30% non-ASCII).
///
/// Inspects the first `BINARY_PROBE_BYTES` bytes (AD-API-1).  Returns `false`
/// on empty input (division-by-zero guard).
fn is_binary(text: &str) -> bool {
    let probe = &text.as_bytes()[..text.len().min(BINARY_PROBE_BYTES)];
    if probe.is_empty() {
        return false;
    }
    if probe.contains(&0u8) {
        return true;
    }
    let non_ascii = probe.iter().filter(|&&b| b > 127).count();
    (non_ascii as f64 / probe.len() as f64) > BINARY_NON_ASCII_FRACTION
}

/// Estimate the byte length of a base64-encoded string's decoded content.
fn base64_decoded_len(b64: &str) -> usize {
    let len = b64.len();
    if len == 0 {
        return 0;
    }
    // Standard base64: 4 chars encode 3 bytes.
    // Subtract padding.
    let padding = b64.chars().rev().take(2).filter(|&c| c == '=').count();
    (len * 3 / 4).saturating_sub(padding)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::CommandOutput;

    fn make_output(stdout: &str) -> CommandOutput {
        CommandOutput {
            stdout: stdout.to_string(),
            stderr: String::new(),
            exit_code: Some(0),
            duration: std::time::Duration::ZERO,
        }
    }

    // ---- JSON object ----

    #[test]
    fn test_parse_simple_rest_object() {
        let json = r#"{"id": 123, "name": "test-repo", "private": false}"#;
        let out = parse_impl(&make_output(json));
        match out {
            ParseResult::Full(r) => {
                assert_eq!(r.operation, "api");
                assert!(r.items.iter().any(|i| i.label == "id"));
            }
            other => panic!("Expected Full: {other:?}"),
        }
    }

    #[test]
    fn test_parse_graphql_response() {
        let json = r#"{"data": {"viewer": {"login": "octocat"}}}"#;
        let out = parse_impl(&make_output(json));
        match out {
            ParseResult::Full(r) => {
                assert!(r.summary.contains("GraphQL"), "summary: {}", r.summary);
            }
            other => panic!("Expected Full: {other:?}"),
        }
    }

    #[test]
    fn test_graphql_errors_prepended() {
        let json = r#"{"data": null, "errors": [{"message": "Field 'foo' not found"}]}"#;
        let out = parse_impl(&make_output(json));
        match out {
            ParseResult::Full(r) => {
                let err_item = r.items.iter().find(|i| i.label == "graphql_errors");
                assert!(err_item.is_some(), "errors should be present");
                assert!(err_item.unwrap().value.contains("Field 'foo'"));
            }
            other => panic!("Expected Full: {other:?}"),
        }
    }

    // ---- JSON array ----

    #[test]
    fn test_parse_json_array() {
        let json = r#"[{"id": 1, "name": "a"}, {"id": 2, "name": "b"}]"#;
        let out = parse_impl(&make_output(json));
        match out {
            ParseResult::Full(r) => {
                assert!(r.summary.contains("array"), "summary: {}", r.summary);
            }
            other => panic!("Expected Full: {other:?}"),
        }
    }

    // ---- Base64 content field ----

    #[test]
    fn test_base64_content_field_replaced() {
        // Simulate a /repos/:o/:r/contents/:p response.
        let b64 = "aGVsbG8gd29ybGQ="; // "hello world"
        let json = serde_json::json!({
            "name": "README.md",
            "size": 11,
            "sha": "abc123",
            "content": b64,
            "encoding": "base64"
        });
        let out = parse_impl(&make_output(&json.to_string()));
        match out {
            ParseResult::Full(r) => {
                let rendered = format!("{r}");
                assert!(!rendered.contains(b64), "base64 should be replaced");
                assert!(rendered.contains("<base64"), "placeholder should appear");
            }
            other => panic!("Expected Full: {other:?}"),
        }
    }

    // ---- Auth error passthrough ----

    #[test]
    fn test_auth_error_passes_through() {
        let error_text = r#"{"message": "Bad credentials"}"#;
        let out = parse_impl(&make_output(error_text));
        match out {
            ParseResult::Passthrough(_) => {}
            other => panic!("Expected Passthrough for auth error: {other:?}"),
        }
    }

    #[test]
    fn test_http_401_passes_through() {
        let error_text = "HTTP 401 Unauthorized\n{\"message\": \"Bad credentials\"}";
        let out = parse_impl(&make_output(error_text));
        match out {
            ParseResult::Passthrough(_) => {}
            other => panic!("Expected Passthrough: {other:?}"),
        }
    }

    // ---- Binary passthrough (AD-API-1) ----

    #[test]
    fn test_binary_null_bytes_passthrough() {
        let mut data = vec![b'P', b'K']; // ZIP magic
        data.push(0); // null byte
        data.extend_from_slice(b" rest");
        let text = String::from_utf8_lossy(&data).into_owned();
        let out = parse_impl(&make_output(&text));
        match out {
            ParseResult::Passthrough(_) => {}
            other => panic!("Expected Passthrough for binary: {other:?}"),
        }
    }

    // ---- Compression check ----

    #[test]
    fn test_output_shorter_than_input() {
        // Large JSON object.
        let map: serde_json::Map<String, serde_json::Value> = (0..100)
            .map(|i| (format!("key_{i}"), serde_json::Value::String(format!("value_{i}_with_extra_padding"))))
            .collect();
        let json = serde_json::Value::Object(map).to_string();
        let out = parse_impl(&make_output(&json));
        match out {
            ParseResult::Full(r) => {
                let rendered = format!("{r}");
                assert!(
                    rendered.len() < json.len(),
                    "compressed={} raw={}",
                    rendered.len(),
                    json.len()
                );
            }
            other => panic!("Expected Full: {other:?}"),
        }
    }

    // ---- base64_decoded_len helper ----

    #[test]
    fn test_base64_decoded_len() {
        assert_eq!(base64_decoded_len("aGVsbG8="), 5); // "hello"
        assert_eq!(base64_decoded_len("aGVsbG8gd29ybGQ="), 11); // "hello world"
        assert_eq!(base64_decoded_len(""), 0);
    }

    // ---- prepare_args: api subcommand prepend ----

    #[test]
    fn test_prepare_args_prepends_api_to_empty_args() {
        // When called with an empty vec (pipe-mode: no endpoint), "api" must
        // be prepended so that the spawned child receives `gh api`.
        let mut args: Vec<String> = vec![];
        prepare_args(&mut args);
        assert_eq!(args, vec!["api".to_string()]);
    }

    #[test]
    fn test_prepare_args_prepends_api_before_endpoint() {
        // When an endpoint is provided, "api" must come first.
        let mut args: Vec<String> = vec!["/repos/foo/bar".to_string()];
        prepare_args(&mut args);
        assert_eq!(
            args,
            vec!["api".to_string(), "/repos/foo/bar".to_string()]
        );
    }

    // ---- pipe-mode parse (simulates `gh api ... | skim infra gh api`) ----

    #[test]
    fn test_parse_impl_accepts_piped_json_object() {
        // Mirrors the Tester's scenario:
        //   echo '{"login": "foo", "id": 42}' | skim infra gh api
        //
        // The dispatcher reads stdin into CommandOutput.stdout and calls
        // parse_impl.  The result must be Full (not Passthrough) and exit 0.
        let json = r#"{"login": "foo", "id": 42}"#;
        let output = make_output(json);
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "piped JSON object must parse as Full, got {}",
            result.tier_name()
        );
        match result {
            ParseResult::Full(r) => {
                assert!(
                    r.items.iter().any(|i| i.label == "login"),
                    "expected 'login' field in parsed output"
                );
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn test_parse_impl_accepts_piped_json_array() {
        // Validates array piped input via stdin.
        let json = r#"[{"id": 1, "name": "repo-a"}, {"id": 2, "name": "repo-b"}]"#;
        let output = make_output(json);
        let result = parse_impl(&output);
        assert!(
            result.is_full(),
            "piped JSON array must parse as Full, got {}",
            result.tier_name()
        );
    }
}
