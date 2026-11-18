//! Whole-run token budgets. Count the exact stdout bytes with the CLI's encoding.

mod common;

use rskim_tokens::{Counter, Encoding};
use std::fs;
use std::process::Output;
use std::sync::LazyLock;
use tempfile::TempDir;

static COUNTER: LazyLock<Counter> = LazyLock::new(|| Counter::new(Encoding::Cl100k).unwrap());

fn tokens(text: &str) -> usize {
    COUNTER.count(text)
}

fn home(files: &[(&str, String)]) -> TempDir {
    let home = TempDir::new().unwrap();
    fs::create_dir(home.path().join("input")).unwrap();
    for (name, source) in files {
        fs::write(home.path().join("input").join(name), source).unwrap();
    }
    home
}

fn rust_source(index: usize) -> String {
    (0..40)
        .map(|j| {
            format!(
                "pub struct Record{index}_{j} {{\n    pub value: i32,\n    pub label: String,\n}}\n"
            )
        })
        .collect::<String>()
        + "pub fn calculate(value: i32) -> i32 {\n    let result = value + 1;\n    result * 2\n}\n"
}

fn rust_home(count: usize) -> TempDir {
    let files: Vec<_> = (0..count)
        .map(|i| (format!("f{i:02}.rs"), rust_source(i)))
        .collect();
    home(
        &files
            .iter()
            .map(|(n, s)| (n.as_str(), s.clone()))
            .collect::<Vec<_>>(),
    )
}

fn python_home() -> TempDir {
    let files: Vec<_> = (0..12)
        .map(|i| {
            let source = (0..40)
                .map(|j| {
                    format!(
                        "def function_{i:02}_{j:03}(value: int) -> int:\n    return value + 1\n"
                    )
                })
                .collect::<String>();
            (format!("f{i:02}.py"), source)
        })
        .collect();
    home(
        &files
            .iter()
            .map(|(n, s)| (n.as_str(), s.clone()))
            .collect::<Vec<_>>(),
    )
}

fn run(home: &TempDir, args: &[&str]) -> Output {
    common::skim_sandboxed(home.path())
        .args(args)
        .arg("--no-cache")
        .assert()
        .success()
        .get_output()
        .clone()
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).unwrap()
}

fn stderr(output: &Output) -> &str {
    std::str::from_utf8(&output.stderr).unwrap()
}

fn bounded(output: &Output, n: usize) {
    assert!(
        tokens(stdout(output)) <= n,
        "{} tokens > {n}: {}",
        tokens(stdout(output)),
        stdout(output)
    );
}

fn notice(output: &Output) -> &str {
    let text = stderr(output);
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.contains("budget covers the whole output of the run"),
        "{text}"
    );
    assert!(
        text.contains(
            "use a larger --tokens or fewer files for more, or SKIM_PASSTHROUGH=1 for full output"
        ),
        "{text}"
    );
    assert!(!text.contains("token budget:"), "{text}");
    assert!(!text.contains("escalated"), "{text}");
    text
}

fn trailer(shown: usize, total: usize) -> String {
    format!(
        "// [skim] {} files omitted ({shown} of {total} shown) — SKIM_PASSTHROUGH=1 for full output\n",
        total - shown
    )
}

/// Headers are fixture paths, not arbitrary comment lines in a body.
fn blocks(text: &str) -> Vec<(&str, &str)> {
    let headers: Vec<_> = text.match_indices("// input/").collect();
    headers
        .iter()
        .enumerate()
        .map(|(i, (start, _))| {
            let end = headers.get(i + 1).map_or(text.len(), |(pos, _)| *pos);
            let block = &text[*start..end];
            let (path, body) = block.split_once('\n').unwrap();
            (path.trim_start_matches("// "), body.trim_end_matches('\n'))
        })
        .collect()
}

#[test]
fn help_describes_the_whole_run_budget_mode_order_and_floor() {
    let home = TempDir::new().unwrap();
    for flag in ["--help", "-h"] {
        let output = run(&home, &[flag]);
        let help = stdout(&output)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        for fact in [
            "One budget covers the whole output of a run",
            "full -> minimal -> pseudo -> structure -> signatures -> types",
            "the omitted-files line can exceed N",
        ] {
            assert!(help.contains(fact), "{flag} must say {fact:?}: {help}");
        }
    }
}

