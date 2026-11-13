//! Integration tests: `skim search` scans the files of a repository at query time.
//!
//! Each test builds a small git repository, runs the real binary and checks
//! stdout, stderr and the exit code. The sandbox pins the working folder and
//! the home folder, so a test never reads the state of the machine.

mod common;

use std::fs;
#[cfg(unix)]
use std::io::Read as _;
#[cfg(unix)]
use std::process::Stdio;

use common::search_repo::{Run, SearchRepo, finish};
use serde_json::Value;

// ============================================================================
// Fixtures and helpers
// ============================================================================

/// A repository with one definition, one call and one hit of each other role.
fn launch_repo() -> SearchRepo {
    let repo = SearchRepo::new();
    repo.write(
        "src/workers/launch.rs",
        "pub fn launch_worker(\n    slot: u32,\n) -> u32 {\n    slot\n}\n\npub fn launch_worker_pool() {}\n",
    );
    repo.write(
        "src/commands/worker.rs",
        "use crate::workers::launch::launch_worker;\n\n// launch_worker starts one pane\npub fn run() {\n    let s = \"launch_worker\";\n    launch_worker(1);\n}\n",
    );
    repo.write(
        "docs/design.md",
        "# Design\n\nlaunch_worker starts one pane\n",
    );
    repo.write("notes.txt", "launch_worker=1\n");
    repo.write(".env", "SECRET=launch_worker\n");
    repo.track_all();
    repo
}

/// The stdout of a run as one JSON object. The output must be one line.
fn json_of(run: &Run) -> Value {
    assert_eq!(run.stdout.lines().count(), 1, "one line: {}", run.stdout);
    assert!(run.stdout.ends_with('\n'));
    serde_json::from_str(&run.stdout).expect("stdout is one JSON object")
}

/// The `path` of each listed line (`path:LINE: text`), in output order.
fn listed_paths(stdout: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in stdout.lines() {
        let Some((path, rest)) = line.split_once(':') else {
            continue;
        };
        let digits = rest.split(':').next().unwrap_or("");
        let is_listed = !digits.is_empty()
            && digits.bytes().all(|b| b.is_ascii_digit() || b == b',')
            && !path.contains(' ');
        if is_listed && !paths.iter().any(|p| p == path) {
            paths.push(path.to_owned());
        }
    }
    paths
}

/// The count that `-c` printed for one path.
fn count_of(run: &Run, path: &str) -> Option<usize> {
    run.stdout.lines().find_map(|line| {
        let (name, count) = line.rsplit_once(':')?;
        (name == path).then(|| count.parse().ok())?
    })
}

/// The file header lines of the word view (`path  word=N ...`).
fn word_headers(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| {
            let (path, rest) = line.split_once("  ")?;
            let first = rest.split(' ').next()?;
            (first.contains('=') && !path.contains(' ')).then(|| path.to_owned())
        })
        .collect()
}

// ============================================================================
// The labelled view
// ============================================================================

#[test]
fn a_bare_identifier_gives_the_definition_the_calls_and_counts_that_add_up() {
    let repo = launch_repo();
    let run = repo.search(&["launch_worker"]);
    assert_eq!(run.code, 0);
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "def src/workers/launch.rs:1-5: pub fn launch_worker(\n\
         calls: 1 line in 1 file\n\
         src/commands/worker.rs:6: [run] launch_worker(1);\n\
         other: 1 import, 1 string, 1 comment, 3 text, 1 partial lines (--all lists them)\n\
         total: 9 lines in 5 files, all listed or counted; 5 files searched, 0 left out\n"
    );

    // The total is the sum of the per-file line counts of `-c`.
    let counts = repo.search(&["-c", "launch_worker"]);
    let sum: usize = counts
        .stdout
        .lines()
        .filter_map(|l| l.rsplit_once(':')?.1.parse::<usize>().ok())
        .sum();
    assert_eq!(sum, 9);
}

#[test]
fn a_hit_inside_a_longer_name_is_partial_and_dash_w_drops_it() {
    let repo = launch_repo();
    let all = repo.search(&["launch_worker", "--all"]);
    assert!(
        all.stdout.contains(
            "partial: 1 line in 1 file\nsrc/workers/launch.rs:7: pub fn launch_worker_pool() {}\n"
        ),
        "{}",
        all.stdout
    );

    let whole = repo.search(&["-w", "launch_worker", "--all"]);
    assert_eq!(whole.code, 0);
    assert!(!whole.stdout.contains("partial:"), "{}", whole.stdout);
    assert!(!whole.stdout.contains("launch_worker_pool"));
    assert!(whole.stdout.contains("total: 8 lines in 5 files"));
}

