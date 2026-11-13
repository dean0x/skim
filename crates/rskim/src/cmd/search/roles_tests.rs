//! Tests of the role rules: the agreement with the role oracle of the
//! scoreboard, and the rules one by one.
//!
//! The oracle (`rskim_oracle::roles`) restates, from the grammars alone, the
//! lines where a name is defined, called and imported. The agreement test reads
//! every word of one fixture per oracle language and compares the oracle with
//! the role the engine gives each occurrence ([`Labels::role`]): the lines with
//! a `def`, a `call` and an `import` role must be the oracle's lines.
//!
//! The two must agree line for line. Ruby is not an oracle language: its calls
//! have a test of their own, as have the languages the oracle does not cover.
//!
//! Run with `--nocapture` to print the agreement per language.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{BTreeMap, BTreeSet};

use rskim_core::Language;
use rskim_oracle::roles::{RoleSites, role_sites};
use rskim_oracle::structural::OracleLang;

use super::{
    Labelling, Labels, Role, Unlabelled, declaration_keywords, has_declaration_keyword, is_partial,
};

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
// What the engine says about a name
// ============================================================================

/// The 1-based lines where the engine gives `name` each role.
#[derive(Debug, Default)]
struct EngineSites {
    defs: BTreeSet<u32>,
    calls: BTreeSet<u32>,
    imports: BTreeSet<u32>,
    refs: BTreeSet<u32>,
    comments: BTreeSet<u32>,
    strings: BTreeSet<u32>,
}

/// The kind of one disagreement between the engine and the oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Gap {
    DefMissed,
    DefExtra,
    CallMissed,
    CallExtra,
    ImportMissed,
    ImportExtra,
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

fn labels_of(fixture: &Fixture) -> Labels {
    Labels::new(fixture.source, fixture.lang, fixture.ext)
        .unwrap_or_else(|e| panic!("{}: {e:?}", fixture.label))
}

