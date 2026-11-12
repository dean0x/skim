//! CLI integration tests for `skim FILE --symbol NAME` (issue 540, PR 2).
//!
//! `--symbol` prints the raw source lines of one named declaration. These tests
//! drive the real binary and pin the case table: one match, several matches,
//! qualified names, the refusals and their exit codes, and how the line bounds
//! and line numbers compose after the selection.
//!
//! Every test runs in a throw-away home through `common::skim_sandboxed`, which
//! gives the child its own parser cache, switches analytics off and pins the
//! working directory. File names are therefore relative and the headers of a
//! multi-match read are deterministic. The cache key of a symbol read is pinned
//! in `cli_cache.rs`.
//!
//! The raw source of every expected output is sliced from the fixture itself,
//! so a test states which fixture lines a read must print and not a second copy
//! of their text.

use std::fs;
use std::path::Path;

use tempfile::TempDir;

mod common;

/// Geometry fixture. Line numbers (1-based) that the tests rely on:
///
/// | lines | declaration |
/// |-------|-------------|
/// | 2-5   | `struct Point` |
/// | 8-10  | `Point::new` |
/// | 12-15 | `Point::norm` |
/// | 18-20 | `struct Shape` |
/// | 23-25 | `Shape::norm` |
/// | 28-35 | `fn checksum` (8 lines) |
const RUST_SRC: &str = "\
//! Geometry helpers.
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub fn new(x: i32, y: i32) -> Self {
        Point { x, y }
    }

    pub fn norm(&self) -> i32 {
        let sq = |v: i32| v * v;
        sq(self.x) + sq(self.y)
    }
}

pub struct Shape {
    pub sides: u32,
}

impl Shape {
    pub fn norm(&self) -> u32 {
        self.sides
    }
}

pub fn checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    for byte in data {
        sum = sum.wrapping_mul(31);
        sum = sum.wrapping_add(u32::from(*byte));
    }
    sum
}
";

/// Two classes with a method of the same name.
const TS_SRC: &str = "\
class A {
  run(): number {
    return 1;
  }
}

class B {
  run(): number {
    return 2;
  }
}
";

/// A component declared after JSX. The plain TypeScript grammar cannot parse
/// the JSX and loses the later declaration (see the contrast check in the
/// test), so only the TSX grammar finds `Footer`.
const TSX_SRC: &str = "\
const banner = <div>hello</div>;

export function Footer() {
  return <footer>{banner}</footer>;
}
";

/// The result of one `skim` run.
struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Write `content` to the file `name` inside `home`. Tests name the file by
/// that relative path on the command line.
fn put(home: &Path, name: &str, content: &str) {
    fs::write(home.join(name), content).unwrap();
}

/// Run `skim <args>` with `home` as the working directory and the sandbox.
fn run(home: &Path, args: &[&str]) -> Run {
    finish(common::skim_sandboxed(home).args(args).output())
}

/// Run `skim <args>` with `stdin` on standard input.
fn run_with_stdin(home: &Path, args: &[&str], stdin: &str) -> Run {
    finish(
        common::skim_sandboxed(home)
            .args(args)
            .write_stdin(stdin)
            .output(),
    )
}

fn finish(output: std::io::Result<std::process::Output>) -> Run {
    let output = output.expect("skim must start");
    Run {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    }
}

/// A home with `lib.rs` (the geometry fixture) inside.
fn rust_home() -> TempDir {
    let home = TempDir::new().unwrap();
    put(home.path(), "lib.rs", RUST_SRC);
    home
}

/// Lines `first..=last` (1-based, inclusive) of `source`, each with its `\n`.
fn lines(source: &str, first: usize, last: usize) -> String {
    source
        .split_inclusive('\n')
        .skip(first - 1)
        .take(last - first + 1)
        .collect()
}

/// The part of a not-found message after `declarations: `.
fn listed_names(stderr: &str) -> &str {
    stderr
        .split_once("declarations: ")
        .unwrap_or_else(|| panic!("no declaration list in: {stderr}"))
        .1
        .trim_end()
}

// ============================================================================
// TP-30 (AC-11): one match prints exactly the declaration lines
// ============================================================================

