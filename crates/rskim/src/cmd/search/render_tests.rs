//! Tests for the output grammar: the labelled view, the word view, `-l` and
//! `-c`, the escapes, the notes of each bound, and the JSON envelope.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;

use super::*;
use crate::cmd::search::rank::FilePrior;
use crate::cmd::search::roles::{Holder, RoleCounts};
use crate::cmd::search::scan::{ContextLine, Cost, ParseStats};
use crate::cmd::search::universe::LeftOutCounts;

// ============================================================================
// Helpers
// ============================================================================

fn record(line: u32, role: Role, holder: Option<&str>, text: &str) -> Record {
    Record {
        line,
        column: 1,
        role,
        holder: holder.map(|name| Holder {
            name: name.to_owned(),
            first_line: line,
            last_line: line,
        }),
        def_lines: None,
        text: text.to_owned(),
        truncated_bytes: 0,
        distinct_words: 1,
    }
}

fn definition(line: u32, first: u32, last: u32, name: &str, text: &str) -> Record {
    Record {
        def_lines: Some((first, last)),
        ..record(line, Role::Def, Some(name), text)
    }
}

/// A matched file whose counts come from its records.
fn file(path: impl Into<PathBuf>, prior: FilePrior, records: Vec<Record>) -> FileScan {
    let mut roles = RoleCounts::default();
    let (mut defs, mut rest) = (Vec::new(), Vec::new());
    for record in records {
        roles.add(record.role);
        if record.role == Role::Def {
            defs.push(record);
        } else {
            rest.push(record);
        }
    }
    FileScan {
        path: path.into(),
        prior,
        lines: (defs.len() + rest.len()) as u32,
        word_lines: Vec::new(),
        unlabelled: None,
        roles,
        defs,
        records: rest,
        context: Vec::new(),
    }
}

fn scan_of(mut files: Vec<FileScan>) -> ScanOutput {
    files.sort_by(|a, b| a.path.cmp(&b.path));
    ScanOutput {
        files,
        searched_files: 364,
        left_out: LeftOutCounts::default(),
        cost: Cost::default(),
        parse: ParseStats::default(),
        context_hidden: 0,
    }
}

fn pattern(text: &str) -> QueryInfo {
    QueryInfo {
        text: text.to_owned(),
        words: Vec::new(),
        mode: MatchMode::Literal,
        ignore_case: false,
    }
}

fn words(list: &[&str]) -> QueryInfo {
    QueryInfo {
        text: list.join(" "),
        words: list.iter().map(|w| (*w).to_owned()).collect(),
        mode: MatchMode::Words,
        ignore_case: false,
    }
}

fn render_with(query: &QueryInfo, scan: &ScanOutput, layout: Layout, format: Format) -> Rendered {
    render(
        &Report {
            query,
            scan,
            layout,
            duration_ms: 21,
        },
        format,
    )
}

fn text(query: &QueryInfo, scan: &ScanOutput) -> String {
    render_with(query, scan, Layout::default(), Format::Text).stdout
}

fn lines(output: &str) -> Vec<&str> {
    output.lines().collect()
}

fn call_file(path: &str, prior: FilePrior, numbers: &[u32]) -> FileScan {
    let records = numbers
        .iter()
        .map(|n| record(*n, Role::Call, Some("run"), &format!("call{n}()")))
        .collect();
    file(path, prior, records)
}

// ============================================================================
// Labelled view
// ============================================================================

#[test]
fn the_default_view_follows_the_grammar() {
    let scan = scan_of(vec![
        file(
            "src/workers/launch.ts",
            FilePrior::Source,
            vec![definition(
                302,
                302,
                361,
                "launchWorker",
                "export const launchWorker = async (",
            )],
        ),
        file(
            "src/commands/worker.ts",
            FilePrior::Source,
            vec![
                record(
                    3,
                    Role::Import,
                    None,
                    "import { launchWorker } from \"../x\";",
                ),
                record(
                    457,
                    Role::Call,
                    Some("runWorker"),
                    "  const launched = await launchWorker(deps, slot);",
                ),
            ],
        ),
        file(
            "test/unit/worker-launch.test.ts",
            FilePrior::Test,
            vec![record(
                88,
                Role::Call,
                None,
                "await launchWorker(fake, slot);",
            )],
        ),
        file(
            "docs/design.md",
            FilePrior::Docs,
            vec![record(40, Role::Text, None, "launchWorker starts one pane")],
        ),
    ]);

    let output = text(&pattern("launchWorker"), &scan);

    assert_eq!(
        output,
        "def src/workers/launch.ts:302-361: export const launchWorker = async (\n\
         calls: 2 lines in 2 files\n\
         src/commands/worker.ts:457: [runWorker] const launched = await launchWorker(deps, slot);\n\
         test/unit/worker-launch.test.ts:88: [top] await launchWorker(fake, slot);\n\
         other: 1 import, 1 text lines (--all lists them)\n\
         total: 5 lines in 4 files, all listed or counted; 364 files searched, 0 left out\n"
    );
}

#[test]
fn a_definition_whose_name_is_not_on_its_first_line_says_where_the_name_is() {
    let scan = scan_of(vec![file(
        "a.rs",
        FilePrior::Source,
        vec![definition(12, 10, 20, "run", "fn run(")],
    )]);

    let output = text(&pattern("run"), &scan);

    assert_eq!(lines(&output)[0], "def a.rs:10-20 (name at 12): fn run(");
}

