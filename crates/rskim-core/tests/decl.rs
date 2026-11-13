//! Tests for the declaration rule, the time-bounded parse and the TSX grammar.
//!
//! Each fixture is an inline source string, so every expected row is visible
//! next to the code it describes. Rows are 0-based, as in the library.

#![allow(clippy::unwrap_used, clippy::expect_used)] // Unwrapping/expect is acceptable in tests

use rskim_core::{
    DeclScan, Declaration, Language, Parser, SkimError, declarations, enclosing_declaration,
};
use std::time::Duration;

// ============================================================================
// Helpers
// ============================================================================

fn scan(lang: Language, ext: &str, src: &str) -> DeclScan {
    let mut parser = Parser::new_for_extension(lang, ext).unwrap();
    let tree = parser.parse(src).unwrap();
    declarations(&tree, src, lang)
}

fn find<'a>(scan: &'a DeclScan, name: &str) -> &'a Declaration {
    let found = scan.decls.iter().find(|d| d.name == name);
    assert!(found.is_some(), "no declaration named {name:?}");
    found.unwrap()
}

fn names_and_kinds(scan: &DeclScan) -> Vec<(&str, &'static str)> {
    scan.decls
        .iter()
        .map(|d| (d.name.as_str(), d.kind))
        .collect()
}

/// Assert the name range of every declaration spells its name in `src`.
fn assert_name_ranges_spell_names(scan: &DeclScan, src: &str) {
    for d in &scan.decls {
        assert_eq!(&src[d.name_bytes.clone()], d.name, "name range of {d:?}");
    }
}

fn offset_of(src: &str, needle: &str) -> usize {
    let found = src.find(needle);
    assert!(found.is_some(), "needle {needle:?} not in source");
    found.unwrap()
}

fn holder(decls: &[Declaration], src: &str, needle: &str) -> Option<String> {
    enclosing_declaration(decls, offset_of(src, needle)).map(|d| d.name.clone())
}

/// A Rust source with `count` small functions. Parsing it takes far more than
/// the 100 parser operations between two checks of the time bound.
fn many_rust_functions(count: usize) -> String {
    (0..count)
        .map(|i| format!("fn f{i}() {{ let x = {i}; }}\n"))
        .collect()
}

// ============================================================================
// TP-44: declarations gives name, kind, name range and row range
// ============================================================================

const RUST_SRC: &str = r#"pub struct Point {
    x: i32,
    y: i32,
}

enum Shape {
    Circle(u32),
    Square { side: u32 },
}

impl<T> Wrapper<T> {
    fn get(&self) -> u32 {
        0
    }
}

trait Draw {
    fn draw(&self);
}

mod inner {
    pub fn helper() {}
}

type Alias = u32;
const LIMIT: u32 = 5;
static NAME: &str = "n";
macro_rules! shout {
    () => {};
}
union Bits { a: u32, b: f32 }
"#;

#[test]
fn rust_declarations_have_name_kind_and_rows() {
    let scan = scan(Language::Rust, "rs", RUST_SRC);

    assert_eq!(
        names_and_kinds(&scan),
        vec![
            ("Point", "struct_item"),
            ("x", "field_declaration"),
            ("y", "field_declaration"),
            ("Shape", "enum_item"),
            ("Circle", "enum_variant"),
            ("Square", "enum_variant"),
            ("side", "field_declaration"),
            ("get", "function_item"),
            ("Draw", "trait_item"),
            ("draw", "function_signature_item"),
            ("inner", "mod_item"),
            ("helper", "function_item"),
            ("Alias", "type_item"),
            ("LIMIT", "const_item"),
            ("NAME", "static_item"),
            ("shout", "macro_definition"),
            ("Bits", "union_item"),
            ("a", "field_declaration"),
            ("b", "field_declaration"),
        ]
    );
    assert_name_ranges_spell_names(&scan, RUST_SRC);
    assert!(!scan.truncated);

    let point = find(&scan, "Point");
    assert_eq!((point.first_row, point.last_row), (0, 3));
    let get = find(&scan, "get");
    assert_eq!((get.first_row, get.last_row), (11, 13));
    let shout = find(&scan, "shout");
    assert_eq!((shout.first_row, shout.last_row), (27, 29));
    let limit = find(&scan, "LIMIT");
    assert_eq!((limit.first_row, limit.last_row), (25, 25));
}