#[test]
fn directory_stdout_obeys_one_budget() {
    let home = rust_home(20);
    let plain = run(&home, &["input", "--mode", "structure"]);
    assert!(tokens(stdout(&plain)) > 180);
    let output = run(&home, &["input", "--tokens", "180"]);
    bounded(&output, 180);
    notice(&output);
}

#[test]
fn explicit_list_and_quoted_glob_obey_one_budget() {
    let home = rust_home(20);
    let paths: Vec<_> = (0..20).map(|i| format!("input/f{i:02}.rs")).collect();
    for paths in [
        vec!["input/*.rs"],
        paths.iter().map(String::as_str).collect(),
    ] {
        let mut plain_args = paths.clone();
        plain_args.extend(["--mode", "structure"]);
        assert!(tokens(stdout(&run(&home, &plain_args))) > 180);
        let mut args = paths;
        args.extend(["--tokens", "180"]);
        let output = run(&home, &args);
        bounded(&output, 180);
        notice(&output);
    }
}

#[test]
fn annotated_headers_and_markers_share_the_budget() {
    let home = rust_home(4);
    let plain = run(&home, &["input/*.rs", "--mode", "types", "-n"]);
    assert!(tokens(stdout(&plain)) > 260);
    let output = run(
        &home,
        &["input/*.rs", "--mode", "types", "-n", "--tokens", "260"],
    );
    bounded(&output, 260);
    assert!(stdout(&output).contains("1\t"), "{}", stdout(&output));
    assert!(stdout(&output).contains("truncated"));
}

#[test]
fn one_match_glob_and_directory_bound_annotated_stdout() {
    let home = rust_home(1);
    let single = run(
        &home,
        &["input/f00.rs", "--mode", "types", "--tokens", "90", "-n"],
    );
    assert!(
        tokens(stdout(&single)) > 90,
        "the old single-file count excludes annotations"
    );
    for path in ["input/*.rs", "input"] {
        let output = run(&home, &[path, "--mode", "types", "--tokens", "90", "-n"]);
        bounded(&output, 90);
        notice(&output);
    }
}

#[test]
fn below_floor_is_exactly_the_trailer_and_succeeds() {
    let home = rust_home(3);
    let expected = trailer(0, 3);
    assert!(tokens(&expected) > 1);
    let output = run(&home, &["input/*.rs", "--tokens", "1"]);
    assert_eq!(stdout(&output), expected);
    assert!(notice(&output).contains("3 of 3 files are omitted"));
}

#[test]
fn invalid_budgets_are_refused_for_glob_and_list() {
    let home = rust_home(2);
    for paths in [vec!["input/*.rs"], vec!["input/f00.rs", "input/f01.rs"]] {
        for budget in ["0", "999999999999"] {
            let output = common::skim_sandboxed(home.path())
                .args(&paths)
                .args(["--tokens", budget])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
            assert!(output.stdout.is_empty());
            assert!(stderr(&output).contains("--tokens"), "{}", stderr(&output));
        }
    }
}

#[test]
fn json_glob_steps_from_full_to_structure_without_file_notices() {
    let value = "sample text ".repeat(16);
    let fields = (0..24)
        .map(|i| format!("\"field_{i}\":\"{value}\""))
        .collect::<Vec<_>>()
        .join(",");
    let long = format!("{{{fields}}}\n");
    let short = format!("{{\"note\":\"{}\"}}\n", "sample text ".repeat(96));
    let n = 256;
    // Pass 1 must step down for the long file and keep the short file in full.
    assert!(tokens(&long) > n);
    assert!(tokens(&short) <= n);
    let home = home(&[("a.json", long), ("b.json", short)]);
    let structure = run(&home, &["input/*.json", "--mode", "structure"]);
    bounded(&structure, n);

    let output = run(
        &home,
        &["input/*.json", "--mode", "full", "--tokens", &n.to_string()],
    );
    bounded(&output, n);
    assert_eq!(output.stdout, structure.stdout);
    let text = notice(&output);
    assert!(text.starts_with("[skim] structure view:"), "{text}");
    assert!(text.contains("stepped down from full"), "{text}");
}

