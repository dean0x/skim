//! Integration tests: `skim search` refuses the flags it no longer accepts.
//!
//! The AST and temporal arms are gone. A command line that still names one of
//! their flags must fail loudly, name the replacement, and leave no trace:
//! exit code 2, nothing on stdout, nothing written to the project or to the
//! cache folder (no index is built on the way out).

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

/// A removed flag, an argument list that names it, and the replacement the
/// refusal must point to.
struct Case {
    flag: &'static str,
    args: &'static [&'static str],
    advice: &'static str,
}

const CASES: &[Case] = &[
    Case {
        flag: "--ast",
        args: &["--ast", "try-catch"],
        advice: "ast-grep",
    },
    Case {
        flag: "--ast",
        args: &["--ast=try-catch"],
        advice: "ast-grep",
    },
    Case {
        flag: "--ast",
        args: &["needle", "--ast", "try-catch"],
        advice: "ast-grep",
    },
    Case {
        flag: "--hot",
        args: &["--hot"],
        advice: "skim heatmap",
    },
    Case {
        flag: "--cold",
        args: &["--cold", "--limit", "5"],
        advice: "skim heatmap",
    },
    Case {
        flag: "--risky",
        args: &["--risky", "--json"],
        advice: "skim heatmap",
    },
    Case {
        flag: "--blast-radius",
        args: &["--blast-radius", "src/a.rs"],
        advice: "git log --name-only",
    },
    Case {
        flag: "--weights",
        args: &["needle", "--weights", "1,1,1"],
        advice: "no fused ranking",
    },
    // A removed flag beside an action flag: the action must not run.
    Case {
        flag: "--hot",
        args: &["--hot", "--build"],
        advice: "skim heatmap",
    },
    Case {
        flag: "--ast",
        args: &["--build", "--ast", "try-catch"],
        advice: "ast-grep",
    },
];

/// Sorted list of every path under `dir`, relative to it.
fn tree(dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            out.push(path.strip_prefix(base).unwrap().display().to_string());
            if path.is_dir() {
                walk(base, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[test]
fn removed_flag_exits_2_names_replacement_and_writes_nothing() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    fs::create_dir_all(proj.path().join("src")).unwrap();
    fs::write(proj.path().join("src/a.rs"), "pub fn needle() {}\n").unwrap();
    let proj_before = tree(proj.path());

    for case in CASES {
        Command::cargo_bin("skim")
            .unwrap()
            .arg("search")
            .args(case.args)
            .arg("--root")
            .arg(proj.path())
            .env("SKIM_CACHE_DIR", cache.path())
            .env("SKIM_DISABLE_ANALYTICS", "1")
            .assert()
            .code(2)
            .stdout(predicate::str::is_empty())
            .stderr(
                predicate::str::contains(format!("{} was removed", case.flag))
                    .and(predicate::str::contains(case.advice)),
            );

        assert_eq!(
            tree(proj.path()),
            proj_before,
            "{:?}: the refusal must not write into the project",
            case.args
        );
        assert!(
            tree(cache.path()).is_empty(),
            "{:?}: the refusal must not write into the cache folder, found {:?}",
            case.args,
            tree(cache.path())
        );
    }
}

/// The attached text of `--flag=value` must not be echoed to the terminal.
#[test]
fn removed_flag_refusal_does_not_echo_attached_value() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--ast=SECRET-VALUE", "--root"])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("SECRET-VALUE").not());
}

/// A removed flag name after `--` is query text, not a flag: the command runs as
/// an ordinary query. It exits 0 when a line holds the text.
#[test]
fn removed_flag_name_after_double_dash_is_a_query() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    fs::create_dir_all(proj.path().join("src")).unwrap();
    fs::write(proj.path().join("src/a.rs"), "// the --ast flag\n").unwrap();

    Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "--root"])
        .arg(proj.path())
        .args(["--", "--ast"])
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .stdout(predicate::str::contains("src/a.rs:1:"))
        .stderr(predicate::str::contains("was removed").not());
}
