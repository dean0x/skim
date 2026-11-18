//! Default multi-file bounds and exact unchanged streams from a8293f35.

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
        run(self
            .command()
            .arg("--no-cache")
            .args(args)
            .args(&self.paths))
    }

    fn command(&self) -> assert_cmd::Command {
        common::skim_sandboxed(self.home.path())
    }

    fn add(&mut self, path: String, source: &str) {
        let file = self.home.path().join(&path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, source).unwrap();
        self.paths.push(path);
    }
}

fn run(command: &mut assert_cmd::Command) -> Output {
    command.assert().success().get_output().clone()
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).unwrap()
}

fn stderr(output: &Output) -> &str {
    std::str::from_utf8(&output.stderr).unwrap()
}

fn total_chars(output: &Output) -> usize {
    stdout(output).chars().count() + stderr(output).chars().count()
}

const RUN_NOTICE: &str = "the 29000-character output limit covers the whole output of the run";

fn bounded_notice(output: &Output) -> &str {
    let notice: Vec<_> = stderr(output)
        .split_inclusive('\n')
        .filter(|line| line.starts_with("[skim] ") && !line.contains("tokens →"))
        .collect();
    assert_eq!(notice.len(), 1, "{}", stderr(output));
    assert!(notice[0].contains(RUN_NOTICE), "{}", notice[0]);
    assert!(stdout(output).chars().count() + notice[0].chars().count() < LIMIT);
    notice[0]
}

fn assert_same_streams(actual: &Output, expected: &Output) {
    assert_eq!(stdout(actual), stdout(expected));
    assert_eq!(stderr(actual), stderr(expected));
}

fn trailer(shown: usize, total: usize) -> String {
    format!(
        "// [skim] {} files omitted ({shown} of {total} shown) — SKIM_PASSTHROUGH=1 for full output\n",
        total - shown
    )
}

// Fixture bodies do not contain path headers. The blank line before the next
// header belongs to the join, not to either body.
fn blocks(text: &str) -> Vec<(&str, &str)> {
    let text = text
        .rsplit_once("\n// [skim] ")
        .filter(|(_, marker)| marker.contains("files omitted ("))
        .map_or(text, |(bodies, _)| &text[..bodies.len() + 1]);
    let headers: Vec<_> = text
        .match_indices("// ")
        .filter(|(offset, _)| *offset == 0 || text.as_bytes()[offset - 1] == b'\n')
        .filter(|(offset, _)| text[*offset..].lines().next().unwrap().ends_with(".rs"))
        .map(|(offset, _)| offset)
        .collect();
    headers
        .iter()
        .enumerate()
        .map(|(i, &start)| {
            let end = headers.get(i + 1).map_or(text.len(), |offset| offset - 1);
            text[start + 3..end].split_once('\n').unwrap()
        })
        .collect()
}

fn deep_dir(index: usize) -> String {
    format!("z{index:03}{}", "d".repeat(60)) + &format!("/{}", "d".repeat(64)).repeat(12)
}

fn cut_fixture() -> Fixture {
    let mut fixture = Fixture::new(0, 0);
    let source: String = (0..40)
        .map(|i| {
            format!("pub struct Record{i} {{\n    pub value: i32,\n    pub label: String,\n}}\n")
        })
        .collect();
    for i in 0..40 {
        let dir = deep_dir(i);
        fixture.add(format!("{dir}/file_{i:03}.rs"), &source);
    }
    fixture
}