#[test]
fn rust_container_is_the_impl_type_without_generics_or_the_enclosing_item() {
    let scan = scan(Language::Rust, "rs", RUST_SRC);

    assert_eq!(find(&scan, "get").container.as_deref(), Some("Wrapper"));
    assert_eq!(find(&scan, "draw").container.as_deref(), Some("Draw"));
    assert_eq!(find(&scan, "helper").container.as_deref(), Some("inner"));
    assert_eq!(find(&scan, "x").container.as_deref(), Some("Point"));
    assert_eq!(find(&scan, "Circle").container.as_deref(), Some("Shape"));
    assert_eq!(find(&scan, "side").container.as_deref(), Some("Square"));
    assert_eq!(find(&scan, "Point").container, None);
}

#[test]
fn rust_container_of_a_scoped_impl_type_is_its_last_segment() {
    let src = "impl<T> crate::shapes::Circle<T> {\n    fn area(&self) {}\n}\n";
    let scan = scan(Language::Rust, "rs", src);

    assert_eq!(find(&scan, "area").container.as_deref(), Some("Circle"));
}

#[test]
fn rust_function_nested_in_a_function_has_no_container() {
    let src = "impl Outer {\n    fn run() {\n        fn helper() {}\n    }\n}\n";
    let scan = scan(Language::Rust, "rs", src);

    assert_eq!(find(&scan, "run").container.as_deref(), Some("Outer"));
    assert_eq!(find(&scan, "helper").container, None);
}

const TS_SRC: &str = r#"export function plain(a: number): number {
  const local = 1;
  return a + local;
}

const arrow = (x: number) => {
  return x;
};

class Box<T> {
  size(): number {
    return 1;
  }
}

interface Shape { area(): number }
type Id = string;
enum Color { Red, Green }
declare function over(a: string): void;
function* gen() { yield 1; }
abstract class Base {}
const { skipped } = obj;
"#;

#[test]
fn typescript_declarations_have_name_kind_and_rows() {
    let scan = scan(Language::TypeScript, "ts", TS_SRC);

    assert_eq!(
        names_and_kinds(&scan),
        vec![
            ("plain", "function_declaration"),
            ("arrow", "variable_declarator"),
            ("Box", "class_declaration"),
            ("size", "method_definition"),
            ("Shape", "interface_declaration"),
            ("Id", "type_alias_declaration"),
            ("Color", "enum_declaration"),
            ("over", "function_signature"),
            ("gen", "generator_function_declaration"),
            ("Base", "abstract_class_declaration"),
        ]
    );
    assert_name_ranges_spell_names(&scan, TS_SRC);
    assert!(!scan.truncated);

    let plain = find(&scan, "plain");
    assert_eq!((plain.first_row, plain.last_row), (0, 3));
    let arrow = find(&scan, "arrow");
    assert_eq!((arrow.first_row, arrow.last_row), (5, 7));
    let boxed = find(&scan, "Box");
    assert_eq!((boxed.first_row, boxed.last_row), (9, 13));
    assert_eq!(find(&scan, "size").container.as_deref(), Some("Box"));
}

#[test]
fn typescript_local_variables_and_destructuring_are_not_declarations() {
    let scan = scan(Language::TypeScript, "ts", TS_SRC);

    assert!(scan.decls.iter().all(|d| d.name != "local"));
    assert!(scan.decls.iter().all(|d| d.name != "skipped"));
}

#[test]
fn javascript_declarations_cover_functions_classes_and_methods() {
    let src = "function f() {}\nclass K {\n  m() {}\n}\nconst g = function () {};\n";
    let scan = scan(Language::JavaScript, "js", src);

    assert_eq!(
        names_and_kinds(&scan),
        vec![
            ("f", "function_declaration"),
            ("K", "class_declaration"),
            ("m", "method_definition"),
            ("g", "variable_declarator"),
        ]
    );
    assert_eq!(find(&scan, "m").container.as_deref(), Some("K"));
}

const TSX_SRC: &str = r#"const banner = <div>hello</div>;
function App() {
  return <b>{banner}</b>;
}
"#;

