//! `skim FILE --symbol NAME`: print the source lines of one named declaration.
//!
//! The pure parts of the flag live here: which declarations a name selects,
//! the stdout text and `-n` line map of the selection, the stderr notice, and
//! the error values with their exit codes. The pipeline that reads the file,
//! parses it and consults the cache is [`crate::process::process_symbol_file`].
//!
//! The text is raw source: no mode transform runs, so what a declaration shows
//! is exactly the lines `git show` would show for that range.

use std::collections::HashSet;
use std::path::Path;

use rskim_core::{AstWalkConfig, DeclScan, Declaration, Language};

/// Most declaration names a "no declaration" message lists.
pub(crate) const MAX_NEAREST_NAMES: usize = 10;

/// Why a `--symbol` read printed nothing. Each variant is one exit code.
///
/// The text is the whole message without a program prefix; `main` writes it
/// after `skim: `.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SymbolError {
    /// The command line cannot mean a symbol read. Exit 2.
    #[error("{0}")]
    Usage(String),
    /// The file has no declaration rule: a data format, Markdown, or no
    /// detected language. Exit 3.
    #[error("{0}")]
    Unsupported(String),
    /// The file parsed and holds no declaration with that name. Exit 1.
    #[error("{0}")]
    NotFound(String),
}

impl SymbolError {
    /// The process exit code this error maps to.
    pub(crate) fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            Self::Unsupported(_) => 3,
            Self::NotFound(_) => 1,
        }
    }
}

/// What a `--symbol` read hands back to the caller that prints it.
///
/// It is not a [`crate::process::ProcessResult`]: that type feeds the analytics
/// row, and a `--symbol` read records none (the row would count the selection
/// as a token saving).
#[derive(Debug)]
#[must_use]
pub(crate) struct SymbolRead {
    /// Text for stdout.
    pub(crate) output: String,
    /// Notice for stderr, without a trailing newline. It is unconditional:
    /// the reader sees part of the file.
    pub(crate) notice: String,
    /// Token count of the whole file, when `--show-stats` asked for it.
    pub(crate) original_tokens: Option<usize>,
    /// Token count of `output`, when `--show-stats` asked for it.
    pub(crate) transformed_tokens: Option<usize>,
}

/// The selected lines as stdout text, with the data `-n` and the notice need.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Rendered {
    /// The stdout text.
    pub(crate) text: String,
    /// One entry per line of `text`: the 1-based source line, or `0` for a
    /// header or separator line that has no source line.
    pub(crate) line_map: Vec<usize>,
    /// The stderr notice.
    pub(crate) notice: String,
}

/// Reject a language that has no declaration rule.
///
/// Markdown parses with tree-sitter but has no declaring node, and JSON, YAML
/// and TOML do not parse with it, so all four are refused instead of printing
/// "no declaration" for every name.
pub(crate) fn require_supported(language: Language, path: &Path) -> Result<(), SymbolError> {
    if language == Language::Markdown || language.is_serde_based() {
        return Err(SymbolError::Unsupported(format!(
            "--symbol does not support {} (file {path:?})",
            language.name()
        )));
    }
    Ok(())
}

/// The error for a file whose language is not known.
pub(crate) fn unknown_language(path: &Path) -> SymbolError {
    SymbolError::Unsupported(format!(
        "--symbol does not support a file with no detected language: {path:?} (try --language)"
    ))
}

/// Split `Container::name` or `Container.name` at the last separator.
fn split_qualified(query: &str) -> Option<(&str, &str)> {
    query.rsplit_once("::").or_else(|| query.rsplit_once('.'))
}