fn assert_omissions(fixture: &Fixture, output: &Output, shown: usize) {
    let total = fixture.paths.len();
    assert!((1..total).contains(&shown), "{shown} shown out of {total}");
    assert!(stdout(output).ends_with(&trailer(shown, total)));
    let notice = bounded_notice(output);
    assert!(
        notice.contains(&format!(
            "{} of {total} files are omitted after {}, first omitted {},",
            total - shown,
            fixture.paths[shown - 1],
            fixture.paths[shown]
        )),
        "{notice}"
    );
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

#[test]
fn default_limits_directory_glob_and_explicit_list() {
    let fixture = Fixture::new(3, 250);
    assert!(total_chars(&fixture.run(&["--mode=structure"])) > LIMIT);
    let explicit = fixture.run(&[]);
    assert!(!stdout(&explicit).is_empty());
    bounded_notice(&explicit);
    for input in [".", "*.rs"] {
        let batch = run(fixture.command().arg("--no-cache").arg(input));
        bounded_notice(&batch);
        assert_eq!(
            stdout(&batch)
                .lines()
                .filter(|l| l.contains("pub fn"))
                .count(),
            stdout(&explicit)
                .lines()
                .filter(|l| l.contains("pub fn"))
                .count()
        );
    }
}

#[test]
fn one_match_and_deduped_inputs_use_the_run_bound() {
    let fixture = Fixture::new(1, 800);
    let typed = run(fixture.command().args(["--mode=structure", "*.rs"]));
    assert!(total_chars(&typed) > LIMIT);
    let list = run(fixture.command().args(["file_000.rs", "file_000.rs"]));
    assert!(stdout(&list).contains("pub fn value_000_000"));
    assert!(!bounded_notice(&list).contains("fewer files"));
    for input in [".", "*.rs"] {
        assert_same_streams(&run(fixture.command().arg(input)), &list);
    }
}

fn pad_to(fixture: &mut Fixture, target: usize, ch: char) {
    let path = "zz_pad.txt";
    fixture.add(path.into(), "\n");
    let base = total_chars(&fixture.run(&["--mode=structure"]));
    let body = ch.to_string().repeat(target.checked_sub(base).unwrap()) + "\n";
    fs::write(fixture.home.path().join(path), body).unwrap();
}

#[test]
fn exact_character_boundary_keeps_limit_minus_one_but_not_limit() {
    for ch in ['x', 'é'] {
        for target in [LIMIT - 1, LIMIT] {
            let mut fixture = Fixture::new(2, 10);
            pad_to(&mut fixture, target, ch);
            let typed = fixture.run(&["--mode=structure"]);
            assert_eq!(total_chars(&typed), target);
            let default = fixture.run(&[]);
            if target < LIMIT {
                assert_same_streams(&default, &typed);
            } else {
                assert_ne!(stdout(&default), stdout(&typed));
                assert!(stdout(&default).contains("pub fn value_000_000"));
                assert!(bounded_notice(&default).contains("stepped down from structure"));
            }
        }
    }
}

#[test]
fn multibyte_output_below_the_character_limit_keeps_its_bytes() {
    let mut fixture = Fixture::new(2, 10);
    pad_to(&mut fixture, 20_000, 'é');
    let typed = fixture.run(&["--mode=structure"]);
    assert!(typed.stdout.len() >= LIMIT);
    assert_eq!(total_chars(&typed), 20_000);
    assert_same_streams(&fixture.run(&[]), &typed);
}

fn assert_exact_cut(body: &str, full: &str) {
    let mut lines: Vec<_> = body.lines().collect();
    let marker = lines.pop().unwrap();
    let hidden = full.lines().count() - lines.len();
    assert!(hidden > 0);
    assert!(
        marker.contains(&format!("({hidden} lines truncated)")),
        "{marker}"
    );
    assert!(
        lines
            .into_iter()
            .eq(full.lines().take(full.lines().count() - hidden))
    );
}

#[test]
fn types_cut_counts_hidden_lines_and_omits_a_deep_path_suffix() {
    let fixture = cut_fixture();
    let output = fixture.run(&[]);
    let shown = blocks(stdout(&output));
    assert_omissions(&fixture, &output, shown.len());
    assert!(
        shown.iter().map(|(p, _)| *p).eq(fixture
            .paths
            .iter()
            .take(shown.len())
            .map(String::as_str))
    );
    let notice = bounded_notice(&output);
    assert!(
        notice.starts_with("[skim] types view:") && notice.contains("stepped down from structure")
    );
    let types = fixture.run(&["--mode=types"]);
    let full = blocks(stdout(&types));
    let mut cut = 0;
    for ((_, body), (_, full_body)) in shown.iter().zip(full) {
        if body != &full_body {
            assert_exact_cut(body, full_body);
            cut += 1;
        }
    }
    assert!(cut > 0);
    assert!(notice.contains(&format!("{cut} files are cut")), "{notice}");
}

#[test]
fn each_notice_remedy_exposes_more_content() {
    let fixture = cut_fixture();
    let default = fixture.run(&[]);
    let notice = bounded_notice(&default);
    assert!(notice.ends_with("use --tokens N, a typed --mode or fewer files for more, or SKIM_PASSTHROUGH=1 for full output\n"));
    assert!(!notice.contains("--symbol") && !notice.contains("--max-lines"));
    let fewer = run(fixture.command().args(&fixture.paths[..2]));
    let raw = run(fixture
        .command()
        .env("SKIM_PASSTHROUGH", "1")
        .args(&fixture.paths));
    for output in [
        fixture.run(&["--tokens", "20000"]),
        fixture.run(&["--mode=structure"]),
        fewer,
        raw,
    ] {
        assert!(
            stdout(&output).matches("pub struct").count()
                > stdout(&default).matches("pub struct").count()
        );
    }
}

#[test]
fn unknown_first_file_gets_a_raw_cut_and_the_next_file_shows() {
    let mut fixture = Fixture::new(1, 2);
    let raw = "unknown-language content\n".repeat(2_000);
    fixture.add("a.unknown".into(), &raw);
    let output = fixture.run(&[]);
    let notice = bounded_notice(&output);
    assert!(
        notice.contains("1 file is cut") && !notice.contains("omitted"),
        "{notice}"
    );
    let text = stdout(&output).strip_prefix("// a.unknown\n").unwrap();
    let (body, next) = text.split_once("\n// file_000.rs\n").unwrap();
    assert!(body.lines().last().unwrap().starts_with("# "));
    assert_exact_cut(body, &raw);
    assert!(next.contains("pub fn value_000_000"));
}

#[test]
fn empty_types_view_uses_a_cut_of_structure_and_keeps_the_next_file() {
    let mut fixture = Fixture::new(1, 850);
    fixture.add(
        "zz_type.rs".into(),
        "pub struct Anchor { pub value: i32 }\n",
    );
    let structure = fixture.run(&["--mode=structure"]);
    let full = blocks(stdout(&structure));
    assert!(full[0].1.chars().count() > LIMIT);
    let types = fixture.run(&["--mode=types"]);
    assert!(blocks(stdout(&types))[0].1.trim().is_empty());
    let output = fixture.run(&[]);
    let notice = bounded_notice(&output);
    assert!(notice.starts_with("[skim] types view:") && notice.contains("1 file is cut"));
    let shown = blocks(stdout(&output));
    assert_eq!(shown.len(), 2);
    assert_eq!(shown[0].0, "file_000.rs");
    assert_exact_cut(shown[0].1, full[0].1);
    assert_eq!(shown[1], ("zz_type.rs", blocks(stdout(&types))[1].1));
}

#[test]
fn file_errors_and_missing_paths_do_not_change_the_run_counts() {
    let fixture = cut_fixture();
    let baseline = fixture.run(&[]);
    fs::write(fixture.home.path().join("zz_bad.rs"), [0xff, 0xfe]).unwrap();
    let output = run(fixture
        .command()
        .args(&fixture.paths)
        .args(["zz_bad.rs", "missing.rs"]));
    let err = stderr(&output);
    assert!(
        err.lines()
            .any(|line| line.starts_with("Warning:") && line.contains("missing.rs"))
    );
    assert!(err.contains("Error processing zz_bad.rs:"));
    assert!(err.contains("Processed 40 file(s) successfully, 1 failed"));
    assert_eq!(stdout(&output), stdout(&baseline));
    assert_eq!(bounded_notice(&output), bounded_notice(&baseline));

    let failed = fixture
        .command()
        .args(["zz_bad.rs", "missing.rs"])
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(1));
    assert!(failed.stdout.is_empty());
    let err = stderr(&failed);
    assert!(err.contains("All 1 file(s) failed to process"), "{err}");
    assert!(!err.contains(RUN_NOTICE), "{err}");
}

