//! sqlite3 parser with three-tier degradation (#117).
//!
//! Executes `sqlite3` and parses the output into structured [`DbResult`].
//!
//! Three tiers:
//! - **Tier 1 (Full)**: Parse pipe-separated output with header (injected via
//!   `-header -separator '|'` when the user hasn't already specified a format flag).
//! - **Tier 2 (Degraded)**: Regex fallback for other formats.
//! - **Tier 3 (Passthrough)**: Raw stdout for meta commands, schema dumps, etc.
//!
//! # Argument injection
//!
//! `prepare_args` injects `-header` and `-separator '|'` UNLESS the user has
//! already supplied one of: `-header`, `-separator`, `-json`, `-csv`, `-column`,
//! `-line`, `-tabs`.  This is safe because sqlite3 accepts these before the
//! database filename.

use std::sync::LazyLock;

use regex::Regex;

use crate::output::ParseResult;
use crate::output::canonical::DbResult;
use crate::runner::CommandOutput;

use crate::cmd::{ToolRunConfig, run_tool, user_has_flag};
use crate::analytics::CommandType;

const CONFIG: ToolRunConfig<'static> = ToolRunConfig {
    program: "sqlite3",
    env_overrides: &[],
    install_hint: "Install SQLite: https://www.sqlite.org/download.html",
    family: "db",
    skip_ansi_strip: true,
    command_type: CommandType::Db,
};

/// Flags that indicate the user has already set an output format.
/// When present, we skip `-header -separator '|'` injection.
const FORMAT_FLAGS: &[&str] = &[
    "-header",
    "-separator",
    "-json",
    "-csv",
    "-column",
    "-line",
    "-tabs",
];

/// Matches a sqlite3 error line.
static RE_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(Error|Parse error):").unwrap());

/// Run `skim sqlite3 [args...]`.
pub(crate) fn run(
    args: &[String],
    ctx: &crate::cmd::RunContext,
) -> anyhow::Result<std::process::ExitCode> {
    run_tool(CONFIG, args, ctx, prepare_sqlite3_args, parse_impl)
}

/// Inject `-header -separator '|'` when the user hasn't specified a format flag.
///
/// We insert before the `--` separator if present, otherwise at the beginning
/// of the arg list so that sqlite3 sees them as program flags rather than
/// SQL or filename arguments.
fn prepare_sqlite3_args(args: &mut Vec<String>) {
    if user_has_flag(args, FORMAT_FLAGS) {
        return;
    }
    // Insert at position 0 so flags precede the db filename
    args.insert(0, "|".to_string());
    args.insert(0, "-separator".to_string());
    args.insert(0, "-header".to_string());
}

/// Three-tier parse function for sqlite3 output.
fn parse_impl(output: &CommandOutput) -> ParseResult<DbResult> {
    let text = &output.stdout;

    // Error output → passthrough
    if text.lines().any(|l| RE_ERROR.is_match(l.trim())) {
        return ParseResult::Passthrough(text.clone());
    }

    // Meta-command output (`.schema`, `.tables`, etc.) → passthrough
    // Heuristic: no `|` character in any line → not pipe-separated tabular
    if !text.lines().any(|l| l.contains('|')) {
        if !text.trim().is_empty() {
            return ParseResult::Passthrough(text.clone());
        }
        // Empty output → empty DbResult
        return ParseResult::Full(DbResult::new(
            "sqlite3".to_string(),
            "query returned 0 row(s)".to_string(),
            vec![],
            vec![],
            0,
            false,
        ));
    }

    if let Some(result) = try_parse_pipe_separated(text) {
        return ParseResult::Full(result);
    }

    if let Some(result) = try_parse_regex_fallback(text) {
        return ParseResult::Degraded(
            result,
            vec!["sqlite3: pipe-separated parse failed, using regex fallback".to_string()],
        );
    }

    ParseResult::Passthrough(text.clone())
}

// ============================================================================
// Tier 1: pipe-separated with header
// ============================================================================