#[test]
fn rust_glob_accepts_the_first_early_mode_that_fits() {
    let source = format!(
        "fn total() -> u32 {{\n    let mut total = 0;\n{}    total\n}}\n\n// {}\nfn other() {{}}\n",
        "    total += 1; total += 2;\n".repeat(64),
        "note ".repeat(64),
    );
    let home = home(&[("a.rs", source.clone()), ("b.rs", source)]);
    for (start, served) in [("full", "minimal"), ("minimal", "pseudo")] {
        let plain = run(&home, &["input/*.rs", "--mode", start]);
        let expected = run(&home, &["input/*.rs", "--mode", served]);
        let n = tokens(stdout(&expected));
        assert!(tokens(stdout(&plain)) > n, "{start} must exceed {n}");

        let output = run(
            &home,
            &["input/*.rs", "--mode", start, "--tokens", &n.to_string()],
        );
        bounded(&output, n);
        assert_eq!(output.stdout, expected.stdout);
        let text = notice(&output);
        assert!(
            text.starts_with(&format!("[skim] {served} view:")),
            "{text}"
        );
        assert!(
            text.contains(&format!("stepped down from {start}")),
            "{text}"
        );
    }
}

#[test]
fn whole_run_steps_to_the_independent_signatures_output() {
    let home = rust_home(2);
    let structure = run(&home, &["input/*.rs", "--mode", "structure"]);
    let signatures = run(&home, &["input/*.rs", "--mode", "signatures"]);
    let n = blocks(stdout(&structure))
        .iter()
        .map(|(p, b)| tokens(&format!("// {p}\n{b}\n")))
        .max()
        .unwrap()
        + 10;
    assert!(tokens(stdout(&structure)) > n);
    assert!(tokens(stdout(&signatures)) <= n);
    let output = run(&home, &["input/*.rs", "--tokens", &n.to_string()]);
    assert_eq!(output.stdout, signatures.stdout);
    assert!(notice(&output).starts_with("[skim] signatures view:"));
}

#[test]
fn empty_types_keep_signatures_and_jobs_keep_exact_bytes() {
    let home = python_home();
    let types = run(&home, &["input/*.py", "--mode", "types", "--no-header"]);
    assert!(stdout(&types).trim().is_empty());
    let signatures = run(&home, &["input/*.py", "--mode", "signatures"]);
    let pieces = blocks(stdout(&signatures));
    assert_eq!(pieces.len(), 12);
    let costs: Vec<_> = pieces
        .iter()
        .map(|(p, b)| tokens(&format!("// {p}\n{b}\n")))
        .collect();
    assert!(costs.iter().all(|cost| *cost <= 1000));
    assert!(
        costs.iter().sum::<usize>() > 2000,
        "the signatures rung must fail the 2N rule"
    );
    let one = run(&home, &["input/*.py", "--tokens", "1000", "--jobs", "1"]);
    let four = run(&home, &["input/*.py", "--tokens", "1000", "--jobs", "4"]);
    assert_eq!(one.stdout, four.stdout);
    assert_eq!(one.stderr, four.stderr);
    bounded(&one, 1000);
    assert!(stdout(&one).contains("def function_"));
    assert!(stdout(&one).contains("lines truncated"));
    let shown = blocks(stdout(&one));
    let paths: Vec<_> = shown.iter().map(|(p, _)| *p).collect();
    let mut sorted = paths.clone();
    sorted.sort();
    assert_eq!(paths, sorted);
    assert_eq!(
        notice(&one),
        "[skim] signatures view: all but signatures removed: 12/12 files; the --tokens 1000 budget covers the whole output of the run, so the view stepped down from structure; 12 files are cut, and the last line of each counts its hidden view lines; use a larger --tokens or fewer files for more, or SKIM_PASSTHROUGH=1 for full output\n"
    );
}