#[test]
fn jobs_and_warm_cache_keep_the_run_bytes_without_caching_cuts() {
    let fixture = cut_fixture();
    let cold = run(fixture.command().args(["--jobs", "1"]).args(&fixture.paths));
    assert!(bounded_notice(&cold).contains("files are cut"));
    for args in [
        &["--jobs", "8"][..],
        &["--jobs", "8", "--no-cache"],
        &["--jobs", "1"],
    ] {
        assert_same_streams(
            &run(fixture.command().args(args).args(&fixture.paths)),
            &cold,
        );
    }
    for mode in ["--mode=structure", "--mode=signatures", "--mode=types"] {
        let cached = run(fixture.command().arg(mode).args(&fixture.paths));
        assert_same_streams(&cached, &fixture.run(&[mode]));
        assert!(!stdout(&cached).contains("lines truncated"));
    }
}

#[test]
fn numbered_output_counts_and_headerless_output_ends_in_the_trailer() {
    let mut fixture = Fixture::new(2, 15);
    pad_to(&mut fixture, LIMIT - 1, 'x');
    let unnumbered = fixture.run(&[]);
    assert_eq!(total_chars(&unnumbered), LIMIT - 1);
    assert!(total_chars(&fixture.run(&["--mode=structure", "-n"])) >= LIMIT);
    let numbered = fixture.run(&["-n"]);
    bounded_notice(&numbered);
    assert!(
        stdout(&numbered)
            .lines()
            .any(|l| l.contains("pub fn") && l.trim_start().starts_with('1'))
    );

    // Even one raw cut marker per file exceeds the limit, without headers.
    let mut raw = Fixture::new(0, 0);
    for i in 0..450 {
        raw.add(
            format!("raw_{i:03}.txt"),
            &"one raw line of content\n".repeat(10),
        );
    }
    let output = raw.run(&["--no-header"]);
    let text = stdout(&output);
    assert!(!text.contains("// raw_"));
    assert_omissions(&raw, &output, text.matches("lines truncated").count());
}