#[test]
fn a_result_with_no_definition_says_so() {
    let scan = scan_of(vec![call_file("a.rs", FilePrior::Source, &[4])]);

    let output = text(&pattern("run"), &scan);

    assert_eq!(lines(&output)[0], "def: none in labelled files");
}

#[test]
fn a_name_with_several_definitions_gets_the_clash_note() {
    let scan = scan_of(vec![
        file(
            "crates/rskim/src/cmd/build/mod.rs",
            FilePrior::Source,
            vec![definition(
                150,
                150,
                188,
                "run_parsed_command",
                "pub(super) fn run_parsed_command(",
            )],
        ),
        file(
            "crates/rskim/src/cmd/git/mod.rs",
            FilePrior::Source,
            vec![definition(
                414,
                414,
                470,
                "run_parsed_command",
                "pub(super) fn run_parsed_command<F>(",
            )],
        ),
    ]);

    let output = text(&pattern("run_parsed_command"), &scan);

    assert_eq!(
        &lines(&output)[..3],
        [
            "def crates/rskim/src/cmd/build/mod.rs:150-188: pub(super) fn run_parsed_command(",
            "def crates/rskim/src/cmd/git/mod.rs:414-470: pub(super) fn run_parsed_command<F>(",
            "note: 2 definitions of run_parsed_command; calls are not split by definition",
        ]
    );
}

#[test]
fn a_spent_parse_budget_turns_the_clash_note_into_an_at_least_note() {
    let mut scan = scan_of(vec![
        file(
            "a.rs",
            FilePrior::Source,
            vec![definition(1, 1, 3, "go", "fn go() {")],
        ),
        file(
            "b.rs",
            FilePrior::Source,
            vec![definition(1, 1, 3, "go", "fn go() {")],
        ),
    ]);
    scan.parse.budget_hit = true;
    scan.parse.unlabelled_files = 37;
    scan.parse.by_reason.insert(Unlabelled::Budget, 37);

    let output = text(&pattern("go"), &scan);

    assert!(
        output.contains(
            "note: at least 2 definitions of go (37 matched files have no labels); \
             calls are not split by definition\n"
        ),
        "{output}"
    );
    assert!(
        output.contains(
            "note: 37 matched files have no labels (parse budget: 500 files or 32 MiB); \
             their lines are under \"unlabelled\"\n"
        ),
        "{output}"
    );
}

#[test]
fn a_parse_budget_of_one_file_shows_in_the_note() {
    let mut scan = scan_of(vec![call_file("a.rs", FilePrior::Source, &[1])]);
    scan.parse.unlabelled_files = 1;
    scan.parse.by_reason.insert(Unlabelled::Budget, 1);
    let mut layout = Layout::default();
    layout.limits.parse_files = 1;

    let output = render_with(&pattern("run"), &scan, layout, Format::Text).stdout;

    assert!(
        output.contains(
            "note: 1 matched file has no labels (parse budget: 1 file or 32 MiB); \
             their lines are under \"unlabelled\"\n"
        ),
        "{output}"
    );
}

#[test]
fn several_reasons_for_missing_labels_are_counted_apart() {
    let mut scan = scan_of(vec![call_file("a.rs", FilePrior::Source, &[1])]);
    scan.parse.unlabelled_files = 3;
    scan.parse.by_reason.insert(Unlabelled::Budget, 2);
    scan.parse.by_reason.insert(Unlabelled::ParseFailed, 1);

    let output = text(&pattern("run"), &scan);

    assert!(
        output.contains(
            "note: 3 matched files have no labels (2 parse budget: 500 files or 32 MiB, \
             1 the parser failed); their lines are under \"unlabelled\"\n"
        ),
        "{output}"
    );
}

#[test]
fn all_lists_every_section_in_order_and_has_no_other_line() {
    let scan = scan_of(vec![file(
        "a.rs",
        FilePrior::Source,
        vec![
            record(1, Role::Partial, None, "launchWorkerPool"),
            record(2, Role::Text, None, "prose"),
            record(3, Role::Comment, Some("f"), "// note"),
            record(4, Role::StringLit, Some("f"), "\"launchWorker\""),
            record(5, Role::Import, None, "use launchWorker;"),
            record(6, Role::Unlabelled, None, "unknown"),
            record(7, Role::Ref, Some("f"), "let x = launchWorker;"),
            record(8, Role::Call, Some("f"), "launchWorker()"),
        ],
    )]);
    let layout = Layout {
        all: true,
        ..Layout::default()
    };

    let output = render_with(&pattern("launchWorker"), &scan, layout, Format::Text).stdout;

    let headers: Vec<&str> = output
        .lines()
        .filter(|l| l.ends_with("in 1 file"))
        .collect();
    assert_eq!(
        headers,
        [
            "calls: 1 line in 1 file",
            "refs: 1 line in 1 file",
            "unlabelled: 1 line in 1 file",
            "imports: 1 line in 1 file",
            "strings: 1 line in 1 file",
            "comments: 1 line in 1 file",
            "text: 1 line in 1 file",
            "partial: 1 line in 1 file",
        ]
    );
    assert!(!output.contains("other:"), "{output}");
    assert!(output.contains("a.rs:2: prose\n"), "{output}");
    assert!(output.contains("a.rs:1: launchWorkerPool\n"), "{output}");
    assert!(output.contains("a.rs:3: [f] // note\n"), "{output}");
}

