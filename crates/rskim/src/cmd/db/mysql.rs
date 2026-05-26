//! mysql parser with three-tier degradation (#117).
//!
//! Executes `mysql` and parses the output into structured [`DbResult`].
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Parse MySQL batch TSV output (tab-separated, no borders).
//! - **Tier 2 (Degraded)**: Parse MySQL bordered table format (`+---+---+`).
//! - **Tier 3 (Passthrough)**: Raw stdout concatenation.
//!
//! # Multiple result sets
//!
//! When multiple result sets are present (blank-line-separated TSV blocks),
//! only the first is parsed. The presence of additional sets is noted in the
//! query summary.
//!
//! # Empty set
//!
//! `Empty set (0.00 sec)` is detected and returns a [`DbResult`] with zero rows.

use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::DbResult;
use crate::runner::CommandOutput;

use crate::analytics::CommandType;
use crate::cmd::{ToolRunConfig, run_tool};

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "mysql",
    env_overrides: &[("MYSQL_PAGER", "cat"), ("PAGER", "cat")],
    install_hint: "Install MySQL client: https://dev.mysql.com/downloads/",
    family: "db",
    skip_ansi_strip: true,
    command_type: CommandType::Db,
};

/// Matches MySQL's "Empty set" output.
static RE_EMPTY_SET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Empty set").unwrap());

/// Matches MySQL's "N rows in set" footer.
static RE_ROWS_IN_SET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+) rows? in set").unwrap());

/// Matches a MySQL border line: `+------+------+`.
static RE_BORDER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\+[-+]+\+$").unwrap());

/// Matches a MySQL error line.
static RE_ERROR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)^ERROR\s+\d+").unwrap());

/// Run `skim mysql [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    run_tool(CONFIG, args, ctx, |_| {}, parse_impl)
}

/// Three-tier parse function for mysql output.
fn parse_impl(output: &CommandOutput) -> ParseResult<DbResult> {
    let text = &output.stdout;

    // Error output → passthrough
    if text.lines().any(|l| RE_ERROR.is_match(l.trim())) {
        return ParseResult::Passthrough(text.clone());
    }

    // Empty set
    if text.lines().any(|l| RE_EMPTY_SET.is_match(l.trim())) {
        return ParseResult::Full(DbResult::new(
            "mysql".to_string(),
            "query returned 0 row(s)".to_string(),
            vec![],
            vec![],
            0,
            false,
        ));
    }

    if let Some(result) = try_parse_tsv(text) {
        return ParseResult::Full(result);
    }

    if let Some(result) = try_parse_bordered(text) {
        return ParseResult::Degraded(
            result,
            vec!["mysql: TSV parse failed, using bordered table parser".to_string()],
        );
    }

    ParseResult::Passthrough(text.clone())
}

// ============================================================================
// Tier 1: batch TSV
// ============================================================================

/// Parse MySQL batch TSV output (produced by `mysql -e "..." database`).
///
/// Format:
/// ```text
/// col1\tcol2\tcol3
/// val\tval\tval
/// ```
fn try_parse_tsv(text: &str) -> Option<DbResult> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return None;
    }

    // Detect TSV: first line must contain a tab and must NOT start with `+`
    let first = lines[0];
    if !first.contains('\t') || first.starts_with('+') {
        return None;
    }

    // Check if there are multiple result sets (blank line between them).
    // Find end of first result set.
    let first_block_end = lines
        .iter()
        .position(|l| l.is_empty())
        .unwrap_or(lines.len());

    let block = &lines[..first_block_end];
    let multi_result = first_block_end < lines.len();

    let columns: Vec<String> = block[0].split('\t').map(|s| s.trim().to_string()).collect();
    if columns.is_empty() {
        return None;
    }

    let rows: Vec<Vec<String>> = block[1..]
        .iter()
        .filter(|l| !l.is_empty())
        .map(|l| {
            let mut cells: Vec<String> = l.split('\t').map(|s| s.trim().to_string()).collect();
            cells.truncate(columns.len());
            while cells.len() < columns.len() {
                cells.push(String::new());
            }
            cells
        })
        .collect();

    let row_count = rows.len();
    let truncated = row_count > 100;

    let summary = if multi_result {
        format!("query returned {row_count} row(s) (multiple result sets — first shown)")
    } else {
        format!("query returned {row_count} row(s)")
    };

    Some(DbResult::new(
        "mysql".to_string(),
        summary,
        columns,
        rows,
        row_count,
        truncated,
    ))
}

// ============================================================================
// Tier 2: bordered table
// ============================================================================

