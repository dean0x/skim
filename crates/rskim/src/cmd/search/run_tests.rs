//! Tests of the query set-up in `mod.rs`: the root rule, the query compile and
//! the output form.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use super::args::{ListMode, QueryFlags, SearchAction, parse_flags};
use super::gitdir::RootRules;
use super::render::{Format, MatchMode};
use super::scan::QueryPlan;
use super::{build_query, format_of, resolve_scan_root};

fn flags(args: &[&str]) -> QueryFlags {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    match parse_flags(&args).unwrap() {
        SearchAction::Query(flags) => *flags,
        other => panic!("expected a query, got {other:?}"),
    }
}

/// A canonical temporary folder; the walk of a test stops there.
fn sandbox() -> (tempfile::TempDir, PathBuf, RootRules) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().canonicalize().unwrap();
    let rules = RootRules {
        ceilings: vec![path.clone()],
        home: None,
    };
    (dir, path, rules)
}

// ============================================================================
// resolve_scan_root
// ============================================================================

#[test]
fn an_explicit_root_is_canonical_and_gives_no_notice() {
    let (_dir, path, rules) = sandbox();
    std::fs::create_dir(path.join("proj")).unwrap();
    let root = resolve_scan_root(Some(&path.join("proj/../proj")), &path, &rules).unwrap();
    assert_eq!(root.path, path.join("proj"));
    assert_eq!(root.notice, None);
}

#[test]
fn an_explicit_root_must_exist_and_be_a_folder() {
    let (_dir, path, rules) = sandbox();
    let missing = resolve_scan_root(Some(&path.join("missing")), &path, &rules).unwrap_err();
    assert!(missing.message().starts_with("--root "), "{missing}");

    std::fs::write(path.join("file.txt"), "x").unwrap();
    let file = resolve_scan_root(Some(&path.join("file.txt")), &path, &rules).unwrap_err();
    assert!(file.message().ends_with("is not a directory"), "{file}");
}

#[test]
fn an_implicit_search_from_the_filesystem_root_is_refused() {
    let err = resolve_scan_root(None, Path::new("/"), &RootRules::default()).unwrap_err();
    assert_eq!(
        err.message(),
        "refusing to search / implicitly; pass --root /"
    );
    // `--root /` is the way to ask for it.
    let ok = resolve_scan_root(Some(Path::new("/")), Path::new("/"), &RootRules::default());
    assert_eq!(ok.unwrap().path, Path::new("/"));
}

#[test]
fn an_implicit_root_is_the_repository_and_the_notice_names_it() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().canonicalize().unwrap().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(repo.join("src/deep")).unwrap();
    let rules = RootRules::default();

    let below = resolve_scan_root(None, &repo.join("src/deep"), &rules).unwrap();
    assert_eq!(below.path, repo);
    let notice = below.notice.unwrap();
    assert!(notice.starts_with("searching "), "{notice}");
    assert!(notice.contains(&format!("{repo:?}")), "{notice}");

    let at_root = resolve_scan_root(None, &repo, &rules).unwrap();
    assert_eq!(at_root.path, repo);
    assert_eq!(at_root.notice, None);
}

#[test]
fn an_implicit_root_outside_a_repository_is_the_working_folder() {
    let (_dir, path, rules) = sandbox();
    std::fs::create_dir_all(path.join("a/b")).unwrap();
    let root = resolve_scan_root(None, &path.join("a/b"), &rules).unwrap();
    assert_eq!(root.path, path.join("a/b"));
    assert_eq!(root.notice, None);
}

// ============================================================================
// build_query
// ============================================================================

#[test]
fn a_pattern_gets_the_match_mode_of_its_flags() {
    let cases = [
        (&["needle"][..], MatchMode::Literal),
        (&["-w", "needle"][..], MatchMode::Word),
        (&["-E", "needle"][..], MatchMode::Regex),
        (&["-E", "-w", "needle"][..], MatchMode::Regex),
    ];
    for (args, mode) in cases {
        let (query, info) = build_query(&flags(args)).unwrap();
        assert_eq!(info.mode, mode, "{args:?}");
        assert_eq!(info.text, "needle");
        assert!(info.words.is_empty());
        assert!(matches!(query.plan, QueryPlan::Pattern(_)));
    }
}

#[test]
fn the_identifier_flag_follows_the_shape_of_the_pattern() {
    let (bare, _) = build_query(&flags(&["needle"])).unwrap();
    assert!(bare.identifier);
    let (regex, _) = build_query(&flags(&["-E", "needle"])).unwrap();
    assert!(!regex.identifier);
    let (dotted, _) = build_query(&flags(&["a.b"])).unwrap();
    assert!(!dotted.identifier);
}

#[test]
fn case_flags_decide_the_case_of_a_pattern() {
    let ignore = |args: &[&str]| build_query(&flags(args)).unwrap().1.ignore_case;
    assert!(!ignore(&["Needle"]));
    assert!(ignore(&["-i", "Needle"]));
    assert!(ignore(&["-S", "needle"]));
    assert!(!ignore(&["-S", "Needle"]));
    assert!(ignore(&["-i", "-S", "Needle"]), "-i wins over -S");
}

#[test]
fn a_word_query_has_one_case_rule_for_the_whole_phrase() {
    let (query, info) = build_query(&flags(&["-S", "alpha Beta"])).unwrap();
    assert_eq!(info.mode, MatchMode::Words);
    assert_eq!(info.words, ["alpha", "Beta"]);
    assert_eq!(info.text, "alpha Beta");
    assert!(!info.ignore_case, "one capital makes the phrase sensitive");
    match query.plan {
        QueryPlan::Words(matchers) => assert_eq!(matchers.len(), 2),
        QueryPlan::Pattern(_) => panic!("expected the word plan"),
    }
    assert!(!query.identifier);

    let (_, lower) = build_query(&flags(&["-S", "alpha beta"])).unwrap();
    assert!(lower.ignore_case);
}

#[test]
fn an_invalid_pattern_is_a_usage_error_that_quotes_the_pattern() {
    let err = build_query(&flags(&["-E", "("])).err().unwrap();
    assert!(err.message().starts_with("invalid pattern \"(\""), "{err}");
    // A literal bracket is not an error.
    assert!(build_query(&flags(&["("])).is_ok());
}

// ============================================================================
// format_of
// ============================================================================

#[test]
fn the_list_mode_and_the_json_flag_pick_the_output_form() {
    assert_eq!(format_of(&flags(&["x"])), Format::Text);
    assert_eq!(format_of(&flags(&["x", "--json"])), Format::Json);
    assert_eq!(format_of(&flags(&["-l", "x"])), Format::FilesWithMatches);
    assert_eq!(format_of(&flags(&["-c", "x"])), Format::Counts);
    assert_eq!(flags(&["-c", "x"]).list, ListMode::Counts);
}
