//! AC-3 (#541): the oracle is compile-time independent of skim's search
//! stack.
//!
//! The crate graph is the enforcement. Every dependency in every dependency
//! table of this crate's manifest — `[dependencies]`, `[dev-dependencies]`,
//! `[build-dependencies]` and their `[target.<cfg>.*]` forms — must be a
//! registry crate on [`ALLOWED`], resolved through `[workspace.dependencies]`
//! when it says `workspace = true`. A `rskim-*` crate fails with its own
//! message however it is named (a `package =` rename included), and so does
//! any `path` or `git` source (a local crate could be skim's code under
//! another name). Two ways to pull code in without a dependency are closed
//! too: the crate has no build script, and no source file uses `include!`
//! or a `#[path]` that leaves the crate.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // test code — fail loudly

use std::path::{Path, PathBuf};

use toml::{Table, Value};

/// The crates the oracle may depend on, in any dependency table: tree-sitter,
/// the grammars it parses with, and small utility crates.
const ALLOWED: &[&str] = &[
    "anyhow",
    "rayon",
    "serde",
    "sha2",
    "toml",
    "tree-sitter",
    "tree-sitter-go",
    "tree-sitter-javascript",
    "tree-sitter-python",
    "tree-sitter-rust",
    "tree-sitter-typescript",
];