#[test]
fn a_hit_with_no_holder_names_the_top_only_in_code_roles() {
    let scan = scan_of(vec![file(
        "a.rs",
        FilePrior::Source,
        vec![
            record(1, Role::Comment, None, "// c"),
            record(2, Role::Import, None, "use x;"),
            record(3, Role::Ref, None, "x"),
        ],
    )]);
    let layout = Layout {
        all: true,
        ..Layout::default()
    };

    let output = render_with(&pattern("x"), &scan, layout, Format::Text).stdout;

    assert!(output.contains("a.rs:1: [top] // c\n"), "{output}");
    assert!(output.contains("a.rs:2: use x;\n"), "{output}");
    assert!(output.contains("a.rs:3: [top] x\n"), "{output}");
}

#[test]
fn a_group_above_the_compact_size_lists_line_numbers_only() {
    let numbers: Vec<u32> = (1..=31).collect();
    let scan = scan_of(vec![call_file("a.rs", FilePrior::Source, &numbers)]);

    let output = text(&pattern("call"), &scan);

    let expected_numbers = (1..=31)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(",");
    assert!(
        output.contains(&format!(
            "calls: 31 lines in 1 file (line numbers only; --full adds the text)\n\
             a.rs:{expected_numbers}\n"
        )),
        "{output}"
    );
    assert!(!output.contains("call1()"), "{output}");
}

#[test]
fn a_group_at_the_compact_size_still_lists_its_text() {
    let numbers: Vec<u32> = (1..=COMPACT_GROUP_LINES as u32).collect();
    let scan = scan_of(vec![call_file("a.rs", FilePrior::Source, &numbers)]);

    let output = text(&pattern("call"), &scan);

    assert!(output.contains("calls: 30 lines in 1 file\n"), "{output}");
    assert!(output.contains("a.rs:1: [run] call1()\n"), "{output}");
}

#[test]
fn full_lists_the_text_of_a_large_group() {
    let numbers: Vec<u32> = (1..=40).collect();
    let scan = scan_of(vec![call_file("a.rs", FilePrior::Source, &numbers)]);
    let layout = Layout {
        full: true,
        ..Layout::default()
    };

    let output = render_with(&pattern("call"), &scan, layout, Format::Text).stdout;

    assert!(output.contains("calls: 40 lines in 1 file\n"), "{output}");
    assert!(output.contains("a.rs:40: [run] call40()\n"), "{output}");
}

#[test]
fn context_prints_blocks_in_the_path_line_text_form() {
    let mut a = call_file("a.rs", FilePrior::Source, &[5, 7, 20]);
    a.context = [4, 6, 8, 19, 21]
        .iter()
        .map(|n| ContextLine {
            line: *n,
            text: format!("  ctx{n}"),
            truncated_bytes: 0,
        })
        .collect();
    let scan = scan_of(vec![a]);
    let layout = Layout {
        context: 1,
        ..Layout::default()
    };

    let output = render_with(&pattern("call"), &scan, layout, Format::Text).stdout;

    assert_eq!(
        &lines(&output)[1..11],
        [
            "calls: 3 lines in 1 file",
            "a.rs-4-  ctx4",
            "a.rs:5: [run] call5()",
            "a.rs-6-  ctx6",
            "a.rs:7: [run] call7()",
            "a.rs-8-  ctx8",
            "--",
            "a.rs-19-  ctx19",
            "a.rs:20: [run] call20()",
            "a.rs-21-  ctx21",
        ]
    );
}

#[test]
fn context_blocks_that_touch_join_and_blocks_that_do_not_are_split() {
    let ctx = |numbers: &[u32]| -> Vec<ContextLine> {
        numbers
            .iter()
            .map(|n| ContextLine {
                line: *n,
                text: format!("c{n}"),
                truncated_bytes: 0,
            })
            .collect()
    };
    let layout = Layout {
        context: 1,
        ..Layout::default()
    };

    // Lines 5 and 8 reach 4-6 and 7-9: adjacent, so one block.
    let mut touching = call_file("a.rs", FilePrior::Source, &[5, 8]);
    touching.context = ctx(&[4, 6, 7, 9]);
    let output = render_with(
        &pattern("call"),
        &scan_of(vec![touching]),
        layout,
        Format::Text,
    )
    .stdout;
    assert!(!output.contains("--\n"), "{output}");

    // Lines 5 and 9 reach 4-6 and 8-10: a gap at 7, so two blocks.
    let mut apart = call_file("a.rs", FilePrior::Source, &[5, 9]);
    apart.context = ctx(&[4, 6, 8, 10]);
    let output = render_with(
        &pattern("call"),
        &scan_of(vec![apart]),
        layout,
        Format::Text,
    )
    .stdout;
    assert_eq!(output.matches("--\n").count(), 1, "{output}");
}

#[test]
fn context_blocks_of_two_files_are_split_by_a_separator() {
    let mut a = call_file("a.rs", FilePrior::Source, &[5]);
    a.context = vec![ContextLine {
        line: 4,
        text: "before".to_owned(),
        truncated_bytes: 0,
    }];
    let b = call_file("b.rs", FilePrior::Source, &[9]);
    let layout = Layout {
        context: 1,
        ..Layout::default()
    };

    let output = render_with(&pattern("call"), &scan_of(vec![a, b]), layout, Format::Text).stdout;

    assert!(
        output.contains("a.rs:5: [run] call5()\n--\nb.rs:9: [run] call9()\n"),
        "{output}"
    );
}

#[test]
fn files_with_matches_lists_paths_in_order_and_summarises_on_stderr() {
    let scan = scan_of(vec![
        call_file("src/workers/launch.ts", FilePrior::Source, &[1, 2, 3]),
        call_file("src/commands/worker.ts", FilePrior::Source, &[4, 5]),
    ]);

    let rendered = render_with(
        &pattern("x"),
        &scan,
        Layout::default(),
        Format::FilesWithMatches,
    );

    assert_eq!(
        rendered.stdout,
        "src/commands/worker.ts\nsrc/workers/launch.ts\n"
    );
    assert_eq!(
        rendered.stderr,
        ["skim search: total: 5 lines in 2 files; 364 files searched, 0 left out"]
    );
}