/// Parse MySQL bordered table format.
///
/// Format:
/// ```text
/// +------+------+------+
/// | col1 | col2 | col3 |
/// +------+------+------+
/// | val  | val  | val  |
/// +------+------+------+
/// N rows in set (0.01 sec)
/// ```
fn try_parse_bordered(text: &str) -> Option<DbResult> {
    let lines: Vec<&str> = text.lines().collect();

    // Find first border line
    let first_border = lines.iter().position(|l| RE_BORDER.is_match(l.trim()))?;

    // Header is between first and second border
    let header_line = lines.get(first_border + 1)?;
    if !header_line.contains('|') {
        return None;
    }

    let columns = parse_bordered_row(header_line);
    if columns.is_empty() {
        return None;
    }

    // Find second border (after header)
    let second_border = lines[first_border + 1..]
        .iter()
        .position(|l| RE_BORDER.is_match(l.trim()))
        .map(|i| first_border + 1 + i)?;

    // Rows are after second border until next border or end
    let data_start = second_border + 1;
    let mut rows: Vec<Vec<String>> = Vec::new();

    for line in &lines[data_start..] {
        let trimmed = line.trim();
        if RE_BORDER.is_match(trimmed) {
            break;
        }
        if trimmed.starts_with('|') {
            let row = parse_bordered_row(line);
            if !row.is_empty() {
                rows.push(row);
            }
        }
    }

    // Try to get row count from footer
    let row_count = lines
        .iter()
        .rev()
        .find_map(|l| {
            RE_ROWS_IN_SET
                .captures(l.trim())
                .and_then(|c| c[1].parse::<usize>().ok())
        })
        .unwrap_or(rows.len());

    let truncated = rows.len() > 100;

    Some(DbResult::new(
        "mysql".to_string(),
        format!("query returned {row_count} row(s)"),
        columns,
        rows,
        row_count,
        truncated,
    ))
}

/// Parse a single bordered row `| val1 | val2 | val3 |` into cell strings.
fn parse_bordered_row(line: &str) -> Vec<String> {
    line.split('|')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_support::{make_output};

    #[test]
    fn test_tier1_mysql_tsv() {
        let fixture = include_str!("../../../tests/fixtures/cmd/db/mysql_select_tsv.txt");
        let output = make_output(fixture);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Full(_)),
            "expected Full, got {result:?}"
        );
        if let ParseResult::Full(r) = result {
            assert_eq!(r.row_count, 20);
            assert_eq!(r.columns, vec!["id", "username", "email"]);
            assert_eq!(r.rows.len(), 20);
        }
    }

    #[test]
    fn test_tier2_mysql_bordered() {
        let fixture = include_str!("../../../tests/fixtures/cmd/db/mysql_select_bordered.txt");
        let output = make_output(fixture);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Degraded(_, _)),
            "expected Degraded for bordered, got {result:?}"
        );
        if let ParseResult::Degraded(r, _) = result {
            assert_eq!(r.columns, vec!["id", "username", "email"]);
            assert_eq!(r.rows.len(), 20);
        }
    }

    #[test]
    fn test_tier3_mysql_passthrough_garbage() {
        let output = make_output("completely unparseable output");
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Passthrough(_)),
            "expected Passthrough"
        );
    }

    #[test]
    fn test_empty_set() {
        let output = make_output("Empty set (0.00 sec)\n");
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Full(_)),
            "expected Full for empty set"
        );
        if let ParseResult::Full(r) = result {
            assert_eq!(r.row_count, 0);
            assert!(r.rows.is_empty());
        }
    }

    #[test]
    fn test_tier3_passthrough_error() {
        let output = make_output("ERROR 1045 (28000): Access denied for user 'root'@'localhost'\n");
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Passthrough(_)),
            "ERROR must passthrough"
        );
    }

    #[test]
    fn test_multi_result_noted_in_summary() {
        let fixture = include_str!("../../../tests/fixtures/cmd/db/mysql_multi_result.txt");
        let output = make_output(fixture);
        let result = parse_impl(&output);
        // First result set has 3 rows (alice/bob/charlie)
        if let ParseResult::Full(r) = result {
            assert!(
                r.query_summary.contains("multiple result sets"),
                "multi-result should note extra sets"
            );
        }
    }

    #[test]
    fn test_env_overrides() {
        assert!(CONFIG.env_overrides.contains(&("MYSQL_PAGER", "cat")));
        assert!(CONFIG.env_overrides.contains(&("PAGER", "cat")));
    }

    #[test]
    fn test_parse_bordered_row() {
        let row = parse_bordered_row("| alice | alice@example.com | 1 |");
        assert_eq!(row, vec!["alice", "alice@example.com", "1"]);
    }

    #[test]
    fn test_large_result_truncated_flag() {
        // 120 TSV rows → truncated=true
        let mut text = "id\tval\n".to_string();
        for i in 1..=120 {
            text.push_str(&format!("{}\tv{}\n", i, i));
        }
        let output = make_output(&text);
        let result = parse_impl(&output);
        if let ParseResult::Full(r) = result {
            assert!(r.truncated, "expected truncated=true for 120 rows");
        }
    }

    #[test]
    fn test_column_truncation_in_render() {
        let long_val = "a".repeat(50);
        let text = format!("col1\tcol2\n{}\tshort\n", long_val);
        let output = make_output(&text);
        let result = parse_impl(&output);
        if let ParseResult::Full(r) = result {
            let rendered = r.as_ref();
            assert!(
                rendered.contains('…'),
                "long values must be truncated with ellipsis"
            );
        }
    }
}