#[test]
fn types_start_cuts_whole_lines_with_exact_view_counts() {
    let home = rust_home(2);
    let plain = run(&home, &["input/*.rs", "--mode", "types"]);
    assert!(tokens(stdout(&plain)) > 400);
    let output = run(&home, &["input/*.rs", "--mode", "types", "--tokens", "400"]);
    bounded(&output, 400);
    let expected = blocks(stdout(&plain));
    let shown = blocks(stdout(&output));
    assert_eq!(shown.len(), 2);
    for (path, body) in shown {
        let original = expected.iter().find(|(p, _)| *p == path).unwrap().1;
        let lines: Vec<_> = body.lines().collect();
        let marker = lines.last().unwrap();
        assert!(marker.contains("lines truncated"), "{body}");
        assert_eq!(body.matches("lines truncated").count(), 1);
        let hidden: usize = marker
            .split('(')
            .nth(1)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let kept = lines.len() - 1;
        assert_eq!(hidden, original.lines().count() - kept, "{body}");
        assert_eq!(
            &lines[..kept],
            &original.lines().take(kept).collect::<Vec<_>>()
        );
    }
    assert!(notice(&output).starts_with("[skim] types view:"));
}

#[test]
fn omitted_suffix_has_one_exact_last_line_trailer() {
    let home = rust_home(12);
    let output = run(&home, &["input/*.rs", "--mode", "types", "--tokens", "180"]);
    bounded(&output, 180);
    let shown = blocks(stdout(&output)).len();
    assert!(shown > 0 && shown < 12, "{shown}: {}", stdout(&output));
    let expected = trailer(shown, 12);
    assert_eq!(stdout(&output).lines().last(), expected.lines().next());
    assert_eq!(stdout(&output).matches("files omitted (").count(), 1);
    assert!(notice(&output).contains(&format!("{} of 12 files are omitted", 12 - shown)));
}

#[test]
fn fake_headers_and_trailers_do_not_change_plan_counts() {
    let home = rust_home(12);
    let fake = format!("// fake.rs\n{}", trailer(0, 99));
    fs::write(home.path().join("input/a.unknown"), &fake).unwrap();
    let output = run(&home, &["input/*", "--mode", "types", "--tokens", "180"]);
    bounded(&output, 180);
    assert!(stdout(&output).contains(&fake));
    let shown = blocks(stdout(&output)).len();
    assert!(shown > 0 && shown < 13);
    assert_eq!(
        stdout(&output).lines().last(),
        trailer(shown, 13).lines().next()
    );
    assert!(notice(&output).contains(&format!("{} of 13 files are omitted", 13 - shown)));
}

#[test]
fn line_bounds_keep_each_pass_one_body_or_omit_it() {
    let home = rust_home(10);
    for flag in ["--max-lines", "--last-lines"] {
        let plain = run(&home, &["input/*.rs", "--mode", "types", flag, "3"]);
        assert!(tokens(stdout(&plain)) > 180);
        let output = run(
            &home,
            &[
                "input/*.rs",
                "--mode",
                "types",
                flag,
                "3",
                "--tokens",
                "180",
            ],
        );
        bounded(&output, 180);
        let shown = blocks(stdout(&output));
        assert!(!shown.is_empty());
        for (path, body) in shown {
            let body = body.split("// [skim]").next().unwrap().trim_end();
            let independent = run(
                &home,
                &[path, "--mode", "types", flag, "3", "--tokens", "180"],
            );
            assert_eq!(body, stdout(&independent).trim_end());
            assert!(body.matches("lines truncated").count() <= 1);
        }
        assert!(
            stdout(&output)
                .lines()
                .last()
                .unwrap()
                .contains("files omitted")
        );
    }
}

