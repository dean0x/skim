//! CLI output for Markdown at the nesting limit.

mod common;
#[path = "common/markdown_nesting.rs"]
mod markdown;

use markdown::{clean, nested_report, run};
use std::{fs, path::Path, process::Output};
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