#[test]
fn counts_prints_one_count_per_file() {
    let scan = scan_of(vec![
        call_file("src/workers/launch.ts", FilePrior::Source, &[1, 2, 3]),
        call_file("src/commands/worker.ts", FilePrior::Source, &[4, 5]),
    ]);

    let rendered = render_with(&pattern("x"), &scan, Layout::default(), Format::Counts);

    assert_eq!(
        rendered.stdout,
        "src/commands/worker.ts:2\nsrc/workers/launch.ts:3\n"
    );
    assert_eq!(rendered.stderr.len(), 1);
}

#[test]
fn the_total_line_names_each_reason_a_file_was_left_out() {
    let mut scan = scan_of(vec![call_file("a.rs", FilePrior::Source, &[1])]);
    scan.left_out.add(LeftOut::Binary);
    scan.left_out.add(LeftOut::Binary);
    scan.left_out.add(LeftOut::OutsideRoot);

    let output = text(&pattern("call"), &scan);

    assert!(
        output.ends_with(
            "total: 1 line in 1 file, all listed or counted; \
             364 files searched, 3 left out (2 binary, 1 outside_root)\n"
        ),
        "{output}"
    );
}

#[test]
fn a_limit_pages_each_section_and_names_the_files_it_leaves_out() {
    let scan = scan_of(vec![
        call_file("a.rs", FilePrior::Source, &[1]),
        call_file("b.rs", FilePrior::Source, &[1]),
        call_file("c.rs", FilePrior::Source, &[1]),
    ]);
    let layout = Layout {
        limit: Some(1),
        ..Layout::default()
    };

    let output = render_with(&pattern("call"), &scan, layout, Format::Text).stdout;

    assert!(
        output.contains(
            "calls: 3 lines in 3 files\n\
             a.rs:1: [run] call1()\n\
             not listed: 2 more files in calls (--limit N lists more)\n"
        ),
        "{output}"
    );

    let layout = Layout {
        limit: Some(1),
        offset: 1,
        ..Layout::default()
    };
    let output = render_with(&pattern("call"), &scan, layout, Format::Text).stdout;
    assert!(output.contains("b.rs:1: [run] call1()\n"), "{output}");
    assert!(!output.contains("a.rs:1"), "{output}");
}

// ============================================================================
// Bounds
// ============================================================================

#[test]
fn a_line_longer_than_the_width_cap_prints_with_the_hidden_byte_count() {
    let mut long = record(7, Role::Call, Some("f"), &"x".repeat(300));
    long.truncated_bytes = 812;
    let mut def = definition(1, 1, 9, "f", &"y".repeat(300));
    def.truncated_bytes = 5;
    let scan = scan_of(vec![file("a.rs", FilePrior::Source, vec![def, long])]);

    let output = text(&pattern("f"), &scan);

    assert!(
        output.contains(&format!("a.rs:7: [f] {}…[+812 bytes]\n", "x".repeat(300))),
        "{output}"
    );
    assert!(
        output.contains(&format!("def a.rs:1-9: {}…[+5 bytes]\n", "y".repeat(300))),
        "{output}"
    );
}

#[test]
fn lines_the_stored_line_cap_left_out_get_a_note_and_a_marker() {
    let mut a = call_file("a.rs", FilePrior::Source, &[1, 2, 3]);
    a.lines = 5;
    a.roles.add(Role::Call);
    a.roles.add(Role::Call);
    let scan = scan_of(vec![a]);
    let mut layout = Layout::default();
    layout.limits.max_stored_lines = 3;

    let rendered = render_with(&pattern("call"), &scan, layout, Format::Text);

    assert!(
        rendered
            .stdout
            .contains("note: 2 lines in 1 file are counted, not listed (stored-line cap 3)\n"),
        "{}",
        rendered.stdout
    );
    assert_eq!(
        rendered.stderr,
        ["[skim] lines elided beyond 3 of 5 — use -c or -l for every file, or narrow the pattern"]
    );
}

#[test]
fn definitions_the_definition_cap_left_out_get_a_note_naming_the_cap_and_a_marker() {
    let mut a = file(
        "a.rs",
        FilePrior::Source,
        vec![record(1, Role::Def, Some("needle"), "fn needle() {}")],
    );
    a.lines = 4;
    for _ in 0..3 {
        a.roles.add(Role::Def);
    }
    let scan = scan_of(vec![a]);
    let mut layout = Layout::default();
    layout.limits.max_def_sites = 1;

    let rendered = render_with(&pattern("needle"), &scan, layout, Format::Text);

    assert!(
        rendered
            .stdout
            .contains("note: 3 lines in 1 file are counted, not listed (definition cap 1)\n"),
        "{}",
        rendered.stdout
    );
    assert_eq!(
        rendered.stderr,
        ["[skim] lines elided beyond 1 of 4 — use -c or -l for every file, or narrow the pattern"]
    );
}
#[test]
fn a_cap_that_cut_nothing_prints_no_note() {
    let a = call_file("a.rs", FilePrior::Source, &[1, 2, 3]);
    let scan = scan_of(vec![a]);
    let mut layout = Layout::default();
    layout.limits.max_stored_lines = 3;

    let rendered = render_with(&pattern("call"), &scan, layout, Format::Text);

    assert!(
        !rendered.stdout.contains("counted, not listed"),
        "{}",
        rendered.stdout
    );
    assert!(rendered.stderr.is_empty(), "{:?}", rendered.stderr);
}