#[test]
fn one_match_prints_exactly_the_declaration_lines_and_a_marker_on_stderr() {
    // TP-30
    let home = rust_home();

    let out = run(home.path(), &["lib.rs", "--symbol", "Point"]);

    assert_eq!(out.code, Some(0));
    assert_eq!(
        out.stdout,
        lines(RUST_SRC, 2, 5),
        "stdout is the struct's source lines and nothing else"
    );
    assert_eq!(
        out.stderr,
        "[skim] --symbol \"Point\": lines 2-5 of 35 shown (1 above, 30 below not shown) \
         — omit --symbol for the whole file\n",
        "the marker states the lines above and below that stdout does not show"
    );
}

#[test]
fn a_declaration_that_ends_the_file_reports_no_lines_below() {
    // TP-30 (the boundary of the above/below count)
    let home = rust_home();

    let out = run(home.path(), &["lib.rs", "--symbol", "checksum"]);

    assert_eq!(out.code, Some(0));
    assert_eq!(out.stdout, lines(RUST_SRC, 28, 35));
    assert!(
        out.stderr
            .contains("lines 28-35 of 35 shown (27 above, 0 below not shown)"),
        "stderr: {}",
        out.stderr
    );
}

// ============================================================================
// TP-31 (AC-12): not found
// ============================================================================

#[test]
fn unknown_name_exits_1_and_lists_the_declaration_names_of_the_file() {
    // TP-31
    let home = rust_home();

    let out = run(home.path(), &["lib.rs", "--symbol", "nope"]);

    assert_eq!(out.code, Some(1));
    assert_eq!(out.stdout, "", "a failed read prints nothing on stdout");
    assert_eq!(
        out.stderr,
        "skim: no declaration named \"nope\" in \"lib.rs\"; declarations: \
         \"Point\", \"x\", \"y\", \"new\", \"norm\", \"Shape\", \"sides\", \"checksum\"\n"
    );
    assert!(
        !out.stderr.contains("node cap"),
        "a walk that finished must not blame the node cap"
    );
}

#[test]
fn unknown_name_lists_at_most_ten_names_and_puts_close_names_first() {
    // TP-31
    let home = TempDir::new().unwrap();
    let mut source: String = (0..14).map(|i| format!("fn f{i:02}() {{}}\n")).collect();
    source.push_str("fn the_RUN_loop() {}\n");
    put(home.path(), "many.rs", &source);

    let out = run(home.path(), &["many.rs", "--symbol", "run"]);

    assert_eq!(out.code, Some(1));
    let names = listed_names(&out.stderr);
    assert_eq!(
        names.matches('"').count(),
        20,
        "ten quoted names, two quotes each: {names}"
    );
    assert!(
        names.ends_with(", …"),
        "more names exist than are listed: {names}"
    );
    assert!(
        names.starts_with("\"the_RUN_loop\", \"f00\""),
        "the name that holds the query (ignoring case) comes first: {names}"
    );
}

// ============================================================================
// TP-32 (AC-12): two or more matches
// ============================================================================

#[test]
fn two_declarations_with_one_name_print_both_with_k_of_n_headers() {
    // TP-32
    let home = rust_home();

    let out = run(home.path(), &["lib.rs", "--symbol", "norm"]);

    let expected = format!(
        "==> lib.rs:12-15 norm (1 of 2) <==\n{}\n==> lib.rs:23-25 norm (2 of 2) <==\n{}",
        lines(RUST_SRC, 12, 15),
        lines(RUST_SRC, 23, 25),
    );
    assert_eq!(out.code, Some(0));
    assert_eq!(out.stdout, expected);
    assert_eq!(
        out.stderr,
        "[skim] --symbol \"norm\": 2 declarations, 7 of 35 lines shown (28 not shown) \
         — omit --symbol for the whole file\n"
    );
}

// ============================================================================
// TP-33 (AC-12): qualified names
// ============================================================================

#[test]
fn rust_type_path_name_selects_only_the_method_inside_that_impl() {
    // TP-33
    let home = rust_home();

    let point = run(home.path(), &["lib.rs", "--symbol", "Point::norm"]);
    let shape = run(home.path(), &["lib.rs", "--symbol", "Shape::norm"]);

    assert_eq!(point.code, Some(0));
    assert_eq!(point.stdout, lines(RUST_SRC, 12, 15));
    assert!(
        point.stderr.contains("lines 12-15 of 35 shown"),
        "{}",
        point.stderr
    );
    assert_eq!(shape.code, Some(0));
    assert_eq!(shape.stdout, lines(RUST_SRC, 23, 25));
}