#[test]
fn tsx_extension_selects_the_tsx_grammar_and_finds_a_component_after_jsx() {
    let mut tsx = Parser::new_for_extension(Language::TypeScript, "tsx").unwrap();
    let tsx_tree = tsx.parse(TSX_SRC).unwrap();
    assert!(!tsx_tree.root_node().has_error(), "TSX grammar parses JSX");

    let scan = declarations(&tsx_tree, TSX_SRC, Language::TypeScript);
    let app = find(&scan, "App");
    assert_eq!((app.first_row, app.last_row), (1, 3));
    assert_eq!(app.kind, "function_declaration");

    let mut ts = Parser::new(Language::TypeScript).unwrap();
    let ts_tree = ts.parse(TSX_SRC).unwrap();
    assert!(
        ts_tree.root_node().has_error(),
        "the plain TS grammar must not parse JSX; the test would prove nothing otherwise"
    );
}

#[test]
fn extension_other_than_tsx_keeps_the_language_grammar() {
    let src = "fn main() {}\n";
    for ext in ["rs", "tsx", ""] {
        let mut parser = Parser::new_for_extension(Language::Rust, ext).unwrap();
        let tree = parser.parse(src).unwrap();
        assert!(!tree.root_node().has_error(), "ext {ext:?}");
        assert_eq!(parser.language(), Language::Rust);
    }
    let mut ts = Parser::new_for_extension(Language::TypeScript, "ts").unwrap();
    assert!(
        !ts.parse("const a: number = 1;")
            .unwrap()
            .root_node()
            .has_error()
    );
}

#[test]
fn new_for_extension_rejects_languages_without_a_tree_sitter_grammar() {
    let err = Parser::new_for_extension(Language::Json, "json")
        .err()
        .expect("JSON has no tree-sitter grammar");
    assert!(matches!(err, SkimError::ConfigError(_)));
}

#[test]
fn python_declarations_have_name_kind_rows_and_container() {
    let src = r#"import os

class Account:
    def deposit(self, amount):
        return amount

    def withdraw(self):
        def inner():
            pass
        return inner

def top():
    return 1
"#;
    let scan = scan(Language::Python, "py", src);

    assert_eq!(
        names_and_kinds(&scan),
        vec![
            ("Account", "class_definition"),
            ("deposit", "function_definition"),
            ("withdraw", "function_definition"),
            ("inner", "function_definition"),
            ("top", "function_definition"),
        ]
    );
    let account = find(&scan, "Account");
    assert_eq!((account.first_row, account.last_row), (2, 9));
    let deposit = find(&scan, "deposit");
    assert_eq!((deposit.first_row, deposit.last_row), (3, 4));
    assert_eq!(deposit.container.as_deref(), Some("Account"));
    assert_eq!(
        find(&scan, "withdraw").container.as_deref(),
        Some("Account")
    );
    assert_eq!(find(&scan, "inner").container, None);
    assert_eq!(find(&scan, "top").container, None);
}

#[test]
fn go_declarations_have_name_kind_rows_and_receiver_container() {
    let src = r#"package main

type Server struct {
	port int
}

func (s *Server) Start() error {
	return nil
}

func Run() {}

const Version = "1"
var counter int
"#;
    let scan = scan(Language::Go, "go", src);

    assert_eq!(
        names_and_kinds(&scan),
        vec![
            ("Server", "type_spec"),
            ("Start", "method_declaration"),
            ("Run", "function_declaration"),
            ("Version", "const_spec"),
            ("counter", "var_spec"),
        ]
    );
    let server = find(&scan, "Server");
    assert_eq!((server.first_row, server.last_row), (2, 4));
    let start = find(&scan, "Start");
    assert_eq!((start.first_row, start.last_row), (6, 8));
    assert_eq!(start.container.as_deref(), Some("Server"));
    assert_eq!(find(&scan, "Run").container, None);
}

#[test]
fn go_specs_that_name_several_things_declare_every_name() {
    // `const Max, Min = 1, 2` is one `const_spec` with two `name` fields.
    // Each name is a declaration with the rows of the whole spec, so
    // `--symbol Min` finds it. Locals of a function body stay locals.
    let src = r#"package main

const Max, Min = 1, 2

var (
	left, right int
	solo        int
)

func Run() {
	var a, b int
	_, _ = a, b
}
"#;
    let scan = scan(Language::Go, "go", src);

    assert_eq!(
        names_and_kinds(&scan),
        vec![
            ("Max", "const_spec"),
            ("Min", "const_spec"),
            ("left", "var_spec"),
            ("right", "var_spec"),
            ("solo", "var_spec"),
            ("Run", "function_declaration"),
        ]
    );
    assert_name_ranges_spell_names(&scan, src);
    let min = find(&scan, "Min");
    assert_eq!((min.first_row, min.last_row), (2, 2));
    assert_eq!(min.name_bytes.start, offset_of(src, "Min"));
    let right = find(&scan, "right");
    assert_eq!((right.first_row, right.last_row), (5, 5));
}