#[test]
fn unknown_extension_is_whole_or_omitted_never_recut() {
    let source = (0..100)
        .map(|i| format!("unknown value {i}\n"))
        .collect::<String>();
    let home = home(&[("a.unknown", source.clone()), ("b.rs", rust_source(1))]);
    for n in [150, 1000] {
        let budget = n.to_string();
        let single = run(
            &home,
            &["input/a.unknown", "--mode", "types", "--tokens", &budget],
        );
        assert_eq!(stdout(&single) == source, n == 1000);
        let output = run(&home, &["input/*", "--mode", "types", "--tokens", &budget]);
        bounded(&output, n);
        let shown = blocks(stdout(&output));
        if let Some((path, body)) = shown.first() {
            assert_eq!(*path, "input/a.unknown");
            let shown = body.split("// [skim]").next().unwrap().trim_end();
            assert_eq!(shown, stdout(&single).trim_end());
        } else {
            assert_eq!(n, 150);
            assert_eq!(stdout(&output), trailer(0, 2));
        }
    }
}

#[test]
fn notice_remedies_are_valid_multi_file_commands() {
    let home = rust_home(4);
    let cut = run(&home, &["input/*.rs", "--mode", "types", "--tokens", "180"]);
    notice(&cut);
    let larger = run(&home, &["input/*.rs", "--tokens", "10000"]);
    assert!(!stderr(&larger).contains("budget covers the whole output"));
    let fewer = run(&home, &["input/f00.rs", "input/f01.rs", "--tokens", "180"]);
    bounded(&fewer, 180);
    let raw = common::skim_sandboxed(home.path())
        .args(["input/f00.rs", "input/f01.rs", "--tokens", "180"])
        .env("SKIM_PASSTHROUGH", "1")
        .assert()
        .success()
        .get_output()
        .clone();
    assert_eq!(stdout(&raw), rust_source(0) + &rust_source(1));
    assert!(raw.stderr.is_empty());
}

#[test]
fn cut_analytics_rows_stay_unmeasured_and_omit_unshown_files() {
    let home = rust_home(12);
    let output = common::skim_sandboxed(home.path())
        .args(["input/*.rs", "--tokens", "180"])
        .env("SKIM_DISABLE_ANALYTICS", "0")
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(notice(&output).contains("stepped down from structure"));
    let shown = blocks(stdout(&output)).len();
    assert!(shown > 0 && shown < 12);
    let db = rusqlite::Connection::open(home.path().join(".cache/skim/analytics.db")).unwrap();
    let rows: Vec<(String, Option<i64>, Option<i64>, String)> = db
        .prepare("SELECT original_cmd, notice_tokens, notice_bytes, mode FROM token_savings")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), shown);
    for (cmd, notice_tokens, notice_bytes, mode) in rows {
        assert!(
            blocks(stdout(&output))
                .iter()
                .any(|(p, _)| cmd == format!("skim {p}")),
            "{cmd}"
        );
        assert_eq!((notice_tokens, notice_bytes), (None, None));
        assert_eq!(mode, "structure");
    }
}

