//! Integration tests: `skim search` treats files, paths, flags and patterns as
//! untrusted input.
//!
//! Each test builds a small git repository and runs the real binary. The
//! checks cover the flags that start a program, paths that point outside the
//! root, caps on output, unusual path names, the root walk, analytics and
//! files that block a reader.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::search_repo::{Run, SearchRepo, finish};
use serde_json::Value;

/// The stdout of a run as one JSON object.
fn json_of(run: &Run) -> Value {
    assert_eq!(run.stdout.lines().count(), 1, "one line: {}", run.stdout);
    serde_json::from_str(&run.stdout).expect("stdout is one JSON object")
}

// ============================================================================
// Flags that would start a program
// ============================================================================

#[cfg(unix)]
#[test]
fn flags_that_start_a_program_exit_2_and_the_named_program_never_runs() {
    use std::os::unix::fs::PermissionsExt as _;

    let repo = SearchRepo::new();
    repo.write("a.txt", "needle\n");
    repo.track_all();

    let outside = tempfile::tempdir().unwrap();
    let marker = outside.path().join("ran");
    let script = outside.path().join("hook.sh");
    fs::write(
        &script,
        format!("#!/bin/sh\necho ran > '{}'\ncat\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let script = script.display().to_string();

    let cases: Vec<Vec<String>> = vec![
        vec!["--pre".into(), script.clone(), "needle".into()],
        vec![format!("--pre={script}"), "needle".into()],
        vec!["--pre-glob".into(), "*.txt".into(), "needle".into()],
        vec!["--hostname-bin".into(), script.clone(), "needle".into()],
        vec![format!("--hostname-bin={script}"), "needle".into()],
        vec!["-z".into(), "needle".into()],
        vec!["--search-zip".into(), "needle".into()],
        vec!["-P".into(), "needle".into()],
        vec!["--pcre2".into(), "needle".into()],
        vec!["--engine".into(), "pcre2".into(), "needle".into()],
        vec!["--engine=auto".into(), "needle".into()],
    ];
    for args in &cases {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let run = repo.search(&refs);
        assert_eq!(run.code, 2, "{args:?}");
        assert_eq!(run.stdout, "", "{args:?}");
        assert!(
            run.stderr.contains("unknown flag"),
            "{args:?}: {}",
            run.stderr
        );
        assert!(!marker.exists(), "{args:?} started a program");
    }
}

// ============================================================================
// Paths that point outside the root
// ============================================================================

#[cfg(unix)]
#[test]
fn a_tracked_path_below_a_symlinked_folder_is_outside_the_root() {
    let repo = SearchRepo::new();
    repo.write("sub/file.txt", "needle inside\n");
    repo.write("keep.txt", "needle kept\n");
    repo.track_all();

    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("file.txt"), "needle OUTSIDE-LINE\n").unwrap();
    fs::remove_dir_all(repo.root().join("sub")).unwrap();
    std::os::unix::fs::symlink(outside.path(), repo.root().join("sub")).unwrap();

    let json_run = repo.search(&["needle", "--json"]);
    assert_eq!(json_run.code, 0);
    assert!(
        !json_run.stdout.contains("OUTSIDE-LINE"),
        "{}",
        json_run.stdout
    );
    let json = json_of(&json_run);
    assert_eq!(json["left_out"]["outside_root"], 1);
    assert_eq!(json["total_lines"], 1);

    let text = repo.search(&["needle", "--all"]);
    assert!(!text.stdout.contains("OUTSIDE-LINE"), "{}", text.stdout);
    assert!(!text.stdout.contains("sub/file.txt"));
    assert!(text.stdout.contains("1 outside_root"), "{}", text.stdout);
}

#[test]
fn a_glob_with_an_absolute_path_or_a_parent_component_finds_no_file_outside_the_root() {
    let repo = SearchRepo::new();
    repo.write("a.txt", "needle\n");
    repo.track_all();
    let outside = tempfile::tempdir().unwrap();
    let file = outside.path().join("out.txt");
    fs::write(&file, "needle OUTSIDE-LINE\n").unwrap();
    let absolute = file.display().to_string();
    let parent = format!(
        "../{}/out.txt",
        outside.path().file_name().unwrap().to_string_lossy()
    );

    for glob in [absolute.as_str(), parent.as_str(), "../*", "/etc/passwd"] {
        let run = repo.search(&["-l", "needle", "-g", glob]);
        assert_eq!(run.code, 1, "{glob}: {}", run.stderr);
        assert_eq!(run.stdout, "", "{glob}");
        assert!(!run.stderr.contains("OUTSIDE"), "{glob}");
    }
}

#[test]
fn a_folder_glob_leaves_out_a_file_two_folders_below_it_as_rg_does() {
    let repo = SearchRepo::new();
    repo.write("src/a.rs", "needle\n");
    repo.write("vendor/lib/deep/c.rs", "needle\n");
    repo.write("docs/vendor/d.rs", "needle\n");
    repo.write("top.rs", "needle\n");
    repo.track_all();

    let run = repo.search(&["-l", "needle", "-g", "!vendor"]);
    assert_eq!(run.code, 0);
    assert_eq!(run.stdout, "src/a.rs\ntop.rs\n");

    // Where rg is installed, it gives the same files for the same glob.
    let rg = Command::new("rg")
        .current_dir(repo.root())
        .env_remove("RIPGREP_CONFIG_PATH")
        .args(["-l", "--no-config", "needle", "-g", "!vendor"])
        .output();
    if let Ok(rg) = rg
        && rg.status.code() == Some(0)
    {
        let mut theirs: Vec<String> = String::from_utf8_lossy(&rg.stdout)
            .lines()
            .map(str::to_owned)
            .collect();
        theirs.sort();
        assert_eq!(theirs.join("\n") + "\n", run.stdout);
    }
}

// ============================================================================
// Caps on the output
// ============================================================================

#[test]
fn the_caret_pattern_on_a_file_above_the_stored_line_cap_prints_the_marker_and_the_exact_total() {
    let repo = SearchRepo::new();
    let source: String = (1..=10_500).map(|i| format!("line {i}\n")).collect();
    repo.write("big.txt", source);
    repo.track_all();

    let run = repo.search(&["-E", "^"]);
    assert_eq!(run.code, 0);
    assert!(
        run.stdout
            .contains("total: 10500 lines in 1 file, all listed or counted;"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains(
            "note: 500 lines in 1 file are counted, not listed (stored-line cap 10000)\n"
        ),
        "{}",
        run.stdout
    );
    assert!(
        run.stderr.contains(
            "[skim] lines elided beyond 10000 of 10500 — use -c or -l for every file, or narrow the pattern"
        ),
        "{}",
        run.stderr
    );
    assert!(
        run.stdout.len() < 150_000,
        "output size {}",
        run.stdout.len()
    );

    // `-c` counts every line.
    let counts = repo.search(&["-E", "^", "-c"]);
    assert_eq!(counts.stdout, "big.txt:10500\n");
}

#[test]
fn context_stops_at_the_cap_and_the_marker_states_the_hidden_line_count() {
    let repo = SearchRepo::new();
    let mut source = String::new();
    for _ in 0..1100 {
        source.push_str("MARK\n");
        source.push_str(&"pad\n".repeat(200));
    }
    repo.write("ctx.txt", source);
    repo.track_all();

    let run = repo.search(&["-C", "100", "MARK", "--all"]);
    assert_eq!(run.code, 0);
    assert!(
        run.stdout
            .contains("note: 119900 context lines are not shown (context cap 100000)\n"),
        "{}",
        &run.stdout[run.stdout.len().saturating_sub(400)..]
    );
    assert!(
        run.stderr.contains(
            "[skim] context lines elided beyond 100000 of 219900 — use a smaller -C, or narrow the pattern"
        ),
        "{}",
        run.stderr
    );
    assert!(
        run.stdout
            .contains("total: 1100 lines in 1 file, all listed or counted;")
    );
    assert!(
        run.stdout.len() < 4_000_000,
        "output size {}",
        run.stdout.len()
    );
}

// ============================================================================
// Patterns
// ============================================================================

#[test]
fn a_regex_of_nested_thousand_fold_repeats_exits_2_with_the_pattern_and_no_panic() {
    let repo = SearchRepo::new();
    repo.write("a.txt", "needle\n");
    repo.track_all();
    let pattern = "((a{1000}){1000}){1000}";
    let started = Instant::now();
    let run = repo.search(&["-E", pattern]);
    assert_eq!(run.code, 2);
    assert_eq!(run.stdout, "");
    assert!(
        run.stderr
            .contains(&format!("invalid pattern \"{pattern}\"")),
        "{}",
        run.stderr
    );
    assert!(!run.stderr.contains("panicked"));
    assert!(started.elapsed() < Duration::from_secs(10));
}

// ============================================================================
// Path names
// ============================================================================

#[test]
fn a_path_with_a_newline_and_an_escape_byte_prints_as_one_quoted_path() {
    let repo = SearchRepo::new();
    repo.write("we\nird\u{1b}[31m.txt", "needle one\n");
    repo.write("ok.txt", "needle ok\n");
    repo.track_all();

    let run = repo.search(&["-E", "needle"]);
    assert_eq!(run.code, 0);
    assert!(!run.stdout.contains('\u{1b}'), "no raw ESC byte");
    assert!(
        run.stdout
            .contains("\"we\\nird\\033[31m.txt\":1: needle one\n"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("ok.txt:1: needle ok\n"));
    assert!(run.stdout.contains("total: 2 lines in 2 files,"));

    let files = repo.search(&["-l", "needle"]);
    assert!(!files.stdout.contains('\u{1b}'));
    assert_eq!(files.stdout.lines().count(), 2, "{:?}", files.stdout);
    let counts = repo.search(&["-c", "needle"]);
    assert!(!counts.stdout.contains('\u{1b}'));
    assert_eq!(counts.stdout.lines().count(), 2, "{:?}", counts.stdout);

    let json = json_of(&repo.search(&["needle", "--json"]));
    assert!(
        json["files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "we\nird\u{1b}[31m.txt")
    );
}

#[cfg(unix)]
#[test]
fn a_path_that_is_not_utf8_prints_with_octal_escapes_and_is_marked_lossy_in_json() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt as _;

    let repo = SearchRepo::new();
    let name = OsStr::from_bytes(b"bad\xff.txt");
    if fs::write(repo.root().join(name), "needle two\n").is_err() {
        // This file system rejects names that are not UTF-8.
        return;
    }
    repo.write("ok.txt", "needle ok\n");
    repo.track_all();

    let text = repo.search(&["-E", "needle"]);
    assert_eq!(text.code, 0);
    assert!(
        text.stdout.contains("\"bad\\377.txt\":1: needle two\n"),
        "{}",
        text.stdout
    );

    let json = json_of(&repo.search(&["needle", "--json"]));
    let lossy = json["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path_lossy"] == true)
        .expect("a lossy path is marked");
    assert!(lossy["path"].as_str().unwrap().starts_with("bad"));
}

// ============================================================================
// The root walk
// ============================================================================

/// Run `git init` and `git add -A` in `dir` with no user configuration.
fn git_init_and_add(dir: &Path, home: &Path) {
    for args in [&["init", "-q", "."][..], &["add", "-A"][..]] {
        let status = Command::new("git")
            .current_dir(dir)
            .env("HOME", home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
}

#[test]
fn a_folder_below_the_home_folder_that_holds_a_repository_searches_only_that_folder() {
    let repo = SearchRepo::new();
    let home = repo.home().to_path_buf();
    fs::create_dir_all(home.join("work")).unwrap();
    fs::write(home.join("top.txt"), "needle top\n").unwrap();
    fs::write(home.join("work/in.txt"), "needle in\n").unwrap();
    git_init_and_add(&home, &home);

    let run = repo.search_in(&home.join("work"), &["-l", "needle"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "in.txt\n");
    assert!(!run.stderr.contains("searching"), "{}", run.stderr);
}

#[test]
fn a_git_ceiling_stops_the_root_walk_and_a_folder_outside_a_repository_is_searched_alone() {
    let repo = SearchRepo::new();
    repo.write("top.txt", "needle top\n");
    repo.write("sub/deep/in.txt", "needle in\n");
    repo.track_all();
    let sub = repo.root().join("sub");
    let deep = sub.join("deep");

    // Without a ceiling, the walk finds the repository above the folder.
    let open = repo.search_in(&deep, &["-l", "needle"]);
    assert_eq!(open.stdout, "sub/deep/in.txt\ntop.txt\n");

    // A ceiling at `sub` stops the walk below it: the folder is the root.
    let mut cmd = repo.skim();
    cmd.current_dir(&deep)
        .env("GIT_CEILING_DIRECTORIES", sub.canonicalize().unwrap())
        .args(["search", "-l", "needle"]);
    let run = finish(&mut cmd);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "in.txt\n");

    // A folder with no repository above it is searched alone.
    let plain = tempfile::tempdir().unwrap();
    fs::write(plain.path().join("p.txt"), "needle plain\n").unwrap();
    let run = repo.search_in(plain.path(), &["-l", "needle"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "p.txt\n");
}

// ============================================================================
// Analytics
// ============================================================================

/// The row count of every table in the analytics database, by table name.
fn analytics_row_counts(db: &Path) -> BTreeMap<String, usize> {
    let conn = rusqlite::Connection::open(db).expect("the analytics database exists");
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let count: usize = conn
                .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
                    row.get(0)
                })
                .unwrap();
            (table, count)
        })
        .collect()
}

/// Every table of the analytics database holds no text value that contains `needle`.
fn assert_analytics_holds_no_text(db: &Path, needle: &str) {
    let conn = rusqlite::Connection::open(db).expect("the analytics database exists");
    for table in analytics_row_counts(db).keys() {
        let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\"")).unwrap();
        let columns = stmt.column_count();
        let mut rows = stmt.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for column in 0..columns {
                if let Ok(text) = row.get::<_, String>(column) {
                    assert!(
                        !text.contains(needle),
                        "table {table} holds the pattern: {text}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_query_records_no_analytics_row_and_none_holds_a_token_shaped_pattern() {
    let repo = SearchRepo::new();
    repo.write("a.rs", "pub fn needle() {}\n");
    repo.track_all();
    let db = repo.home().join("analytics.db");
    let pattern = format!("{}{}", "ghp_", "A".repeat(36));

    // A read of a file records a row: the database exists and is readable.
    // The process joins its background writers before it exits, so the row is
    // there when the run ends.
    let mut read = repo.skim();
    read.env_remove("SKIM_DISABLE_ANALYTICS")
        .env("SKIM_ANALYTICS_DB", &db)
        .arg("a.rs");
    assert_eq!(finish(&mut read).code, 0);
    let before = analytics_row_counts(&db);
    assert!(
        before.values().sum::<usize>() > 0,
        "the read of a file left a row to count: {before:?}"
    );

    let mut query = repo.skim();
    query
        .env_remove("SKIM_DISABLE_ANALYTICS")
        .env("SKIM_ANALYTICS_DB", &db)
        .args(["search", &pattern]);
    assert_eq!(finish(&mut query).code, 1);

    assert_eq!(
        analytics_row_counts(&db),
        before,
        "a query changed the rows of the analytics database"
    );
    assert_analytics_holds_no_text(&db, &pattern);
}

// ============================================================================
// Files that block a reader
// ============================================================================

#[cfg(unix)]
#[test]
fn a_tracked_path_replaced_by_a_fifo_is_unreadable_and_the_query_ends() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let repo = SearchRepo::new();
    repo.write("pipe.txt", "needle\n");
    repo.write("ok.txt", "needle\n");
    repo.track_all();
    let fifo = repo.root().join("pipe.txt");
    fs::remove_file(&fifo).unwrap();
    let c_path = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: `c_path` is a valid NUL-terminated path that lives for the call.
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
    assert_eq!(rc, 0, "mkfifo");

    let mut child = repo
        .std_search_command(&["needle", "--json"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the query did not end within 10 seconds");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let output = child.wait_with_output().unwrap();
    // ok.txt matches, so the query ends with a match.
    assert_eq!(
        status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["total_lines"], 1);
    assert_eq!(json["left_out"]["unreadable"], 1);
    assert_eq!(json["searched_files"], 1);
}
