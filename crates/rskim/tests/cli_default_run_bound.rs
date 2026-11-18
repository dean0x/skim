//! Exact multi-file streams from a8293f35, before the default run bound.

mod common;

use std::fs;
use std::process::Output;
use tempfile::TempDir;

const LIMIT: usize = 29_000;

struct Fixture {
    home: TempDir,
    paths: Vec<String>,
}

impl Fixture {
    fn new(file_count: usize, functions_per_file: usize) -> Self {
        let home = TempDir::new().unwrap();
        let paths = (0..file_count)
            .map(|file| {
                let path = format!("file_{file:03}.rs");
                let source: String = (0..functions_per_file)
                    .map(|function| {
                        format!(
                            "pub fn value_{file:03}_{function:03}(input: i32) -> i32 {{\n    let doubled = input * 2;\n    let adjusted = doubled + 10;\n    adjusted - 1\n}}\n"
                        )
                    })
                    .collect();
                fs::write(home.path().join(&path), source).unwrap();
                path
            })
            .collect();
        Self { home, paths }
    }

    fn run(&self, args: &[&str]) -> Output {
        common::skim_sandboxed(self.home.path())
            .arg("--no-cache")
            .args(args)
            .args(&self.paths)
            .assert()
            .success()
            .get_output()
            .clone()
    }
}

fn assert_over_limit(fixture: Fixture, args: &[&str], stdout: &str, stderr: &str) {
    assert!(
        stdout.chars().count() > LIMIT,
        "golden stdout must exceed the limit"
    );
    let actual = fixture.run(args);
    assert_eq!(std::str::from_utf8(&actual.stdout).unwrap(), stdout);
    assert_eq!(std::str::from_utf8(&actual.stderr).unwrap(), stderr);
}

#[test]
fn fitting_default_matches_head_stdout_and_typed_structure_stderr() {
    let fixture = Fixture::new(3, 30);
    let default = fixture.run(&[]);
    let typed = fixture.run(&["--mode=structure"]);
    let stdout = std::str::from_utf8(&default.stdout).unwrap();
    let stderr = std::str::from_utf8(&default.stderr).unwrap();
    assert!(stdout.chars().count() + stderr.chars().count() < LIMIT);
    assert_eq!(
        stdout,
        include_str!("fixtures/default_run_bound/fit.stdout")
    );
    assert_eq!(default.stderr, typed.stderr);
}

#[test]
fn typed_signatures_above_the_limit_match_head_streams() {
    assert_over_limit(
        Fixture::new(3, 250),
        &["--mode=signatures"],
        include_str!("fixtures/default_run_bound/typed_signatures.stdout"),
        include_str!("fixtures/default_run_bound/typed_signatures.stderr"),
    );
}

#[test]
fn max_lines_above_the_limit_matches_head_streams() {
    assert_over_limit(
        Fixture::new(120, 5),
        &["--max-lines", "5"],
        include_str!("fixtures/default_run_bound/typed_max_lines.stdout"),
        include_str!("fixtures/default_run_bound/typed_max_lines.stderr"),
    );
}

#[test]
fn last_lines_above_the_limit_matches_head_streams() {
    assert_over_limit(
        Fixture::new(120, 5),
        &["--last-lines", "5"],
        include_str!("fixtures/default_run_bound/typed_last_lines.stdout"),
        include_str!("fixtures/default_run_bound/typed_last_lines.stderr"),
    );
}

#[test]
fn tokens_above_the_character_limit_match_head_streams() {
    assert_over_limit(
        Fixture::new(3, 250),
        &["--tokens", "20000"],
        include_str!("fixtures/default_run_bound/tokens_over_limit.stdout"),
        include_str!("fixtures/default_run_bound/tokens_over_limit.stderr"),
    );
}
