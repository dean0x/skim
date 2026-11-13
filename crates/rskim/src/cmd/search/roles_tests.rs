//! Role agreement test: what the parts of the search engine that exist say
//! about a name, against the role oracle of the scoreboard.
//!
//! The oracle (`rskim_oracle::roles`) restates, from the grammars alone, the
//! lines where a name is defined, called and imported. This test reads every
//! word of one fixture per oracle language and compares the oracle with the
//! parts of the engine that decide a role today:
//!
//! - `def` comes from the declaration rule (`rskim_core::declarations`): a
//!   declaration whose name range holds the word.
//! - `comment` and `string` come from the classifier (`classify_tree`).
//! - A word the classifier puts in the function-body field is a code
//!   identifier: a call or a reference. The call rule and the import rule need
//!   the callee table and the import ancestors, and arrive with the role rules
//!   themselves, so this test checks the part that exists: every call and
//!   import site of the oracle holds a code occurrence, never a comment or a
//!   string only.
//!
//! The expected-mismatch table lists each disagreement that is known today,
//! with its reason. The test fails both for a mismatch that is not in the table
//! and for a table row that no longer mismatches, so a row leaves the table in
//! the commit that closes its gap. Ruby is not an oracle language: its calls
//! have no row here.
//!
//! Run with `--nocapture` to print the agreement per language.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use rskim_core::{Language, Parser, declarations};
use rskim_oracle::roles::{RoleSites, role_sites};
use rskim_oracle::structural::OracleLang;

use super::classifier::classify_tree;
use super::field::SearchField;

// ============================================================================
// Fixtures
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

/// Parsed with the TSX grammar only: the plain TypeScript grammar reads JSX as
/// a type assertion, so the declarations after it would be lost.
const TSX: &str = "\
import { Card } from \"./card\";

function Page() {
  return <Card title=\"Card\" />;
}
const App = () => <Page />;
const x = Card();
// Card
";

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
// Serve
var label = \"Serve\"
";

/// One fixture: the oracle language, the core language and file extension the
/// engine parses it with, and the source.
struct Fixture {
    label: &'static str,
    oracle: OracleLang,
    lang: Language,
    ext: &'static str,
    source: &'static str,
}

fn fixtures() -> Vec<Fixture> {
    let fixture = |label, oracle, lang, ext, source| Fixture {
        label,
        oracle,
        lang,
        ext,
        source,
    };
    vec![
        fixture("rust", OracleLang::Rust, Language::Rust, "rs", RUST),
        fixture("python", OracleLang::Python, Language::Python, "py", PYTHON),
        fixture(
            "typescript",
            OracleLang::TypeScript,
            Language::TypeScript,
            "ts",
            TYPESCRIPT,
        ),
        fixture(
            "tsx",
            OracleLang::Tsx,
            Language::TypeScript,
            "tsx",
            TYPESCRIPT,
        ),
        fixture("tsx-jsx", OracleLang::Tsx, Language::TypeScript, "tsx", TSX),
        fixture(
            "javascript",
            OracleLang::JavaScript,
            Language::JavaScript,
            "js",
            JAVASCRIPT,
        ),
        fixture("go", OracleLang::Go, Language::Go, "go", GO),
    ]
}

// ============================================================================
// What the engine parts say about a name
// ============================================================================

/// The 1-based lines where the engine parts see `name`, by the field they give.
#[derive(Debug, Default)]
struct EngineSites {
    /// A declaration names `name` here.
    defs: BTreeSet<u32>,
    /// The classifier puts an occurrence in the function-body field: a call or
    /// a reference.
    code: BTreeSet<u32>,
    /// An occurrence in any other code field (a definition tier, an import).
    other_code: BTreeSet<u32>,
    comments: BTreeSet<u32>,
    strings: BTreeSet<u32>,
}

/// The kind of one disagreement between the engine parts and the oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Gap {
    /// The oracle has a definition on this line; no declaration names it.
    DefMissed,
    /// A declaration names the word on this line; the oracle has no definition.
    DefExtra,
    /// The oracle has a call on this line; the classifier gives no
    /// function-body occurrence there.
    CallNotCode,
    /// The oracle has a site (a definition, a call or an import) on this line;
    /// the classifier gives only comment or string occurrences there.
    SiteInTextOnly,
}