fn tokens(text: &str) -> usize {
    static COUNTER: std::sync::LazyLock<rskim_tokens::Counter> = std::sync::LazyLock::new(|| {
        rskim_tokens::Counter::new(rskim_tokens::Encoding::Cl100k).unwrap()
    });
    COUNTER.count(text)
}

#[test]
fn stats_count_only_the_shown_bodies() {
    let fixture = cut_fixture();
    let baseline = fixture.run(&[]);
    let output = fixture.run(&["--show-stats"]);
    assert_eq!(stdout(&output), stdout(&baseline));
    assert_eq!(bounded_notice(&output), bounded_notice(&baseline));
    let shown = blocks(stdout(&output));
    let raw: usize = shown
        .iter()
        .map(|(path, _)| tokens(&fs::read_to_string(fixture.home.path().join(path)).unwrap()))
        .sum();
    let served: usize = shown.iter().map(|(_, body)| tokens(body)).sum();
    let stats = stderr(&output)
        .lines()
        .find(|line| line.contains("tokens →"))
        .unwrap()
        .replace(',', "");
    assert!(
        stats.contains(&format!("{raw} tokens → {served} tokens")),
        "{stats}"
    );
    assert!(
        stats.ends_with(&format!("across {} file(s)", shown.len())),
        "{stats}"
    );
}

#[test]
fn stats_report_zero_tokens_when_only_the_headers_exceed_the_limit() {
    let mut fixture = Fixture::new(0, 0);
    for i in 0..40 {
        fixture.add(format!("{}/empty.rs", deep_dir(i)), "");
    }
    let output = fixture.run(&["--show-stats"]);
    let shown = stdout(&output).matches("/empty.rs\n").count();
    assert_omissions(&fixture, &output, shown);
    assert!(stderr(&output).contains(&format!(
        "0 tokens → 0 tokens (0.0% reduction) across {shown} file(s)"
    )));
}