#[test]
fn a_group_above_thirty_lines_prints_line_numbers_and_full_prints_the_text() {
    let repo = SearchRepo::new();
    let mut source = String::from("pub fn target() {}\n");
    for i in 1..=35 {
        source.push_str(&format!("fn f{i}() {{ target(); }}\n"));
    }
    repo.write("src/many.rs", source);
    repo.track_all();

    let compact = repo.search(&["target"]);
    assert_eq!(compact.code, 0);
    assert!(
        compact
            .stdout
            .contains("calls: 35 lines in 1 file (line numbers only; --full adds the text)\n"),
        "{}",
        compact.stdout
    );
    let numbers: Vec<String> = (2..=36).map(|n| n.to_string()).collect();
    assert!(
        compact
            .stdout
            .contains(&format!("src/many.rs:{}\n", numbers.join(","))),
        "{}",
        compact.stdout
    );
    assert!(!compact.stdout.contains("fn f1()"));

    let full = repo.search(&["target", "--full"]);
    assert!(full.stdout.contains("calls: 35 lines in 1 file\n"));
    assert!(
        full.stdout
            .contains("src/many.rs:2: [f1] fn f1() { target(); }\n"),
        "{}",
        full.stdout
    );
    assert!(
        full.stdout
            .contains("src/many.rs:36: [f35] fn f35() { target(); }\n")
    );
}

#[test]
fn all_lists_every_section_and_a_regex_implies_it() {
    let repo = launch_repo();
    let expected = "def src/workers/launch.rs:1-5: pub fn launch_worker(\n\
         calls: 1 line in 1 file\n\
         src/commands/worker.rs:6: [run] launch_worker(1);\n\
         imports: 1 line in 1 file\n\
         src/commands/worker.rs:1: use crate::workers::launch::launch_worker;\n\
         strings: 1 line in 1 file\n\
         src/commands/worker.rs:5: [run] let s = \"launch_worker\";\n\
         comments: 1 line in 1 file\n\
         src/commands/worker.rs:3: [top] // launch_worker starts one pane\n\
         text: 3 lines in 3 files\n\
         .env:1: SECRET=launch_worker\n\
         docs/design.md:3: launch_worker starts one pane\n\
         notes.txt:1: launch_worker=1\n\
         partial: 1 line in 1 file\n\
         src/workers/launch.rs:7: pub fn launch_worker_pool() {}\n\
         total: 9 lines in 5 files, all listed or counted; 5 files searched, 0 left out\n";
    let run = repo.search(&["launch_worker", "--all"]);
    assert_eq!(run.code, 0);
    assert_eq!(run.stdout, expected);

    // `-E` with an identifier-shaped pattern is not a bare identifier: it lists
    // every section without `--all`.
    let regex = repo.search(&["-E", "launch_worker"]);
    assert_eq!(regex.code, 0);
    assert!(regex.stdout.contains("imports: 1 line in 1 file\n"));
    assert!(regex.stdout.contains("text: 3 lines in 3 files\n"));
    assert!(!regex.stdout.contains("other:"));
}

#[test]
fn context_lines_use_the_path_dash_line_dash_text_form_with_a_separator() {
    let repo = launch_repo();
    let run = repo.search(&["-C", "1", "launch_worker", "--all"]);
    assert_eq!(run.code, 0);
    let lines = run.lines();
    assert!(
        lines.contains(&"src/commands/worker.rs-5-    let s = \"launch_worker\";"),
        "{}",
        run.stdout
    );
    assert!(lines.contains(&"src/commands/worker.rs:6: [run] launch_worker(1);"));
    assert!(lines.contains(&"src/commands/worker.rs-7-}"));
    assert!(lines.contains(&"--"), "a separator between blocks");
    // The attached form gives the same output.
    let attached = repo.search(&["-C1", "launch_worker", "--all"]);
    assert_eq!(attached.stdout, run.stdout);
}

// ============================================================================
// -l and -c
// ============================================================================

#[test]
fn dash_l_and_dash_c_print_rg_forms_in_path_order_and_the_summary_on_stderr() {
    let repo = launch_repo();
    let files = repo.search(&["-l", "launch_worker"]);
    assert_eq!(files.code, 0);
    assert_eq!(
        files.stdout,
        ".env\ndocs/design.md\nnotes.txt\nsrc/commands/worker.rs\nsrc/workers/launch.rs\n"
    );
    assert_eq!(
        files.stderr,
        "skim search: total: 9 lines in 5 files; 5 files searched, 0 left out\n"
    );

    let counts = repo.search(&["-c", "launch_worker"]);
    assert_eq!(
        counts.stdout,
        ".env:1\ndocs/design.md:1\nnotes.txt:1\nsrc/commands/worker.rs:4\nsrc/workers/launch.rs:2\n"
    );
    assert_eq!(counts.stderr, files.stderr);
}

#[test]
fn dash_l_and_dash_c_list_every_file_and_ignore_limit_and_offset() {
    let repo = launch_repo();
    for flag in ["-l", "-c"] {
        let plain = repo.search(&[flag, "launch_worker"]);
        let paged = repo.search(&[flag, "launch_worker", "--limit", "1", "--offset", "2"]);
        assert_eq!(paged.code, 0);
        assert_eq!(paged.stdout, plain.stdout, "{flag} ignores paging");
    }
    let help = repo.search(&["--help"]);
    assert!(help.stdout.contains("--limit and --offset do"));
}

// ============================================================================
// Which files are searched
// ============================================================================