#[test]
fn go_local_variables_and_constants_are_not_declarations() {
    // The same rule as TypeScript: a variable in a function body is a local.
    // A type declared in a function body stays a declaration, as in Rust.
    let src = r#"package main

var top int

func Run() {
	var local int
	const inner = 2
	type hidden struct{}
	handler := func() {
		var deeper int
		_ = deeper
	}
	_, _ = local, handler
}
"#;
    let scan = scan(Language::Go, "go", src);

    assert_eq!(
        names_and_kinds(&scan),
        vec![
            ("top", "var_spec"),
            ("Run", "function_declaration"),
            ("hidden", "type_spec"),
        ]
    );
    assert_eq!(find(&scan, "hidden").container, None);
}

#[test]
fn java_declarations_follow_the_generic_rule() {
    let src = r#"public class Greeter {
    public String greet() {
        return "hi";
    }
}
interface Named { String name(); }
"#;
    let scan = scan(Language::Java, "java", src);

    assert_eq!(
        names_and_kinds(&scan),
        vec![
            ("Greeter", "class_declaration"),
            ("greet", "method_declaration"),
            ("Named", "interface_declaration"),
            ("name", "method_declaration"),
        ]
    );
    assert_eq!(find(&scan, "greet").container.as_deref(), Some("Greeter"));
    assert_eq!(find(&scan, "name").container.as_deref(), Some("Named"));
    let greet = find(&scan, "greet");
    assert_eq!((greet.first_row, greet.last_row), (1, 3));
}

#[test]
fn ruby_declarations_follow_the_generic_rule() {
    let src = "module Shop\n  class Cart\n    def total\n      1\n    end\n  end\nend\n";
    let scan = scan(Language::Ruby, "rb", src);

    assert_eq!(
        names_and_kinds(&scan),
        vec![("Shop", "module"), ("Cart", "class"), ("total", "method")]
    );
    assert_eq!(find(&scan, "total").container.as_deref(), Some("Cart"));
    assert_eq!(find(&scan, "Cart").container.as_deref(), Some("Shop"));
}

#[test]
fn a_language_whose_declarations_have_no_name_field_gives_an_empty_scan() {
    let scan = scan(Language::Markdown, "md", "# Title\n\nbody\n");

    assert!(scan.decls.is_empty());
    assert!(!scan.truncated);
}

// ============================================================================
// TP-46: enclosing_declaration gives the innermost function or container
// ============================================================================

#[test]
fn rust_enclosing_declaration_is_the_innermost_function() {
    let src = r#"fn outer() {
    let f = |x: u32| { x + 1 + MARK_CLOSURE };
    fn inner() { MARK_INNER }
    const LOCAL: u32 = MARK_CONST;
}
struct S { field: u32 }
impl S {
    fn m(&self) -> u32 { MARK_METHOD }
}
"#;
    let scan = scan(Language::Rust, "rs", src);
    let decls = &scan.decls;

    assert_eq!(holder(decls, src, "MARK_CLOSURE").as_deref(), Some("outer"));
    assert_eq!(holder(decls, src, "MARK_INNER").as_deref(), Some("inner"));
    assert_eq!(holder(decls, src, "MARK_CONST").as_deref(), Some("outer"));
    assert_eq!(holder(decls, src, "MARK_METHOD").as_deref(), Some("m"));
    assert_eq!(holder(decls, src, "field").as_deref(), Some("S"));
}

#[test]
fn typescript_enclosing_declaration_is_the_innermost_function_or_class() {
    let src = r#"function outer() {
  const f = (x: number) => { return x + MARK_CLOSURE; };
  arr.map((y) => y + MARK_ARG);
}
class K {
  m() { return MARK_METHOD; }
  field = MARK_FIELD;
}
"#;
    let scan = scan(Language::TypeScript, "ts", src);
    let decls = &scan.decls;

    assert_eq!(holder(decls, src, "MARK_CLOSURE").as_deref(), Some("outer"));
    assert_eq!(holder(decls, src, "MARK_ARG").as_deref(), Some("outer"));
    assert_eq!(holder(decls, src, "MARK_METHOD").as_deref(), Some("m"));
    assert_eq!(holder(decls, src, "MARK_FIELD").as_deref(), Some("K"));
}

