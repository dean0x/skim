//! Bounded symbol reads from the HEAD tree.

use rskim_core::{
    Declaration, Language, Parser, ast_size_limit, declarations, detect_language_from_path,
    enclosing_declaration,
};

use super::card::{CARD_TEXT_MAX_CHARS, sanitize};
use super::git_source::{HeadEntry, HeadTree, disk_path};

const MAX_FILES: usize = 500;
const MAX_TOTAL_BYTES: u64 = 32 * (1 << 20);
const MAX_FILE_BYTES: u64 = 1 << 20;
const MAX_SYMBOLS: usize = 12;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SymbolReason {
    Language,
    SizeCap,
    ParseBudget,
    ParseError,
    ReadError,
    TruncatedScan,
    NoDeclarations,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Symbol {
    pub(super) kind: &'static str,
    pub(super) name: String,
    pub(super) line: usize,
}

#[derive(Debug, Default)]
pub(super) struct FileSymbols {
    pub(super) rows: Vec<Symbol>,
    pub(super) more: usize,
    pub(super) reason: Option<SymbolReason>,
}

impl SymbolReason {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Language => "language",
            Self::SizeCap => "size cap",
            Self::ParseBudget => "parse budget",
            Self::ParseError => "parse error",
            Self::ReadError => "read error",
            Self::TruncatedScan => "truncated scan",
            Self::NoDeclarations => "no declarations",
        }
    }
}

pub(super) struct Reader<'a, F> {
    head: Option<&'a HeadTree>,
    read_blob: F,
    files_left: usize,
    bytes_left: u64,
}

impl<'a, F: Fn(&str, u64) -> Option<String>> Reader<'a, F> {
    pub(super) fn new(head: Option<&'a HeadTree>, read_blob: F) -> Self {
        Self {
            head,
            read_blob,
            files_left: MAX_FILES,
            bytes_left: MAX_TOTAL_BYTES,
        }
    }

    pub(super) fn read(&mut self, path: &str) -> FileSymbols {
        self.inspect(path).unwrap_or_else(|reason| FileSymbols {
            reason: Some(reason),
            ..FileSymbols::default()
        })
    }

    fn inspect(&mut self, path: &str) -> Result<FileSymbols, SymbolReason> {
        let Some(HeadEntry::Regular { oid, size, .. }) = self.head.and_then(|tree| tree.get(path))
        else {
            return Err(SymbolReason::ReadError);
        };
        let path = disk_path(path).ok_or(SymbolReason::ReadError)?;
        let lang = detect_language_from_path(&path).ok_or(SymbolReason::Language)?;
        let ast_limit = ast_size_limit(lang).ok_or(SymbolReason::Language)?;
        if *size > MAX_FILE_BYTES.min(ast_limit) {
            return Err(SymbolReason::SizeCap);
        }
        if self.files_left == 0 || *size > self.bytes_left {
            return Err(SymbolReason::ParseBudget);
        }
        // Charge each attempt, including a failed read. This also bounds git spawns.
        self.files_left -= 1;
        self.bytes_left -= *size;
        let source = (self.read_blob)(oid, *size).ok_or(SymbolReason::ReadError)?;
        let mut parser = Parser::new_for_path(lang, &path).map_err(|_| SymbolReason::ParseError)?;
        let tree = parser
            .parse(&source)
            .map_err(|_| SymbolReason::ParseError)?;
        let scan = declarations(&tree, &source, lang);
        let mut found = FileSymbols::default();
        for d in scan.decls {
            // Ask the declaration owner if this kind can enclose a byte.
            if enclosing_declaration(std::slice::from_ref(&d), d.byte_range.start) != Some(&d) {
                continue;
            }
            // A bracketed JS or TS method key is an expression, not a declared name.
            if matches!(lang, Language::JavaScript | Language::TypeScript)
                && d.kind == "method_definition"
                && d.name.starts_with('[')
            {
                continue;
            }
            if found.rows.len() == MAX_SYMBOLS {
                found.more = found.more.saturating_add(1);
            } else {
                found.rows.push(symbol_row(&d));
            }
        }
        if scan.truncated {
            found.reason = Some(SymbolReason::TruncatedScan);
        } else if found.rows.is_empty() {
            found.reason = Some(SymbolReason::NoDeclarations);
        }
        Ok(found)
    }
}

