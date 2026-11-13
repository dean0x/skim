//! Unit tests for `roles.rs` (co-located file, `#[path]`-included).
//!
//! Each oracle language has one fixture and a frozen table: for a name, the
//! lines of its definitions, calls and imports. The tables were written by
//! reading each fixture line against the grammar, not by running the oracle.
//! A fixture line holds the name in a comment or a string, as an argument, as
//! a path segment or inside a longer identifier where the table says it is not
//! a site, so each table also pins what the queries must leave out.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::*;
use crate::structural::tests::{grammar_header_problem, locked_dependency_versions};

/// A name with the lines of its definitions, calls and imports.
type Row = (&'static str, &'static [u32], &'static [u32], &'static [u32]);

/// The rows of `table` that `role_sites` does not give, as messages.
fn mismatches(lang: OracleLang, source: &str, table: &[Row]) -> Vec<String> {
    table
        .iter()
        .filter_map(|&(name, defs, calls, imports)| {
            let expected = RoleSites {
                defs: defs.to_vec(),
                calls: calls.to_vec(),
                imports: imports.to_vec(),
            };
            let found =
                role_sites(lang, source, name).unwrap_or_else(|e| panic!("{lang} {name}: {e:#}"));
            (found != expected)
                .then(|| format!("{lang} {name}: expected {expected:?}, found {found:?}"))
        })
        .collect()
}

