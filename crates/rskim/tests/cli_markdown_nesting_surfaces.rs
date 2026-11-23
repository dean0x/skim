//! Raw Markdown output at the nesting limit on batch, Git and search reads.

mod common;
#[path = "common/markdown_nesting.rs"]
mod markdown;

use common::search_repo::SearchRepo;
use markdown::{clean, nested_report, run};
use serde_json::Value;
use std::fs;
use tempfile::TempDir;

fn commit(repo: &SearchRepo, message: &str) {
    repo.track_all();
    repo.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-qm",
        message,
    ]);
}

fn skim_git<'a>(
    cmd: &'a mut assert_cmd::Command,
    repo: &SearchRepo,
    args: &[&str],
) -> &'a mut assert_cmd::Command {
    clean(cmd)
        .current_dir(repo.root())
        .env("HOME", repo.home())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_EXTERNAL_DIFF")
        .env_remove("GIT_DIFF_OPTS")
        .env_remove("GIT_PAGER")
        .arg("git")
        .args(args)
}

#[test]
fn directory_and_glob_keep_raw_markdown_between_heading_views() {
    let dir = TempDir::new().unwrap();
    let files = ["a.md", "b.md", "c.md"].map(|name| dir.path().join(name));
    let prose = "This line gives the report some text below the heading.\n".repeat(20);
    let text = nested_report("Release notes");
    fs::write(&files[0], format!("# First\n\n{prose}")).unwrap();
    fs::write(&files[1], &text).unwrap();
    fs::write(&files[2], format!("# Last\n\n{prose}")).unwrap();
    let expected = format!(
        "// {}\n# First\n\n// {}\n{text}\n// {}\n# Last\n",
        files[0].display(),
        files[1].display(),
        files[2].display()
    );

    for input in [dir.path().to_path_buf(), dir.path().join("*.md")] {
        let output = run(clean(&mut common::skim()).arg("--no-cache").arg(input));
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    }
}

#[test]
fn git_show_blob_with_a_mode_reads_raw_markdown() {
    let repo = SearchRepo::new();
    let text = nested_report("Release notes");
    repo.write("report.md", &text);
    commit(&repo, "Add report");
    let db = repo.home().join("analytics.db");
    let args = ["show", "HEAD:report.md", "--mode", "structure"];

    let output = run(skim_git(
        &mut common::skim_with_analytics(&db),
        &repo,
        &args,
    ));
    assert_eq!(output.stdout, text.as_bytes());
    assert_eq!(output.stderr, b"");
    // A separate change owns the tier that this Git view reports.
    let tiers = rusqlite::Connection::open(db)
        .unwrap()
        .prepare("SELECT parse_tier FROM token_savings")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(tiers, ["full"]);

    let output = run(skim_git(&mut common::skim(), &repo, &args).env("SKIM_DEBUG", "1"));
    assert_eq!(output.stdout, text.as_bytes());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.lines().any(|line| {
            line == "[skim:guardrail] compressed output not strictly smaller; emitting raw"
        }),
        "{stderr}"
    );
}

#[test]
fn git_diff_full_prints_only_the_hunk_at_the_markdown_limit() {
    let repo = SearchRepo::new();
    // The indent reaches the estimate limit. The grammar reads it as code text.
    let indent = " ".repeat(397);
    let report = |body| format!("# Report\n\n```\n{indent}{body}\n```\n");
    repo.write("report.md", report("Old notes"));
    commit(&repo, "Add report");
    repo.write("report.md", report("New notes"));

    // With no hunk context, only a successful parse can add the heading and fence.
    let working = run(skim_git(
        &mut repo.skim(),
        &repo,
        &["diff", "--mode=full", "-U0"],
    ));
    commit(&repo, "Update report");
    let revision = run(skim_git(
        &mut repo.skim(),
        &repo,
        &["diff", "--mode=full", "-U0", "HEAD~1", "HEAD"],
    ));
    let expected = format!("report.md (modified)\n-4 {indent}Old notes\n+4 {indent}New notes\n");
    for output in [working, revision] {
        assert_eq!(String::from_utf8(output.stdout).unwrap(), expected);
    }
}

#[test]
fn search_keeps_the_text_match_when_it_has_no_labels() {
    let repo = SearchRepo::new();
    let text = nested_report("ReportNeedle");
    repo.write("report.md", &text);
    repo.track_all();

    let listed = run(clean(&mut repo.skim()).args(["search", "ReportNeedle", "-l"]));
    assert_eq!(listed.stdout, b"report.md\n");
    let output = run(clean(&mut repo.skim()).args(["search", "ReportNeedle", "--json"]));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let file = &report["files"][0];
    assert_eq!(report["total_lines"], 1);
    assert_eq!(report["parse"]["unlabelled_files"], 1);
    assert_eq!(file["path"], "report.md");
    assert_eq!(file["unlabelled_reason"], "parse_failed");
    assert_eq!(file["occurrences"][0]["text"], text.lines().last().unwrap());
}

#[test]
fn cached_markdown_keeps_the_raw_output_and_empty_stderr() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("report.md");
    let cache = dir.path().join("cache");
    let text = nested_report("Release notes");
    fs::write(&path, &text).unwrap();
    let read = || {
        run(clean(&mut common::skim())
            .env("SKIM_CACHE_DIR", &cache)
            .arg(&path))
    };

    let cold = read();
    let entry = fs::read_dir(&cache)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "json"))
        .expect("cache entry for report.md");
    // A second cold read would replace the entry and change this time.
    let mark = std::time::SystemTime::UNIX_EPOCH;
    fs::File::options()
        .write(true)
        .open(&entry)
        .unwrap()
        .set_modified(mark)
        .unwrap();
    let warm = read();
    assert_eq!(fs::metadata(entry).unwrap().modified().unwrap(), mark);
    assert_eq!(cold.stdout, text.as_bytes());
    assert_eq!(cold.stderr, b"");
    assert_eq!(warm.stdout, cold.stdout);
    assert_eq!(warm.stderr, cold.stderr);
}
