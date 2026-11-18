//! Default single-input views stay below the character limit, notice included.
use assert_cmd::Command;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

const LIMIT: usize = 29_000;

fn skim_cmd(cache: &Path) -> Command {
    Command::from_std(skim_std_cmd(cache))
}

fn skim_std_cmd(cache: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(assert_cmd::cargo::cargo_bin!("skim"));
    cmd.env_remove("SKIM_PASSTHROUGH")
        .env_remove("SKIM_REWRITTEN_FROM")
        .env_remove("SKIM_DEBUG")
        .env("SKIM_CACHE_DIR", cache)
        .env("SKIM_DISABLE_ANALYTICS", "1");
    cmd
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Streams {
    out: String,
    err: String,
}

fn run(cache: &Path, file: &Path, args: &[&str]) -> Streams {
    let output = skim_cmd(cache).arg(file).args(args).output().unwrap();
    assert!(output.status.success(), "{output:?}");
    Streams {
        out: String::from_utf8(output.stdout).unwrap(),
        err: String::from_utf8(output.stderr).unwrap(),
    }
}

fn stdin(cache: &Path, source: &str, args: &[&str]) -> Streams {
    let output = skim_cmd(cache)
        .arg("-")
        .args(args)
        .write_stdin(source)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    Streams {
        out: String::from_utf8(output.stdout).unwrap(),
        err: String::from_utf8(output.stderr).unwrap(),
    }
}

fn bounded(view: &Streams) {
    assert!(
        view.out.chars().count() + view.err.chars().count() < LIMIT,
        "stdout: {} characters; stderr: {}",
        view.out.chars().count(),
        view.err
    );
    assert_eq!(view.err.lines().count(), 1, "{}", view.err);
    assert!(
        view.err.contains("29000-character output limit"),
        "{}",
        view.err
    );
    assert!(!view.err.contains("token"), "{}", view.err);
    assert!(!view.err.contains(['\u{2013}', '\u{2014}']));
    assert!(!view.err.contains(" - "));
}

fn functions(count: usize, docs: bool) -> String {
    let mut source = String::from("pub struct Small { pub value: usize }\n");
    for i in 0..count {
        if docs {
            source.push_str(&format!(
                "/// Documentation_{i}: {}\n",
                "descriptive words ".repeat(12)
            ));
        }
        source.push_str(&format!("pub fn function_{i}(first_argument_with_long_name: usize, second_argument_with_long_name: usize) -> usize {{\n    let body = first_argument_with_long_name + second_argument_with_long_name;\n    body + 123456789\n}}\n"));
    }
    source
}

fn fixture(name: &str, source: &str) -> (TempDir, std::path::PathBuf) {
    let home = TempDir::new().unwrap();
    let file = home.path().join(name);
    fs::write(&file, source).unwrap();
    (home, file)
}

fn over_structure(home: &Path, file: &Path, source: &str, args: &[&str]) -> Streams {
    let mut flags = vec!["--mode=structure", "--no-cache"];
    flags.extend_from_slice(args);
    let typed = run(home, file, &flags);
    assert!(typed.out.chars().count() + typed.err.chars().count() >= LIMIT);
    assert_ne!(typed.out, source, "fixture must not degrade to raw");
    typed
}

#[test]
fn default_steps_to_signatures_with_one_exact_notice() {
    let source = functions(100, true).replace("Documentation_", "probe_word_unique_");
    let (home, file) = fixture("probe_word_unique.rs", &source);
    let typed = over_structure(home.path(), &file, &source, &[]);
    let view = run(home.path(), &file, &[]);
    bounded(&view);
    assert_eq!(
        view.out,
        run(home.path(), &file, &["--mode=signatures"]).out
    );
    assert_eq!(
        view.err,
        format!(
            "[skim] signatures view: all but signatures removed; the default view has {} characters, over the 29000-character output limit; use --symbol NAME or --max-lines N for more, or SKIM_PASSTHROUGH=1 for full output\n",
            typed.out.chars().count() + typed.err.chars().count()
        )
    );
    assert!(!view.err.contains("probe_word_unique"));
    assert!(!view.err.contains("Documentation_"));
}

#[test]
fn default_bound_also_applies_to_a_regular_file_destination() {
    let source = functions(100, true);
    let (home, file) = fixture("redirect.rs", &source);
    over_structure(home.path(), &file, &source, &[]);
    let piped = run(home.path(), &file, &["--no-cache"]);
    bounded(&piped);
    let destination = home.path().join("view.txt");
    let output = skim_std_cmd(home.path())
        .arg(&file)
        .arg("--no-cache")
        .stdout(fs::File::create(&destination).unwrap())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(fs::read_to_string(destination).unwrap(), piped.out);
    assert_eq!(output.stderr, piped.err.as_bytes());
}

#[test]
fn default_steps_to_types_when_signatures_exceed_the_limit() {
    let source = functions(400, false);
    let (home, file) = fixture("types.rs", &source);
    over_structure(home.path(), &file, &source, &[]);
    assert!(
        run(home.path(), &file, &["--mode=signatures"])
            .out
            .chars()
            .count()
            > LIMIT
    );
    let view = run(home.path(), &file, &[]);
    bounded(&view);
    assert_eq!(view.out, run(home.path(), &file, &["--mode=types"]).out);
    assert!(
        view.err
            .starts_with("[skim] types view: all but types removed;")
    );
}

#[test]
fn line_prefixes_can_trigger_the_bound() {
    let source = functions(81, true);
    let (home, file) = fixture("numbered.rs", &source);
    let plain = run(home.path(), &file, &["--mode=structure"]);
    assert!(plain.out.chars().count() + plain.err.chars().count() < LIMIT);
    over_structure(home.path(), &file, &source, &["-n"]);
    bounded(&run(home.path(), &file, &["-n"]));
}

#[test]
fn rust_stdin_steps_down_and_has_only_stdin_remedies() {
    let source = functions(100, true);
    let home = TempDir::new().unwrap();
    let typed = stdin(
        home.path(),
        &source,
        &["--language=rust", "--mode=structure"],
    );
    assert!(typed.out.chars().count() > LIMIT);
    let view = stdin(home.path(), &source, &["--language=rust"]);
    bounded(&view);
    assert!(view.err.contains("use --max-lines N for more"));
    assert!(!view.err.contains("--symbol"));
    assert!(
        !stdin(home.path(), &source, &["--language=rust", "--max-lines=8"])
            .out
            .is_empty()
    );
    let raw = skim_cmd(home.path())
        .arg("-")
        .arg("--language=rust")
        .env("SKIM_PASSTHROUGH", "1")
        .write_stdin(source.clone())
        .output()
        .unwrap();
    assert!(raw.status.success());
    assert_eq!(raw.stdout, source.as_bytes());
    assert!(raw.stderr.is_empty());
}

#[test]
fn multibyte_view_below_the_character_limit_stays_identical() {
    let source = format!(
        "/// {}\npub fn sample() {{\n{}\n}}\n",
        "界".repeat(LIMIT / 2),
        "let x = 123;\n".repeat(200)
    );
    let (home, file) = fixture("unicode.rs", &source);
    let typed = run(home.path(), &file, &["--mode=structure"]);
    assert!(typed.out.len() > LIMIT);
    assert!(typed.out.chars().count() + typed.err.chars().count() < LIMIT);
    assert_eq!(run(home.path(), &file, &[]), typed);
}

fn fields() -> String {
    let mut source = String::from("pub struct Large {\n");
    for i in 0..4000 {
        source.push_str(&format!("    pub field_{i}: usize,\n"));
    }
    source.push_str("}\n");
    source
}

fn hidden_count(view: &str) -> usize {
    let marker = view.lines().last().unwrap();
    let start = marker.find("(").unwrap() + 1;
    marker[start..]
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn types_cut_counts_source_lines_after_the_last_kept_field() {
    let source = fields();
    let (home, file) = fixture("fields.rs", &source);
    assert!(
        run(home.path(), &file, &["--mode=types"])
            .out
            .chars()
            .count()
            > LIMIT
    );
    let view = run(home.path(), &file, &[]);
    bounded(&view);
    assert!(view.err.starts_with("[skim] types view:"));
    assert!(view.err.contains("this view is cut to the limit"));
    let last = view.out.lines().rev().nth(1).unwrap();
    let last_source = source
        .lines()
        .position(|line| line.trim() == last.trim())
        .unwrap()
        + 1;
    assert_eq!(
        hidden_count(&view.out),
        source.lines().skip(last_source).count()
    );
}

#[test]
fn types_cut_after_a_synthetic_separator_counts_from_the_last_source_line() {
    use rskim_core::{Language, Mode, TransformConfig, TruncationCountSource};
    let source =
        "type First = usize;\ntype Second = usize;\ntype Third = usize;\ntype Fourth = usize;\n";
    let mut config = TransformConfig::with_mode(Mode::Types);
    config.line_numbers = true;
    let (text, _, map, degraded) =
        rskim_core::transform_with_line_map(source, Language::Rust, &config).unwrap();
    assert!(!degraded);
    let map = map.unwrap();
    assert_eq!(&map[..2], &[1, 0], "types insert an unmapped separator");
    let cut = rskim_core::truncate_with_count_source(
        &text,
        Language::Rust,
        3,
        |candidate| candidate.lines().count(),
        None,
        None,
        TruncationCountSource::LineMap {
            map: &map,
            source_total: source.lines().count(),
        },
    )
    .unwrap();
    assert_eq!(cut.lines().nth(1), Some(""));
    assert_eq!(hidden_count(&cut), source.lines().skip(1).count());
}

#[test]
fn numbered_types_cut_keeps_source_numbers_and_an_unnumbered_marker() {
    let source = fields();
    let (home, file) = fixture("fields.rs", &source);
    let view = run(home.path(), &file, &["-n"]);
    bounded(&view);
    for line in view.out.lines().take(view.out.lines().count() - 1) {
        let (number, text) = line.split_once('\t').unwrap();
        let number: usize = number.trim().parse().unwrap();
        assert_eq!(source.lines().nth(number - 1).unwrap().trim(), text.trim());
    }
    assert!(!view.out.lines().last().unwrap().contains('\t'));
}

#[test]
fn serde_cut_counts_hidden_view_lines() {
    let source = serde_json::to_string_pretty(
        &(0..3000)
            .map(|i| (format!("key_{i}"), i))
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
    .unwrap();
    let (home, file) = fixture("data.json", &source);
    let typed = over_structure(home.path(), &file, &source, &[]);
    let view = run(home.path(), &file, &[]);
    bounded(&view);
    assert!(view.err.starts_with("[skim] structure view:"));
    assert_eq!(
        hidden_count(&view.out),
        typed.out.lines().count() - (view.out.lines().count() - 1)
    );
}

#[test]
fn guard_raw_over_the_limit_cannot_escape_the_bound() {
    let source: String = (0..1600)
        .map(|i| format!("fn empty_function_{i}() {{}}\n"))
        .collect();
    let (home, file) = fixture("empty.rs", &source);
    assert!(source.chars().count() > LIMIT);
    let (structure, _, _, degraded) = rskim_core::transform_with_line_map(
        &source,
        rskim_core::Language::Rust,
        &rskim_core::TransformConfig::with_mode(rskim_core::Mode::Structure),
    )
    .unwrap();
    assert!(!degraded);
    assert!(structure.chars().count() > source.chars().count());
    // The CLI guard hides the larger structure view by serving raw.
    assert_eq!(run(home.path(), &file, &["--mode=structure"]).out, source);
    let view = run(home.path(), &file, &[]);
    bounded(&view);
    assert_ne!(view.out, source);
}

#[test]
fn one_over_limit_multibyte_line_serves_only_a_marker() {
    let source = "界".repeat(LIMIT + 1);
    let (home, file) = fixture("one.unknown", &source);
    let view = run(home.path(), &file, &[]);
    bounded(&view);
    assert_eq!(view.out.lines().count(), 1);
    assert_eq!(hidden_count(&view.out), 1);
    assert!(view.err.starts_with("[skim] full view:"));
}

#[test]
fn unknown_file_and_stdin_cut_raw_with_exact_counts() {
    let source = "unknown source line with no grammar\n".repeat(2000);
    let (home, file) = fixture("raw.unknown", &source);
    for view in [
        run(home.path(), &file, &[]),
        stdin(home.path(), &source, &[]),
    ] {
        bounded(&view);
        assert!(
            view.err
                .starts_with("[skim] full view: lines outside range omitted;")
        );
        assert!(view.err.contains("this view is cut"));
        assert_eq!(
            hidden_count(&view.out),
            source.lines().count() - (view.out.lines().count() - 1)
        );
    }
}

#[test]
fn unknown_stdin_below_the_limit_keeps_its_unannotated_raw_bytes() {
    let home = TempDir::new().unwrap();
    for source in [
        "untyped raw line\n".repeat(4),
        "界\n".repeat((LIMIT - 1) / 2),
    ] {
        assert!(source.chars().count() < LIMIT);
        for args in [vec![], vec!["-n"]] {
            let default = stdin(home.path(), &source, &args);
            let mut explicit = args;
            explicit.push("--mode=structure");
            assert_eq!(default, stdin(home.path(), &source, &explicit));
            assert_eq!(default.out, source);
            assert!(default.err.is_empty());
        }
    }
}

#[test]
fn numbered_unknown_raw_cut_keeps_identity_numbers() {
    let source = "unknown source line with no grammar\n".repeat(2000);
    let (home, file) = fixture("raw.unknown", &source);
    for view in [
        run(home.path(), &file, &["-n"]),
        stdin(home.path(), &source, &["-n"]),
    ] {
        bounded(&view);
        let lines: Vec<_> = view.out.lines().collect();
        for (i, line) in lines[..lines.len() - 1].iter().enumerate() {
            assert_eq!(line.split_once('\t').unwrap().0.trim(), (i + 1).to_string());
        }
        assert!(!lines.last().unwrap().contains('\t'));
        assert_eq!(hidden_count(&view.out), 2000 - (lines.len() - 1));
    }
}

#[test]
fn markdown_header_rungs_cut_the_view_instead_of_raw_text() {
    let source: String = (0..700)
        .map(|i| {
            format!(
                "# Heading_{i} {}\n\nA paragraph that structure removes.\n\n",
                "header words ".repeat(5)
            )
        })
        .collect();
    let (home, file) = fixture("headings.md", &source);
    let typed = over_structure(home.path(), &file, &source, &[]);
    // Markdown retains headings in all three rungs, unlike code declarations.
    assert_eq!(
        run(home.path(), &file, &["--mode=signatures"]).out,
        typed.out
    );
    assert_eq!(run(home.path(), &file, &["--mode=types"]).out, typed.out);
    let view = run(home.path(), &file, &[]);
    bounded(&view);
    assert!(view.err.starts_with("[skim] types view:"));
    assert!(!view.out.contains("A paragraph"));
}

#[test]
fn key_cap_degrade_cuts_raw_and_labels_full() {
    let source = serde_json::to_string_pretty(
        &(0..10001)
            .map(|i| (format!("key_{i}"), i))
            .collect::<std::collections::BTreeMap<_, _>>(),
    )
    .unwrap();
    let (home, file) = fixture("cap.json", &source);
    assert_eq!(run(home.path(), &file, &["--mode=structure"]).out, source);
    let view = run(home.path(), &file, &[]);
    bounded(&view);
    assert!(view.err.starts_with("[skim] full view:"));
    assert_eq!(
        hidden_count(&view.out),
        source.lines().count() - (view.out.lines().count() - 1)
    );
}

#[test]
fn under_limit_default_streams_equal_typed_structure() {
    let source = functions(2, true);
    let (home, file) = fixture("small.rs", &source);
    assert_eq!(
        run(home.path(), &file, &[]),
        run(home.path(), &file, &["--mode=structure"])
    );
}

#[test]
fn explicit_selections_keep_their_unbounded_contracts_and_valid_remedies() {
    let source = functions(400, true);
    let (home, file) = fixture("explicit.rs", &source);
    for args in [
        vec!["--mode=structure"],
        vec!["--max-lines=8"],
        vec!["--last-lines=8"],
        vec!["--tokens=100000"],
        vec!["--symbol", "Small"],
    ] {
        let view = run(home.path(), &file, &args);
        assert!(!view.err.contains("output limit"));
        assert!(!view.out.is_empty());
        if args[0] == "--mode=structure" {
            assert!(view.out.chars().count() > LIMIT);
        }
    }
    let output = skim_cmd(home.path())
        .arg(&file)
        .env("SKIM_PASSTHROUGH", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, source.as_bytes());
    assert!(
        !String::from_utf8(output.stderr)
            .unwrap()
            .contains("output limit")
    );
}

#[test]
fn directory_glob_and_explicit_file_list_stay_unbounded() {
    let home = TempDir::new().unwrap();
    let inputs = home.path().join("inputs");
    fs::create_dir(&inputs).unwrap();
    fs::write(inputs.join("a.rs"), functions(100, true)).unwrap();
    fs::write(inputs.join("b.rs"), functions(100, true)).unwrap();
    for args in [
        vec![inputs.to_str().unwrap().to_owned()],
        vec![format!("{}/*.rs", inputs.display())],
        vec![
            inputs.join("a.rs").display().to_string(),
            inputs.join("b.rs").display().to_string(),
        ],
    ] {
        let default = skim_cmd(home.path()).args(&args).output().unwrap();
        let typed = skim_cmd(home.path())
            .args(&args)
            .arg("--mode=structure")
            .output()
            .unwrap();
        assert!(default.status.success() && typed.status.success());
        assert_eq!(default.stdout, typed.stdout);
        assert_eq!(default.stderr, typed.stderr);
        assert!(String::from_utf8(default.stdout).unwrap().chars().count() > LIMIT);
    }
}

#[test]
fn warm_default_read_replays_the_cold_bound_even_after_content_changes() {
    let (home, file) = fixture("cached.rs", &functions(100, true));
    let mtime = fs::metadata(&file).unwrap().modified().unwrap();
    let cold = run(home.path(), &file, &[]);
    bounded(&cold);
    fs::write(&file, "pub fn replaced() {}\n").unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert_eq!(run(home.path(), &file, &[]), cold);
}

#[test]
fn default_and_explicit_structure_cache_entries_stay_separate_in_both_orders() {
    for default_first in [true, false] {
        let source = functions(100, true);
        let (home, file) = fixture("cached.rs", &source);
        let default_cold = run(home.path(), &file, &["--no-cache"]);
        bounded(&default_cold);
        let typed_cold = run(home.path(), &file, &["--mode=structure", "--no-cache"]);
        let orders: [&[&str]; 2] = if default_first {
            [&[], &["--mode=structure"]]
        } else {
            [&["--mode=structure"], &[]]
        };
        for _ in 0..2 {
            for flags in orders {
                assert_eq!(
                    run(home.path(), &file, flags),
                    if flags.is_empty() {
                        &default_cold
                    } else {
                        &typed_cold
                    }
                    .clone()
                );
            }
        }
    }
}
