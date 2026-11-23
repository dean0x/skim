//! CLI output for Markdown at the nesting limit.

mod common;
#[path = "common/markdown_nesting.rs"]
mod markdown;

use markdown::{clean, nested_report, run};
use std::{fs, path::Path, process::Output, time::Duration};
use tempfile::TempDir;

const BANNER: &str = "[skim] notice: input exceeds a structural cap in Structure mode (size or nesting) — degraded to passthrough";
const FIRST_LINES: &str = "# Report\n\nNotes\n\n";
const LAST_LINES: &str = "\nEnd\n\nDone\n";
const CUT_MARKER: &str = "<!-- ... (5 lines truncated) — SKIM_PASSTHROUGH=1 for full output -->\n";

fn file_read(path: &Path, args: &[&str], debug: bool) -> Output {
    let mut cmd = common::skim();
    clean(&mut cmd).arg("--no-cache").arg(path).args(args);
    if debug {
        cmd.env("SKIM_DEBUG", "1");
    }
    run(&mut cmd)
}

fn stdin_read(text: &str, args: &[&str], debug: bool) -> Output {
    let mut cmd = common::skim();
    clean(&mut cmd).arg("-").args(args).write_stdin(text);
    if debug {
        cmd.env("SKIM_DEBUG", "1");
    }
    run(&mut cmd)
}

fn assert_raw(output: &Output, text: &str) {
    assert_eq!(output.stdout, text.as_bytes());
    assert_eq!(output.stderr, b"");
}

#[test]
fn nested_markdown_reads_raw_in_each_mode() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("report.md");
    let text = nested_report("Release notes");
    fs::write(&path, &text).unwrap();

    assert_raw(&file_read(&path, &[], false), &text);
    for mode in [
        "structure",
        "signatures",
        "types",
        "full",
        "minimal",
        "pseudo",
    ] {
        assert_raw(&file_read(&path, &["--mode", mode], false), &text);
    }
    assert_raw(&file_read(&path, &["--tokens", "100000"], false), &text);
}

#[test]
fn nested_list_at_the_limit_reads_raw() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("report.md");
    for levels in [200, 250] {
        let text: String = (0..levels)
            .map(|depth| format!("{}- Notes\n", "  ".repeat(depth)))
            .collect();
        fs::write(&path, &text).unwrap();

        assert_raw(&file_read(&path, &["--mode", "structure"], false), &text);
    }
}

#[test]
fn nested_markdown_keeps_source_line_numbers() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("report.md");
    let text = nested_report("Release notes");
    fs::write(&path, &text).unwrap();

    let output = file_read(&path, &["-n"], false);
    let expected = format!("1\t# Report\n2\t\n3\t{} Release notes\n", ">".repeat(200));
    assert_raw(&output, &expected);
}

#[test]
fn nested_markdown_stdin_reads_raw_with_either_language_hint() {
    let text = nested_report("Release notes");
    for args in [["--filename", "report.md"], ["-l", "markdown"]] {
        assert_raw(&stdin_read(&text, &args, false), &text);
    }
}

#[test]
fn nested_markdown_notice_names_the_cap_once_with_debug() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("report.md");
    let text = nested_report("Release notes");
    fs::write(&path, &text).unwrap();

    for output in [
        file_read(&path, &["--mode", "structure"], true),
        stdin_read(
            &text,
            &["--filename", "report.md", "--mode", "structure"],
            true,
        ),
    ] {
        assert_eq!(output.stdout, text.as_bytes());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(
            stderr.lines().filter(|line| *line == BANNER).count(),
            1,
            "{stderr}"
        );
    }
}

#[test]
fn markdown_below_the_limit_keeps_its_heading_view() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("report.md");
    let text = format!("# Report\n\n{0}```\n{0}Notes\n{0}```\n", ">".repeat(198));
    fs::write(&path, text).unwrap();

    let output = file_read(&path, &["--mode", "structure"], false);
    assert_eq!(output.stdout, b"# Report\n");
}

/// The longest time one child read may take before the test fails.
const CHILD_TIME_LIMIT: Duration = Duration::from_secs(60);

/// Read `text` as a Markdown file in structure mode. The process must not
/// stop: it must exit 0 and print the input unchanged, with empty stderr.
fn assert_deep_input_reads_raw(text: &str) {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("deep.md");
    fs::write(&path, text).unwrap();

    let mut cmd = common::skim();
    clean(&mut cmd)
        .timeout(CHILD_TIME_LIMIT)
        .arg("--no-cache")
        .arg(&path)
        .args(["--mode", "structure"]);
    assert_raw(&run(&mut cmd), text);
}