#[test]
fn a_lockfile_a_text_file_and_an_extensionless_script_are_searched() {
    let repo = SearchRepo::new();
    repo.write("Cargo.lock", "name = \"needle\"\n");
    repo.write("notes.txt", "needle\n");
    repo.write("bin/run", "#!/bin/sh\necho needle\n");
    repo.write("src/lib.rs", "pub fn other() {}\n");
    repo.track_all();
    let run = repo.search(&["-l", "needle"]);
    assert_eq!(run.code, 0);
    assert_eq!(run.stdout, "Cargo.lock\nbin/run\nnotes.txt\n");
}

#[test]
fn a_toml_file_with_a_backslash_before_a_line_end_does_not_stop_the_query() {
    let repo = SearchRepo::new();
    repo.write("conf/path.toml", "needle = \"C:\\\n\"\n");
    repo.write("src/lib.rs", "pub fn needle() {}\n");
    repo.track_all();

    let run = repo.search(&["needle", "--all"]);

    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stderr, "");
    assert_eq!(
        run.stdout,
        "def src/lib.rs:1-1: pub fn needle() {}\n\
         text: 1 line in 1 file\n\
         conf/path.toml:1: needle = \"C:\\\n\
         total: 2 lines in 2 files, all listed or counted; 2 files searched, 0 left out\n"
    );
}

#[test]
fn a_tracked_env_file_is_listed_and_a_glob_leaves_it_out() {
    let repo = launch_repo();
    let listed = repo.search(&["-l", "launch_worker"]);
    assert!(
        listed.stdout.lines().any(|l| l == ".env"),
        "{}",
        listed.stdout
    );

    let without = repo.search(&["-l", "launch_worker", "-g", "!.env"]);
    assert!(!without.stdout.lines().any(|l| l == ".env"));
    assert_eq!(without.code, 0);

    // The help text states the rule.
    let help = repo.search(&["--help"]);
    assert_eq!(help.code, 0);
    assert!(help.stdout.contains(".env"));
    assert!(help.stdout.contains("may hold secrets"));
    assert!(help.stdout.contains("-g '!NAME'"));
}

#[test]
fn an_untracked_file_is_searched_unless_an_ignore_file_leaves_it_out() {
    let repo = SearchRepo::new();
    repo.write(".gitignore", "build/\n");
    repo.write("src/a.rs", "pub fn needle() {}\n");
    repo.track_all();
    repo.write("fresh.rs", "needle\n");
    repo.write("build/gen.rs", "needle\n");
    let run = repo.search(&["-l", "needle"]);
    assert_eq!(run.code, 0);
    assert_eq!(run.stdout, "fresh.rs\nsrc/a.rs\n");
}

#[cfg(unix)]
#[test]
fn files_that_cannot_be_searched_are_counted_under_their_reason() {
    use std::os::unix::fs::PermissionsExt as _;

    let repo = SearchRepo::new();
    repo.write("ok.txt", "needle\n");
    repo.write("blob.bin", b"needle\0binary\n");
    repo.write("latin.txt", b"needle \xff\xfe\n");
    let mut big = vec![b'a'; 5 * 1024 * 1024 + 100];
    big.extend_from_slice(b"\nneedle\n");
    repo.write("big.txt", big);
    repo.write("locked.txt", "needle\n");
    repo.write("gone.txt", "needle\n");
    std::os::unix::fs::symlink("ok.txt", repo.root().join("link.txt")).unwrap();
    repo.track_all();
    fs::remove_file(repo.root().join("gone.txt")).unwrap();
    let locked = repo.root().join("locked.txt");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    // A user that can read a file without permission (root) cannot make one
    // unreadable, so the count of that reason changes with the user.
    let readable_anyway = fs::read(&locked).is_ok();

    let run = repo.search(&["needle", "--json"]);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(run.code, 0);
    let json = json_of(&run);
    let left_out = &json["left_out"];
    assert_eq!(left_out["binary"], 1);
    assert_eq!(left_out["not_utf8"], 1);
    assert_eq!(left_out["too_large"], 1);
    assert_eq!(left_out["vanished"], 1);
    assert_eq!(left_out["symlink"], 1);
    assert_eq!(left_out["unreadable"], if readable_anyway { 0 } else { 1 });
    let searched = if readable_anyway { 2 } else { 1 };
    assert_eq!(json["searched_files"], searched);

    // The text form names the same reasons.
    let text = repo.search(&["-l", "needle"]);
    assert!(text.stderr.contains("left out ("), "{}", text.stderr);
    assert!(text.stderr.contains("1 binary"));
    assert!(text.stderr.contains("1 not_utf8"));
    assert!(text.stderr.contains("1 too_large"));
    assert!(text.stderr.contains("1 vanished"));
    assert!(text.stderr.contains("1 symlink"));
}

