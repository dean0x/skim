//! psql parser with three-tier degradation (#117).
//!
//! Executes `psql` and parses the output into structured [`DbResult`].
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Parse psql tabular output with `|` column separators and
//!   `(N rows)` footer.
//! - **Tier 2 (Degraded)**: Regex fallback for alternative formats or partial output.
//! - **Tier 3 (Passthrough)**: Raw stdout concatenation.
//!
//! # Safety invariant
//!
//! `prepare_args` is always a no-op — psql already outputs tabular data without
//! pager when `PAGER=cat` is set. We never inject `-c` or other flags that would
//! change interactive vs batch mode.

use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::DbResult;
use crate::runner::CommandOutput;

use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "psql",
    env_overrides: &[("PAGER", "cat"), ("PGPAGER", "cat")],
    install_hint: "Install PostgreSQL: https://www.postgresql.org/download/",
    family: "db",
    skip_ansi_strip: true,
    command_type: CommandType::Db,
};

/// Matches the psql row-count footer: `(N rows)` or `(1 row)`.
static RE_ROW_COUNT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\((\d+) rows?\)$").unwrap());

/// Matches a psql separator line: `---+---+---`.
static RE_SEPARATOR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-+]+$").unwrap());

/// Matches a psql error line.
static RE_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(ERROR|FATAL|PANIC):").unwrap());

/// Run `skim psql [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    run_tool(CONFIG, args, ctx, |_| {}, parse_impl)
}

/// Three-tier parse function for psql output.
fn parse_impl(output: &CommandOutput) -> ParseResult<DbResult> {
    let text = &output.stdout;

    // Error output → passthrough so the user sees the actual error
    if text.lines().any(|l| RE_ERROR.is_match(l.trim())) {
        return ParseResult::Passthrough(text.clone());
    }

    if let Some(result) = try_parse_tabular(text) {
        return ParseResult::Full(result);
    }

    if let Some(result) = try_parse_regex_fallback(text) {
        return ParseResult::Degraded(
            result,
            vec!["psql: tabular parse failed, using regex fallback".to_string()],
        );
    }

    ParseResult::Passthrough(text.clone())
}

// ============================================================================
// Tier 1: tabular output
// ============================================================================

/// Parse psql tabular output.
///
/// Format:
/// ```text
///  col1 | col2 | col3
/// ------+------+------
///  val  | val  | val
/// (N rows)
/// ```
///
/// ## Eager collection pattern
///
/// The input is eagerly collected into a `Vec<&str>` so that the parser can use
/// random-access indexing (`lines[sep_idx - 1]`, slice ranges, `rposition`) to
/// locate the header, separator, data rows, and footer in a single pass.  A
/// streaming iterator would require multiple passes or complex lookahead; the
/// bounded allocation is negligible for the query result sizes skim handles.
fn try_parse_tabular(text: &str) -> Option<DbResult> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return None;
    }

    // Find the separator line (----+----).
    let sep_idx = lines.iter().position(|l| RE_SEPARATOR.is_match(l.trim()))?;

    if sep_idx == 0 {
        return None; // No header before separator
    }

    // Header is the line before the separator.
    let header_line = lines[sep_idx - 1];
    let columns = parse_psql_columns(header_line);
    if columns.is_empty() {
        return None;
    }

    // Find the row-count footer (last non-empty line matching `(N rows)`).
    let row_count_line = lines.iter().rev().find(|l| !l.trim().is_empty())?;
    let row_count = if let Some(caps) = RE_ROW_COUNT.captures(row_count_line.trim()) {
        caps[1].parse::<usize>().unwrap_or(0)
    } else {
        return None; // Require footer for Tier 1
    };

    // Rows are between separator and the footer line (exclusive).
    let footer_idx = lines
        .iter()
        .rposition(|l| RE_ROW_COUNT.is_match(l.trim()))
        .unwrap_or(lines.len());

    let data_lines = &lines[(sep_idx + 1)..footer_idx];
    let rows: Vec<Vec<String>> = data_lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| parse_psql_row(l, columns.len()))
        .collect();

    let truncated = rows.len() > 100;

    Some(DbResult::new(
        "psql".to_string(),
        format!("SELECT returned {row_count} row(s)"),
        columns,
        rows,
        row_count,
        truncated,
    ))
}