#[test]
fn typescript_dotted_name_selects_only_the_method_inside_that_class() {
    // TP-33
    let home = TempDir::new().unwrap();
    put(home.path(), "svc.ts", TS_SRC);

    let a = run(home.path(), &["svc.ts", "--symbol", "A.run"]);
    let b = run(home.path(), &["svc.ts", "--symbol", "B.run"]);

    assert_eq!(a.code, Some(0));
    assert_eq!(a.stdout, lines(TS_SRC, 2, 4));
    assert_eq!(b.code, Some(0));
    assert_eq!(b.stdout, lines(TS_SRC, 8, 10));
}

#[test]
fn a_qualifier_with_no_such_container_finds_nothing() {
    // TP-33 (the qualifier restricts the match; it is not ignored)
    let home = rust_home();

    let out = run(home.path(), &["lib.rs", "--symbol", "Circle::norm"]);

    assert_eq!(out.code, Some(1));
    assert_eq!(out.stdout, "");
    assert!(
        out.stderr
            .starts_with("skim: no declaration named \"Circle::norm\" in \"lib.rs\""),
        "stderr: {}",
        out.stderr
    );
}

// ============================================================================
// TP-34 (AC-12): Markdown and data formats exit 3
// ============================================================================

#[test]
fn markdown_and_data_formats_exit_3_and_name_the_language() {
    // TP-34
    let cases = [
        ("notes.md", "# Title\n\ntext\n", "Markdown"),
        ("data.json", "{\"a\": 1}\n", "JSON"),
        ("data.yaml", "a: 1\n", "YAML"),
        ("data.toml", "a = 1\n", "TOML"),
    ];
    let home = TempDir::new().unwrap();
    for (file, content, language) in cases {
        put(home.path(), file, content);

        let out = run(home.path(), &[file, "--symbol", "a"]);

        assert_eq!(out.code, Some(3), "{file}: stderr {}", out.stderr);
        assert_eq!(out.stdout, "", "{file}");
        assert_eq!(
            out.stderr,
            format!("skim: --symbol does not support {language} (file \"{file}\")\n"),
            "{file}"
        );
    }
}

#[test]
fn a_file_with_no_detected_language_exits_3_and_the_language_flag_is_the_remedy() {
    // TP-34
    let home = TempDir::new().unwrap();
    put(home.path(), "LICENSE", "fn license_holder() {}\n");

    let refused = run(home.path(), &["LICENSE", "--symbol", "license_holder"]);
    let named = run(
        home.path(),
        &[
            "LICENSE",
            "--symbol",
            "license_holder",
            "--language",
            "rust",
        ],
    );

    assert_eq!(refused.code, Some(3));
    assert_eq!(
        refused.stderr,
        "skim: --symbol does not support a file with no detected language: \"LICENSE\" \
         (try --language)\n"
    );
    assert_eq!(named.code, Some(0), "stderr: {}", named.stderr);
    assert_eq!(named.stdout, "fn license_holder() {}\n");
}

// ============================================================================
// TP-35 (AC-12): .tsx uses the TSX grammar
// ============================================================================

#[test]
fn tsx_file_finds_a_component_declared_after_jsx() {
    // TP-35
    let home = TempDir::new().unwrap();
    put(home.path(), "page.tsx", TSX_SRC);
    // The same bytes under a `.ts` name are parsed with the plain TypeScript
    // grammar. If that grammar found `Footer` too, this test would not need
    // the TSX grammar to pass.
    put(home.path(), "page.ts", TSX_SRC);

    let tsx = run(home.path(), &["page.tsx", "--symbol", "Footer"]);
    let ts = run(home.path(), &["page.ts", "--symbol", "Footer"]);

    assert_eq!(tsx.code, Some(0), "stderr: {}", tsx.stderr);
    assert_eq!(tsx.stdout, lines(TSX_SRC, 3, 5));
    assert_eq!(
        ts.code,
        Some(1),
        "fixture check: the TypeScript grammar must lose Footer; stderr: {}",
        ts.stderr
    );
}

// ============================================================================
// TP-36 (AC-12): inputs that are not exactly one file exit 2
// ============================================================================

#[test]
fn stdin_exits_2() {
    // TP-36
    let home = rust_home();

    let out = run_with_stdin(
        home.path(),
        &["-", "--symbol", "norm", "--language", "rust"],
        RUST_SRC,
    );

    assert_eq!(out.code, Some(2));
    assert_eq!(out.stdout, "");
    assert_eq!(out.stderr, "skim: --symbol needs exactly one file path\n");
}

