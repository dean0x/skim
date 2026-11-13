//! Integration tests: `skim search` refuses the flags it no longer accepts.
//!
//! The AST and temporal arms and the index commands are gone. A command line
//! that still names one of their flags must fail loudly, name the replacement,
//! and leave no trace: exit code 2, nothing on stdout, nothing written to the
//! project or to the cache folder (no index is built on the way out).

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
    // The old index commands. `--update` is the line an old git hook runs.
    Case {
        flag: "--build",
        args: &["--build"],
        advice: "has no index",
    },
    Case {
        flag: "--rebuild",
        args: &["--rebuild"],
        advice: "has no index",
    },
    Case {
        flag: "--update",
        args: &["--update"],
        advice: "has no index",
    },
    Case {
        flag: "--stats",
        args: &["--stats", "--json"],
        advice: "has no index",
    },
    Case {
        flag: "--install-hooks",
        args: &["--install-hooks"],
        advice: "skim init --uninstall",
    },
    Case {
        flag: "--remove-hooks",
        args: &["--remove-hooks"],
        advice: "skim init --uninstall",
    },
    // Two removed flags on one line: the first one in the line is named.
    Case {
        flag: "--hot",
        args: &["--hot", "--build"],
        advice: "skim heatmap",
    },
    Case {
        flag: "--build",
        args: &["--build", "--ast", "try-catch"],
        advice: "has no index",
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

/// Every file below `dir` with its bytes, keyed by the path relative to `dir`.
fn snapshot(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                out.insert(
                    path.strip_prefix(base).unwrap().display().to_string(),
                    fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// Run `git` in `dir`, isolated from the host configuration, and assert success.
fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A query never deletes or rewrites old data: a repository with the old search
/// hooks and a cache folder with old index files are byte for byte the same
/// after the query. Removal belongs to `skim init` and `skim --clear-cache`.
#[test]
fn query_leaves_old_hooks_and_old_index_data_unchanged() {
    let proj = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    fs::create_dir_all(proj.path().join("src")).unwrap();
    fs::write(proj.path().join("src/a.rs"), "pub fn needle() {}\n").unwrap();
    git(proj.path(), &["init", "-q"]);
    git(proj.path(), &["add", "src/a.rs"]);

    let hooks = proj.path().join(".git").join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    for name in ["post-commit", "post-merge", "post-checkout"] {
        fs::write(
            hooks.join(name),
            "#!/bin/sh\necho mine\n# skim-search-start\nskim search --update 2>/dev/null &\n# skim-search-end\n",
        )
        .unwrap();
    }
    let shard = cache.path().join("search").join("0123456789abcdef");
    fs::create_dir_all(&shard).unwrap();
    for name in [
        "index.skidx",
        "index.skpost",
        "temporal.db",
        ".skim-build.lock",
    ] {
        fs::write(shard.join(name), name.as_bytes()).unwrap();
    }
    let hooks_before = snapshot(&hooks);
    let cache_before = snapshot(cache.path());

    Command::cargo_bin("skim")
        .unwrap()
        .args(["search", "needle", "--root"])
        .arg(proj.path())
        .env("SKIM_CACHE_DIR", cache.path())
        .env("SKIM_DISABLE_ANALYTICS", "1")
        .assert()
        .success()
        .stdout(predicate::str::contains("src/a.rs"));

    assert_eq!(snapshot(&hooks), hooks_before, "a query rewrote a hook");
    assert_eq!(
        snapshot(cache.path()),
        cache_before,
        "a query changed the cache folder"
    );
}
