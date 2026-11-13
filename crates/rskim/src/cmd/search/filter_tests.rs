//! Tests of the `-g` and `-t` filters.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;

use super::{FilterError, PathFilter};

fn filter(globs: &[&str], types: &[&str]) -> PathFilter {
    let globs: Vec<String> = globs.iter().map(|s| (*s).to_owned()).collect();
    let types: Vec<String> = types.iter().map(|s| (*s).to_owned()).collect();
    PathFilter::new(Path::new("/root"), &globs, &types).unwrap()
}

fn kept(filter: &PathFilter, paths: &[&str]) -> Vec<String> {
    paths
        .iter()
        .filter(|p| filter.keeps(Path::new(p)))
        .map(|p| (*p).to_owned())
        .collect()
}

const TREE: [&str; 6] = [
    "src/a.rs",
    "src/b.py",
    "README.md",
    "vendor/lib/deep/c.rs",
    "docs/vendor/d.rs",
    "top.rs",
];

#[test]
fn no_filter_keeps_every_path() {
    assert_eq!(kept(&filter(&[], &[]), &TREE).len(), TREE.len());
}

#[test]
fn a_whitelist_glob_keeps_only_the_files_it_fits() {
    let got = kept(&filter(&["*.rs"], &[]), &TREE);
    assert_eq!(
        got,
        [
            "src/a.rs",
            "vendor/lib/deep/c.rs",
            "docs/vendor/d.rs",
            "top.rs"
        ]
    );
}

#[test]
fn an_exclude_glob_leaves_out_the_files_it_fits() {
    let got = kept(&filter(&["!*.md"], &[]), &TREE);
    assert!(!got.contains(&"README.md".to_owned()));
    assert_eq!(got.len(), TREE.len() - 1);
}

#[test]
fn a_folder_glob_excludes_each_file_below_the_folder() {
    let got = kept(&filter(&["!vendor"], &[]), &TREE);
    assert!(
        !got.contains(&"vendor/lib/deep/c.rs".to_owned()),
        "a file two folders below vendor stays out: {got:?}"
    );
    assert!(
        !got.contains(&"docs/vendor/d.rs".to_owned()),
        "a folder named vendor at any depth counts: {got:?}"
    );
    assert!(got.contains(&"src/a.rs".to_owned()));
}

#[test]
fn a_path_glob_fits_the_path_from_the_root() {
    let got = kept(&filter(&["src/*.rs"], &[]), &TREE);
    assert_eq!(got, ["src/a.rs"]);
}

#[test]
fn a_whitelisted_folder_does_not_keep_its_files_by_itself() {
    // As with rg, `-g src` names the folder; the files need `-g 'src/**'`.
    let got = kept(&filter(&["src"], &[]), &TREE);
    assert!(got.is_empty(), "{got:?}");
    let got = kept(&filter(&["src/**"], &[]), &TREE);
    assert_eq!(got, ["src/a.rs", "src/b.py"]);
}

#[test]
fn an_absolute_glob_or_a_glob_with_a_parent_fits_no_listed_path() {
    for glob in [
        "/etc/passwd",
        "/root/src/a.rs",
        "../outside/*.rs",
        "src/../src/a.rs",
    ] {
        let got = kept(&filter(&[glob], &[]), &TREE);
        assert!(got.is_empty(), "{glob}: {got:?}");
    }
}

#[test]
fn a_type_keeps_the_files_of_that_type() {
    let got = kept(&filter(&[], &["rust"]), &TREE);
    assert_eq!(
        got,
        [
            "src/a.rs",
            "vendor/lib/deep/c.rs",
            "docs/vendor/d.rs",
            "top.rs"
        ]
    );
    let got = kept(&filter(&[], &["rust", "py"]), &TREE);
    assert_eq!(got.len(), 5);
}

#[test]
fn a_glob_and_a_type_both_apply() {
    let got = kept(&filter(&["!vendor"], &["rust"]), &TREE);
    assert_eq!(got, ["src/a.rs", "top.rs"]);
}

#[test]
fn an_unknown_type_lists_the_known_types() {
    let err = PathFilter::new(Path::new("/root"), &[], &["nosuchtype".to_owned()])
        .err()
        .unwrap();
    match &err {
        FilterError::UnknownType { name, known } => {
            assert_eq!(name, "nosuchtype");
            assert!(known.iter().any(|k| k == "rust"));
            assert!(known.windows(2).all(|w| w[0] <= w[1]), "sorted");
        }
        other => panic!("expected an unknown type, got {other:?}"),
    }
    let text = err.to_string();
    assert!(text.starts_with("unknown file type \"nosuchtype\"; known types: "));
}

#[test]
fn an_invalid_glob_is_an_error() {
    let err = PathFilter::new(Path::new("/root"), &["[".to_owned()], &[])
        .err()
        .unwrap();
    assert!(matches!(err, FilterError::Glob { .. }), "{err:?}");
    assert!(err.to_string().starts_with("invalid glob "));
}