#[test]
fn enclosing_declaration_of_a_byte_outside_every_declaration_is_none() {
    let src = "// header\nfn only() {}\n// footer\n";
    let scan = scan(Language::Rust, "rs", src);

    assert_eq!(holder(&scan.decls, src, "header"), None);
    assert_eq!(holder(&scan.decls, src, "footer"), None);
    assert_eq!(
        enclosing_declaration(&scan.decls, src.len()).map(|d| &d.name),
        None
    );
}

#[test]
fn enclosing_declaration_range_includes_its_first_byte_and_excludes_its_end() {
    let src = "fn a() {}\nfn b() {}\n";
    let scan = scan(Language::Rust, "rs", src);
    let a_start = offset_of(src, "fn a");
    let b_start = offset_of(src, "fn b");

    assert_eq!(
        enclosing_declaration(&scan.decls, a_start).map(|d| d.name.as_str()),
        Some("a")
    );
    assert_eq!(
        enclosing_declaration(&scan.decls, b_start).map(|d| d.name.as_str()),
        Some("b")
    );
    // The newline after `fn a() {}` is outside both declarations.
    assert_eq!(
        enclosing_declaration(&scan.decls, b_start - 1).map(|d| d.name.as_str()),
        None
    );
}

// ============================================================================
// TP-45 and TP-120: the time-bounded parse
// ============================================================================

#[test]
fn parse_past_its_time_bound_gives_no_tree() {
    let src = many_rust_functions(5000);
    let mut parser = Parser::new(Language::Rust).unwrap();

    let result = parser.parse_bounded(&src, Duration::ZERO).unwrap();

    assert!(result.is_none(), "a zero bound must stop a large parse");
}

#[test]
fn parse_within_its_time_bound_gives_the_tree() {
    let src = many_rust_functions(200);
    let mut parser = Parser::new(Language::Rust).unwrap();

    let tree = parser
        .parse_bounded(&src, Duration::from_secs(60))
        .unwrap()
        .expect("a generous bound must not stop the parse");

    assert_eq!(
        tree.root_node().to_sexp(),
        parser.parse(&src).unwrap().root_node().to_sexp()
    );
}

#[test]
fn parser_after_a_stopped_parse_gives_the_tree_a_new_parser_gives() {
    let file_a = many_rust_functions(5000);
    let file_b = "struct Other { value: u32 }\nfn other() -> u32 { 7 }\n";
    let expected = Parser::new(Language::Rust)
        .unwrap()
        .parse(file_b)
        .unwrap()
        .root_node()
        .to_sexp();
    let mut reused = Parser::new(Language::Rust).unwrap();

    assert!(
        reused
            .parse_bounded(&file_a, Duration::ZERO)
            .unwrap()
            .is_none()
    );
    let actual = reused.parse(file_b).unwrap().root_node().to_sexp();

    assert_eq!(actual, expected);
}

// ============================================================================
// TP-121: declarations reports a walk that stops at a cap
// ============================================================================

#[test]
fn declarations_above_the_node_cap_are_truncated() {
    // About nine nodes per function: well above the 100,000-node cap.
    let src = many_rust_functions(15_000);
    let scan = scan(Language::Rust, "rs", &src);

    assert!(scan.truncated);
    assert!(
        !scan.decls.is_empty(),
        "declarations before the cap are kept"
    );
    assert!(
        scan.decls.len() < 15_000,
        "declarations past the cap are dropped"
    );
}

#[test]
fn declarations_below_the_node_cap_are_not_truncated() {
    let src = many_rust_functions(100);
    let scan = scan(Language::Rust, "rs", &src);

    assert!(!scan.truncated);
    assert_eq!(scan.decls.len(), 100);
}

#[test]
fn declarations_below_the_depth_cap_are_not_truncated() {
    let src = format!("fn f() {}{}{}\n", "{", "{".repeat(100), "}".repeat(101));
    let scan = scan(Language::Rust, "rs", &src);

    assert!(!scan.truncated);
    assert_eq!(scan.decls.len(), 1);
}

#[test]
fn declarations_above_the_depth_cap_are_truncated() {
    let src = format!("fn f() {}{}{}\n", "{", "{".repeat(600), "}".repeat(601));
    let scan = scan(Language::Rust, "rs", &src);

    assert!(scan.truncated);
    assert_eq!(
        scan.decls.len(),
        1,
        "declarations above the depth cap are kept"
    );
}