#[test]
fn lines_of_a_file_that_changed_are_counted_and_the_note_names_no_cap() {
    let mut a = call_file("a.rs", FilePrior::Source, &[]);
    a.lines = 4;
    for _ in 0..4 {
        a.roles.add(Role::Unlabelled);
    }
    a.unlabelled = Some(Unlabelled::Changed);
    let mut scan = scan_of(vec![a]);
    scan.parse.unlabelled_files = 1;
    scan.parse.by_reason.insert(Unlabelled::Changed, 1);

    let rendered = render_with(&pattern("call"), &scan, Layout::default(), Format::Text);

    assert!(
        rendered.stdout.contains("unlabelled: 4 lines in 1 file\n"),
        "{}",
        rendered.stdout
    );
    assert!(
        rendered
            .stdout
            .contains("note: 4 lines in 1 file are counted, not listed\n"),
        "{}",
        rendered.stdout
    );
    assert!(rendered.stderr.is_empty());
}

#[test]
fn context_lines_the_cap_left_out_get_a_note() {
    let mut scan = scan_of(vec![call_file("a.rs", FilePrior::Source, &[5])]);
    scan.context_hidden = 7;
    let layout = Layout {
        context: 2,
        ..Layout::default()
    };

    let output = render_with(&pattern("call"), &scan, layout, Format::Text).stdout;

    assert!(
        output.contains("note: 7 context lines are not shown (context cap 100000)\n"),
        "{output}"
    );
}

#[test]
fn context_lines_the_cap_left_out_get_a_stderr_marker_with_exact_counts() {
    let mut file = call_file("a.rs", FilePrior::Source, &[5]);
    file.context = (1..=3)
        .map(|line| ContextLine {
            line,
            text: String::new(),
            truncated_bytes: 0,
        })
        .collect();
    let mut scan = scan_of(vec![file]);
    scan.context_hidden = 7;
    let layout = Layout {
        context: 2,
        ..Layout::default()
    };

    let rendered = render_with(&pattern("call"), &scan, layout, Format::Text);

    assert_eq!(
        rendered.stderr,
        ["[skim] context lines elided beyond 3 of 10 — use a smaller -C, or narrow the pattern"]
    );
}

#[test]
fn no_context_marker_when_the_cap_left_nothing_out() {
    let scan = scan_of(vec![call_file("a.rs", FilePrior::Source, &[5])]);
    let rendered = render_with(&pattern("call"), &scan, Layout::default(), Format::Text);
    assert!(rendered.stderr.is_empty(), "{:?}", rendered.stderr);
}

// ============================================================================
// Escapes
// ============================================================================

#[test]
fn a_path_with_a_control_character_prints_c_quoted() {
    assert_eq!(quote_path(Path::new("src/a.rs")), "src/a.rs");
    assert_eq!(quote_path(Path::new("a b/é.rs")), "a b/é.rs");
    assert_eq!(
        quote_path(Path::new("a\nb\u{1b}c.rs")),
        "\"a\\nb\\033c.rs\""
    );
    assert_eq!(
        quote_path(Path::new("q\"\\\t\r\u{7f}.rs")),
        "\"q\\\"\\\\\\t\\r\\177.rs\""
    );
}

#[cfg(unix)]
#[test]
fn a_path_that_is_not_utf8_prints_with_octal_escapes() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let path = Path::new(OsStr::from_bytes(b"caf\xe9/a.rs"));

    assert_eq!(quote_path(path), "\"caf\\351/a.rs\"");
}

#[test]
fn control_characters_in_a_line_print_as_visible_escapes() {
    assert_eq!(escape_text("plain\ttext"), "plain\ttext");
    assert_eq!(
        escape_text("a\u{1b}[31mb\u{7f}\u{0}c\u{9b}d"),
        "a\\x1B[31mb\\x7F\\x00c\\u{9b}d"
    );
}

#[test]
fn no_control_byte_of_a_path_or_a_line_reaches_the_text_output() {
    let scan = scan_of(vec![file(
        "dir/a\nb\u{1b}[2J.rs",
        FilePrior::Source,
        vec![
            definition(1, 1, 2, "f\u{1b}", "fn f\u{1b}[31m() {"),
            record(5, Role::Call, Some("g\u{7}"), "run\u{1b}[2J()"),
        ],
    )]);
    let layout = Layout {
        context: 0,
        ..Layout::default()
    };

    for format in [Format::Text, Format::FilesWithMatches, Format::Counts] {
        let rendered = render_with(&pattern("run"), &scan, layout, format);
        let all = format!("{}{}", rendered.stdout, rendered.stderr.join("\n"));
        assert!(
            !all.chars().any(|c| c.is_ascii_control() && c != '\n'),
            "{format:?}: {all:?}"
        );
        assert!(
            all.contains("\"dir/a\\nb\\033[2J.rs\""),
            "{format:?}: {all}"
        );
    }
    let output = text(&pattern("run"), &scan);
    assert!(output.contains("fn f\\x1B[31m() {"), "{output}");
    assert!(output.contains("[g\\x07] run\\x1B[2J()"), "{output}");
}

// ============================================================================
// Word view
// ============================================================================

fn word_file(
    path: &str,
    prior: FilePrior,
    counts: &[u32],
    lines: u32,
    records: Vec<Record>,
) -> FileScan {
    let mut f = file(path, prior, records);
    f.lines = lines;
    f.word_lines = counts.to_vec();
    f
}