/// One disagreement: fixture label, word, kind, 1-based line.
type Mismatch = (&'static str, String, Gap, u32);

fn is_word_char(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// Byte offsets where `name` occurs as a whole word.
fn occurrences(source: &str, name: &str) -> Vec<usize> {
    source
        .match_indices(name)
        .map(|(start, _)| start)
        .filter(|&start| {
            !is_word_char(source[..start].chars().next_back())
                && !is_word_char(source[start + name.len()..].chars().next())
        })
        .collect()
}

fn line_of(source: &str, byte: usize) -> u32 {
    let newlines = source.as_bytes()[..byte]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();
    u32::try_from(newlines + 1).unwrap()
}

fn field_at(ranges: &[(Range<usize>, SearchField)], byte: usize) -> SearchField {
    ranges
        .iter()
        .find(|(range, _)| range.contains(&byte))
        .map(|(_, field)| *field)
        .unwrap_or_else(|| panic!("no classifier range holds byte {byte}"))
}

/// Parse `fixture` once and read the engine parts for each word of `names`.
fn engine_sites(fixture: &Fixture, names: &BTreeSet<&str>) -> BTreeMap<String, EngineSites> {
    let source = fixture.source;
    let mut parser = Parser::new_for_extension(fixture.lang, fixture.ext)
        .unwrap_or_else(|e| panic!("{}: {e}", fixture.label));
    let tree = parser.parse(source).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{} fixture has parse errors",
        fixture.label
    );
    let scan = declarations(&tree, source, fixture.lang);
    assert!(
        !scan.truncated,
        "{} declaration walk truncated",
        fixture.label
    );
    let ranges = classify_tree(&tree, source, fixture.lang);

    names
        .iter()
        .map(|&name| {
            let mut sites = EngineSites::default();
            for decl in scan.decls.iter().filter(|d| d.name == name) {
                sites.defs.insert(line_of(source, decl.name_bytes.start));
            }
            for start in occurrences(source, name) {
                let line = line_of(source, start);
                let lines = match field_at(&ranges, start) {
                    SearchField::Comment => &mut sites.comments,
                    SearchField::StringLiteral => &mut sites.strings,
                    SearchField::FunctionBody => &mut sites.code,
                    _ => &mut sites.other_code,
                };
                lines.insert(line);
            }
            (name.to_string(), sites)
        })
        .collect()
}

/// Every word (`[A-Za-z_][A-Za-z0-9_]*`) of `source`.
fn words(source: &str) -> BTreeSet<&str> {
    let mut found = BTreeSet::new();
    let mut start = None;
    for (i, c) in source.char_indices().chain([(source.len(), ' ')]) {
        let word_char = c.is_ascii_alphanumeric() || c == '_';
        match (start, word_char) {
            (None, true) if !c.is_ascii_digit() => start = Some(i),
            (Some(s), false) => {
                found.insert(&source[s..i]);
                start = None;
            }
            _ => {}
        }
    }
    found
}

fn lines(sites: &[u32]) -> BTreeSet<u32> {
    sites.iter().copied().collect()
}

/// Compare the oracle with the engine parts for every word of one fixture.
/// Returns the disagreements and the count of oracle sites compared.
fn compare(fixture: &Fixture) -> (BTreeSet<Mismatch>, usize) {
    let names = words(fixture.source);
    let engine = engine_sites(fixture, &names);
    let mut mismatches = BTreeSet::new();
    let mut compared = 0usize;

    for name in &names {
        let oracle = role_sites(fixture.oracle, fixture.source, name)
            .unwrap_or_else(|e| panic!("{} {name}: {e:#}", fixture.label));
        let RoleSites {
            defs,
            calls,
            imports,
        } = &oracle;
        let seen = &engine[*name];
        let mut push = |gap, line| {
            mismatches.insert((fixture.label, name.to_string(), gap, line));
        };

        let oracle_defs = lines(defs);
        for &line in oracle_defs.difference(&seen.defs) {
            push(Gap::DefMissed, line);
        }
        for &line in seen.defs.difference(&oracle_defs) {
            push(Gap::DefExtra, line);
        }
        for line in lines(calls).difference(&seen.code) {
            push(Gap::CallNotCode, *line);
        }
        let site_lines: BTreeSet<u32> = [defs, calls, imports]
            .into_iter()
            .flat_map(|sites| sites.iter().copied())
            .collect();
        for line in site_lines {
            if !seen.code.contains(&line) && !seen.other_code.contains(&line) {
                push(Gap::SiteInTextOnly, line);
            }
        }
        compared += defs.len() + calls.len() + imports.len();
    }
    (mismatches, compared)
}

// ============================================================================
// Expected mismatches
// ============================================================================

/// A disagreement that is known today, and why.
struct KnownGap {
    label: &'static str,
    name: &'static str,
    gap: Gap,
    line: u32,
    reason: &'static str,
}

/// Every disagreement between the engine parts and the oracle that the test
/// accepts. A row leaves this table in the commit that closes its gap.
const KNOWN_GAPS: &[KnownGap] = &[
    KnownGap {
        label: "python",
        name: "build",
        gap: Gap::CallNotCode,
        line: 13,
        reason: PYTHON_CALL,
    },
    KnownGap {
        label: "python",
        name: "Widget",
        gap: Gap::CallNotCode,
        line: 15,
        reason: PYTHON_CALL,
    },
    KnownGap {
        label: "python",
        name: "print",
        gap: Gap::CallNotCode,
        line: 16,
        reason: PYTHON_CALL,
    },
    KnownGap {
        label: "python",
        name: "use",
        gap: Gap::CallNotCode,
        line: 17,
        reason: PYTHON_CALL,
    },
    KnownGap {
        label: "go",
        name: "Min",
        gap: Gap::DefMissed,
        line: 10,
        reason: "one `const_spec` names `Max` and `Min`; the declaration rule yields the \
                 first name of a spec only",
    },
];

/// The reason of every Python call row. Ruby has the same node, and no row
/// because it is not an oracle language.
const PYTHON_CALL: &str = "a bare Python call is a `call` node, which has import-level priority \
                           in `node_kind_priority`; the classifier reads its `function` child \
                           as a declaration name and gives the symbol-name field, not the \
                           function-body field. The call rule of the role engine reads the \
                           callee position and not this field";

/// Words that the fixtures put in a comment or in a string, found by reading
/// each fixture: (fixture label, word, comment lines, string lines).
const TEXT_ROWS: &[(&str, &str, &[u32], &[u32])] = &[
    ("rust", "render", &[4], &[11]),
    ("python", "build", &[4], &[16]),
    ("typescript", "fetchAll", &[27], &[]),
    ("typescript", "db", &[], &[1, 3]),
    ("tsx", "fetchAll", &[27], &[]),
    ("tsx-jsx", "Card", &[8], &[4]),
    ("javascript", "load", &[13], &[14]),
    ("go", "Serve", &[23], &[24]),
    ("go", "fmt", &[], &[4]),
];

// ============================================================================
// Tests
// ============================================================================

fn known_mismatches() -> BTreeSet<Mismatch> {
    KNOWN_GAPS
        .iter()
        .map(|g| (g.label, g.name.to_string(), g.gap, g.line))
        .collect()
}

#[test]
fn engine_parts_agree_with_the_role_oracle_except_for_the_known_gaps() {
    let mut measured = BTreeSet::new();
    for fixture in fixtures() {
        let (mismatches, compared) = compare(&fixture);
        eprintln!(
            "role agreement {:<10} {compared:>3} oracle sites, {:>2} mismatches",
            fixture.label,
            mismatches.len()
        );
        measured.extend(mismatches);
    }

    let expected = known_mismatches();
    let unexpected: Vec<_> = measured.difference(&expected).collect();
    let stale: Vec<_> = expected.difference(&measured).collect();
    assert!(
        unexpected.is_empty() && stale.is_empty(),
        "mismatches not in the known-gap table: {unexpected:#?}\n\
         known-gap rows that no longer mismatch: {stale:#?}"
    );
    for gap in KNOWN_GAPS {
        assert!(
            !gap.reason.is_empty(),
            "{} {} has no reason",
            gap.label,
            gap.name
        );
    }
}

#[test]
fn comment_and_string_occurrences_get_the_text_fields_and_are_never_oracle_sites() {
    let fixtures = fixtures();
    for &(label, name, comment_lines, string_lines) in TEXT_ROWS {
        let fixture = fixtures
            .iter()
            .find(|f| f.label == label)
            .unwrap_or_else(|| panic!("no fixture {label}"));
        let names = BTreeSet::from([name]);
        let seen = &engine_sites(fixture, &names)[name];
        assert_eq!(
            seen.comments,
            lines(comment_lines),
            "{label} {name}: comments"
        );
        assert_eq!(seen.strings, lines(string_lines), "{label} {name}: strings");

        let oracle = role_sites(fixture.oracle, fixture.source, name).unwrap();
        for line in comment_lines.iter().chain(string_lines) {
            let site = [&oracle.defs, &oracle.calls, &oracle.imports]
                .iter()
                .any(|sites| sites.contains(line) && !seen.code.contains(line));
            assert!(
                !site,
                "{label} {name}: the oracle has a site on line {line}, which holds the word \
                 only in a comment or a string"
            );
        }
    }
}