/// The dependency tables a manifest (or a `[target.<cfg>]` table) can hold.
const DEPENDENCY_TABLES: &[&str] = &["dependencies", "dev-dependencies", "build-dependencies"];

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_toml(path: &Path) -> Table {
    let raw = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    raw.parse::<Table>()
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The root manifest's `[workspace.dependencies]`.
fn workspace_dependencies() -> Table {
    let root = read_toml(&crate_dir().join("../../Cargo.toml"));
    root.get("workspace")
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

/// Why one dependency entry is not allowed, if it is not.
fn entry_violation(key: &str, spec: &Value, workspace: &Table) -> Option<String> {
    let spec = match spec.get("workspace").and_then(Value::as_bool) {
        Some(true) => match workspace.get(key) {
            Some(resolved) => resolved,
            None => {
                return Some(format!(
                    "{key}: `workspace = true` but no [workspace.dependencies] entry"
                ));
            }
        },
        _ => spec,
    };
    let package = spec.get("package").and_then(Value::as_str).unwrap_or(key);
    if [key, package].iter().any(|name| name.starts_with("rskim")) {
        return Some(format!(
            "{key} (package {package}): an rskim-* crate — the oracle must be compile-time \
             independent of skim"
        ));
    }
    if let Some(source) = ["path", "git"].into_iter().find(|s| spec.get(*s).is_some()) {
        return Some(format!(
            "{key}: a `{source}` dependency (only registry crates are allowed)"
        ));
    }
    (!ALLOWED.contains(&package))
        .then(|| format!("{key} (package {package}): not on the allow-list"))
}

/// Every disallowed dependency of `manifest`, as `<table> <reason>`.
fn violations(manifest: &Table, workspace: &Table) -> Vec<String> {
    let mut out = Vec::new();
    for (name, table) in dependency_tables(manifest) {
        for (key, spec) in table {
            if let Some(reason) = entry_violation(key, spec, workspace) {
                out.push(format!("{name} {reason}"));
            }
        }
    }
    out
}

fn manifest() -> Table {
    read_toml(&crate_dir().join("Cargo.toml"))
}

#[test]
fn the_oracle_depends_only_on_allow_listed_crates() {
    let manifest = manifest();
    assert_eq!(
        violations(&manifest, &workspace_dependencies()),
        Vec::<String>::new()
    );
    // Not vacuous: the check reads the tables the oracle actually uses.
    let deps = table(&manifest, "dependencies");
    assert!(deps.contains_key("tree-sitter"), "{deps:?}");
    assert!(table(&manifest, "dev-dependencies").contains_key("toml"));
}

#[test]
fn every_dependency_table_and_every_way_of_naming_rskim_is_caught() {
    let workspace: Table = r#"
        anyhow = "1.0"
        aliased = { package = "rskim-core", path = "crates/rskim-core" }
        local = { path = "crates/other" }
    "#
    .parse()
    .unwrap();
    let cases = [
        (
            "[dependencies]\nrskim-search = { path = \"../rskim-search\" }",
            "rskim-search",
        ),
        ("[dev-dependencies]\nrskim-core = \"2\"", "rskim-core"),
        (
            "[build-dependencies]\nrskim-research = \"0.1\"",
            "rskim-research",
        ),
        (
            "[target.'cfg(unix)'.dependencies]\nrskim-bench = { path = \"../rskim-bench\" }",
            "rskim-bench",
        ),
        (
            "[target.'cfg(windows)'.dev-dependencies]\nrskim-llm = \"0.1\"",
            "rskim-llm",
        ),
        (
            "[dependencies]\nsearch = { package = \"rskim-search\", version = \"0.1\" }",
            "rskim-search",
        ),
        (
            "[dependencies]\naliased = { workspace = true }",
            "rskim-core",
        ),
        ("[dependencies.rskim-core]\nversion = \"2\"", "rskim-core"),
    ];
    for (src, crate_name) in cases {
        let found = violations(&src.parse().unwrap(), &workspace);
        assert!(
            found.len() == 1 && found[0].contains(crate_name) && found[0].contains("rskim-* crate"),
            "{src}: {found:?}"
        );
    }
    for (src, needle) in [
        ("[dependencies]\nregex = \"1\"", "not on the allow-list"),
        (
            "[dependencies]\ntree-sitter = { path = \"../vendored\" }",
            "`path` dependency",
        ),
        (
            "[dependencies]\nanyhow = { git = \"https://example.com/a\" }",
            "`git` dependency",
        ),
        (
            "[dependencies]\nlocal = { workspace = true }",
            "`path` dependency",
        ),
        (
            "[dependencies]\nmissing = { workspace = true }",
            "no [workspace.dependencies] entry",
        ),
    ] {
        let found = violations(&src.parse().unwrap(), &workspace);
        assert!(
            found.len() == 1 && found[0].contains(needle),
            "{src}: {found:?}"
        );
    }
    let clean = "[dependencies]\nanyhow = { workspace = true }\ntree-sitter = \"0.25\"\n\
                 [dev-dependencies]\ntoml = \"0.8\"";
    assert_eq!(
        violations(&clean.parse().unwrap(), &workspace),
        Vec::<String>::new()
    );
}

/// Every `.rs` file under `dir`, recursively.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `include!` and every `#[path = "…"]` value in `src` that leaves its
/// directory (`..` or an absolute path). A plain text scan: a comment that
/// spells either one also fails, which errs on the safe side.
fn code_inclusions(src: &str) -> Vec<String> {
    // Built with concat! so this file does not flag itself.
    let include = concat!("include", "!(");
    let path_attr = concat!("#[", "path");
    let mut out = Vec::new();
    if src.contains(include) {
        out.push(include.to_string());
    }
    for (at, _) in src.match_indices(path_attr) {
        let attr = &src[at..src[at..].find(']').map_or(src.len(), |end| at + end + 1)];
        let value = attr.split('"').nth(1).unwrap_or("");
        if value.contains("..") || value.starts_with('/') {
            out.push(attr.to_string());
        }
    }
    out
}

#[test]
fn no_build_script_and_no_source_file_pulls_in_code_from_outside_the_crate() {
    let dir = crate_dir();
    assert!(
        !dir.join("build.rs").exists(),
        "the oracle has no build script"
    );
    assert!(
        table(&manifest(), "package").get("build").is_none(),
        "the oracle has no build script"
    );
    let mut files = Vec::new();
    for sub in ["src", "tests", "benches", "examples"] {
        rust_files(&dir.join(sub), &mut files);
    }
    assert!(
        files.iter().any(|f| f.ends_with("src/structural.rs")),
        "{files:?}"
    );
    let found: Vec<String> = files
        .iter()
        .flat_map(|f| {
            let src = std::fs::read_to_string(f).unwrap();
            code_inclusions(&src)
                .into_iter()
                .map(move |hit| format!("{}: {hit}", f.display()))
        })
        .collect();
    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn the_inclusion_scan_flags_include_and_escaping_paths_only() {
    let include = concat!("include", "!(\"x.rs\");");
    assert_eq!(code_inclusions(include).len(), 1);
    let escaping = concat!("#[", "path = \"../../rskim-search/src/lib.rs\"]\nmod m;");
    assert_eq!(code_inclusions(escaping).len(), 1);
    let absolute = concat!("#[", "path = \"/tmp/m.rs\"]\nmod m;");
    assert_eq!(code_inclusions(absolute).len(), 1);
    let local = concat!("#[", "path = \"structural_tests.rs\"]\nmod tests;");
    let data = "include_str!(\"../queries/a.scm\")";
    assert_eq!(code_inclusions(local), Vec::<String>::new());
    assert_eq!(code_inclusions(data), Vec::<String>::new());
}

/// `manifest[name]` as a table.
fn table<'a>(manifest: &'a Table, name: &str) -> &'a Table {
    manifest
        .get(name)
        .and_then(Value::as_table)
        .unwrap_or_else(|| panic!("the manifest has no [{name}] table"))
}