fn best(line: u32, role: Role, holder: Option<&str>, text: &str, distinct: u32) -> Record {
    Record {
        distinct_words: distinct,
        ..record(line, role, holder, text)
    }
}

#[test]
fn the_word_view_lists_the_best_lines_of_each_file_that_holds_all_words() {
    let scan = scan_of(vec![
        word_file(
            "crates/x/src/staleness.rs",
            FilePrior::Source,
            &[12, 30, 9],
            40,
            vec![
                best(
                    321,
                    Role::Def,
                    Some("check_staleness"),
                    "pub(super) fn check_staleness(",
                    1,
                ),
                best(
                    532,
                    Role::Comment,
                    Some("auto_refresh_if_stale"),
                    "// temporal staleness check first",
                    2,
                ),
                best(700, Role::Ref, Some("other"), "check_staleness();", 1),
            ],
        ),
        word_file(
            "crates/x/src/other.rs",
            FilePrior::Source,
            &[1, 1, 1],
            1,
            vec![best(
                4,
                Role::Comment,
                None,
                "// temporal staleness check",
                3,
            )],
        ),
        word_file(
            "crates/x/tests/stale.rs",
            FilePrior::Test,
            &[2, 4, 1],
            5,
            vec![
                best(
                    10,
                    Role::Comment,
                    None,
                    "// staleness check covers the temporal rows",
                    3,
                ),
                best(11, Role::Comment, None, "// staleness rows", 2),
                best(12, Role::Ref, None, "staleness", 1),
            ],
        ),
        word_file("notes.txt", FilePrior::Docs, &[3, 0, 0], 3, Vec::new()),
    ]);

    let output = text(&words(&["temporal", "staleness", "check"]), &scan);

    assert_eq!(
        output,
        "files: 3 hold all 3 words; 3 listed\n\
         crates/x/src/staleness.rs  temporal=12 staleness=30 check=9\n\
         crates/x/src/staleness.rs:321: [check_staleness] pub(super) fn check_staleness(\n\
         crates/x/src/staleness.rs:532: [auto_refresh_if_stale] // temporal staleness check first\n\
         crates/x/src/other.rs  temporal=1 staleness=1 check=1\n\
         crates/x/src/other.rs:4: [top] // temporal staleness check\n\
         crates/x/tests/stale.rs  temporal=2 staleness=4 check=1\n\
         crates/x/tests/stale.rs:10: [top] // staleness check covers the temporal rows\n\
         crates/x/tests/stale.rs:11: [top] // staleness rows\n\
         total: 49 lines in 4 files hold a word; 364 files searched, 0 left out\n"
    );
}

#[test]
fn the_best_lines_hold_the_most_words_then_the_earliest_line_and_include_definitions() {
    let scan = scan_of(vec![word_file(
        "a.rs",
        FilePrior::Source,
        &[6, 6],
        6,
        vec![
            best(5, Role::Ref, None, "a", 1),
            best(9, Role::Ref, None, "b", 1),
            best(30, Role::Def, Some("f"), "fn f(a, b)", 2),
            best(40, Role::Comment, None, "// a b", 2),
            best(50, Role::Ref, None, "a b", 2),
        ],
    )]);

    let output = text(&words(&["a", "b"]), &scan);

    let listed: Vec<&str> = output.lines().filter(|l| l.starts_with("a.rs:")).collect();
    assert_eq!(listed, ["a.rs:30: [f] fn f(a, b)", "a.rs:40: [top] // a b"]);
}

#[test]
fn fewer_than_three_files_with_all_words_switch_to_the_fallback() {
    let scan = scan_of(vec![
        word_file(
            "a.rs",
            FilePrior::Source,
            &[2, 2, 2],
            2,
            vec![
                best(1, Role::Ref, Some("f"), "a b c", 3),
                best(2, Role::Ref, Some("f"), "a b c", 3),
            ],
        ),
        word_file(
            "b.rs",
            FilePrior::Source,
            &[5, 0, 0],
            1,
            vec![best(1, Role::Text, None, "a", 1)],
        ),
        word_file(
            "c.rs",
            FilePrior::Source,
            &[1, 1, 0],
            1,
            vec![best(1, Role::Text, None, "a b", 2)],
        ),
    ]);

    let output = text(&words(&["a", "b", "c"]), &scan);

    assert_eq!(
        output,
        "note: only 1 file holds all 3 words; files with fewer words follow (most words first)\n\
         files: 1 holds all 3 words, 2 hold some; 3 listed\n\
         a.rs  a=2 b=2 c=2\n\
         a.rs:1: [f] a b c\n\
         a.rs:2: [f] a b c\n\
         c.rs  a=1 b=1\n\
         c.rs:1: a b\n\
         b.rs  a=5\n\
         b.rs:1: a\n\
         total: 4 lines in 3 files hold a word; 364 files searched, 0 left out\n"
    );
}

#[test]
fn no_file_with_all_words_is_named_as_such() {
    let scan = scan_of(vec![word_file(
        "b.rs",
        FilePrior::Source,
        &[5, 0],
        1,
        vec![best(1, Role::Text, None, "a", 1)],
    )]);

    let output = text(&words(&["a", "b"]), &scan);

    assert_eq!(
        &lines(&output)[..2],
        [
            "note: no file holds all 2 words; files with fewer words follow (most words first)",
            "files: 0 hold all 2 words, 1 hold some; 1 listed",
        ]
    );
}

