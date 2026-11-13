//! The scoring path is compile-time independent of skim's code.
//!
//! The crate graph is the enforcement. A scoreboard that imported skim's own
//! code could grade skim with skim's own rule, and no behavioural test would
//! notice. `rskim-bench` may depend on `rskim-research` and `rskim-oracle`
//! (and on itself: the dev-dependency that turns `test-utils` on for the
//! integration tests). `rskim-research` may depend on no `rskim-*` crate. The
//! oracle checks its own manifest in `crates/rskim-oracle/tests/`.
//!
//! Every dependency in every dependency table of both manifests —
//! `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]` and their
//! `[target.<cfg>.*]` forms — is checked, resolved through
//! `[workspace.dependencies]` when it says `workspace = true`. An `rskim-*`
//! crate that is not allowed fails with its own message however it is named (a
//! `package =` rename included). So does any `git` source, and any `path` that
//! does not lead to an allowed crate: a local crate could be skim's code under
//! another name. Registry crates that carry no `rskim` name are free.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code — fail loudly

use std::path::{Path, PathBuf};

use toml::{Table, Value};

/// A crate of the scoring path and the `rskim-*` crates its manifest may name.
struct ScoringCrate {
    /// The crate's folder under `crates/`.
    dir: &'static str,
    allowed: &'static [&'static str],
}

const SCORING_CRATES: &[ScoringCrate] = &[
    ScoringCrate {
        dir: "rskim-bench",
        allowed: &["rskim-research", "rskim-oracle", "rskim-bench"],
    },
    // Pinned clones and corpus config loading: no skim crate is needed.
    ScoringCrate {
        dir: "rskim-research",
        allowed: &[],
    },
];

/// The dependency tables a manifest (or a `[target.<cfg>]` table) can hold.
const DEPENDENCY_TABLES: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

/// Where a dependency entry is read, and what it may name.
struct Scope<'a> {
    /// Base of a `path` written in the crate's own manifest.
    crate_dir: &'a Path,
    /// Base of a `path` written in `[workspace.dependencies]`.
    workspace_root: &'a Path,
    workspace: &'a Table,
    allowed: &'a [&'a str],
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_toml(path: &Path) -> Table {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    raw.parse::<Table>()
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The root manifest's `[workspace.dependencies]`.
fn workspace_dependencies() -> Table {
    read_toml(&workspace_root().join("Cargo.toml"))
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(Value::as_table)
        .cloned()
        .expect("the root manifest has [workspace.dependencies]")
}

/// Every dependency table of `manifest`, named as in the manifest.
fn dependency_tables(manifest: &Table) -> Vec<(String, &Table)> {
    let mut out = Vec::new();
    for &kind in DEPENDENCY_TABLES {
        if let Some(table) = manifest.get(kind).and_then(Value::as_table) {
            out.push((format!("[{kind}]"), table));
        }
    }
    if let Some(targets) = manifest.get("target").and_then(Value::as_table) {
        for (cfg, target) in targets {
            for &kind in DEPENDENCY_TABLES {
                if let Some(table) = target.get(kind).and_then(Value::as_table) {
                    out.push((format!("[target.{cfg:?}.{kind}]"), table));
                }
            }
        }
    }
    out
}

/// Why `rel`, a `path` relative to `base`, does not lead to an allowed crate.
fn path_violation(key: &str, base: &Path, rel: &str, allowed: &[&str]) -> Option<String> {
    let dir = match base.join(rel).canonicalize() {
        Ok(dir) => dir,
        Err(e) => return Some(format!("{key}: the `path` {rel:?} cannot be resolved: {e}")),
    };
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    (!allowed.contains(&name))
        .then(|| format!("{key}: a `path` dependency on {name:?}, which is not an allowed crate"))
}

/// Why one dependency entry is not allowed, if it is not.
fn entry_violation(key: &str, spec: &Value, scope: &Scope) -> Option<String> {
    let (spec, base) = match spec.get("workspace").and_then(Value::as_bool) {
        Some(true) => match scope.workspace.get(key) {
            Some(resolved) => (resolved, scope.workspace_root),
            None => {
                return Some(format!(
                    "{key}: `workspace = true` but no [workspace.dependencies] entry"
                ));
            }
        },
        _ => (spec, scope.crate_dir),
    };
    let package = spec.get("package").and_then(Value::as_str).unwrap_or(key);
    if [key, package]
        .iter()
        .any(|name| name.starts_with("rskim") && !scope.allowed.contains(name))
    {
        return Some(format!(
            "{key} (package {package}): an rskim-* crate that is not allowed here — the \
             scoring path must be compile-time independent of skim"
        ));
    }
    if spec.get("git").is_some() {
        return Some(format!(
            "{key}: a `git` dependency (only registry crates and the allowed crates are \
             allowed)"
        ));
    }
    let rel = spec.get("path")?.as_str().unwrap_or("");
    path_violation(key, base, rel, scope.allowed)
}

/// Every disallowed dependency of `manifest`, as `<table> <reason>`.
fn violations(manifest: &Table, scope: &Scope) -> Vec<String> {
    let mut out = Vec::new();
    for (name, table) in dependency_tables(manifest) {
        for (key, spec) in table {
            if let Some(reason) = entry_violation(key, spec, scope) {
                out.push(format!("{name} {reason}"));
            }
        }
    }
    out
}

/// `manifest[name]` as a table.
fn table<'a>(manifest: &'a Table, name: &str) -> &'a Table {
    manifest
        .get(name)
        .and_then(Value::as_table)
        .unwrap_or_else(|| panic!("the manifest has no [{name}] table"))
}