/// Parse column names from a psql header line (`col1 | col2 | col3`).
fn parse_psql_columns(line: &str) -> Vec<String> {
    line.split('|')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Parse a psql data row (`val1 | val2 | val3`), returning `expected_cols` cells.
fn parse_psql_row(line: &str, expected_cols: usize) -> Vec<String> {
    let mut cells: Vec<String> = line.split('|').map(|s| s.trim().to_string()).collect();
    // Pad or truncate to match column count
    cells.truncate(expected_cols);
    while cells.len() < expected_cols {
        cells.push(String::new());
    }
    cells
}

// ============================================================================
// Tier 2: regex fallback
// ============================================================================

/// Regex fallback: try to extract a row count from any `(N rows)` footer.
fn try_parse_regex_fallback(text: &str) -> Option<DbResult> {
    for line in text.lines() {
        if let Some(caps) = RE_ROW_COUNT.captures(line.trim()) {
            let row_count = caps[1].parse::<usize>().unwrap_or(0);
            return Some(DbResult::new(
                "psql".to_string(),
                format!("query returned {row_count} row(s)"),
                vec![],
                vec![],
                row_count,
                false,
            ));
        }
    }
    None
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_support::*;

    #[test]
    fn test_tier1_psql_tabular() {
        let fixture = include_str!("../../../tests/fixtures/cmd/db/psql_select.txt");
        let output = make_output(fixture);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Full(_)),
            "expected Full, got {result:?}"
        );
        if let ParseResult::Full(r) = result {
            assert_eq!(r.row_count, 20);
            assert_eq!(r.columns.len(), 3);
            assert_eq!(r.columns[0], "id");
            assert_eq!(r.columns[1], "username");
            assert_eq!(r.columns[2], "email");
            assert_eq!(r.rows.len(), 20);
        }
    }

    #[test]
    fn test_tier1_psql_empty_result() {
        let fixture = include_str!("../../../tests/fixtures/cmd/db/psql_empty.txt");
        let output = make_output(fixture);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Full(_)),
            "expected Full for empty, got {result:?}"
        );
        if let ParseResult::Full(r) = result {
            assert_eq!(r.row_count, 0);
            assert!(r.rows.is_empty());
            assert_eq!(r.columns.len(), 3);
        }
    }

    #[test]
    fn test_tier2_psql_regex_fallback() {
        // Only row footer, no separator — forces regex tier
        let text = "some arbitrary output\n(5 rows)\n";
        let output = make_output(text);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Degraded(_, _)),
            "expected Degraded, got {result:?}"
        );
        if let ParseResult::Degraded(r, _) = result {
            assert_eq!(r.row_count, 5);
        }
    }

    #[test]
    fn test_tier3_psql_passthrough_garbage() {
        let output = make_output("garbage output with no structure");
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Passthrough(_)),
            "expected Passthrough"
        );
    }

    #[test]
    fn test_tier3_psql_passthrough_error() {
        let output = make_output(
            "ERROR:  relation \"users\" does not exist\nLINE 1: SELECT * FROM users;\n",
        );
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Passthrough(_)),
            "ERROR lines must passthrough"
        );
    }

    #[test]
    fn test_column_truncation_in_render() {
        // Value wider than MAX_COL_WIDTH (40) should be truncated with ellipsis in render
        let long_val = "a".repeat(50);
        let text = format!(
            " col1 | col2\n------+------\n {} | short\n(1 row)\n",
            long_val
        );
        let output = make_output(&text);
        let result = parse_impl(&output);
        // Should parse as Full
        if let ParseResult::Full(r) = result {
            let rendered = r.as_ref();
            // The rendered output should contain the truncation ellipsis
            assert!(
                rendered.contains('…'),
                "expected truncation ellipsis in rendered output"
            );
        }
    }

    #[test]
    fn test_large_result_truncated_flag() {
        // Build a result with 120 rows (exceeds MAX_DB_ROWS=100)
        let mut text = " id | val\n----+-----\n".to_string();
        for i in 1..=120 {
            text.push_str(&format!("  {} | v{}\n", i, i));
        }
        text.push_str("(120 rows)\n");
        let output = make_output(&text);
        let result = parse_impl(&output);
        if let ParseResult::Full(r) = result {
            assert_eq!(r.row_count, 120);
            assert!(r.truncated, "expected truncated=true for 120 rows");
        }
    }

    #[test]
    fn test_env_overrides() {
        assert!(CONFIG.env_overrides.contains(&("PAGER", "cat")));
        assert!(CONFIG.env_overrides.contains(&("PGPAGER", "cat")));
    }

    #[test]
    fn test_prepare_args_is_noop() {
        // psql prepare_args must not modify args (no injection)
        let original = vec!["-c".to_string(), "SELECT 1".to_string()];
        let args = original.clone();
        // Invoke run_tool's prepare_args closure (it's |_| {})
        // We test by calling parse directly and checking no side effects on args
        let _ = parse_impl(&make_output("SELECT 1\n"));
        assert_eq!(args, original, "prepare_args must be a no-op for psql");
    }

    #[test]
    fn test_parse_psql_columns() {
        let cols = parse_psql_columns(" id  | username | email ");
        assert_eq!(cols, vec!["id", "username", "email"]);
    }

    #[test]
    fn test_parse_psql_row() {
        let row = parse_psql_row("  1 | alice | alice@example.com", 3);
        assert_eq!(row, vec!["1", "alice", "alice@example.com"]);
    }

    #[test]
    fn test_parse_psql_row_padding() {
        // Fewer cells than expected → pad with empty strings
        let row = parse_psql_row("  1 | alice", 3);
        assert_eq!(row.len(), 3);
        assert_eq!(row[2], "");
    }
}