/// Label `fixture` once and read the role of each occurrence of each word of
/// `names`.
fn engine_sites(fixture: &Fixture, names: &BTreeSet<&str>) -> BTreeMap<String, EngineSites> {
    let source = fixture.source;
    let labels = labels_of(fixture);
    names
        .iter()
        .map(|&name| {
            let mut sites = EngineSites::default();
            for start in occurrences(source, name) {
                let line = line_of(source, start);
                let lines = match labels.role(&(start..start + name.len())) {
                    Role::Def => &mut sites.defs,
                    Role::Call => &mut sites.calls,
                    Role::Import => &mut sites.imports,
                    Role::Comment => &mut sites.comments,
                    Role::StringLit => &mut sites.strings,
                    Role::Ref => &mut sites.refs,
                    other => panic!("{} {name}: unexpected role {other:?}", fixture.label),
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

/// Compare the oracle with the engine for every word of one fixture. Returns
/// the disagreements and the count of oracle sites compared.
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
        let mut check = |missed, extra, oracle_lines: &[u32], engine_lines: &BTreeSet<u32>| {
            let oracle_lines = lines(oracle_lines);
            for &line in oracle_lines.difference(engine_lines) {
                mismatches.insert((fixture.label, name.to_string(), missed, line));
            }
            for &line in engine_lines.difference(&oracle_lines) {
                mismatches.insert((fixture.label, name.to_string(), extra, line));
            }
        };
        check(Gap::DefMissed, Gap::DefExtra, defs, &seen.defs);
        check(Gap::CallMissed, Gap::CallExtra, calls, &seen.calls);
        check(Gap::ImportMissed, Gap::ImportExtra, imports, &seen.imports);
        compared += defs.len() + calls.len() + imports.len();
    }
    (mismatches, compared)
}

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
// Agreement with the oracle
// ============================================================================

#[test]
fn engine_roles_agree_with_the_role_oracle() {
    let mut mismatches = BTreeSet::new();
    for fixture in fixtures() {
        let (found, compared) = compare(&fixture);
        eprintln!(
            "role agreement {:<10} {compared:>3} oracle sites, {:>2} mismatches",
            fixture.label,
            found.len()
        );
        assert!(compared > 0, "{} compared no oracle site", fixture.label);
        mismatches.extend(found);
    }
    assert!(
        mismatches.is_empty(),
        "the engine and the role oracle disagree: {mismatches:#?}"
    );
}

#[test]
fn comment_and_string_occurrences_get_the_text_roles_and_are_never_oracle_sites() {
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
                .any(|sites| sites.contains(line) && !seen.refs.contains(line));
            assert!(
                !site,
                "{label} {name}: the oracle has a site on line {line}, which holds the word \
                 only in a comment or a string"
            );
        }
    }
}

// ============================================================================
// The rules, one by one
// ============================================================================

/// The role of the first whole-word occurrence of `needle` at or after the
/// first occurrence of `after` (or of the start).
fn role_of(lang: Language, ext: &str, source: &str, after: &str, needle: &str) -> Role {
    let labels = Labels::new(source, lang, ext).unwrap();
    let from = source.find(after).unwrap();
    let start = occurrences(source, needle)
        .into_iter()
        .find(|&s| s >= from)
        .unwrap_or_else(|| panic!("no {needle:?} after {after:?}"));
    labels.role(&(start..start + needle.len()))
}

#[test]
fn a_call_in_python_ruby_and_go_gets_the_call_role() {
    let python = "def build():\n    pass\n\ndef main():\n    build()\n    obj.build()\n";
    assert_eq!(
        role_of(Language::Python, "py", python, "def main", "build"),
        Role::Call
    );
    assert_eq!(
        role_of(Language::Python, "py", python, "obj.", "build"),
        Role::Call
    );

    let ruby = "def build(x)\nend\n\ndef main\n  build(1)\n  obj.build\n  Widget.create(2)\nend\n";
    assert_eq!(
        role_of(Language::Ruby, "rb", ruby, "def main", "build"),
        Role::Call
    );
    assert_eq!(
        role_of(Language::Ruby, "rb", ruby, "obj.", "build"),
        Role::Call
    );
    assert_eq!(
        role_of(Language::Ruby, "rb", ruby, "Widget", "create"),
        Role::Call
    );
    assert_eq!(
        role_of(Language::Ruby, "rb", ruby, "Widget", "Widget"),
        Role::Ref,
        "the receiver of a method call is not the callee"
    );
    assert_eq!(role_of(Language::Ruby, "rb", ruby, "", "build"), Role::Def);

    let go = "package main\n\nfunc Serve() {}\n\nfunc main() {\n\tServe()\n\ts.Serve()\n\tgo Serve()\n}\n";
    assert_eq!(
        role_of(Language::Go, "go", go, "func main", "Serve"),
        Role::Call
    );
    assert_eq!(role_of(Language::Go, "go", go, "s.", "Serve"), Role::Call);
    assert_eq!(
        role_of(Language::Go, "go", go, "go Serve", "Serve"),
        Role::Call
    );
}

#[test]
fn a_tsx_file_with_jsx_labels_the_definition_and_the_call() {
    let tsx = "import { Card } from \"./card\";\n\nfunction Page() {\n  return <Card title=\"x\" />;\n}\nconst top = Card();\n";
    assert_eq!(
        role_of(Language::TypeScript, "tsx", tsx, "", "Page"),
        Role::Def
    );
    assert_eq!(
        role_of(Language::TypeScript, "tsx", tsx, "const top", "Card"),
        Role::Call
    );
    assert_eq!(
        role_of(Language::TypeScript, "tsx", tsx, "", "Card"),
        Role::Import,
        "the first occurrence is the import"
    );
    assert_eq!(
        role_of(Language::TypeScript, "tsx", tsx, "return", "Card"),
        Role::Ref,
        "a JSX element is not a call"
    );
}

#[test]
fn the_callee_table_covers_the_languages_without_an_oracle() {
    let cases: [(Language, &str, &str, &str, &str); 6] = [
        (
            Language::Java,
            "java",
            "class A { void run() { helper(1); obj.helper(2); new Helper(); } }\n",
            "helper(1)",
            "helper",
        ),
        (
            Language::CSharp,
            "cs",
            "class A { void Run() { Helper(1); obj.Helper(2); } }\n",
            "Helper(1)",
            "Helper",
        ),
        (
            Language::C,
            "c",
            "void run(void) { helper(1); }\n",
            "helper(1)",
            "helper",
        ),
        (
            Language::Cpp,
            "cpp",
            "void run() { obj.helper(1); }\n",
            "obj.helper",
            "helper",
        ),
        (
            Language::Kotlin,
            "kt",
            "fun run() { helper(1) }\n",
            "helper(1)",
            "helper",
        ),
        (
            Language::Swift,
            "swift",
            "func run() { helper(1) }\n",
            "helper(1)",
            "helper",
        ),
    ];
    for (lang, ext, source, after, needle) in cases {
        assert_eq!(
            role_of(lang, ext, source, after, needle),
            Role::Call,
            "{lang:?}: {source}"
        );
    }
    let bash = "run() { helper 1; }\nhelper() { :; }\n";
    assert_eq!(
        role_of(Language::Bash, "sh", bash, "helper 1", "helper"),
        Role::Call
    );
}

#[test]
fn a_receiver_a_path_segment_and_an_argument_are_references_not_calls() {
    let rust = "fn run() { view::render(); panel.render(); apply(render); }\nfn render() {}\n";
    assert_eq!(role_of(Language::Rust, "rs", rust, "", "view"), Role::Ref);
    assert_eq!(
        role_of(Language::Rust, "rs", rust, "view::", "render"),
        Role::Call
    );
    assert_eq!(
        role_of(Language::Rust, "rs", rust, "panel", "panel"),
        Role::Ref
    );
    assert_eq!(
        role_of(Language::Rust, "rs", rust, "apply(", "render"),
        Role::Ref
    );
}

#[test]
fn a_call_inside_a_rust_macro_argument_gets_the_call_role() {
    let source = "fn t() {\n    assert_eq!(strip_ansi(input), 1);\n    \
                  assert!(view::paint(x));\n    assert!(panel.draw(y));\n}\n";
    assert_eq!(
        role_of(Language::Rust, "rs", source, "assert_eq", "strip_ansi"),
        Role::Call
    );
    assert_eq!(
        role_of(Language::Rust, "rs", source, "assert!(view", "paint"),
        Role::Call
    );
    assert_eq!(
        role_of(Language::Rust, "rs", source, "assert!(panel", "draw"),
        Role::Call
    );
    // The receiver and the path segment are not callees.
    assert_eq!(
        role_of(Language::Rust, "rs", source, "assert!(view", "view"),
        Role::Ref
    );
    assert_eq!(
        role_of(Language::Rust, "rs", source, "assert!(panel", "panel"),
        Role::Ref
    );
}

#[test]
fn a_plain_argument_and_a_definition_in_a_macro_argument_are_not_calls() {
    let source = "fn t() {\n    assert_eq!(strip_ansi(input), 1);\n    \
                  keep!(fn made(a: u8) {});\n    keep!(struct Shape(u8););\n    \
                  keep!(nested!(inner));\n    keep!(table[slot]);\n}\n";
    assert_eq!(
        role_of(Language::Rust, "rs", source, "assert_eq", "input"),
        Role::Ref
    );
    assert_ne!(
        role_of(Language::Rust, "rs", source, "keep!(fn", "made"),
        Role::Call
    );
    assert_ne!(
        role_of(Language::Rust, "rs", source, "keep!(struct", "Shape"),
        Role::Call
    );
    // A nested macro name is followed by `!`, so the token rule does not match it.
    assert_eq!(
        role_of(Language::Rust, "rs", source, "keep!(nested", "nested"),
        Role::Ref
    );
    assert_eq!(
        role_of(Language::Rust, "rs", source, "keep!(nested", "inner"),
        Role::Ref
    );
    // An index is a bracketed token tree, not a parenthesised one.
    assert_eq!(
        role_of(Language::Rust, "rs", source, "keep!(table", "table"),
        Role::Ref
    );
}

#[test]
fn an_import_needs_an_import_node_above_the_identifier() {
    let ts =
        "import { a } from \"./a\";\nexport { b } from \"./b\";\nexport { c };\nconst c = 1;\n";
    assert_eq!(
        role_of(Language::TypeScript, "ts", ts, "", "a"),
        Role::Import
    );
    assert_eq!(
        role_of(Language::TypeScript, "ts", ts, "", "b"),
        Role::Import
    );
    assert_eq!(
        role_of(Language::TypeScript, "ts", ts, "export { c }", "c"),
        Role::Ref,
        "an export with no source is not an import"
    );
    let rust = "use std::fmt::Display;\nfn f(d: Display) {}\n";
    assert_eq!(
        role_of(Language::Rust, "rs", rust, "", "Display"),
        Role::Import
    );
    assert_eq!(
        role_of(Language::Rust, "rs", rust, "fn f", "Display"),
        Role::Ref
    );
}

#[test]
fn a_declared_name_is_a_def_only_when_the_match_is_the_whole_name() {
    let rust = "fn launch_worker() {}\n";
    let labels = Labels::new(rust, Language::Rust, "rs").unwrap();
    assert_eq!(labels.role(&(3..16)), Role::Def);
    assert_eq!(
        labels.role(&(3..9)),
        Role::Ref,
        "part of the name is not the name"
    );
    assert_eq!(labels.def_lines(&(3..16)), Some((1, 1)));
    assert_eq!(labels.def_lines(&(3..9)), None);
}

#[test]
fn markdown_and_data_formats_give_text_outside_comments_and_strings() {
    let md = "# launch\n\nlaunch the worker <!-- launch -->\n";
    let labels = Labels::new(md, Language::Markdown, "md").unwrap();
    assert_eq!(labels.role(&(2..8)), Role::Text);

    let json = "{\"launch\": \"launch now\"}\n";
    let labels = Labels::new(json, Language::Json, "json").unwrap();
    assert_eq!(labels.role(&(2..8)), Role::Text, "a key is text");
    assert_eq!(labels.role(&(12..18)), Role::StringLit);

    assert_eq!(Labels::text().role(&(0..3)), Role::Text);
}

#[test]
fn markdown_prose_and_code_blocks_are_text_not_comments() {
    let md =
        "# launch\n\nlaunch the worker\n\n- launch item\n\n> launch quote\n\n```\nlaunch();\n```\n";
    let labels = Labels::new(md, Language::Markdown, "md").unwrap();
    for (at, what) in [
        (md.find("launch the").unwrap(), "a paragraph"),
        (md.find("launch item").unwrap(), "a list item"),
        (md.find("launch quote").unwrap(), "a block quote"),
        (md.find("launch();").unwrap(), "a code block"),
    ] {
        assert_eq!(labels.role(&(at..at + 6)), Role::Text, "{what} is text");
    }
}

// ============================================================================
// Lines
// ============================================================================

fn classify(
    labelling: &Labelling,
    line: &str,
    matches: impl IntoIterator<Item = std::ops::Range<usize>>,
    identifier: bool,
) -> super::LineHit {
    labelling
        .classify_line(line, 0, matches, identifier)
        .unwrap()
}

#[test]
fn a_line_takes_the_best_role_among_its_matches() {
    let source = "// run\nfn run() { run(); }\n";
    let labelling = Labelling::Labelled(Labels::new(source, Language::Rust, "rs").unwrap());
    // The line `fn run() { run(); }` starts at byte 7: the def, then the call.
    let line = "fn run() { run(); }";
    let hit = labelling
        .classify_line(line, 7, vec![15..18, 3..6], true)
        .unwrap();
    assert_eq!(hit.role, Role::Def);
    assert_eq!(hit.column, 3);
    assert_eq!(hit.def_lines, Some((2, 2)));
}

#[test]
fn a_hit_inside_a_longer_identifier_is_partial_and_needs_no_parse() {
    let line = "export function launchWorkerPool(";
    let unlabelled = Labelling::Unlabelled(Unlabelled::Budget);
    let hit = classify(&unlabelled, line, std::iter::once(16..28), true);
    assert_eq!(hit.role, Role::Partial);
    assert!(is_partial(line, &(16..28)));
    // Not an identifier query: the file's state decides.
    let hit = classify(&unlabelled, line, std::iter::once(16..28), false);
    assert_eq!(hit.role, Role::Unlabelled);
}

#[test]
fn a_line_with_a_whole_hit_and_a_partial_hit_is_not_partial() {
    let line = "launchWorker(launchWorkerPool)";
    let unlabelled = Labelling::Unlabelled(Unlabelled::Budget);
    let hit = classify(&unlabelled, line, vec![0..12, 13..25], true);
    assert_eq!(hit.role, Role::Unlabelled);
    assert_eq!(hit.column, 0);
}

#[test]
fn the_holder_is_the_enclosing_declaration_or_none_at_the_top_level() {
    let source = "const TOP: u32 = helper();\nfn outer() {\n    helper();\n}\nfn helper() {}\n";
    let labels = Labels::new(source, Language::Rust, "rs").unwrap();
    let in_outer = source.find("    helper").unwrap() + 4;
    let holder = labels.holder(in_outer).unwrap();
    assert_eq!(holder.name, "outer");
    assert_eq!((holder.first_line, holder.last_line), (2, 4));
    assert_eq!(labels.holder(source.find("helper()").unwrap()), None);
    assert_eq!(Labels::text().holder(3), None);
}

#[test]
fn the_declaration_keyword_is_a_whole_word_before_the_match() {
    let rust = declaration_keywords(Language::Rust);
    assert!(has_declaration_keyword("pub(super) fn ", rust));
    assert!(has_declaration_keyword("    struct ", rust));
    assert!(!has_declaration_keyword("let defn = ", rust));
    assert!(!has_declaration_keyword("my_fn_helper(", rust));
    assert!(!has_declaration_keyword("fn", &[]));
    assert!(declaration_keywords(Language::Json).is_empty());
    assert!(declaration_keywords(Language::Python).contains(&"def"));
}

#[test]
fn role_tiers_are_in_the_documented_order() {
    let keys: Vec<&str> = Role::ALL.iter().map(|r| r.key()).collect();
    assert_eq!(
        keys,
        [
            "def",
            "call",
            "ref",
            "unlabelled",
            "import",
            "string",
            "comment",
            "text",
            "partial"
        ]
    );
    assert!(Role::ALL.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(Role::Def.tier(), 0);
    assert_eq!(Role::Partial.tier(), 8);
}

#[test]
fn a_partial_hit_in_a_parsed_file_has_no_holder() {
    let source = "fn outer() {\n    launchWorkerPool();\n}\n";
    let labelling = Labelling::Labelled(Labels::new(source, Language::Rust, "rs").unwrap());
    let line = "    launchWorkerPool();";

    let hit = labelling
        .classify_line(line, 13, std::iter::once(4..16), true)
        .unwrap();

    assert_eq!(hit.role, Role::Partial);
    assert_eq!(hit.holder, None);
    assert_eq!(hit.def_lines, None);
}