/// The declarations `query` names, in source order.
///
/// A name that equals a declaration name matches it. When none does and the
/// query is `A::b` or `A.b`, the match is every declaration named `b` whose
/// container is `A`. Only one level of qualifier exists, so `a::b::c` matches
/// nothing.
pub(crate) fn select<'a>(decls: &'a [Declaration], query: &str) -> Vec<&'a Declaration> {
    let exact: Vec<&Declaration> = decls.iter().filter(|d| d.name == query).collect();
    if !exact.is_empty() {
        return exact;
    }
    let Some((container, name)) = split_qualified(query) else {
        return exact;
    };
    decls
        .iter()
        .filter(|d| d.name == name && d.container.as_deref() == Some(container))
        .collect()
}

/// The error for a query that selected nothing.
///
/// When the walk stopped at a cap, a missing name may lie past it, so the
/// message names the cap instead of listing a partial set of names as if it
/// were the file's. Otherwise it lists at most [`MAX_NEAREST_NAMES`] names:
/// those that contain the query (ignoring case) first, then the rest in source
/// order. Every name is quoted, because a name comes from the file.
pub(crate) fn not_found(path: &Path, query: &str, scan: &DeclScan) -> SymbolError {
    if scan.truncated {
        return SymbolError::NotFound(format!(
            "no declaration named {query:?} in the first {} syntax nodes of {path:?} (node cap)",
            AstWalkConfig::DEFAULT_MAX_NODES
        ));
    }

    let mut seen: HashSet<&str> = HashSet::new();
    let unique: Vec<&str> = scan
        .decls
        .iter()
        .map(|d| d.name.as_str())
        .filter(|name| seen.insert(*name))
        .collect();

    let needle = split_qualified(query)
        .map_or(query, |(_, name)| name)
        .to_lowercase();
    let (near, rest): (Vec<&str>, Vec<&str>) = unique
        .iter()
        .copied()
        .partition(|name| name.to_lowercase().contains(&needle));

    let mut listed: Vec<String> = near
        .into_iter()
        .chain(rest)
        .take(MAX_NEAREST_NAMES)
        .map(|name| format!("{name:?}"))
        .collect();
    if listed.is_empty() {
        listed.push("(none)".to_string());
    } else if unique.len() > MAX_NEAREST_NAMES {
        listed.push("…".to_string());
    }

    SymbolError::NotFound(format!(
        "no declaration named {query:?} in {path:?}; declarations: {}",
        listed.join(", ")
    ))
}

/// Build the stdout text, line map and notice for the selected declarations.
///
/// One match prints its lines and nothing else. Two or more print one block
/// each, in source order, every block led by a header and separated from the
/// block before it by an empty line. Lines keep their own terminators, so a
/// CRLF file stays CRLF.
///
/// `walk_truncated` is [`DeclScan::truncated`]: when a cap stopped the walk, a
/// later declaration with the same name may exist, and the notice says so.
pub(crate) fn render(
    path: &Path,
    source: &str,
    matches: &[&Declaration],
    query: &str,
    walk_truncated: bool,
) -> Rendered {
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let total = lines.len();
    let count = matches.len();
    let Some(last_index) = total.checked_sub(1) else {
        return Rendered {
            text: String::new(),
            line_map: Vec::new(),
            notice: format!("[skim] --symbol {query:?}: the file has no lines"),
        };
    };

    let mut text = String::new();
    let mut line_map: Vec<usize> = Vec::new();
    let mut ranges: Vec<(usize, usize)> = Vec::with_capacity(count);

    for (k, decl) in matches.iter().enumerate() {
        let last = decl.last_row.min(last_index);
        let first = decl.first_row.min(last);
        ranges.push((first, last));

        if count > 1 {
            if k > 0 {
                if !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push('\n');
                line_map.push(0);
            }
            text.push_str(&format!(
                "==> {}:{}-{} {} ({} of {count}) <==\n",
                path.display(),
                first + 1,
                last + 1,
                decl.name,
                k + 1,
            ));
            line_map.push(0);
        }
        for (offset, line) in lines[first..=last].iter().enumerate() {
            text.push_str(line);
            line_map.push(first + 1 + offset);
        }
    }

    Rendered {
        text,
        line_map,
        notice: notice_for(query, &ranges, total, walk_truncated),
    }
}

