//! CLI tests: the transform modes parse a `.tsx` file with the TSX grammar.
//!
//! The plain TypeScript grammar reads JSX as a type assertion. It then loses
//! or garbles every declaration after the first JSX element. The file path
//! picks the grammar, so each surface that knows the path must pass it on:
//! a file read in each mode, `--tokens` with and without `-n`, the default
//! character bound when it steps down, a multi-file run, and stdin with
//! `--filename`. The language flag still wins: `--language javascript` keeps
//! the JavaScript grammar, and stdin with only `--language typescript` keeps
//! the TypeScript grammar.
//!
//! Every test runs in a throw-away home through `common::skim_sandboxed`, so
//! the parser cache and the working directory are the test's own. Reads pass
//! `--no-cache`, because the cache key does not hold the language flag.

use std::fs;
use std::path::Path;

use tempfile::TempDir;

mod common;

const COMPONENT: &str = include_str!("../../../tests/fixtures/typescript/component.tsx");

/// A `.ts` file with an angle-bracket type assertion. The TSX grammar reads
/// `<Config>` as a JSX element that never closes. The body of `load` is long
/// enough that its structure view saves more than the view notice costs.
const TYPE_ASSERTION: &str = "declare const raw: unknown;
const value = <Config>raw;
export function load(path: string): Config {
  const text = read(path);
  const lines = text.split(\"\\n\").filter((line) => line.trim().length > 0);
  const entries = lines.map((line) => line.split(\"=\").map((part) => part.trim()));
  return Object.fromEntries(entries) as Config;
}
";

/// The default character bound of a single read (`MAX_STDOUT_CHARS`).
const DEFAULT_LIMIT: usize = 29_000;

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn put(home: &Path, name: &str, content: &str) {
    fs::write(home.join(name), content).unwrap();
}

/// Run `skim <args>` in `cwd` with the sandbox of `home`.
fn run_in(home: &Path, cwd: &Path, args: &[&str], stdin: Option<&str>) -> Run {
    let mut cmd = common::skim_sandboxed(home);
    cmd.current_dir(cwd).args(args);
    if let Some(stdin) = stdin {
        cmd.write_stdin(stdin);
    }
    let output = cmd.output().expect("skim must start");
    let run = Run {
        code: output.status.code(),
        stdout: String::from_utf8(output.stdout).expect("stdout is UTF-8"),
        stderr: String::from_utf8(output.stderr).expect("stderr is UTF-8"),
    };
    assert_eq!(run.code, Some(0), "skim {args:?}: stderr: {}", run.stderr);
    run
}

fn run(home: &Path, args: &[&str]) -> Run {
    run_in(home, home, args, None)
}

fn run_stdin(home: &Path, args: &[&str], stdin: &str) -> Run {
    run_in(home, home, args, Some(stdin))
}

fn component_home() -> TempDir {
    let home = TempDir::new().unwrap();
    put(home.path(), "component.tsx", COMPONENT);
    home
}

/// The 1-based line of `COMPONENT` that starts with `prefix`.
fn source_line(prefix: &str) -> usize {
    COMPONENT
        .lines()
        .position(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("fixture has no line {prefix:?}"))
        + 1
}

/// The structure view of the TSX grammar: `ItemList`, which follows the JSX
/// of `Button`, keeps its signature and loses its body.
fn assert_tsx_structure(view: &str) {
    let item_list = view
        .lines()
        .find(|line| line.contains("export function ItemList("))
        .unwrap_or_else(|| panic!("ItemList is lost:\n{view}"));
    assert!(item_list.ends_with("{...}"), "body is kept: {item_list}");
    assert!(!view.contains("<li key"), "a JSX body is kept:\n{view}");
    assert!(view.contains("export function Footer(): JSX.Element"));
}

/// Copies of [`COMPONENT`] in [`large_component_source`].
const COPIES: usize = 40;

/// [`COMPONENT`] [`COPIES`] times. Its structure view is over the default
/// bound and its signatures view is under it.
fn large_component_source() -> String {
    COMPONENT.repeat(COPIES)
}

/// The signatures view of [`large_component_source`] under the TSX grammar,
/// served by the default bound after structure does not fit.
fn assert_tsx_step_down(run: &Run) {
    assert!(run.stdout.chars().count() < DEFAULT_LIMIT, "{}", run.stderr);
    assert!(run.stderr.contains("signatures"), "{}", run.stderr);
    for signature in [
        "function ItemList({ items }: { items: Item[] }): JSX.Element",
        "function Footer(): JSX.Element",
    ] {
        assert_eq!(
            run.stdout.matches(signature).count(),
            COPIES,
            "{signature}:\n{}",
            run.stdout
        );
    }
    assert!(!run.stdout.contains("<footer>"), "{}", run.stdout);
}

// ============================================================================
// File reads
// ============================================================================

#[test]
fn tsx_file_read_keeps_the_declarations_after_jsx_in_every_mode() {
    let home = component_home();
    let item_list = "export function ItemList({ items }: { items: Item[] }): JSX.Element";
    let top_level_comments = ["An item is one row", "The footer closes every page"];
    // (mode, text the view must hold, text the view must not hold)
    let cases: [(&str, &[&str], &[&str]); 5] = [
        ("structure", &[item_list, "{...}"], &["<li key"]),
        (
            "signatures",
            &[
                "function Button(",
                "function ItemList({ items }: { items: Item[] }): JSX.Element",
                "function Footer(",
            ],
            &["<footer>", "<section>"],
        ),
        (
            "types",
            &["interface ButtonProps", "type Item = ", "class Panel"],
            &[],
        ),
        // Minimal keeps the comments inside a body and removes the comments
        // between declarations.
        (
            "minimal",
            &["// Render one row for each item.", item_list],
            &top_level_comments,
        ),
        (
            "pseudo",
            &["type Item = { id: number; name: string }", item_list],
            &top_level_comments,
        ),
    ];
    for (mode, kept, removed) in cases {
        let flag = format!("--mode={mode}");
        let out = run(home.path(), &["component.tsx", &flag, "--no-cache"]);
        assert!(
            out.stdout.len() < COMPONENT.len(),
            "{mode}:\n{}",
            out.stdout
        );
        for text in kept {
            assert!(
                out.stdout.contains(text),
                "{mode} lost {text:?}:\n{}",
                out.stdout
            );
        }
        for text in removed {
            assert!(
                !out.stdout.contains(text),
                "{mode} kept {text:?}:\n{}",
                out.stdout
            );
        }
    }
    let structure = run(
        home.path(),
        &["component.tsx", "--mode=structure", "--no-cache"],
    );
    assert_tsx_structure(&structure.stdout);
}

#[test]
fn tsx_token_budget_read_serves_the_tsx_view_with_source_line_numbers() {
    let home = component_home();

    let plain = run(
        home.path(),
        &["component.tsx", "--tokens", "2000", "--no-cache"],
    );
    let numbered = run(
        home.path(),
        &["component.tsx", "--tokens", "2000", "-n", "--no-cache"],
    );

    assert_tsx_structure(&plain.stdout);
    for prefix in ["export function ItemList(", "export function Footer("] {
        let expected = format!("{}\t{prefix}", source_line(prefix));
        assert!(
            numbered
                .stdout
                .lines()
                .any(|line| line.starts_with(&expected)),
            "no {expected:?}:\n{}",
            numbered.stdout
        );
    }
}

#[test]
fn tsx_default_read_over_the_bound_steps_down_to_the_tsx_signatures_view() {
    let home = TempDir::new().unwrap();
    put(home.path(), "big.tsx", &large_component_source());
    // Fixture check: structure must not fit, or the read never steps down.
    let structure = run(home.path(), &["big.tsx", "--mode=structure", "--no-cache"]);
    assert!(structure.stdout.chars().count() >= DEFAULT_LIMIT);

    let default = run(home.path(), &["big.tsx", "--no-cache"]);

    assert_tsx_step_down(&default);
}

#[test]
fn tsx_multi_file_runs_serve_the_tsx_view_for_each_file() {
    let home = TempDir::new().unwrap();
    put(home.path(), "a.tsx", COMPONENT);
    put(home.path(), "b.tsx", COMPONENT);

    let runs = [
        run(home.path(), &["a.tsx", "b.tsx", "--no-cache"]),
        run(
            home.path(),
            &["a.tsx", "b.tsx", "--tokens", "4000", "--no-cache"],
        ),
    ];

    for out in runs {
        let collapsed = out
            .stdout
            .lines()
            .filter(|line| line.starts_with("export function ItemList(") && line.ends_with("{...}"))
            .count();
        assert_eq!(collapsed, 2, "{}", out.stdout);
        assert!(!out.stdout.contains("<li key"), "{}", out.stdout);
    }
}

#[test]
fn tsx_parent_dir_path_with_a_language_flag_serves_the_tsx_view() {
    // A path with `..` has no detected language, so the token cascade takes
    // its explicit-language fallback. That fallback must keep the path too.
    let home = component_home();
    let sub = home.path().join("sub");
    fs::create_dir(&sub).unwrap();

    let out = run_in(
        home.path(),
        &sub,
        &[
            "../component.tsx",
            "--language",
            "typescript",
            "--tokens",
            "2000",
            "--no-cache",
        ],
        None,
    );

    assert_tsx_structure(&out.stdout);
}

// ============================================================================
// stdin
// ============================================================================

// The three stdin reads are separate tests: each one reaches a different
// transform call, so a failure names the call that lost the file name.

#[test]
fn tsx_stdin_with_a_tsx_filename_serves_the_tsx_view() {
    let home = TempDir::new().unwrap();

    let out = run_stdin(
        home.path(),
        &["-", "--filename", "component.tsx", "--mode=structure"],
        COMPONENT,
    );

    assert_tsx_structure(&out.stdout);
}

#[test]
fn tsx_stdin_token_budget_read_serves_the_tsx_view() {
    let home = TempDir::new().unwrap();

    let out = run_stdin(
        home.path(),
        &["-", "--filename", "component.tsx", "--tokens", "2000"],
        COMPONENT,
    );

    assert_tsx_structure(&out.stdout);
}

#[test]
fn tsx_stdin_token_budget_read_numbers_the_lines_of_the_tsx_view() {
    let home = TempDir::new().unwrap();

    let out = run_stdin(
        home.path(),
        &["-", "--filename", "component.tsx", "--tokens", "2000", "-n"],
        COMPONENT,
    );

    let expected = format!(
        "{}\texport function ItemList(",
        source_line("export function ItemList(")
    );
    assert!(
        out.stdout.lines().any(|line| line.starts_with(&expected)),
        "no {expected:?}:\n{}",
        out.stdout
    );
}

#[test]
fn tsx_stdin_default_read_over_the_bound_steps_down_to_the_tsx_signatures_view() {
    let home = TempDir::new().unwrap();

    let out = run_stdin(
        home.path(),
        &["-", "--filename", "big.tsx"],
        &large_component_source(),
    );

    assert_tsx_step_down(&out);
}

#[test]
fn stdin_with_only_a_typescript_language_flag_keeps_the_typescript_grammar() {
    // No file name, so no extension: the type assertion must still parse.
    let home = TempDir::new().unwrap();

    let out = run_stdin(
        home.path(),
        &["-", "--language", "typescript", "--mode=structure"],
        TYPE_ASSERTION,
    );

    let load = out
        .stdout
        .lines()
        .find(|line| line.starts_with("export function load("))
        .unwrap_or_else(|| panic!("load is lost:\n{}", out.stdout));
    assert!(load.ends_with("{...}"), "{}", out.stdout);
    assert!(!out.stdout.contains("const entries"), "{}", out.stdout);
}

// ============================================================================
// The grammar follows the extension; the language flag wins
// ============================================================================

#[test]
fn ts_file_with_a_type_assertion_keeps_the_typescript_grammar() {
    let home = TempDir::new().unwrap();
    put(home.path(), "load.ts", TYPE_ASSERTION);

    let out = run(home.path(), &["load.ts", "--mode=structure", "--no-cache"]);

    let load = out
        .stdout
        .lines()
        .find(|line| line.starts_with("export function load("))
        .unwrap_or_else(|| panic!("load is lost:\n{}", out.stdout));
    assert!(load.ends_with("{...}"), "{}", out.stdout);
    assert!(!out.stdout.contains("const entries"), "{}", out.stdout);
}

#[test]
fn tsx_file_read_with_a_javascript_language_flag_keeps_the_javascript_grammar() {
    let home = component_home();
    put(home.path(), "component.js", COMPONENT);

    // Pseudo is left out: its JavaScript view of this fixture is the TSX view.
    for mode in ["structure", "signatures"] {
        let flag = format!("--mode={mode}");
        let as_js = run(
            home.path(),
            &[
                "component.tsx",
                "--language",
                "javascript",
                &flag,
                "--no-cache",
            ],
        );
        let js_file = run(home.path(), &["component.js", &flag, "--no-cache"]);
        let as_tsx = run(home.path(), &["component.tsx", &flag, "--no-cache"]);

        assert_eq!(as_js.stdout, js_file.stdout, "{mode}");
        // Fixture check: the TSX view differs, so the equality above shows
        // that the language flag, not the extension, picked the grammar.
        assert_ne!(as_js.stdout, as_tsx.stdout, "{mode}");
    }
}