fn assert_table(lang: OracleLang, source: &str, table: &[Row]) {
    let wrong = mismatches(lang, source, table);
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

fn parses_cleanly(lang: OracleLang, source: &str) -> bool {
    !OracleScratch::new()
        .parse(lang, source)
        .unwrap()
        .root_node()
        .has_error()
}

// ============================================================================
// Fixtures and frozen tables
// ============================================================================

const RUST: &str = "\
use std::fmt::Display;
use crate::view::{render, Panel as Pane};

// render is documented here
struct Panel { width: u32 }
enum Mode { Fast, Slow }
union Bits { raw: u32 }
trait Draw { fn render(&self); }
type Id = u32;
const LIMIT: u32 = 3;
static NAME: &str = \"render\";
mod view { pub fn render() {} }
macro_rules! shout { () => {}; }

fn render() {}
fn run() {
    render();
    view::render();
    panel.render();
    render::<u8>();
    shout!();
    let render = 1;
    apply(render);
    Panel::new();
}
";

const RUST_TABLE: &[Row] = &[
    // Declared as a trait method, a module function and a free function;
    // called bare, through a path, through a field and with a turbofish;
    // imported in a use list. Line 4 (comment), 11 (string), 22 (a binding)
    // and 23 (an argument) are not sites.
    ("render", &[8, 12, 15], &[17, 18, 19, 20], &[2]),
    // A path segment before `::new` is not the callee.
    ("Panel", &[5], &[], &[2]),
    ("Pane", &[], &[], &[2]),
    ("width", &[5], &[], &[]),
    ("Mode", &[6], &[], &[]),
    ("Fast", &[6], &[], &[]),
    ("Bits", &[7], &[], &[]),
    ("raw", &[7], &[], &[]),
    ("Draw", &[8], &[], &[]),
    ("Id", &[9], &[], &[]),
    ("LIMIT", &[10], &[], &[]),
    ("NAME", &[11], &[], &[]),
    ("view", &[12], &[], &[2]),
    ("shout", &[13], &[21], &[]),
    ("new", &[], &[24], &[]),
    ("apply", &[], &[23], &[]),
    ("run", &[16], &[], &[]),
    ("Display", &[], &[], &[1]),
    ("std", &[], &[], &[1]),
    // A longer identifier that contains the name is not the name.
    ("rend", &[], &[], &[]),
];

const PYTHON: &str = "\
import os
from pkg.tools import build as make, Widget

# build is described here
class Widget:
    def build(self):
        return os.build()

def build():
    pass

def main():
    build()
    obj.build()
    w = Widget()
    print(\"build\")
    use(build)
    Widget.build(1)
";

const PYTHON_TABLE: &[Row] = &[
    // A method and a function; bare, attribute and class-qualified calls;
    // the first name of an aliased import. Lines 4, 16 and 17 are not sites.
    ("build", &[6, 9], &[7, 13, 14, 18], &[2]),
    ("make", &[], &[], &[2]),
    // Called as a constructor; the object of `Widget.build` is not a callee.
    ("Widget", &[5], &[15], &[2]),
    ("os", &[], &[], &[1]),
    ("pkg", &[], &[], &[2]),
    ("tools", &[], &[], &[2]),
    ("main", &[12], &[], &[]),
    ("print", &[], &[16], &[]),
    ("use", &[], &[17], &[]),
    ("w", &[], &[], &[]),
    ("obj", &[], &[], &[]),
    ("buil", &[], &[], &[]),
];

/// Parsed with the TypeScript grammar and with the TSX grammar.
const TYPESCRIPT: &str = "\
import { fetchAll, Row } from \"./db\";
import * as util from \"./util\";
export { fetchAll } from \"./db\";
export * from \"./other\";

interface Row { id: number }
type Id = number;
enum Kind { A, B }
abstract class Base { abstract go(): void; }
class Repo extends Base {
  fetchAll(): void {}
  go(): void {}
}
declare function load(): void;
function* gen() {}
const total = 1;
let items = [fetchAll()];
function work() {
  const inner = 1;
  fetchAll();
  repo.fetchAll();
  new Repo();
  new util.Repo();
  load<string>();
  inner;
}
// fetchAll
";

const TYPESCRIPT_TABLE: &[Row] = &[
    // A method name; bare and member calls; an import and a re-export.
    // Line 27 (comment) is not a site.
    ("fetchAll", &[11], &[17, 20, 21], &[1, 3]),
    ("Row", &[6], &[], &[1]),
    ("Id", &[7], &[], &[]),
    ("Kind", &[8], &[], &[]),
    // `extends Base` is a reference.
    ("Base", &[9], &[], &[]),
    // Constructed with `new`, bare and through a namespace.
    ("Repo", &[10], &[22, 23], &[]),
    // Line 9 declares an abstract signature, not a method definition.
    ("go", &[12], &[], &[]),
    // A signature with a type argument call.
    ("load", &[14], &[24], &[]),
    ("gen", &[15], &[], &[]),
    ("total", &[16], &[], &[]),
    ("items", &[17], &[], &[]),
    ("work", &[18], &[], &[]),
    // Inside a function body, a declarator is not a definition.
    ("inner", &[], &[], &[]),
    ("util", &[], &[], &[2]),
    // Enum members and property signatures are not declaration kinds.
    ("A", &[], &[], &[]),
    ("id", &[], &[], &[]),
    ("fetch", &[], &[], &[]),
];

/// Parsed with the TSX grammar only: JSX is not a call.
const TSX: &str = "\
import { Card } from \"./card\";

function Page() {
  return <Card title=\"Card\" />;
}
const App = () => <Page />;
const x = Card();
// Card
";

const TSX_TABLE: &[Row] = &[
    ("Card", &[], &[7], &[1]),
    ("Page", &[3], &[], &[]),
    ("App", &[6], &[], &[]),
    ("x", &[7], &[], &[]),
    ("title", &[], &[], &[]),
];

const JAVASCRIPT: &str = "\
import fs, { readFile as rf } from \"fs\";
export { rf } from \"./io\";

class Store { load() { return fs.load(); } }
function load() {}
const open = () => load();
var legacy = function () {};
export function save() {
  const local = load();
  new Store();
  fs.open();
}
// load
const text = \"load\";
";

const JAVASCRIPT_TABLE: &[Row] = &[
    // Line 4 declares the method and calls `fs.load`. Lines 13 and 14 are not
    // sites.
    ("load", &[4, 5], &[4, 6, 9], &[]),
    ("open", &[6], &[11], &[]),
    ("legacy", &[7], &[], &[]),
    ("save", &[8], &[], &[]),
    ("Store", &[4], &[10], &[]),
    ("text", &[14], &[], &[]),
    // Inside a function body, a declarator is not a definition.
    ("local", &[], &[], &[]),
    ("fs", &[], &[], &[1]),
    ("rf", &[], &[], &[1, 2]),
    ("readFile", &[], &[], &[1]),
];

const GO: &str = "\
package main

import (
    \"fmt\"
    str \"strings\"
)

type Server struct{ port int }
type Handler interface{ Serve() }
const Max, Min = 1, 2
var count int

func (s *Server) Serve() {}
func Serve() {}
func main() {
    Serve()
    s.Serve()
    fmt.Println(str.ToUpper(\"x\"))
    go Serve()
    defer s.Serve()
    use(Serve)
}
";

const GO_TABLE: &[Row] = &[
    // A method and a function; bare, selector, `go` and `defer` calls. Line 9
    // is an interface method, and line 21 passes the name as an argument.
    ("Serve", &[13, 14], &[16, 17, 19, 20], &[]),
    ("Server", &[8], &[], &[]),
    ("Handler", &[9], &[], &[]),
    // One `const_spec` declares both names.
    ("Max", &[10], &[], &[]),
    ("Min", &[10], &[], &[]),
    ("count", &[11], &[], &[]),
    ("main", &[15], &[], &[]),
    // An import alias; the operand of `str.ToUpper` is not a callee.
    ("str", &[], &[], &[5]),
    ("Println", &[], &[18], &[]),
    ("ToUpper", &[], &[18], &[]),
    ("use", &[], &[21], &[]),
    // The import path is a string, not an identifier.
    ("fmt", &[], &[], &[]),
    ("port", &[], &[], &[]),
];

/// Every language with its fixture and frozen table.
fn fixtures() -> Vec<(OracleLang, &'static str, &'static [Row])> {
    vec![
        (OracleLang::Rust, RUST, RUST_TABLE),
        (OracleLang::Python, PYTHON, PYTHON_TABLE),
        (OracleLang::TypeScript, TYPESCRIPT, TYPESCRIPT_TABLE),
        (OracleLang::Tsx, TYPESCRIPT, TYPESCRIPT_TABLE),
        (OracleLang::Tsx, TSX, TSX_TABLE),
        (OracleLang::JavaScript, JAVASCRIPT, JAVASCRIPT_TABLE),
        (OracleLang::Go, GO, GO_TABLE),
    ]
}

#[test]
fn every_fixture_parses_without_error_nodes() {
    for (lang, source, _) in fixtures() {
        assert!(
            parses_cleanly(lang, source),
            "{lang} fixture has parse errors"
        );
    }
}

#[test]
fn the_fixtures_cover_every_oracle_language_and_every_role() {
    let langs: BTreeSet<OracleLang> = fixtures().iter().map(|&(lang, ..)| lang).collect();
    assert_eq!(langs, OracleLang::ALL.into_iter().collect());
    for (lang, _, table) in fixtures() {
        let has = |pick: fn(&Row) -> bool| table.iter().any(pick);
        assert!(has(|r| !r.1.is_empty()), "{lang}: no definition row");
        assert!(has(|r| !r.2.is_empty()), "{lang}: no call row");
        assert!(has(|r| !r.3.is_empty()), "{lang}: no import row");
    }
}

#[test]
fn the_role_queries_give_the_frozen_lines_for_rust() {
    assert_table(OracleLang::Rust, RUST, RUST_TABLE);
}

#[test]
fn the_role_queries_give_the_frozen_lines_for_python() {
    assert_table(OracleLang::Python, PYTHON, PYTHON_TABLE);
}

#[test]
fn the_role_queries_give_the_frozen_lines_for_typescript() {
    assert_table(OracleLang::TypeScript, TYPESCRIPT, TYPESCRIPT_TABLE);
}

#[test]
fn the_role_queries_give_the_frozen_lines_for_tsx() {
    assert_table(OracleLang::Tsx, TYPESCRIPT, TYPESCRIPT_TABLE);
    assert_table(OracleLang::Tsx, TSX, TSX_TABLE);
}

#[test]
fn the_role_queries_give_the_frozen_lines_for_javascript() {
    assert_table(OracleLang::JavaScript, JAVASCRIPT, JAVASCRIPT_TABLE);
}

#[test]
fn the_role_queries_give_the_frozen_lines_for_go() {
    assert_table(OracleLang::Go, GO, GO_TABLE);
}

#[test]
fn the_declared_names_are_the_definition_rows_of_the_frozen_tables() {
    for (lang, source, table) in fixtures() {
        let found = definition_names(lang, source).unwrap();
        // Every table row: the lines the name is declared on equal the table's.
        for &(name, defs, ..) in table {
            let lines: Vec<u32> = found
                .iter()
                .filter(|(n, _)| n == name)
                .map(|&(_, line)| line)
                .collect();
            assert_eq!(lines, defs, "{lang} {name}");
        }
        // Every declared name is a name `role_sites` agrees is declared there.
        for (name, line) in &found {
            let sites = role_sites(lang, source, name).unwrap();
            assert!(sites.defs.contains(line), "{lang} {name} line {line}");
        }
    }
}

#[test]
fn a_file_with_no_declaration_declares_no_name() {
    assert!(
        definition_names(OracleLang::Rust, "// nothing\n")
            .unwrap()
            .is_empty()
    );
}
#[test]
fn an_empty_name_is_an_error_not_an_empty_answer() {
    let err = role_sites(OracleLang::Rust, RUST, "").unwrap_err();
    assert!(format!("{err:#}").contains("non-empty name"), "{err:#}");
}

#[test]
fn a_name_in_a_file_with_no_site_gives_empty_lists() {
    assert_eq!(
        role_sites(OracleLang::Rust, "fn other() {}\n", "render").unwrap(),
        RoleSites::default()
    );
}

// ============================================================================
// Registry: compilation, files on disk, grammar identity
// ============================================================================

#[test]
fn every_role_query_compiles_with_the_captures_of_its_role() {
    RoleOracle::new().unwrap();
}

#[test]
fn there_is_one_query_for_each_role_and_language() {
    let keys: BTreeSet<(&str, &str)> = ROLE_QUERIES
        .iter()
        .map(|q| (q.role.file_stem(), q.lang.as_str()))
        .collect();
    assert_eq!(ROLE_QUERIES.len(), 18);
    assert_eq!(
        keys.len(),
        18,
        "a (role, language) pair is registered twice"
    );
    for role in ["definition", "call", "import"] {
        for lang in OracleLang::ALL {
            assert!(keys.contains(&(role, lang.as_str())), "{role}.{lang}");
        }
    }
}

#[test]
fn every_scm_file_under_roles_is_registered_and_is_the_file_its_row_names() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("queries/roles");
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".scm"))
        .collect();
    let registered: BTreeSet<String> = ROLE_QUERIES.iter().map(RoleQuery::file_name).collect();
    assert_eq!(on_disk, registered);
    for q in ROLE_QUERIES {
        let from_disk = std::fs::read_to_string(dir.join(q.file_name())).unwrap();
        assert_eq!(
            from_disk,
            q.source,
            "{} is not the file it names",
            q.file_name()
        );
    }
}

#[test]
fn every_role_query_names_the_grammar_version_cargo_lock_resolves() {
    let locked: BTreeMap<String, String> = locked_dependency_versions();
    let problems: Vec<String> = ROLE_QUERIES
        .iter()
        .filter_map(|q| {
            grammar_header_problem(q.source, q.lang, &locked)
                .map(|problem| format!("{}: {problem}", q.file_name()))
        })
        .collect();
    assert!(
        problems.is_empty(),
        "role query grammar headers disagree with Cargo.lock:\n{}",
        problems.join("\n")
    );
}

#[test]
fn a_capture_the_role_does_not_use_is_a_compile_error_not_a_silent_miss() {
    let misspelt = RoleQuery {
        role: Role::Call,
        lang: OracleLang::Rust,
        source: "(call_expression function: (identifier) @calee)",
    };
    let err = RoleOracle::from_queries(&[misspelt]).err().unwrap();
    assert!(format!("{err:#}").contains("@calee"), "{err:#}");
}

#[test]
fn a_query_without_the_capture_its_role_needs_is_a_compile_error() {
    let bare = RoleQuery {
        role: Role::Import,
        lang: OracleLang::Python,
        source: "(import_statement)",
    };
    let err = RoleOracle::from_queries(&[bare]).err().unwrap();
    assert!(format!("{err:#}").contains("no @import capture"), "{err:#}");
}

#[test]
fn a_query_that_does_not_compile_for_its_grammar_is_a_compile_error() {
    let wrong_grammar = RoleQuery {
        role: Role::Definition,
        lang: OracleLang::Python,
        source: "(function_item name: (identifier) @name)",
    };
    assert!(RoleOracle::from_queries(&[wrong_grammar]).is_err());
}

#[test]
fn a_role_and_language_registered_twice_is_a_compile_error() {
    let q = RoleQuery {
        role: Role::Call,
        lang: OracleLang::Go,
        source: "(call_expression function: (identifier) @callee)",
    };
    let err = RoleOracle::from_queries(&[q, q]).err().unwrap();
    assert!(format!("{err:#}").contains("registered twice"), "{err:#}");
}