#[test]
fn two_files_exit_2() {
    // TP-36
    let home = rust_home();
    put(home.path(), "other.rs", "fn norm() {}\n");

    let out = run(home.path(), &["lib.rs", "other.rs", "--symbol", "norm"]);

    assert_eq!(out.code, Some(2));
    assert_eq!(out.stdout, "");
    assert_eq!(out.stderr, "skim: --symbol needs exactly one file path\n");
}

#[test]
fn a_glob_exits_2_even_when_it_matches_one_file() {
    // TP-36
    let home = rust_home();

    let out = run(home.path(), &["li*.rs", "--symbol", "norm"]);

    assert_eq!(out.code, Some(2));
    assert_eq!(out.stdout, "");
    assert_eq!(
        out.stderr,
        "skim: --symbol needs exactly one file path, not a glob: \"li*.rs\"\n"
    );
}

#[test]
fn an_existing_file_whose_name_holds_glob_characters_is_read_as_that_file() {
    // TP-36 (a route file such as `[id].tsx` is one file path, not a glob)
    let home = TempDir::new().unwrap();
    put(home.path(), "[id].tsx", TSX_SRC);

    let out = run(home.path(), &["[id].tsx", "--symbol", "Footer"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(out.stdout, lines(TSX_SRC, 3, 5));
}

#[test]
fn a_directory_exits_2() {
    // TP-36
    let home = rust_home();
    fs::create_dir(home.path().join("src")).unwrap();

    let out = run(home.path(), &["src", "--symbol", "norm"]);

    assert_eq!(out.code, Some(2));
    assert_eq!(out.stdout, "");
    assert_eq!(
        out.stderr,
        "skim: --symbol needs exactly one file path, not a directory: \"src\"\n"
    );
}

// ============================================================================
// TP-37 (AC-12): option mixes that cannot mean a symbol read exit 2
// ============================================================================

#[test]
fn an_empty_name_exits_2() {
    // TP-37
    let home = rust_home();

    let out = run(home.path(), &["lib.rs", "--symbol", ""]);

    assert_eq!(out.code, Some(2));
    assert_eq!(out.stdout, "");
    assert_eq!(out.stderr, "skim: --symbol needs a non-empty name\n");
}

#[test]
fn a_token_budget_exits_2() {
    // TP-37
    let home = rust_home();

    let out = run(
        home.path(),
        &["lib.rs", "--symbol", "norm", "--tokens", "50"],
    );

    assert_eq!(out.code, Some(2));
    assert_eq!(out.stdout, "");
    assert_eq!(
        out.stderr,
        "skim: --symbol does not combine with --tokens\n"
    );
}

#[test]
fn every_mode_other_than_full_exits_2_and_full_is_allowed() {
    // TP-37
    let home = rust_home();

    for mode in ["structure", "signatures", "types", "minimal", "pseudo"] {
        let out = run(
            home.path(),
            &["lib.rs", "--symbol", "Point", "--mode", mode],
        );

        assert_eq!(out.code, Some(2), "--mode {mode}: stderr {}", out.stderr);
        assert_eq!(out.stdout, "", "--mode {mode}");
        assert_eq!(
            out.stderr,
            format!(
                "skim: --symbol prints source lines and needs --mode full, not --mode \"{mode}\"\n"
            ),
            "--mode {mode}"
        );
    }

    let full = run(
        home.path(),
        &["lib.rs", "--symbol", "Point", "--mode", "full"],
    );
    assert_eq!(full.code, Some(0), "stderr: {}", full.stderr);
    assert_eq!(full.stdout, lines(RUST_SRC, 2, 5));
}

#[test]
fn a_name_with_control_bytes_is_quoted_in_the_message() {
    // TP-37
    let home = rust_home();

    let out = run(home.path(), &["lib.rs", "--symbol", "x\u{1b}[31mred\nline"]);

    assert_eq!(out.code, Some(1));
    assert!(
        out.stderr
            .starts_with("skim: no declaration named \"x\\u{1b}[31mred\\nline\" in "),
        "the name is printed with Debug escapes: {:?}",
        out.stderr
    );
    assert!(
        !out.stderr.contains('\u{1b}'),
        "no raw escape byte reaches the terminal: {:?}",
        out.stderr
    );
    assert_eq!(
        out.stderr.trim_end().lines().count(),
        1,
        "a newline in the name must not split the message: {:?}",
        out.stderr
    );
}

// ============================================================================
// TP-38, TP-39, TP-40 (AC-13): bounds and line numbers compose after the slice
// ============================================================================

#[test]
fn max_lines_bounds_the_slice_and_the_marker_counts_toward_the_bound() {
    // TP-38
    let home = rust_home();

    let out = run(
        home.path(),
        &["lib.rs", "--symbol", "checksum", "--max-lines", "4"],
    );

    assert_eq!(out.code, Some(0));
    assert_eq!(
        out.stdout,
        format!(
            "{}// ... (5 lines truncated) — SKIM_PASSTHROUGH=1 for full output\n",
            lines(RUST_SRC, 28, 30)
        ),
        "3 declaration lines, then the marker: 4 lines in all"
    );
    assert!(
        out.stderr.contains("lines 28-35 of 35 shown"),
        "the stderr notice still states the whole declaration: {}",
        out.stderr
    );
}

#[test]
fn line_numbers_start_at_the_first_line_of_the_declaration() {
    // TP-39
    let home = rust_home();

    let out = run(home.path(), &["lib.rs", "--symbol", "Point::norm", "-n"]);

    let expected: String = lines(RUST_SRC, 12, 15)
        .split_inclusive('\n')
        .enumerate()
        .map(|(offset, line)| format!("{}\t{line}", 12 + offset))
        .collect();
    assert_eq!(out.code, Some(0));
    assert_eq!(out.stdout, expected);
}

#[test]
fn last_lines_keeps_the_tail_of_the_slice_behind_a_leading_marker() {
    // TP-40
    let home = rust_home();

    let out = run(
        home.path(),
        &["lib.rs", "--symbol", "checksum", "--last-lines", "3"],
    );

    assert_eq!(out.code, Some(0));
    assert_eq!(
        out.stdout,
        format!(
            "// ... (6 lines above) — SKIM_PASSTHROUGH=1 for full output\n{}",
            lines(RUST_SRC, 34, 35)
        ),
        "the marker, then the last 2 declaration lines: 3 lines in all"
    );
}

#[test]
fn last_lines_with_line_numbers_numbers_the_tail_from_the_source() {
    // TP-39, TP-40
    let home = rust_home();

    let out = run(
        home.path(),
        &["lib.rs", "--symbol", "checksum", "--last-lines", "3", "-n"],
    );

    assert_eq!(out.code, Some(0));
    assert_eq!(
        out.stdout,
        "// ... (6 lines above) — SKIM_PASSTHROUGH=1 for full output\n34\t    sum\n35\t}\n"
    );
}

// ============================================================================
// TP-123 (AC-12): a walk that stops at the node cap
// ============================================================================

/// A Rust file of `count` constants. Each constant is eight syntax nodes, so
/// 20,000 of them are well above the 100,000-node cap of the declaration walk.
fn constants_source(count: usize) -> String {
    (0..count)
        .map(|i| format!("const C{i}: u32 = {i};\n"))
        .collect()
}

#[test]
fn no_match_in_a_file_above_the_node_cap_exits_1_and_names_the_cap() {
    // TP-123
    let home = TempDir::new().unwrap();
    let mut source = constants_source(20_000);
    source.push_str("fn late() {}\n");
    put(home.path(), "big.rs", &source);

    let out = run(home.path(), &["big.rs", "--symbol", "late"]);

    assert_eq!(out.code, Some(1));
    assert_eq!(out.stdout, "");
    assert_eq!(
        out.stderr,
        "skim: no declaration named \"late\" in the first 100000 syntax nodes of \"big.rs\" \
         (node cap)\n",
        "the walk never reached `late`, so the message must not claim the name is absent"
    );
}

#[test]
fn a_match_before_the_node_cap_is_found_and_the_notice_names_the_cap() {
    // TP-123 (the cap fails only a read that found nothing). A second `dup`
    // lies past the cap, so the reader sees one of two declarations; the
    // notice must say that more may exist.
    let home = TempDir::new().unwrap();
    let source = format!(
        "fn dup() {{}}\n{}fn dup() {{ 1 }}\n",
        constants_source(20_000)
    );
    put(home.path(), "big.rs", &source);

    let out = run(home.path(), &["big.rs", "--symbol", "dup"]);

    assert_eq!(out.code, Some(0), "stderr: {}", out.stderr);
    assert_eq!(out.stdout, "fn dup() {}\n");
    assert_eq!(
        out.stderr,
        "[skim] --symbol \"dup\": lines 1-1 of 20002 shown (0 above, 20001 below not shown); \
         the declaration walk stopped at its syntax-node or depth cap, so more declarations \
         with this name may exist — omit --symbol for the whole file\n"
    );
}