/// The stderr notice: which lines of the file the reader saw.
fn notice_for(
    query: &str,
    ranges: &[(usize, usize)],
    total: usize,
    walk_truncated: bool,
) -> String {
    const REMEDY: &str = "omit --symbol for the whole file";
    // The reader may see fewer declarations than the file has, so the notice
    // must say so (the read is loss-bearing either way).
    let cap = if walk_truncated {
        "; the declaration walk stopped at its syntax-node or depth cap, so more declarations with this name may exist"
    } else {
        ""
    };
    if let [(first, last)] = ranges {
        let above = *first;
        let below = total - 1 - *last;
        return format!(
            "[skim] --symbol {query:?}: lines {}-{} of {total} shown ({above} above, {below} below not shown){cap} — {REMEDY}",
            first + 1,
            last + 1
        );
    }

    // Blocks come in source order, and one block can sit inside another, so
    // count each line once.
    let mut shown = 0;
    let mut next_unseen = 0;
    for &(first, last) in ranges {
        let start = first.max(next_unseen);
        if last >= start {
            shown += last - start + 1;
            next_unseen = last + 1;
        }
    }
    format!(
        "[skim] --symbol {query:?}: {} declarations, {shown} of {total} lines shown ({} not shown){cap} — {REMEDY}",
        ranges.len(),
        total - shown
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use rskim_core::{Parser, declarations};

    fn scan(source: &str, language: Language) -> DeclScan {
        let mut parser = Parser::new(language).unwrap();
        let tree = parser.parse(source).unwrap();
        declarations(&tree, source, language)
    }

    const RUST: &str = "\
struct Point {
    x: i32,
}

impl Point {
    fn new() -> Self {
        Point { x: 0 }
    }
}

impl Other {
    fn new() -> Self {
        Other
    }
}

fn norm(p: &Point) -> i32 {
    p.x
}
";

    fn names(found: &[&Declaration]) -> Vec<(String, usize)> {
        found
            .iter()
            .map(|d| (d.name.clone(), d.first_row + 1))
            .collect()
    }

    // ------------------------------------------------------------------
    // select
    // ------------------------------------------------------------------

    #[test]
    fn select_returns_every_declaration_of_that_name_in_source_order() {
        let scan = scan(RUST, Language::Rust);
        let found = select(&scan.decls, "new");
        assert_eq!(names(&found), [("new".into(), 6), ("new".into(), 12)]);
    }

    #[test]
    fn select_qualified_rust_name_keeps_only_that_impl() {
        let scan = scan(RUST, Language::Rust);
        let found = select(&scan.decls, "Other::new");
        assert_eq!(names(&found), [("new".into(), 12)]);
    }

    #[test]
    fn select_qualified_typescript_name_keeps_only_that_class() {
        let source = "class A {\n  run() {}\n}\nclass B {\n  run() {}\n}\n";
        let scan = scan(source, Language::TypeScript);
        let found = select(&scan.decls, "B.run");
        assert_eq!(names(&found), [("run".into(), 5)]);
    }

    #[test]
    fn select_qualifier_reaches_one_level_only() {
        let scan = scan(RUST, Language::Rust);
        assert!(select(&scan.decls, "crate::Other::new").is_empty());
        assert!(select(&scan.decls, "Missing::new").is_empty());
    }

    #[test]
    fn select_prefers_a_declaration_whose_whole_name_has_a_dot() {
        let decl = |name: &str| Declaration {
            name: name.to_string(),
            kind: "table",
            name_bytes: 0..1,
            first_row: 0,
            last_row: 0,
            container: None,
            byte_range: 0..1,
        };
        let decls = [decl("sales.orders"), decl("orders")];
        let found = select(&decls, "sales.orders");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "sales.orders");
    }

    // ------------------------------------------------------------------
    // render and notice
    // ------------------------------------------------------------------

    #[test]
    fn render_one_match_is_the_declaration_lines_and_nothing_else() {
        let scan = scan(RUST, Language::Rust);
        let found = select(&scan.decls, "norm");
        let rendered = render(Path::new("lib.rs"), RUST, &found, "norm", false);

        assert_eq!(rendered.text, "fn norm(p: &Point) -> i32 {\n    p.x\n}\n");
        assert_eq!(rendered.line_map, [17, 18, 19]);
        assert_eq!(
            rendered.notice,
            "[skim] --symbol \"norm\": lines 17-19 of 19 shown (16 above, 0 below not shown) — omit --symbol for the whole file"
        );
    }

    #[test]
    fn render_notice_counts_lines_above_and_below() {
        let scan = scan(RUST, Language::Rust);
        let found = select(&scan.decls, "Point::new");
        let rendered = render(Path::new("lib.rs"), RUST, &found, "Point::new", false);

        assert_eq!(
            rendered.notice,
            "[skim] --symbol \"Point::new\": lines 6-8 of 19 shown (5 above, 11 below not shown) — omit --symbol for the whole file"
        );
    }

    #[test]
    fn render_two_matches_lead_each_block_with_a_header() {
        let scan = scan(RUST, Language::Rust);
        let found = select(&scan.decls, "new");
        let rendered = render(Path::new("src/lib.rs"), RUST, &found, "new", false);

        let expected = "\
==> src/lib.rs:6-8 new (1 of 2) <==
    fn new() -> Self {
        Point { x: 0 }
    }

==> src/lib.rs:12-14 new (2 of 2) <==
    fn new() -> Self {
        Other
    }
";
        assert_eq!(rendered.text, expected);
        assert_eq!(rendered.line_map, [0, 6, 7, 8, 0, 0, 12, 13, 14]);
        assert_eq!(rendered.text.lines().count(), rendered.line_map.len());
        assert_eq!(
            rendered.notice,
            "[skim] --symbol \"new\": 2 declarations, 6 of 19 lines shown (13 not shown) — omit --symbol for the whole file"
        );
    }

    #[test]
    fn render_keeps_crlf_line_endings() {
        let source = "fn a() {\r\n    1\r\n}\r\nfn b() {}\r\n";
        let scan = scan(source, Language::Rust);
        let found = select(&scan.decls, "a");
        let rendered = render(Path::new("a.rs"), source, &found, "a", false);

        assert_eq!(rendered.text, "fn a() {\r\n    1\r\n}\r\n");
    }

    #[test]
    fn render_a_last_line_without_a_newline_stays_without_one() {
        let source = "fn a() {}\nfn b() {}";
        let scan = scan(source, Language::Rust);
        let found = select(&scan.decls, "b");
        let rendered = render(Path::new("a.rs"), source, &found, "b", false);

        assert_eq!(rendered.text, "fn b() {}");
        assert_eq!(rendered.line_map, [2]);
    }

    #[test]
    fn render_counts_a_line_once_when_one_match_sits_inside_another() {
        // Both functions are named `f`; the inner one lies in the outer body.
        let source = "fn f() {\n    fn f() {}\n}\nfn g() {}\n";
        let scan = scan(source, Language::Rust);
        let found = select(&scan.decls, "f");
        assert_eq!(found.len(), 2, "precondition: both `f` are declarations");
        let rendered = render(Path::new("a.rs"), source, &found, "f", false);

        assert_eq!(
            rendered.notice,
            "[skim] --symbol \"f\": 2 declarations, 3 of 4 lines shown (1 not shown) — omit --symbol for the whole file"
        );
    }

    #[test]
    fn render_notice_on_a_truncated_walk_says_more_matches_may_exist() {
        let scan = scan(RUST, Language::Rust);
        let one = select(&scan.decls, "norm");
        let two = select(&scan.decls, "new");

        let one = render(Path::new("lib.rs"), RUST, &one, "norm", true);
        let two = render(Path::new("lib.rs"), RUST, &two, "new", true);

        assert_eq!(
            one.notice,
            "[skim] --symbol \"norm\": lines 17-19 of 19 shown (16 above, 0 below not shown); \
             the declaration walk stopped at its syntax-node or depth cap, so more declarations \
             with this name may exist — omit --symbol for the whole file"
        );
        assert!(
            two.notice
                .contains("(13 not shown); the declaration walk stopped at its syntax-node"),
            "{}",
            two.notice
        );
    }

    // ------------------------------------------------------------------
    // not_found and require_supported
    // ------------------------------------------------------------------

    fn message(error: SymbolError) -> String {
        error.to_string()
    }

    #[test]
    fn not_found_lists_declaration_names_quoted() {
        let scan = scan(RUST, Language::Rust);
        let error = not_found(Path::new("lib.rs"), "missing", &scan);

        assert_eq!(error.exit_code(), 1);
        assert_eq!(
            message(error),
            "no declaration named \"missing\" in \"lib.rs\"; \
             declarations: \"Point\", \"x\", \"new\", \"norm\""
        );
    }

    #[test]
    fn not_found_lists_at_most_ten_names_with_close_names_first() {
        let source: String = (0..14).map(|i| format!("fn f{i:02}() {{}}\n")).collect();
        let source = format!("{source}fn the_RUN_loop() {{}}\n");
        let scan = scan(&source, Language::Rust);
        let error = not_found(Path::new("a.rs"), "run", &scan);

        let text = message(error);
        let listed = text.split("declarations: ").nth(1).unwrap();
        assert!(
            listed.starts_with("\"the_RUN_loop\", \"f00\", \"f01\""),
            "names that hold the query come first, then source order: {listed}"
        );
        assert!(listed.ends_with(", …"), "a cut list ends with an ellipsis");
        assert_eq!(listed.matches(", ").count(), MAX_NEAREST_NAMES);
    }

    #[test]
    fn not_found_quotes_a_name_with_a_control_byte() {
        let scan = DeclScan {
            decls: vec![Declaration {
                name: "bad\u{1b}[2Jname".to_string(),
                kind: "function",
                name_bytes: 0..1,
                first_row: 0,
                last_row: 0,
                container: None,
                byte_range: 0..1,
            }],
            truncated: false,
        };
        let text = message(not_found(Path::new("a.rs"), "x\ny", &scan));

        assert!(
            !text.contains('\u{1b}'),
            "escape byte must not reach stderr"
        );
        assert!(text.contains("\"x\\ny\""), "the query is quoted: {text}");
    }

    #[test]
    fn not_found_on_a_truncated_scan_names_the_node_cap() {
        let scan = DeclScan {
            decls: Vec::new(),
            truncated: true,
        };
        let error = not_found(Path::new("big.rs"), "late", &scan);

        assert_eq!(error.exit_code(), 1);
        assert_eq!(
            message(error),
            "no declaration named \"late\" in the first 100000 syntax nodes of \"big.rs\" (node cap)"
        );
    }

    #[test]
    fn require_supported_refuses_data_formats_and_markdown_with_exit_3() {
        for (language, name) in [
            (Language::Markdown, "Markdown"),
            (Language::Json, "JSON"),
            (Language::Yaml, "YAML"),
            (Language::Toml, "TOML"),
        ] {
            let error = require_supported(language, Path::new("f")).unwrap_err();
            assert_eq!(error.exit_code(), 3);
            assert!(
                message(error).starts_with(&format!("--symbol does not support {name}")),
                "{name}"
            );
        }
        assert!(require_supported(Language::Rust, Path::new("f")).is_ok());
    }

    #[test]
    fn exit_codes_follow_the_case_table() {
        assert_eq!(SymbolError::Usage(String::new()).exit_code(), 2);
        assert_eq!(unknown_language(Path::new("f")).exit_code(), 3);
        assert_eq!(SymbolError::NotFound(String::new()).exit_code(), 1);
    }
}