#[test]
fn analytics_store_only_shown_files_with_unmeasured_notices() {
    let fixture = cut_fixture();
    let output = run(fixture
        .command()
        .env("SKIM_DISABLE_ANALYTICS", "0")
        .args(&fixture.paths));
    let shown = blocks(stdout(&output));
    assert_omissions(&fixture, &output, shown.len());
    let db =
        rusqlite::Connection::open(fixture.home.path().join(".cache/skim/analytics.db")).unwrap();
    let mut query = db.prepare("SELECT original_cmd, raw_tokens, compressed_tokens, notice_tokens, notice_bytes FROM token_savings ORDER BY original_cmd").unwrap();
    let mut rows = query.query([]).unwrap();
    for (path, body) in shown {
        let row = rows.next().unwrap().expect("one row for each shown file");
        let cmd = row.get::<_, String>(0).unwrap();
        assert!(cmd.starts_with(&format!("skim {}/", path.split_once('/').unwrap().0)));
        assert!(format!("skim {path}").starts_with(&cmd));
        let source = fs::read_to_string(fixture.home.path().join(path)).unwrap();
        assert_eq!(row.get::<_, usize>(1).unwrap(), tokens(&source));
        assert_eq!(row.get::<_, usize>(2).unwrap(), tokens(body));
        assert_eq!(row.get::<_, Option<i64>>(3).unwrap(), None);
        assert_eq!(row.get::<_, Option<i64>>(4).unwrap(), None);
    }
    assert!(rows.next().unwrap().is_none());
}

#[test]
fn deep_paths_do_not_charge_cut_clauses_to_the_signatures_rung() {
    let mut fixture = Fixture::new(3, 217);
    let dir = deep_dir(0);
    for i in 1..3 {
        let path = format!("{dir}/{}", fixture.paths[i]);
        fs::create_dir_all(fixture.home.path().join(&dir)).unwrap();
        fs::rename(
            fixture.home.path().join(&fixture.paths[i]),
            fixture.home.path().join(&path),
        )
        .unwrap();
        fixture.paths[i] = path;
    }
    let signatures = fixture.run(&["--mode=signatures"]);
    assert!(total_chars(&fixture.run(&["--mode=structure"])) >= LIMIT);
    assert!(stdout(&signatures).chars().count() + 2 * dir.len() > LIMIT);
    let output = fixture.run(&[]);
    let notice = bounded_notice(&output);
    assert!(
        notice.starts_with("[skim] signatures view:")
            && notice.contains("stepped down from structure"),
        "{notice}"
    );
    assert!(!notice.contains("omitted"));
    assert_eq!(stdout(&output), stdout(&signatures));
}

#[test]
fn multi_file_hook_rewrites_keep_their_typed_views_and_limits() {
    let fixture = Fixture::new(3, 250);
    for (tool, flags) in [
        ("cat", &["--mode=pseudo", "--no-header"][..]),
        ("head -400", &["--mode=full", "--max-lines", "400"]),
        ("tail -400", &["--mode=full", "--last-lines", "400"]),
    ] {
        let command = format!("{tool} {}", fixture.paths.join(" "));
        let rewrite = run(fixture.command().arg("rewrite").arg(command));
        let rewritten = stdout(&rewrite).trim();
        let (origin, args) = rewritten
            .strip_prefix("SKIM_REWRITTEN_FROM=")
            .and_then(|text| text.split_once(" skim "))
            .unwrap_or_else(|| panic!("not a skim rewrite: {rewritten:?}"));
        assert_eq!(origin, tool.split_whitespace().next().unwrap());
        let output = run(fixture
            .command()
            .arg("--no-cache")
            .env("SKIM_REWRITTEN_FROM", origin)
            .args(args.split_whitespace()));
        let typed = run(fixture
            .command()
            .arg("--no-cache")
            .env("SKIM_REWRITTEN_FROM", origin)
            .args(flags)
            .args(&fixture.paths));
        assert!(stdout(&output).chars().count() > LIMIT);
        assert_same_streams(&output, &typed);
    }
}