/// Parse pipe-separated sqlite3 output with `-header`.
///
/// Format:
/// ```text
/// col1|col2|col3
/// val|val|val
/// ```
fn try_parse_pipe_separated(text: &str) -> Option<DbResult> {
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    if lines.is_empty() {
        return None;
    }

    // First line must contain `|` to be pipe-separated
    if !lines[0].contains('|') {
        return None;
    }

    let columns: Vec<String> = lines[0].split('|').map(|s| s.trim().to_string()).collect();

    if columns.is_empty() {
        return None;
    }

    let rows: Vec<Vec<String>> = lines[1..]
        .iter()
        .map(|l| {
            let mut cells: Vec<String> = l.split('|').map(|s| s.trim().to_string()).collect();
            cells.truncate(columns.len());
            while cells.len() < columns.len() {
                cells.push(String::new());
            }
            cells
        })
        .collect();

    let row_count = rows.len();
    let truncated = row_count > 100;

    Some(DbResult::new(
        "sqlite3".to_string(),
        format!("query returned {row_count} row(s)"),
        columns,
        rows,
        row_count,
        truncated,
    ))
}

// ============================================================================
// Tier 2: regex fallback
// ============================================================================

/// Regex fallback: count lines that look like data rows.
fn try_parse_regex_fallback(text: &str) -> Option<DbResult> {
    let data_lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if data_lines.is_empty() {
        return None;
    }
    let row_count = data_lines.len().saturating_sub(1); // minus header guess
    Some(DbResult::new(
        "sqlite3".to_string(),
        format!("query returned ~{row_count} row(s) (estimate)"),
        vec![],
        vec![],
        row_count,
        false,
    ))
}

// ============================================================================
// Unit tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::test_support::*;

    #[test]
    fn test_tier1_sqlite3_pipe_separated() {
        let fixture = include_str!("../../../tests/fixtures/cmd/db/sqlite3_select.txt");
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
    fn test_tier1_sqlite3_empty_output() {
        let output = make_output("");
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Full(_)),
            "expected Full for empty"
        );
        if let ParseResult::Full(r) = result {
            assert_eq!(r.row_count, 0);
        }
    }

    #[test]
    fn test_tier3_sqlite3_schema_passthrough() {
        // .schema output has no `|` → passthrough
        let schema = "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);\n";
        let output = make_output(schema);
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Passthrough(_)),
            "schema output must passthrough"
        );
    }

    #[test]
    fn test_tier3_sqlite3_passthrough_error() {
        let output = make_output("Error: no such table: users\n");
        let result = parse_impl(&output);
        assert!(
            matches!(result, ParseResult::Passthrough(_)),
            "error output must passthrough"
        );
    }

    #[test]
    fn test_prepare_args_injection() {
        let mut args: Vec<String> = vec!["app.db".to_string(), "SELECT 1".to_string()];
        prepare_sqlite3_args(&mut args);
        assert!(
            args.contains(&"-header".to_string()),
            "should inject -header"
        );
        assert!(
            args.contains(&"-separator".to_string()),
            "should inject -separator"
        );
        assert!(
            args.contains(&"|".to_string()),
            "should inject | as separator"
        );
    }

    #[test]
    fn test_prepare_args_no_injection_when_header_present() {
        let mut args = vec!["-header".to_string(), "app.db".to_string()];
        let original = args.clone();
        prepare_sqlite3_args(&mut args);
        assert_eq!(
            args, original,
            "must not inject when -header already present"
        );
    }

    #[test]
    fn test_prepare_args_no_injection_when_json_present() {
        let mut args = vec!["-json".to_string(), "app.db".to_string()];
        let original = args.clone();
        prepare_sqlite3_args(&mut args);
        assert_eq!(args, original, "must not inject when -json already present");
    }

    #[test]
    fn test_prepare_args_no_injection_when_csv_present() {
        let mut args = vec!["-csv".to_string(), "app.db".to_string()];
        let original = args.clone();
        prepare_sqlite3_args(&mut args);
        assert_eq!(args, original, "must not inject when -csv already present");
    }

    #[test]
    fn test_prepare_args_no_injection_when_separator_present() {
        let mut args = vec![
            "-separator".to_string(),
            ",".to_string(),
            "app.db".to_string(),
        ];
        let original = args.clone();
        prepare_sqlite3_args(&mut args);
        assert_eq!(
            args, original,
            "must not inject when -separator already present"
        );
    }

    #[test]
    fn test_large_result_truncated_flag() {
        let mut text = "id|val\n".to_string();
        for i in 1..=120 {
            text.push_str(&format!("{}|v{}\n", i, i));
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
        let text = format!("col1|col2\n{}|short\n", long_val);
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

    #[test]
    fn test_env_overrides_is_empty() {
        assert!(
            CONFIG.env_overrides.is_empty(),
            "sqlite3 needs no env overrides"
        );
    }
}