#[test]
fn the_word_view_lists_ten_files_by_default_and_counts_the_rest() {
    let files: Vec<FileScan> = (0..12)
        .map(|i| {
            word_file(
                &format!("f{i:02}.rs"),
                FilePrior::Source,
                &[1, 1],
                1,
                vec![best(1, Role::Ref, None, "a b", 2)],
            )
        })
        .collect();
    let scan = scan_of(files);

    let output = text(&words(&["a", "b"]), &scan);

    assert_eq!(
        output.lines().filter(|l| l.contains("  a=1 b=1")).count(),
        10
    );
    assert!(
        output.contains("not listed: 2 more files hold the words (--limit N lists more)\n"),
        "{output}"
    );
    assert!(
        output.contains("files: 12 hold all 2 words; 10 listed\n"),
        "{output}"
    );

    let layout = Layout {
        limit: Some(12),
        ..Layout::default()
    };
    let all = render_with(&words(&["a", "b"]), &scan, layout, Format::Text).stdout;
    assert_eq!(all.lines().filter(|l| l.contains("  a=1 b=1")).count(), 12);
    assert!(!all.contains("not listed:"), "{all}");
}

// ============================================================================
// No match
// ============================================================================

#[test]
fn a_query_with_no_match_prints_nothing_on_stdout_and_one_line_on_stderr() {
    let scan = scan_of(Vec::new());

    for format in [Format::Text, Format::FilesWithMatches, Format::Counts] {
        let rendered = render_with(&pattern("zzz"), &scan, Layout::default(), format);
        assert_eq!(rendered.stdout, "", "{format:?}");
        assert_eq!(
            rendered.stderr,
            ["skim search: no match for \"zzz\" (364 files searched, 0 left out)"]
        );
    }
}

#[test]
fn a_literal_with_regex_characters_gets_the_hint_and_a_regex_does_not() {
    let scan = scan_of(Vec::new());
    let mut query = pattern("a.b");

    let line = no_match_line(&query, &scan);
    assert_eq!(
        line,
        "skim search: no match for \"a.b\" (364 files searched, 0 left out); \
         the pattern is a literal, use -E for a regex"
    );

    query.mode = MatchMode::Regex;
    assert!(!no_match_line(&query, &scan).contains("literal"));
}

#[test]
fn a_query_with_a_control_character_prints_escaped_in_the_no_match_line() {
    let line = no_match_line(&pattern("a\u{1b}b"), &scan_of(Vec::new()));

    assert!(!line.contains('\u{1b}'), "{line:?}");
}

// ============================================================================
// JSON
// ============================================================================

fn json_of(query: &QueryInfo, scan: &ScanOutput, layout: Layout) -> Value {
    let rendered = render_with(query, scan, layout, Format::Json);
    assert!(rendered.stdout.ends_with('\n'), "{}", rendered.stdout);
    assert_eq!(rendered.stdout.matches('\n').count(), 1, "one line");
    serde_json::from_str(rendered.stdout.trim_end()).unwrap()
}

#[test]
fn the_json_output_is_one_object_with_the_documented_keys() {
    let scan = scan_of(vec![
        file(
            "src/workers/launch.ts",
            FilePrior::Source,
            vec![
                definition(
                    302,
                    302,
                    361,
                    "launchWorker",
                    "export const launchWorker = async (",
                ),
                record(310, Role::Comment, Some("launchWorker"), "// launchWorker"),
            ],
        ),
        call_file("src/commands/worker.ts", FilePrior::Source, &[4]),
    ]);

    let value = json_of(&pattern("launchWorker"), &scan, Layout::default());

    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "query",
            "match_mode",
            "case",
            "view",
            "total_lines",
            "total_files",
            "has_more",
            "definitions",
            "files",
            "searched_files",
            "left_out",
            "parse",
            "cost",
            "notices",
            "duration_ms",
        ]
    );
    assert_eq!(value["match_mode"], "literal");
    assert_eq!(value["case"], "sensitive");
    assert_eq!(value["view"], "labelled");
    assert_eq!(value["total_lines"], 3);
    assert_eq!(value["total_files"], 2);
    assert_eq!(value["has_more"], false);
    assert_eq!(value["duration_ms"], 21);
    assert_eq!(value["definitions"]["count"], 1);
    assert_eq!(value["definitions"]["at_least"], false);
    assert_eq!(
        value["definitions"]["sites"][0],
        json!({
            "path": "src/workers/launch.ts",
            "line": 302,
            "first_line": 302,
            "end_line": 361,
            "holder": "launchWorker",
        })
    );
    let first = &value["files"][0];
    assert_eq!(first["path"], "src/workers/launch.ts");
    assert_eq!(first["rank"], 1);
    assert_eq!(first["prior"], "source");
    assert_eq!(first["labelled"], true);
    assert_eq!(first["line_count"], 2);
    assert_eq!(first["roles"]["def"], 1);
    assert_eq!(first["roles"]["comment"], 1);
    assert_eq!(first["occurrences"][0]["role"], "def");
    assert_eq!(first["occurrences"][1]["holder_range"], json!([310, 310]));
    assert_eq!(first["occurrences_elided"], 0);
    assert_eq!(value["left_out"]["outside_root"], 0);
    assert_eq!(value["notices"], json!([]));
}