#[cfg(unix)]
fn run_with_closed_stderr(home: &TempDir, args: &[&str]) -> Output {
    let sandbox = common::skim_sandboxed(home.path());
    let mut cmd = std::process::Command::new(sandbox.get_program());
    cmd.current_dir(sandbox.get_current_dir().unwrap());
    for (key, value) in sandbox.get_envs() {
        if let Some(value) = value {
            cmd.env(key, value);
        } else {
            cmd.env_remove(key);
        }
    }
    // Close the read end before exec, so the child cannot finish before closure.
    let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
    drop(reader);
    cmd.args(args)
        .arg("--no-cache")
        .stderr(std::process::Stdio::from(std::os::fd::OwnedFd::from(
            writer,
        )))
        .output()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn closed_stderr_does_not_panic_on_the_run_notice() {
    let output = run_with_closed_stderr(
        &rust_home(4),
        &["input/*.rs", "--mode", "types", "--tokens", "180"],
    );
    assert!(output.status.success(), "{:?}", output.status);
    bounded(&output, 180);
    assert!(!output.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn closed_stderr_keeps_complete_stdout_on_a_fitting_run() {
    let home = golden_home(false);
    let args = ["input/*.rs", "--tokens", "1000"];
    let baseline = run(&home, &args);
    let golden = include_str!("fixtures/tokens_run_budget/fit.stdout");
    assert_eq!(stdout(&baseline), golden);
    assert!(!stderr(&baseline).contains("budget covers the whole output"));
    assert!(stderr(&baseline).contains("SKIM_PASSTHROUGH=1"));

    let output = run_with_closed_stderr(&home, &args);
    assert_eq!(stdout(&output), golden);
    assert!(output.status.success(), "{:?}", output.status);
}

#[cfg(unix)]
#[test]
fn closed_stderr_keeps_complete_stdout_if_one_file_fails() {
    let home = golden_home(false);
    fs::write(home.path().join("input/z.rs"), [0xff]).unwrap();
    let args = ["input/*.rs", "--tokens", "1000"];
    let baseline = run(&home, &args);
    assert!(stderr(&baseline).contains("Error processing input/z.rs:"));
    assert!(stderr(&baseline).contains("2 file(s) successfully, 1 failed"));

    let output = run_with_closed_stderr(&home, &args);
    assert_eq!(
        stdout(&output),
        include_str!("fixtures/tokens_run_budget/fit.stdout")
    );
    assert!(output.status.success(), "{:?}", output.status);
}

/// Both stdout fixtures come from `skim 3.0.0 (f370ffc5)`, not the current build.
/// Capture with these relative paths, `--tokens 1000` or `250`, and `--no-cache`.
/// The escalated fixture keeps the base stdout but suppresses its per-file stderr.
fn golden_home(stepped: bool) -> TempDir {
    home(&[
        (
            "a.rs",
            if stepped {
                rust_source(0)
            } else {
                "pub fn alpha() -> i32 {\n    1\n}\n".into()
            },
        ),
        ("b.rs", "pub fn beta() -> i32 {\n    2\n}\n".into()),
    ])
}

fn assert_golden(stepped: bool, golden: &str, n: usize) {
    let home = golden_home(stepped);
    let plain_a = run(&home, &["input/a.rs", "--mode", "structure"]);
    assert_eq!(
        tokens(stdout(&plain_a)) > n,
        stepped,
        "pass-1 escalation precondition"
    );
    for path in ["input/a.rs", "input/b.rs"] {
        let single = run(&home, &[path, "--tokens", &n.to_string()]);
        assert!(tokens(stdout(&single)) <= n);
    }
    assert_eq!(blocks(golden).len(), 2);
    for (path, body) in blocks(golden) {
        assert!(tokens(&format!("// {path}\n{body}\n")) <= n);
    }
    assert!(tokens(golden) <= n);
    let output = run(
        &home,
        &["input/a.rs", "input/b.rs", "--tokens", &n.to_string()],
    );
    assert_eq!(stdout(&output), golden);
    assert!(!stderr(&output).contains("escalated"));
    assert!(!stderr(&output).contains("budget covers the whole output"));
    assert!(stderr(&output).contains("SKIM_PASSTHROUGH=1"));
}

#[test]
fn fitting_run_equals_base_golden() {
    assert_golden(
        false,
        include_str!("fixtures/tokens_run_budget/fit.stdout"),
        1000,
    );
}

#[test]
fn fitting_pass_one_escalation_equals_base_golden() {
    assert_golden(
        true,
        include_str!("fixtures/tokens_run_budget/pass_one.stdout"),
        250,
    );
}

#[test]
fn tiny_one_match_glob_keeps_the_remedy_cold_and_warm() {
    for budget in ["5", "20"] {
        let home = rust_home(1);
        let read = || {
            common::skim_sandboxed(home.path())
                .args(["input/*.rs", "--mode", "types", "--tokens", budget])
                .assert()
                .success()
                .get_output()
                .clone()
        };
        let cold = read();
        let warm = read();
        assert_eq!(cold.stdout, warm.stdout);
        assert_eq!(cold.stderr, warm.stderr);
        if budget == "5" {
            assert_eq!(stdout(&cold), trailer(0, 1));
            notice(&cold);
        } else {
            bounded(&cold, 20);
            assert!(stdout(&cold).contains("truncated"));
            assert!(!stdout(&cold).contains("SKIM_PASSTHROUGH=1"));
            assert_eq!(stderr(&cold).lines().count(), 1);
            assert!(stderr(&cold).contains("SKIM_PASSTHROUGH=1 for full output"));
        }
    }
}

#[test]
fn stdin_keeps_its_escalation_and_compact_marker_remedy() {
    let home = rust_home(1);
    let escalated = common::skim_sandboxed(home.path())
        .args(["-", "--language", "rust", "--tokens", "250"])
        .write_stdin(rust_source(0))
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(stderr(&escalated).contains("escalated"));
    assert!(stdout(&escalated).contains("calculate"));
    assert!(!stderr(&escalated).contains("whole output of the run"));
    let output = common::skim_sandboxed(home.path())
        .args(["-", "--language", "rust", "--tokens", "5"])
        .write_stdin(rust_source(0))
        .assert()
        .success()
        .get_output()
        .clone();
    assert!(stdout(&output).contains("truncated"));
    assert!(stderr(&output).contains("all modes exceeded"));
    assert!(stderr(&output).contains("output truncated to the --tokens budget"));
    assert!(stderr(&output).contains("SKIM_PASSTHROUGH=1"));
    assert!(!stderr(&output).contains("whole output of the run"));
}

#[test]
fn errors_count_only_successes_and_all_errors_fail() {
    let home = rust_home(12);
    fs::write(home.path().join("input/bad.rs"), [0xff, 0xfe]).unwrap();
    let output = run(&home, &["input/*.rs", "--mode", "types", "--tokens", "180"]);
    bounded(&output, 180);
    assert!(stderr(&output).contains("Error processing input/bad.rs:"));
    assert!(stderr(&output).contains("12 file(s) successfully, 1 failed"));
    assert!(stderr(&output).contains("of 12 files are omitted"));
    assert!(!stderr(&output).contains("of 13 files are omitted"));
    let all = common::skim_sandboxed(home.path())
        .args(["input/bad.*", "--tokens", "180"])
        .output()
        .unwrap();
    assert!(!all.status.success());
    assert!(stderr(&all).contains("All 1 file(s) failed to process"));
}

#[test]
fn stats_count_only_final_shown_bodies() {
    let home = rust_home(12);
    let plain = run(&home, &["input/*.rs", "--mode", "types"]);
    assert!(tokens(stdout(&plain)) > 180);
    let output = run(
        &home,
        &[
            "input/*.rs",
            "--mode",
            "types",
            "--tokens",
            "180",
            "--show-stats",
        ],
    );
    bounded(&output, 180);
    let text = stdout(&output);
    let body_end = text.rfind("// [skim]").unwrap();
    let shown = blocks(&text[..body_end]);
    assert!(!shown.is_empty());
    let original: usize = shown
        .iter()
        .map(|(p, _)| tokens(&fs::read_to_string(home.path().join(p)).unwrap()))
        .sum();
    let transformed: usize = shown.iter().map(|(_, b)| tokens(&format!("{b}\n"))).sum();
    let stats = stderr(&output)
        .lines()
        .find(|l| l.contains("tokens →"))
        .unwrap()
        .replace(',', "");
    assert!(
        stats.contains(&format!("{original} tokens → {transformed} tokens")),
        "{stats}"
    );
    assert!(
        stats.ends_with(&format!("across {} file(s)", shown.len())),
        "{stats}"
    );
    assert_eq!(
        stderr(&output).matches("whole output of the run").count(),
        1
    );
}

#[test]
fn serde_files_use_their_own_cut_marker_syntax() {
    let json = format!(
        "{{{}}}\n",
        (0..100)
            .map(|i| format!("\"field_{i}\": \"value_{i}\""))
            .collect::<Vec<_>>()
            .join(",")
    );
    let yaml = (0..100)
        .map(|i| format!("field_{i}: value_{i}\n"))
        .collect::<String>();
    let home = home(&[("a.json", json), ("b.yaml", yaml)]);
    let plain = run(&home, &["input/*", "--mode", "structure"]);
    assert!(tokens(stdout(&plain)) > 300);
    let output = run(&home, &["input/*", "--tokens", "300"]);
    bounded(&output, 300);
    let shown = blocks(stdout(&output));
    assert_eq!(shown.len(), 2);
    assert!(
        shown[0].1.lines().last().unwrap().starts_with("// ..."),
        "{}",
        shown[0].1
    );
    assert!(
        shown[1].1.lines().last().unwrap().starts_with("# ..."),
        "{}",
        shown[1].1
    );
    assert!(notice(&output).contains("2 files are cut"));
}

#[test]
fn no_header_keeps_the_omission_trailer_last() {
    let home = rust_home(20);
    let output = run(
        &home,
        &[
            "input/*.rs",
            "--mode",
            "types",
            "--tokens",
            "70",
            "--no-header",
        ],
    );
    bounded(&output, 70);
    assert!(!stdout(&output).contains("// input/"));
    assert!(
        stdout(&output)
            .lines()
            .last()
            .unwrap()
            .starts_with("// [skim]")
    );
    assert!(
        stdout(&output)
            .lines()
            .last()
            .unwrap()
            .contains("files omitted")
    );
    notice(&output);
}

#[test]
fn partly_empty_signatures_keep_the_const_body_and_fit() {
    let home = rust_home(2);
    fs::write(
        home.path().join("input/z.rs"),
        format!(
            "{}pub const LIMIT: usize = 17;\n",
            "// fixture detail\n".repeat(100)
        ),
    )
    .unwrap();
    let empty = run(&home, &["input/z.rs", "--mode", "signatures"]);
    assert!(stdout(&empty).trim().is_empty());
    let pass_one = run(&home, &["input/z.rs", "--mode", "structure"]);
    let signatures = run(
        &home,
        &["input/f00.rs", "input/f01.rs", "--mode", "signatures"],
    );
    let expected = format!(
        "{}\n// input/z.rs\n{}",
        stdout(&signatures),
        stdout(&pass_one)
    );
    let structure = run(&home, &["input/*.rs", "--mode", "structure"]);
    let n = blocks(stdout(&structure))
        .iter()
        .map(|(p, b)| tokens(&format!("// {p}\n{b}\n")))
        .max()
        .unwrap()
        + 10;
    assert!(tokens(&expected) <= n);
    assert!(tokens(stdout(&structure)) > n);
    let output = run(&home, &["input/*.rs", "--tokens", &n.to_string()]);
    assert_eq!(stdout(&output), expected);
    assert!(notice(&output).starts_with("[skim] signatures view:"));
}

#[test]
fn partly_empty_cut_keeps_the_exact_pass_one_body_or_omits_it() {
    let home = rust_home(3);
    let constants: String = (0..100)
        .map(|i| format!("pub const LIMIT_{i}: usize = {i};\n"))
        .collect();
    fs::write(home.path().join("input/z.rs"), constants).unwrap();

    for n in [300, 500] {
        let budget = n.to_string();
        let pass_one = run(&home, &["input/z.rs", "--tokens", &budget]);
        let pass_one = stdout(&pass_one);
        assert!(pass_one.contains("lines truncated"));
        let output = run(&home, &["input/*.rs", "--tokens", &budget]);
        bounded(&output, n);
        let shown = blocks(stdout(&output));
        if let Some((_, body)) = stdout(&output).split_once("// input/z.rs\n") {
            assert_eq!(body, pass_one, "changed pass-1 body at {n} tokens");
        } else {
            assert!(stdout(&output).ends_with(&trailer(shown.len(), 4)));
        }

        // Only the unchanged z.rs body can carry a pass-1 marker.
        let cut = shown
            .iter()
            .filter(|(path, body)| {
                body.contains("lines truncated")
                    && (*path, *body) != ("input/z.rs", pass_one.trim_end_matches('\n'))
            })
            .count();
        assert!(cut > 0, "expected run cuts at {n} tokens");
        let report = notice(&output);
        assert!(report.starts_with("[skim] types view:"), "{report}");
        assert!(
            report.contains(&format!("; {cut} files are cut,")),
            "{report}"
        );
    }
}