/// A repository with one searchable file and an untracked folder that cannot
/// be listed. The flag is true when the user can list it anyway (root), so the
/// folder gives no walk error.
#[cfg(unix)]
fn repo_with_unlistable_folder() -> (SearchRepo, std::path::PathBuf, bool) {
    use std::os::unix::fs::PermissionsExt as _;

    let repo = SearchRepo::new();
    repo.write("ok.txt", "needle\n");
    repo.track_all();
    let locked = repo.root().join("locked");
    fs::create_dir(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let listable_anyway = fs::read_dir(&locked).is_ok();
    (repo, locked, listable_anyway)
}

/// Let the temporary folder be removed again.
#[cfg(unix)]
fn unlock(folder: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt as _;

    fs::set_permissions(folder, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn debug_lines_print_only_with_skim_debug_and_carry_the_debug_prefix() {
    let (repo, locked, listable_anyway) = repo_with_unlistable_folder();

    let mut cmd = repo.skim();
    cmd.env("SKIM_DEBUG", "1").args(["search", "needle"]);
    let debug = finish(&mut cmd);
    let plain = repo.search(&["needle"]);
    unlock(&locked);

    assert_eq!(debug.code, 0, "{}", debug.stderr);
    for stage in [
        "list:",
        "read and match:",
        "parse and label:",
        "rank and render:",
    ] {
        let line = format!("skim search [debug]: {stage}");
        assert!(debug.stderr.contains(&line), "{line:?}: {}", debug.stderr);
    }
    if !listable_anyway {
        // The error text is quoted, so a path with a control character or a
        // newline cannot start a second line.
        let line = debug
            .stderr
            .lines()
            .find(|l| l.starts_with("skim search [debug]: walk error: "))
            .unwrap_or_else(|| panic!("no walk error line: {}", debug.stderr));
        let text = line.trim_start_matches("skim search [debug]: walk error: ");
        assert!(
            text.starts_with('"') && text.ends_with('"'),
            "the error text is quoted: {line}"
        );
        assert!(text.contains("locked"), "{line}");
    }
    // A debug line never takes the form of a line that is always on.
    assert!(
        !debug.stderr.contains("skim search: walk error"),
        "{}",
        debug.stderr
    );
    assert!(!plain.stderr.contains("[debug]"), "{}", plain.stderr);
}

#[cfg(unix)]
#[test]
fn a_folder_that_cannot_be_listed_is_counted_as_unreadable() {
    let (repo, locked, listable_anyway) = repo_with_unlistable_folder();

    let run = repo.search(&["needle", "--json"]);
    unlock(&locked);

    assert_eq!(run.code, 0, "{}", run.stderr);
    let json = json_of(&run);
    // The folder is one path the walk could not offer. A user that can list
    // any folder (root) has none.
    let unreadable = if listable_anyway { 0 } else { 1 };
    assert_eq!(json["left_out"]["unreadable"], unreadable);
    assert_eq!(json["searched_files"], 1);
}

// ============================================================================
// Flags
// ============================================================================

/// Five lines that differ in case and in how `alpha` sits in the line.
fn flags_repo() -> SearchRepo {
    let repo = SearchRepo::new();
    repo.write("x.rs", "alpha\nAlpha\nALPHA\nalphabet\nx alpha_y\n");
    repo.write("y.py", "alpha\n");
    repo.write("z.md", "# alpha\n");
    repo.track_all();
    repo
}

#[test]
fn the_match_flags_select_the_lines_that_a_regex_predicts() {
    let repo = flags_repo();
    let cases: [(&[&str], usize); 9] = [
        (&["alpha"], 3),
        (&["-i", "alpha"], 5),
        (&["-S", "alpha"], 5),
        (&["-S", "Alpha"], 1),
        (&["-i", "-S", "Alpha"], 5),
        (&["-w", "alpha"], 1),
        (&["-E", "alpha$"], 1),
        (&["-E", "a.pha"], 3),
        (&["-F", "alpha_y"], 1),
    ];
    for (args, expected) in cases {
        let mut full = vec!["-c"];
        full.extend_from_slice(args);
        let run = repo.search(&full);
        assert_eq!(run.code, 0, "{args:?}");
        assert_eq!(count_of(&run, "x.rs"), Some(expected), "{args:?}");
    }
    // A fixed string reads `.` as a dot.
    let none = repo.search(&["-c", "-F", "a.pha"]);
    assert_eq!(none.code, 1);
}

#[test]
fn glob_and_type_filters_keep_the_files_they_name() {
    let repo = flags_repo();
    let cases: [(&[&str], &str); 7] = [
        (&["-t", "rust"], "x.rs\n"),
        (&["-t", "py"], "y.py\n"),
        (&["-t", "rust", "-t", "py"], "x.rs\ny.py\n"),
        (&["-g", "*.py"], "y.py\n"),
        (&["-g", "!*.py", "-g", "!*.md"], "x.rs\n"),
        (&["-g", "x.rs", "-t", "py"], ""),
        (&["-g", "z.*"], "z.md\n"),
    ];
    for (args, expected) in cases {
        let mut full = vec!["-l", "alpha"];
        full.extend_from_slice(args);
        let run = repo.search(&full);
        assert_eq!(run.stdout, expected, "{args:?}");
        assert_eq!(
            run.code,
            if expected.is_empty() { 1 } else { 0 },
            "{args:?}"
        );
    }
}

#[test]
fn limit_and_offset_page_the_files_of_the_listed_sections() {
    let repo = SearchRepo::new();
    for name in ["a.txt", "b.txt", "c.txt"] {
        repo.write(name, "needle here\n");
    }
    repo.track_all();
    let all = repo.search(&["needle", "--all"]);
    let every = listed_paths(&all.stdout);
    assert_eq!(every, ["a.txt", "b.txt", "c.txt"]);

    let mut paged = Vec::new();
    for offset in ["0", "1", "2"] {
        let page = repo.search(&["needle", "--all", "--limit", "1", "--offset", offset]);
        assert_eq!(page.code, 0);
        paged.extend(listed_paths(&page.stdout));
    }
    assert_eq!(paged, every, "the pages join into the full list");
}

#[test]
fn a_literal_with_a_bracket_is_found_without_dash_e() {
    let repo = SearchRepo::new();
    repo.write("src/a.rs", "fn main() {\n    foo(1);\n    bar();\n}\n");
    repo.track_all();
    let run = repo.search(&["foo(", "-c"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "src/a.rs:1\n");

    let regex = repo.search(&["-E", "foo("]);
    assert_eq!(regex.code, 2);
    assert!(regex.stderr.contains("invalid pattern \"foo(\""));
}

#[test]
fn an_empty_pattern_is_refused_and_an_empty_match_regex_counts_every_line() {
    let repo = SearchRepo::new();
    repo.write("a.txt", "one\ntwo\n\nthree\n");
    repo.track_all();
    let empty = repo.search(&[""]);
    assert_eq!(empty.code, 2);
    assert_eq!(empty.stdout, "");
    assert_eq!(empty.stderr, "skim search: the pattern is empty\n");

    let every = repo.search(&["-E", "x*", "-c"]);
    assert_eq!(every.code, 0);
    assert_eq!(every.stdout, "a.txt:4\n");
}

// ============================================================================
// Exit codes
// ============================================================================

#[test]
fn exit_codes_and_messages() {
    let repo = launch_repo();
    let hit = repo.search(&["launch_worker"]);
    assert_eq!(hit.code, 0);

    let miss = repo.search(&["zzzzqqq"]);
    assert_eq!(miss.code, 1);
    assert_eq!(miss.stdout, "");
    assert_eq!(
        miss.stderr,
        "skim search: no match for \"zzzzqqq\" (5 files searched, 0 left out)\n"
    );

    let cases: [(&[&str], &str); 7] = [
        (&["-E", "("], "invalid pattern \"(\""),
        (
            &["x", "--root", "/nonexistent-folder-for-skim-test"],
            "--root",
        ),
        (
            &["x", "-t", "nosuchtype"],
            "unknown file type \"nosuchtype\"",
        ),
        (&["one", "two"], "one pattern only"),
        (&["-l", "-c", "x"], "-l and -c cannot be combined"),
        (&["--bogus", "x"], "unknown flag \"--bogus\""),
        (&["x", "--limit", "0"], "--limit"),
    ];
    for (args, message) in cases {
        let run = repo.search(args);
        assert_eq!(run.code, 2, "{args:?}");
        assert_eq!(run.stdout, "", "{args:?}");
        assert!(
            run.stderr.starts_with("skim search: ") && run.stderr.contains(message),
            "{args:?}: {}",
            run.stderr
        );
    }
}

#[test]
fn a_root_that_is_a_file_is_refused() {
    let repo = launch_repo();
    let run = repo.search(&["x", "--root", "notes.txt"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("is not a directory"), "{}", run.stderr);
}

#[test]
fn an_unreadable_git_index_exits_2_and_prints_no_result() {
    let repo = launch_repo();
    fs::write(repo.root().join(".git/index"), b"this is not an index").unwrap();
    let run = repo.search(&["launch_worker"]);
    assert_eq!(run.code, 2);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.starts_with("skim search: "), "{}", run.stderr);
}

#[cfg(unix)]
#[test]
fn a_closed_stdout_pipe_gives_the_pipe_closed_exit_code_and_no_error_text() {
    let repo = launch_repo();
    let mut child = repo
        .std_search_command(&["launch_worker", "--all"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Close the read end before the child writes its first byte.
    drop(child.stdout.take());
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(141));
    assert_eq!(stderr, "");
}

// ============================================================================
// JSON
// ============================================================================

#[test]
fn json_output_is_one_object_with_the_documented_keys() {
    let repo = launch_repo();
    let run = repo.search(&["launch_worker", "--json"]);
    assert_eq!(run.code, 0);
    assert_eq!(run.stderr, "");
    let json = json_of(&run);
    let object = json.as_object().unwrap();
    for key in [
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
    ] {
        assert!(object.contains_key(key), "missing key {key}");
    }
    assert_eq!(json["query"], "launch_worker");
    assert_eq!(json["view"], "labelled");
    assert_eq!(json["total_lines"], 9);
    assert_eq!(json["total_files"], 5);
    assert_eq!(json["has_more"], false);
    assert_eq!(json["definitions"]["count"], 1);
    assert_eq!(
        json["definitions"]["sites"][0]["path"],
        "src/workers/launch.rs"
    );
    assert_eq!(json["definitions"]["sites"][0]["first_line"], 1);
    assert_eq!(json["definitions"]["sites"][0]["end_line"], 5);
    let files = json["files"].as_array().unwrap();
    assert_eq!(files.len(), 5);
    assert_eq!(files[0]["path"], "src/workers/launch.rs");
    let scores: Vec<f64> = files.iter().map(|f| f["score"].as_f64().unwrap()).collect();
    assert!(
        scores.windows(2).all(|w| w[0] >= w[1]),
        "score does not rise down the list: {scores:?}"
    );
}

#[test]
fn a_usage_error_under_json_prints_an_error_object_and_exits_2() {
    let repo = launch_repo();
    let run = repo.search(&["-l", "-c", "x", "--json"]);
    assert_eq!(run.code, 2);
    let json = json_of(&run);
    assert_eq!(json["error"], "-l and -c cannot be combined");
    assert!(run.stderr.contains("-l and -c cannot be combined"));
}

#[test]
fn two_runs_of_one_query_give_equal_cost_counters() {
    let repo = launch_repo();
    let first = json_of(&repo.search(&["launch_worker", "--json"]));
    let second = json_of(&repo.search(&["launch_worker", "--json"]));
    assert_eq!(first["cost"], second["cost"]);
    assert_eq!(first["parse"], second["parse"]);
    assert!(first["cost"]["files_read"].as_u64().unwrap() >= 5);
    assert!(first["cost"]["bytes_read"].as_u64().unwrap() > 0);
}

/// A repository of TypeScript files where `ZodType` has each role, in files
/// large enough that a parse takes measurable time.
fn zod_type_repo() -> SearchRepo {
    let repo = SearchRepo::new();
    let filler: String = (0..1_500)
        .map(|j| format!("export function f{j}(a: number): number {{ return a + {j}; }}\n"))
        .collect();
    for i in 0..24 {
        let hits = match i % 4 {
            0 => "export function ZodType(x: number): number {\n  return x;\n}\n".to_owned(),
            1 => "export function run(): number {\n  return ZodType(1);\n}\n".repeat(1 + i % 3),
            2 => "import { ZodType } from \"./z0\";\nlet t: ZodType;\n".to_owned(),
            _ => "// ZodType note\nconst s = \"ZodType\";\n".repeat(1 + i % 5),
        };
        repo.write(&format!("src/z{i}.ts"), format!("{hits}{filler}"));
    }
    repo.track_all();
    repo
}

/// The labels, and so the order, depend only on the files: the same query
/// under parallel load gives one output.
#[test]
fn one_query_run_many_times_under_parallel_load_gives_one_order() {
    const RUNS: usize = 16;
    const ROUNDS: usize = 2;
    let repo = zod_type_repo();
    let args = ["ZodType", "--all"];
    let reference = repo.search(&args);
    assert_eq!(reference.code, 0, "{}", reference.stderr);
    // Every file is labelled: 6 definitions, and the 18 other files listed.
    assert!(
        reference.stdout.contains("note: 6 definitions of ZodType"),
        "{}",
        reference.stdout
    );
    assert_eq!(
        listed_paths(&reference.stdout).len(),
        18,
        "{}",
        reference.stdout
    );
    for _ in 0..ROUNDS {
        let runs: Vec<Run> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..RUNS)
                .map(|_| scope.spawn(|| repo.search(&args)))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for run in runs {
            assert_eq!(run.code, reference.code);
            assert_eq!(run.stdout, reference.stdout);
            assert_eq!(run.stderr, reference.stderr);
        }
    }
}

// ============================================================================
// Definitions
// ============================================================================

#[test]
fn two_rust_definitions_of_one_name_print_both_and_the_clash_note() {
    let repo = SearchRepo::new();
    repo.write("a/one.rs", "pub fn dup() {}\n");
    repo.write("b/two.rs", "pub fn dup() {}\nfn t() { dup(); }\n");
    repo.track_all();
    let run = repo.search(&["dup"]);
    assert_eq!(run.code, 0);
    let lines = run.lines();
    assert!(
        lines.contains(&"def a/one.rs:1-1: pub fn dup() {}"),
        "{}",
        run.stdout
    );
    assert!(lines.contains(&"def b/two.rs:1-1: pub fn dup() {}"));
    assert!(lines.contains(&"note: 2 definitions of dup; calls are not split by definition"));
    let json = json_of(&repo.search(&["dup", "--json"]));
    assert_eq!(json["definitions"]["count"], 2);
}

#[test]
fn two_typescript_definitions_of_one_name_print_the_same_note() {
    let repo = SearchRepo::new();
    repo.write("a/one.ts", "export function dup(): void {}\n");
    repo.write("b/two.ts", "export const dup = () => 1;\n");
    repo.track_all();
    let run = repo.search(&["dup"]);
    assert_eq!(run.code, 0);
    let lines = run.lines();
    assert!(
        lines.contains(&"def a/one.ts:1-1: export function dup(): void {}"),
        "{}",
        run.stdout
    );
    assert!(lines.contains(&"def b/two.ts:1-1: export const dup = () => 1;"));
    assert!(lines.contains(&"note: 2 definitions of dup; calls are not split by definition"));
}

/// One file per language, each with a definition that spans several lines.
const SYMBOL_FIXTURES: [(&str, &str, &str); 6] = [
    (
        "a.rs",
        "launch_item",
        "// header\npub fn launch_item(a: u32) -> u32 {\n    a\n}\n",
    ),
    (
        "a.ts",
        "launchItem",
        "// header\nexport class Pool {\n  start(): void {}\n}\nexport function launchItem(a: number): number {\n  return a;\n}\n",
    ),
    (
        "a.py",
        "launch_item",
        "# header\ndef launch_item(a):\n    \"\"\"doc\"\"\"\n    return a\n",
    ),
    (
        "a.go",
        "LaunchItem",
        "package main\n\nfunc LaunchItem(a int) int {\n\treturn a\n}\n",
    ),
    (
        "A.java",
        "launchItem",
        "class A {\n    int launchItem(int a) {\n        return a;\n    }\n}\n",
    ),
    (
        "a.rb",
        "launch_item",
        "# header\ndef launch_item(a)\n  a\nend\n",
    ),
];

#[test]
fn each_definition_site_resolves_through_symbol_to_the_same_line_range() {
    for (file, name, source) in SYMBOL_FIXTURES {
        let repo = SearchRepo::new();
        repo.write(file, source);
        repo.track_all();
        let json = json_of(&repo.search(&[name, "--json"]));
        let sites = json["definitions"]["sites"].as_array().unwrap();
        assert_eq!(sites.len(), 1, "{file}: {json}");
        let first = sites[0]["first_line"].as_u64().unwrap() as usize;
        let end = sites[0]["end_line"].as_u64().unwrap() as usize;

        let mut cmd = repo.skim();
        cmd.arg(file).args(["--symbol", name]);
        let read = common::search_repo::finish(&mut cmd);
        assert_eq!(read.code, 0, "{file}: {}", read.stderr);
        let expected: Vec<&str> = source
            .lines()
            .skip(first - 1)
            .take(end - first + 1)
            .collect();
        assert_eq!(read.stdout.lines().collect::<Vec<_>>(), expected, "{file}");
        assert!(
            read.stderr.contains(&format!("lines {first}-{end} of")),
            "{file}: {}",
            read.stderr
        );
    }
}

// ============================================================================
// The word view
// ============================================================================

#[test]
fn a_word_query_lists_the_files_that_hold_all_the_words_by_prior_then_count() {
    let repo = SearchRepo::new();
    repo.write("src/a.rs", "// alpha beta gamma\n");
    repo.write("src/f.rs", "// alpha beta gamma\n// alpha beta gamma\n");
    repo.write("docs/b.md", "alpha beta gamma\n");
    repo.write("tests/c.rs", "alpha\nbeta\ngamma\nalpha beta gamma\n");
    repo.write("src/d.rs", "alpha beta\n");
    repo.track_all();
    let run = repo.search(&["alpha beta gamma"]);
    assert_eq!(run.code, 0);
    assert_eq!(
        word_headers(&run.stdout),
        ["src/f.rs", "src/a.rs", "tests/c.rs", "docs/b.md"],
        "source before tests before docs; more word hits first within a prior\n{}",
        run.stdout
    );
    assert!(!run.stdout.contains("note:"), "{}", run.stdout);
    assert!(run.stdout.contains("files: 4 hold all 3 words; 4 listed\n"));
}

#[test]
fn a_word_query_with_fewer_than_three_full_files_adds_files_with_fewer_words() {
    let repo = SearchRepo::new();
    repo.write("src/a.rs", "alpha beta gamma\n");
    repo.write("docs/b.md", "alpha beta gamma\n");
    repo.write("src/d.rs", "alpha beta\n");
    repo.write("src/e.rs", "alpha\n");
    repo.track_all();
    let run = repo.search(&["alpha beta gamma"]);
    assert_eq!(run.code, 0);
    assert!(
        run.stdout.contains(
            "note: only 2 files hold all 3 words; files with fewer words follow (most words first)\n"
        ),
        "{}",
        run.stdout
    );
    let headers = word_headers(&run.stdout);
    assert_eq!(headers.len(), 4, "{}", run.stdout);
    let at = |name: &str| headers.iter().position(|h| h == name).unwrap();
    assert!(at("src/a.rs") < at("src/d.rs") && at("docs/b.md") < at("src/d.rs"));
    assert!(at("src/d.rs") < at("src/e.rs"), "most words first");
}

// ============================================================================
// The root
// ============================================================================

#[test]
fn a_query_from_a_subfolder_prints_the_root_once_on_stderr() {
    let repo = launch_repo();
    let sub = repo.root().join("src");
    let run = repo.search_in(&sub, &["-l", "launch_worker"]);
    assert_eq!(run.code, 0);
    let notice = format!(
        "skim search: searching \"{}\" (the repository that holds the current folder)\n",
        repo.canonical_root().display()
    );
    assert_eq!(run.stderr.matches(&notice).count(), 1, "{}", run.stderr);
    // The whole repository is searched, not the subfolder.
    assert!(run.stdout.lines().any(|l| l == "notes.txt"));

    // No notice when the working folder is the root, or when --root is given.
    let at_root = repo.search(&["-l", "launch_worker"]);
    assert!(!at_root.stderr.contains("searching"), "{}", at_root.stderr);
    let explicit = repo.search_in(&sub, &["-l", "launch_worker", "--root", "."]);
    assert!(
        !explicit.stderr.contains("searching"),
        "{}",
        explicit.stderr
    );
    assert!(!explicit.stdout.lines().any(|l| l == "notes.txt"));
}

#[test]
fn the_filesystem_root_is_not_searched_without_dash_dash_root() {
    let repo = launch_repo();
    let run = repo.search_in(std::path::Path::new("/"), &["launch_worker"]);
    assert_eq!(run.code, 2);
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr.contains("refusing to search /"),
        "{}",
        run.stderr
    );
}

/// A repository whose token sits only in the tracked hidden file `src/.env`.
/// The walk skips a hidden file, so only the git index lists it.
fn hidden_token_repo() -> SearchRepo {
    let repo = SearchRepo::new();
    repo.write("src/lib.rs", "pub fn plain() {}\n");
    repo.write("src/.env", "TOKEN=zq7needle\n");
    repo.track_all();
    repo
}

/// A `GIT_CEILING_DIRECTORIES` value of `count` absolute entries.
#[cfg(unix)]
fn ceiling_entries(count: usize) -> std::ffi::OsString {
    std::env::join_paths((0..count).map(|n| format!("/no-such-ceiling/{n}"))).unwrap()
}

#[cfg(unix)]
#[test]
fn ceiling_entries_past_the_bound_print_their_exact_count_for_an_explicit_root() {
    let repo = hidden_token_repo();
    let explicit_root_with = |entries: usize| {
        let mut cmd = repo.skim();
        cmd.env("GIT_CEILING_DIRECTORIES", ceiling_entries(entries))
            .args(["search", "-l", "--root", "src", "zq7needle"]);
        finish(&mut cmd)
    };

    // At the bound every entry is read, and the index lists the hidden file.
    let at_bound = explicit_root_with(64);
    assert_eq!(at_bound.code, 0, "{}", at_bound.stderr);
    assert!(
        !at_bound.stderr.contains("GIT_CEILING_DIRECTORIES"),
        "{}",
        at_bound.stderr
    );
    assert_eq!(at_bound.stdout, ".env\n");

    // One entry past the bound: the walk does not go up, the hidden file is
    // not listed, and the note says how many entries were dropped.
    let over = explicit_root_with(65);
    assert_eq!(over.code, 1, "{}", over.stderr);
    let note = "skim search: GIT_CEILING_DIRECTORIES has 65 entries; \
                the search reads the first 64 and drops 1.";
    assert!(over.stderr.contains(note), "{}", over.stderr);
    assert_eq!(over.stderr.matches("GIT_CEILING_DIRECTORIES").count(), 1);
}

#[cfg(unix)]
#[test]
fn ceiling_entries_past_the_bound_print_their_exact_count_for_the_implicit_root() {
    let repo = hidden_token_repo();
    let mut cmd = repo.skim();
    cmd.current_dir(repo.root().join("src"))
        .env("GIT_CEILING_DIRECTORIES", ceiling_entries(70))
        .args(["search", "-l", "zq7needle"]);
    let run = finish(&mut cmd);

    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(
        run.stderr
            .contains("has 70 entries; the search reads the first 64 and drops 6."),
        "{}",
        run.stderr
    );
}

#[test]
fn a_repository_with_no_head_file_exits_2_for_an_explicit_root_below_it() {
    let repo = hidden_token_repo();
    fs::remove_file(repo.root().join(".git/HEAD")).unwrap();

    let run = repo.search(&["--root", "src", "zq7needle"]);
    assert_eq!(run.code, 2, "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr
            .starts_with("skim search: cannot read the repository"),
        "{}",
        run.stderr
    );
    assert!(run.stderr.contains("HEAD"), "{}", run.stderr);

    // With --json the error is on stdout, as for every other exit 2.
    let json = repo.search(&["--root", "src", "--json", "zq7needle"]);
    assert_eq!(json.code, 2, "{}", json.stderr);
    let error = json_of(&json);
    assert!(error["error"].as_str().is_some_and(|m| m.contains("HEAD")));

    // The top of the same repository gives the same exit code.
    let top = repo.search(&["zq7needle"]);
    assert_eq!(top.code, 2, "{}", top.stderr);
    assert_eq!(top.stdout, "");
}

#[test]
fn a_tracked_file_that_became_a_folder_is_counted_as_vanished() {
    let repo = SearchRepo::new();
    repo.write("ok.txt", "needle\n");
    repo.write("was_a_file.txt", "needle\n");
    repo.track_all();
    fs::remove_file(repo.root().join("was_a_file.txt")).unwrap();
    fs::create_dir(repo.root().join("was_a_file.txt")).unwrap();

    let run = repo.search(&["needle", "--json"]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    let json = json_of(&run);
    assert_eq!(json["left_out"]["vanished"], 1);
    assert_eq!(json["searched_files"], 1);
}