#[test]
fn the_json_score_never_increases_down_the_files_and_rank_counts_from_one() {
    let scan = scan_of(vec![
        call_file("a.rs", FilePrior::Test, &[1]),
        call_file("b.rs", FilePrior::Source, &[1, 2, 3]),
        file(
            "c.rs",
            FilePrior::Source,
            vec![definition(1, 1, 2, "x", "fn x()")],
        ),
        file(
            "d.rs",
            FilePrior::Docs,
            vec![record(1, Role::Text, None, "x")],
        ),
    ]);

    let value = json_of(&pattern("x"), &scan, Layout::default());

    let scores: Vec<f64> = value["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["score"].as_f64().unwrap())
        .collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "{scores:?}");
    let ranks: Vec<u64> = value["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["rank"].as_u64().unwrap())
        .collect();
    assert_eq!(ranks, [1, 2, 3, 4]);
    assert_eq!(value["files"][0]["path"], "c.rs");
}

#[test]
fn a_json_limit_keeps_the_first_files_and_sets_has_more() {
    let scan = scan_of(vec![
        call_file("a.rs", FilePrior::Source, &[1]),
        call_file("b.rs", FilePrior::Source, &[1, 2]),
        call_file("c.rs", FilePrior::Source, &[1, 2, 3]),
    ]);
    let all = json_of(&pattern("call"), &scan, Layout::default());
    let all_paths: Vec<&str> = all["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();

    let layout = Layout {
        limit: Some(2),
        ..Layout::default()
    };
    let page = json_of(&pattern("call"), &scan, layout);
    let page_paths: Vec<&str> = page["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert_eq!(page_paths, all_paths[..2]);
    assert_eq!(page["has_more"], true);
    assert_eq!(page["total_files"], 3);

    let layout = Layout {
        limit: Some(2),
        offset: 2,
        ..Layout::default()
    };
    let last = json_of(&pattern("call"), &scan, layout);
    assert_eq!(last["files"][0]["path"], all_paths[2]);
    assert_eq!(last["files"][0]["rank"], 3);
    assert_eq!(last["has_more"], false);
}

#[test]
fn json_counts_the_lines_it_does_not_list() {
    let mut a = call_file("a.rs", FilePrior::Source, &[1, 2]);
    a.lines = 6;
    let scan = scan_of(vec![a]);

    let value = json_of(&pattern("call"), &scan, Layout::default());

    assert_eq!(value["files"][0]["line_count"], 6);
    assert_eq!(value["files"][0]["occurrences_elided"], 4);
}

#[test]
fn json_carries_the_notices_of_the_text_output() {
    let mut scan = scan_of(vec![call_file("a.rs", FilePrior::Source, &[1])]);
    scan.parse.unlabelled_files = 1;
    scan.parse.budget_hit = true;
    scan.parse.by_reason.insert(Unlabelled::Budget, 1);
    scan.context_hidden = 3;

    let value = json_of(&pattern("call"), &scan, Layout::default());

    assert_eq!(
        value["notices"],
        json!([
            "1 matched file has no labels (parse budget: 500 files or 32 MiB); \
             their lines are under \"unlabelled\"",
            "3 context lines are not shown (context cap 100000)",
        ])
    );
    assert_eq!(value["parse"]["budget_hit"], true);
    assert_eq!(value["parse"]["unlabelled_files"], 1);
    assert_eq!(value["definitions"]["at_least"], true);
}

#[test]
fn json_of_a_query_with_no_match_has_zero_lines_and_an_empty_list() {
    let scan = scan_of(Vec::new());

    let rendered = render_with(&pattern("zzz"), &scan, Layout::default(), Format::Json);

    let value: Value = serde_json::from_str(rendered.stdout.trim_end()).unwrap();
    assert_eq!(value["total_lines"], 0);
    assert_eq!(value["files"], json!([]));
    assert_eq!(rendered.stderr.len(), 1);
}

#[cfg(unix)]
#[test]
fn json_marks_a_path_that_is_not_utf8_as_lossy_and_still_parses() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let path = PathBuf::from(OsStr::from_bytes(b"caf\xe9.rs"));
    let scan = scan_of(vec![file(
        path,
        FilePrior::Source,
        vec![definition(1, 1, 2, "f", "fn f()")],
    )]);

    let value = json_of(&pattern("f"), &scan, Layout::default());

    assert_eq!(value["files"][0]["path_lossy"], true);
    assert_eq!(value["files"][0]["path"], "caf\u{fffd}.rs");
    assert_eq!(value["definitions"]["sites"][0]["path_lossy"], true);
}

#[test]
fn json_of_the_word_view_adds_the_words_and_their_counts() {
    let scan = scan_of(vec![word_file(
        "a.rs",
        FilePrior::Source,
        &[2, 1],
        2,
        vec![best(3, Role::Ref, Some("f"), "a b", 2)],
    )]);

    let value = json_of(&words(&["a", "b"]), &scan, Layout::default());

    assert_eq!(value["view"], "words");
    assert_eq!(value["match_mode"], "words");
    assert_eq!(value["words"], json!(["a", "b"]));
    assert_eq!(value["files_with_all_words"], 1);
    assert_eq!(value["files"][0]["words_matched"], 2);
    assert_eq!(value["files"][0]["words_total"], 2);
    assert_eq!(value["files"][0]["word_counts"], json!([2, 1]));
}

#[test]
fn an_error_is_one_json_object_with_an_error_key() {
    let line = json_error("bad \"flag\"\n");

    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value, json!({ "error": "bad \"flag\"\n" }));
    assert!(!line.contains('\n'));
}

#[test]
fn the_match_modes_have_the_json_names() {
    let names: Vec<&str> = [
        MatchMode::Literal,
        MatchMode::Word,
        MatchMode::Regex,
        MatchMode::Words,
    ]
    .iter()
    .map(|m| m.key())
    .collect();

    assert_eq!(names, ["literal", "word", "regex", "words"]);
}