#[test]
fn the_scoring_crates_depend_only_on_allowed_rskim_crates() {
    let workspace = workspace_dependencies();
    let root = workspace_root();
    for scoring in SCORING_CRATES {
        let crate_dir = root.join("crates").join(scoring.dir);
        let manifest = read_toml(&crate_dir.join("Cargo.toml"));
        let scope = Scope {
            crate_dir: &crate_dir,
            workspace_root: &root,
            workspace: &workspace,
            allowed: scoring.allowed,
        };
        assert_eq!(
            violations(&manifest, &scope),
            Vec::<String>::new(),
            "{}",
            scoring.dir
        );
        // Not vacuous: the check reads the tables the crate actually uses.
        assert!(
            table(&manifest, "dependencies").contains_key("serde"),
            "{}",
            scoring.dir
        );
    }
    let bench = read_toml(&root.join("crates/rskim-bench/Cargo.toml"));
    let deps = table(&bench, "dependencies");
    assert!(deps.contains_key("rskim-research"), "{deps:?}");
    assert!(deps.contains_key("rskim-oracle"), "{deps:?}");
}

#[test]
fn every_dependency_table_and_every_way_of_naming_a_forbidden_crate_is_caught() {
    let root = workspace_root();
    let crate_dir = root.join("crates/rskim-bench");
    let workspace: Table = r#"
        anyhow = "1.0"
        aliased = { package = "rskim-core", path = "crates/rskim-core" }
        local = { path = "crates/rskim-core" }
        oracle = { path = "crates/rskim-oracle" }
    "#
    .parse()
    .unwrap();
    let scope = Scope {
        crate_dir: &crate_dir,
        workspace_root: &root,
        workspace: &workspace,
        allowed: &["rskim-research", "rskim-oracle"],
    };
    let cases = [
        (
            "[dependencies]\nrskim-core = { path = \"../rskim-core\" }",
            "rskim-core",
        ),
        ("[dependencies]\nrskim = \"2\"", "rskim"),
        ("[dev-dependencies]\nrskim-core = \"2\"", "rskim-core"),
        ("[build-dependencies]\nrskim-llm = \"0.1\"", "rskim-llm"),
        (
            "[target.'cfg(unix)'.dependencies]\nrskim-proxy = { path = \"../rskim-proxy\" }",
            "rskim-proxy",
        ),
        (
            "[target.'cfg(windows)'.dev-dependencies]\nrskim-compress = \"0.1\"",
            "rskim-compress",
        ),
        (
            "[dependencies]\ncore = { package = \"rskim-core\", version = \"2\" }",
            "rskim-core",
        ),
        (
            "[dependencies]\naliased = { workspace = true }",
            "rskim-core",
        ),
        ("[dependencies.rskim-core]\nversion = \"2\"", "rskim-core"),
        // An allowed name that leads to code from another crate.
        (
            "[dependencies]\nrskim-oracle = { path = \"../rskim-core\" }",
            "\"rskim-core\"",
        ),
        // A crate's own name is allowed only where its allow-list says so.
        (
            "[dependencies]\nrskim-bench = { path = \".\" }",
            "rskim-bench",
        ),
    ];
    for (src, needle) in cases {
        let found = violations(&src.parse().unwrap(), &scope);
        assert!(
            found.len() == 1 && found[0].contains(needle),
            "{src}: {found:?}"
        );
    }
    for (src, needle) in [
        (
            "[dependencies]\nsneaky = { path = \"../rskim-core\" }",
            "not an allowed crate",
        ),
        (
            "[dependencies]\nsneaky = { path = \"../does-not-exist\" }",
            "cannot be resolved",
        ),
        (
            "[dependencies]\nanyhow = { git = \"https://example.com/a\" }",
            "`git` dependency",
        ),
        (
            "[dependencies]\nlocal = { workspace = true }",
            "not an allowed crate",
        ),
        (
            "[dependencies]\nmissing = { workspace = true }",
            "no [workspace.dependencies] entry",
        ),
    ] {
        let found = violations(&src.parse().unwrap(), &scope);
        assert!(
            found.len() == 1 && found[0].contains(needle),
            "{src}: {found:?}"
        );
    }
    let clean = "[dependencies]\nanyhow = { workspace = true }\nregex = \"1\"\n\
                 rskim-research = { path = \"../rskim-research\" }\n\
                 rskim-oracle = { path = \"../rskim-oracle\" }\n\
                 oracle = { workspace = true }\n\
                 [dev-dependencies]\ntempfile = \"3\"";
    assert_eq!(
        violations(&clean.parse().unwrap(), &scope),
        Vec::<String>::new()
    );
}

#[test]
fn a_crate_with_an_empty_allow_list_names_no_rskim_crate() {
    let root = workspace_root();
    let crate_dir = root.join("crates/rskim-research");
    let workspace = Table::new();
    let scope = Scope {
        crate_dir: &crate_dir,
        workspace_root: &root,
        workspace: &workspace,
        allowed: &[],
    };
    let src = "[dependencies]\nrskim-oracle = { path = \"../rskim-oracle\" }";
    let found = violations(&src.parse().unwrap(), &scope);
    assert!(
        found.len() == 1 && found[0].contains("rskim-oracle"),
        "{found:?}"
    );
}