/// A list with one level per line, each level two spaces deeper.
fn deep_list(levels: usize, line_end: &str) -> String {
    (0..levels)
        .map(|depth| format!("{}- x{line_end}", "  ".repeat(depth)))
        .collect()
}

#[test]
fn deep_quote_line_after_a_bom_reads_raw() {
    assert_deep_input_reads_raw(&format!("\u{FEFF}{} x\n", ">".repeat(300)));
}

#[test]
fn deep_list_with_bare_cr_line_ends_reads_raw() {
    let list = deep_list(256, "\r");
    assert_deep_input_reads_raw(&list);
    assert_deep_input_reads_raw(&format!("# Report\n\n{list}"));
}

#[test]
fn deep_list_with_crlf_line_ends_reads_raw() {
    assert_deep_input_reads_raw(&deep_list(256, "\r\n"));
}

#[test]
fn deep_list_indented_with_tabs_reads_raw() {
    let text: String = (0..300)
        .map(|depth| format!("{}- x\n", "\t".repeat(depth)))
        .collect();
    assert_deep_input_reads_raw(&text);
}

#[test]
fn deep_line_of_dash_and_tab_markers_reads_raw() {
    assert_deep_input_reads_raw(&format!("{}x\n", "-\t".repeat(300)));
}

#[test]
fn deep_line_of_ordered_markers_with_a_close_paren_reads_raw() {
    assert_deep_input_reads_raw(&format!("{}x\n", "1) ".repeat(300)));
}

#[test]
fn deep_line_of_nine_digit_ordered_markers_reads_raw() {
    assert_deep_input_reads_raw(&format!("{}x\n", "123456789. ".repeat(300)));
}

#[test]
fn deep_line_of_plus_markers_reads_raw() {
    assert_deep_input_reads_raw(&format!("{}x\n", "+ ".repeat(300)));
}

#[test]
fn deep_line_of_quote_and_dash_pairs_reads_raw() {
    assert_deep_input_reads_raw(&format!("{}x\n", "> - ".repeat(128)));
}

#[test]
fn code_and_html_blocks_inside_deep_quotes_read_raw() {
    let quotes = ">".repeat(254);
    for text in [
        format!("{quotes}```\n{quotes}x\n{quotes}```\n"),
        format!("{quotes}<div>\n{quotes}x\n"),
        format!("{quotes}     x\n"),
    ] {
        assert_deep_input_reads_raw(&text);
    }
}

/// Every line counts, also lines that look like they are inside a fence.
#[test]
fn deep_quotes_after_long_backtick_lines_read_raw() {
    let quotes = format!("{} x\n", ">".repeat(300));
    assert_deep_input_reads_raw(&format!("{}\n{quotes}", "`".repeat(256)));
    assert_deep_input_reads_raw(&format!("{}\n```\n{quotes}", "`".repeat(259)));
}

/// Every line counts, also lines with four or more leading spaces.
#[test]
fn deep_quotes_in_an_indented_list_paragraph_read_raw() {
    assert_deep_input_reads_raw(&format!("- x\n\n    {} x\n", ">".repeat(300)));
}

fn long_nested_report() -> String {
    let body = "One line of meeting notes with several words. ".repeat(20);
    format!("{FIRST_LINES}{} {body}\n{LAST_LINES}", ">".repeat(200))
}

#[test]
fn line_limits_keep_raw_lines_and_report_the_exact_cut() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("report.md");
    fs::write(&path, long_nested_report()).unwrap();

    let head = file_read(&path, &["--max-lines", "5"], false);
    let tail = file_read(&path, &["--last-lines", "5"], false);
    assert_eq!(head.stdout, format!("{FIRST_LINES}{CUT_MARKER}").as_bytes());
    assert_eq!(
        tail.stdout,
        format!("<!-- ... (5 lines above) — SKIM_PASSTHROUGH=1 for full output -->\n{LAST_LINES}")
            .as_bytes()
    );
}

#[test]
fn small_token_budget_keeps_whole_raw_lines_and_reports_the_cut() {
    let temp = TempDir::new().unwrap();
    let path = temp.path().join("report.md");
    fs::write(&path, long_nested_report()).unwrap();

    let output = file_read(&path, &["--tokens", "50"], false);
    assert_eq!(
        output.stdout,
        format!("{FIRST_LINES}{CUT_MARKER}").as_bytes()
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("[skim] token budget:"), "{stderr}");
}