fn symbol_row(d: &Declaration) -> Symbol {
    let name = sanitize(&d.name, CARD_TEXT_MAX_CHARS);
    let name = match &d.container {
        Some(container) => format!("{}::{name}", sanitize(container, CARD_TEXT_MAX_CHARS)),
        None => name,
    };
    Symbol {
        kind: d.kind,
        name,
        line: d.first_row.saturating_add(1),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::cmd::heatmap::git_source::HeadEntry;

    const OID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const MIB: u64 = 1 << 20;

    fn regular(size: u64) -> HeadEntry {
        HeadEntry::Regular {
            oid: OID.into(),
            size,
            lines: Some(1),
        }
    }

    fn read(path: &str, source: &str) -> FileSymbols {
        let tree = HeadTree::from([(path.into(), regular(source.len() as u64))]);
        Reader::new(Some(&tree), |_, _| Some(source.into())).read(path)
    }

    fn row(kind: &'static str, name: &str, line: usize) -> Symbol {
        Symbol {
            kind,
            name: name.into(),
            line,
        }
    }

    #[test]
    fn rust_and_go_keep_only_function_and_container_names() {
        let rust = "struct Point { x: i32 }\nenum Axis { Up }\nconst STEP: i32 = 6;\nimpl Point { fn new(x: i32) -> Self { Self { x } } }\n";
        assert_eq!(
            read("point.rs", rust).rows,
            [
                row("struct_item", "Point", 1),
                row("enum_item", "Axis", 2),
                row("function_item", "Point::new", 4),
            ]
        );
        let go = "package point\ntype Point struct { X int }\nconst Step = 6\nfunc (p Point) Move(x int) int { return x }\n";
        assert_eq!(
            read("point.go", go).rows,
            [
                row("type_spec", "Point", 2),
                row("method_declaration", "Point::Move", 4),
            ]
        );
    }

    #[test]
    fn typescript_keeps_top_level_const_but_not_local_const() {
        let ts = "const visible = 6;\nfunction greet(name = 'default') { const local = 1; }\n";
        assert_eq!(
            read("greet.ts", ts).rows,
            [
                row("variable_declarator", "visible", 1),
                row("function_declaration", "greet", 2),
            ]
        );
    }

    #[test]
    fn js_and_ts_keep_plain_methods_but_not_computed_keys() {
        let source = "class Item {\n  plain() {}\n  [method]() {}\n  get [k]() { return 1; }\n}\n";
        let paths = ["item.js", "item.ts", "item.tsx", "item.jsx"];
        let actual = paths.map(|path| (path, read(path, source).rows));
        let expected = paths.map(|path| {
            (
                path,
                vec![
                    row("class_declaration", "Item", 1),
                    row("method_definition", "Item::plain", 2),
                ],
            )
        });
        assert_eq!(actual, expected);
    }

    #[test]
    fn quoted_non_ascii_tsx_path_keeps_declarations_after_jsx() {
        let tsx = "const before = <div />;\nexport function after() { return null; }\n";
        assert_eq!(
            read(r#""na\303\257ve.tsx""#, tsx).rows,
            [
                row("variable_declarator", "before", 1),
                row("function_declaration", "after", 2),
            ]
        );
    }

    #[test]
    fn a_control_byte_in_the_container_name_shows_as_a_replacement_char() {
        let declaration = Declaration {
            kind: "method_definition",
            name: "method".into(),
            container: Some("Con\u{1b}tainer".into()),
            name_bytes: 0..6,
            first_row: 0,
            last_row: 0,
            byte_range: 0..6,
        };
        assert_eq!(
            symbol_row(&declaration),
            row("method_definition", "Con�tainer::method", 1)
        );
    }

    #[test]
    fn symbol_cap_counts_all_extra_non_leaf_declarations() {
        let source: String = (0..15)
            .map(|i| format!("fn f{i}() {{}}\nconst C{i}: i32 = 0;\n"))
            .collect();
        let found = read("f.rs", &source);
        assert_eq!(
            found.rows,
            (0..12)
                .map(|i| row("function_item", &format!("f{i}"), 2 * i + 1))
                .collect::<Vec<_>>()
        );
        assert_eq!(found.more, 3);
        assert_eq!(found.reason, None);
    }

    #[test]
    fn unknown_and_data_languages_do_not_read() {
        for path in ["file.zzz", "file.json", "file.yaml", "file.toml"] {
            let tree = HeadTree::from([(path.into(), regular(1))]);
            let found = Reader::new(Some(&tree), |_, _| panic!("read {path}")).read(path);
            assert!(found.rows.is_empty());
            assert_eq!(found.reason, Some(SymbolReason::Language));
        }
    }

    #[test]
    fn non_regular_and_oversize_entries_do_not_read() {
        let tree = HeadTree::from([
            ("link.rs".into(), HeadEntry::Other),
            ("gitlink.rs".into(), HeadEntry::Other),
            ("oversize.rs".into(), regular(MIB + 1)),
        ]);
        let mut reader = Reader::new(Some(&tree), |_, _| panic!("unexpected blob read"));
        for (path, reason) in [
            ("link.rs", SymbolReason::ReadError),
            ("gitlink.rs", SymbolReason::ReadError),
            ("oversize.rs", SymbolReason::SizeCap),
        ] {
            let found = reader.read(path);
            assert!(found.rows.is_empty());
            assert_eq!(found.reason, Some(reason));
        }
    }

    #[test]
    fn file_budget_stops_even_if_all_reads_fail() {
        let tree: HeadTree = (0..502).map(|i| (format!("{i}.rs"), regular(1))).collect();
        let calls = Cell::new(0);
        let mut reader = Reader::new(Some(&tree), |_, _| {
            calls.set(calls.get() + 1);
            None
        });
        for i in 0..502 {
            let expected = if i < 500 {
                SymbolReason::ReadError
            } else {
                SymbolReason::ParseBudget
            };
            assert_eq!(
                reader.read(&format!("{i}.rs")).reason,
                Some(expected),
                "file {i}"
            );
        }
        assert_eq!(calls.get(), 500);
    }

    #[test]
    fn byte_budget_accepts_32_one_mib_files_then_stops() {
        let tree: HeadTree = (0..34).map(|i| (format!("{i}.rs"), regular(MIB))).collect();
        let calls = Cell::new(0);
        let mut reader = Reader::new(Some(&tree), |_, _| {
            calls.set(calls.get() + 1);
            None
        });
        for i in 0..34 {
            let expected = if i < 32 {
                SymbolReason::ReadError
            } else {
                SymbolReason::ParseBudget
            };
            assert_eq!(
                reader.read(&format!("{i}.rs")).reason,
                Some(expected),
                "file {i}"
            );
        }
        assert_eq!(calls.get(), 32);
    }

    #[test]
    fn markdown_grammar_limit_is_a_parse_error() {
        let found = read("deep.md", &format!("{}text\n", "> ".repeat(300)));
        assert!(found.rows.is_empty());
        assert_eq!(found.reason, Some(SymbolReason::ParseError));
    }

    #[test]
    fn truncated_walk_keeps_partial_rows_and_reports_the_scan_cut() {
        let source = format!(
            "fn first() {{}}\nfn deep() {{ let _x = {}0{}; }}\n",
            "(".repeat(600),
            ")".repeat(600)
        );
        let found = read("deep.rs", &source);
        assert_eq!(
            found.rows,
            [
                row("function_item", "first", 1),
                row("function_item", "deep", 2)
            ]
        );
        assert_eq!(found.reason, Some(SymbolReason::TruncatedScan));
    }

    #[test]
    fn complete_scan_with_no_declarations_has_its_own_reason() {
        let found = read("empty.rs", "// Empty\n");
        assert!(found.rows.is_empty());
        assert_eq!(found.reason, Some(SymbolReason::NoDeclarations));
    }

    #[test]
    fn symbol_production_code_does_not_read_a_clock() {
        let source = include_str!("symbols.rs")
            .split_once("\n#[cfg(test)]\nmod tests {")
            .unwrap()
            .0;
        for clock in ["Instant", "SystemTime", "Duration", "parse_bounded"] {
            assert!(!source.contains(clock), "{clock}");
        }
    }
}
