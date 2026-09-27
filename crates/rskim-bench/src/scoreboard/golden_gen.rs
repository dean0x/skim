//! `golden-gen`: candidate `[[ident]]` entries (#203) and, with `--ast`,
//! candidate `[[ast]]` entries (#541) for a corpus's golden file.
//!
//! The output is a proposal. It is reviewed, pasted into
//! `golden/<corpus>.toml` and frozen there; the committed golden file is the
//! source of truth and CI never regenerates it.
//!
//! Selection (deterministic, no RNG):
//! 1. [`definition_sites`]: `extract_symbols` over every oracle-universe file
//!    whose extension has a symbol extractor ([`extractor_language`]),
//!    keeping definition fields only ([`is_definition`]).
//! 2. [`select`]: names with exactly one definition site in the universe,
//!    at least [`MIN_NAME_BYTES`] long, whose `and` ground truth covers
//!    [`GT_FILES_MIN`]`..=`[`GT_FILES_MAX`] files, ordered by
//!    `sha256("<corpus>:<name>")`, first [`GENERATED_PER_CORPUS`] taken.
//!
//! `def.line` is 1 + the number of `\n` bytes before the name
//! ([`line_of`]), so golden integrity's def-line check holds by
//! construction.
//!
//! `rskim_core::Language` appears here only as `extract_symbols`' dispatch
//! key, chosen from this module's own extension table; nothing here calls
//! `Language::from_extension`, and nothing here scores skim.
//!
//! `[[ast]]` selection ([`generate_ast`]): every `(pattern, language)` the
//! structural oracle covers, where the corpus has files of that language in
//! its AST universe and the entry is non-vacuous (the oracle matches a file,
//! or skim's `--ast <pattern>` returns a row in that language). The proposed
//! precision class is `hard` when the catalog marks the pattern `exact`,
//! `ratchet` otherwise ([`proposed_class`]); once frozen in the golden file,
//! the class is never re-read from the catalog. An entry the oracle matches
//! no file for exists only because skim returns one (a false positive), so it
//! is proposed with `expect_oracle_empty = true`: a false-positive guard that
//! stays scored after a fix empties skim's rows.
//!
//! `golden-gen --ast` calls skim for every catalog pattern
//! ([`crate::scoreboard::structural_metrics::called_patterns`]), the call set
//! the gate uses, and reports the rows no proposed entry would score
//! ([`unscored_after`]) as a comment above the proposal.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Context;
use rskim_oracle::structural::{OracleLang, PatternCoverage};
use rskim_search::SearchField;

use crate::extract::{TYPESCRIPT_EXTRACT_EXTENSIONS, extract_symbols};
use crate::scoreboard::catalog::{CatalogPattern, catalog_coverage};
use crate::scoreboard::golden::{DefSite, PrecisionClass, hex_sha256};
use crate::scoreboard::metrics::sample;
use crate::scoreboard::oracle::{LexicalQuery, MatchMode, ground_truth};
use crate::scoreboard::structural_metrics::{OracleAnswers, distinct_files, rows_in, unscored_in};
use crate::scoreboard::types::{ResultPage, ResultRow};
use crate::scoreboard::universe::Universe;

/// Generated identifier entries per corpus.
pub const GENERATED_PER_CORPUS: usize = 20;

/// Shortest name kept (bytes): shorter names are substrings of too much.
pub const MIN_NAME_BYTES: usize = 6;

/// Fewest `and`-ground-truth files for a kept name (the definition file plus
/// at least one other, so ranking has something to rank against).
pub const GT_FILES_MIN: usize = 2;

/// Most `and`-ground-truth files for a kept name.
pub const GT_FILES_MAX: usize = 60;

/// One definition found by an extractor.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DefinitionSite {
    pub name: String,
    /// Repo-relative path (as in the oracle universe).
    pub path: String,
    /// 1-based line of the name.
    pub line: u32,
}

/// One proposed `[[ident]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub def: DefSite,
    /// Files in the `and` ground truth for `name` (for the reviewer).
    pub gt_files: usize,
    /// Lowercase hex `sha256("<corpus>:<name>")`, the selection order.
    pub order_key: String,
}

/// The extractor for `path`, by extension only (case-sensitive): `.rs`,
/// `.py` / `.pyi`, `.go`, and the plain-TypeScript extensions
/// ([`TYPESCRIPT_EXTRACT_EXTENSIONS`]; `.tsx` needs the TSX grammar).
pub fn extractor_language(path: &str) -> Option<rskim_core::Language> {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let (stem, ext) = file_name.rsplit_once('.')?;
    if stem.is_empty() {
        return None;
    }
    match ext {
        "rs" => Some(rskim_core::Language::Rust),
        "py" | "pyi" => Some(rskim_core::Language::Python),
        "go" => Some(rskim_core::Language::Go),
        ext if TYPESCRIPT_EXTRACT_EXTENSIONS.contains(&ext) => {
            Some(rskim_core::Language::TypeScript)
        }
        _ => None,
    }
}

/// Whether an extracted `field` is a definition for `language`: functions
/// and types everywhere, plus TypeScript's `SymbolName` (a
/// `class_declaration`). Rust's `SymbolName` is an `impl` block, not a
/// definition; `ImportExport` (which re-emits a TS export's declared name) is
/// never one.
pub fn is_definition(language: rskim_core::Language, field: SearchField) -> bool {
    match field {
        SearchField::FunctionSignature | SearchField::TypeDefinition => true,
        SearchField::SymbolName => language == rskim_core::Language::TypeScript,
        _ => false,
    }
}

/// 1 + the number of `\n` bytes in `text` before byte `offset`; `None` when
/// `offset` is past the end of `text`.
pub fn line_of(text: &str, offset: usize) -> Option<u32> {
    let before = text.as_bytes().get(..offset)?;
    let newlines = before.iter().filter(|&&b| b == b'\n').count();
    u32::try_from(newlines).ok()?.checked_add(1)
}

/// Every definition site in `files` (`(path, text)` pairs), sorted.
///
/// # Errors
///
/// A symbol whose byte range lies outside its file, or a line number that
/// does not fit `u32`.
pub fn definition_sites<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> anyhow::Result<Vec<DefinitionSite>> {
    let mut sites = Vec::new();
    for (path, text) in files {
        let Some(language) = extractor_language(path) else {
            continue;
        };
        for symbol in extract_symbols(Path::new(path), text, language) {
            if !is_definition(language, symbol.field) {
                continue;
            }
            let line = line_of(text, symbol.byte_range.start).with_context(|| {
                format!(
                    "{path}: symbol {:?} at byte {} is outside the file",
                    symbol.name, symbol.byte_range.start
                )
            })?;
            sites.push(DefinitionSite {
                name: symbol.name,
                path: path.to_string(),
                line,
            });
        }
    }
    sites.sort();
    Ok(sites)
}

/// `sha256("<corpus>:<name>")` as lowercase hex.
pub fn order_key(corpus: &str, name: &str) -> String {
    hex_sha256(format!("{corpus}:{name}").as_bytes())
}

/// Pick up to `take` candidates from `sites` (see the module docs).
/// `gt_files(name)` returns the size of `name`'s `and` ground truth; it is
/// called lazily, in selection order, only for names that pass the cheaper
/// filters, and at most until `take` candidates are accepted.
///
/// # Errors
///
/// Any `gt_files` error.
pub fn select(
    corpus: &str,
    sites: &[DefinitionSite],
    take: usize,
    mut gt_files: impl FnMut(&str) -> anyhow::Result<usize>,
) -> anyhow::Result<Vec<Candidate>> {
    // name -> every site defining it.
    let mut by_name: BTreeMap<&str, Vec<&DefinitionSite>> = BTreeMap::new();
    for site in sites {
        by_name.entry(site.name.as_str()).or_default().push(site);
    }
    let mut ordered: Vec<(String, &DefinitionSite)> = by_name
        .into_iter()
        .filter(|(name, defs)| defs.len() == 1 && name.len() >= MIN_NAME_BYTES)
        .filter_map(|(name, defs)| defs.first().map(|d| (order_key(corpus, name), *d)))
        .collect();
    ordered.sort();

    let mut out = Vec::with_capacity(take.min(ordered.len()));
    // Bounded by the number of distinct names.
    for (key, def) in ordered {
        if out.len() >= take {
            break;
        }
        let files =
            gt_files(&def.name).with_context(|| format!("ground truth for {:?}", def.name))?;
        if (GT_FILES_MIN..=GT_FILES_MAX).contains(&files) {
            out.push(Candidate {
                name: def.name.clone(),
                def: DefSite {
                    path: def.path.clone(),
                    line: def.line,
                },
                gt_files: files,
                order_key: key,
            });
        }
    }
    Ok(out)
}

/// [`definition_sites`] + [`select`] over a corpus universe, with the oracle's
/// `and` ground truth as `gt_files`.
///
/// # Errors
///
/// As [`definition_sites`] and [`select`], plus a name the oracle refuses.
pub fn generate(corpus: &str, universe: &Universe, take: usize) -> anyhow::Result<Vec<Candidate>> {
    let sites = definition_sites(universe.files())?;
    select(corpus, &sites, take, |name| {
        let query = LexicalQuery::new(name, MatchMode::And, None)
            .with_context(|| format!("oracle query for {name:?}"))?;
        Ok(ground_truth(universe.files(), &query).len())
    })
}

/// TOML `[[ident]]` entries for `candidates`, ids `<corpus>-I01`, `-I02`, …
/// in the given order, each preceded by a review comment.
pub fn render_toml(corpus: &str, candidates: &[Candidate]) -> String {
    let mut out = String::new();
    for (i, c) in candidates.iter().enumerate() {
        out.push_str(&format!(
            "\n# golden-gen: and-GT files: {}; order key {}\n\
             [[ident]]\nid = {}\nquery = {}\ndef = {{ path = {}, line = {} }}\norigin = \"generated\"\n",
            c.gt_files,
            c.order_key.get(..12).unwrap_or(&c.order_key),
            toml_string(&format!("{corpus}-I{:02}", i + 1)),
            toml_string(&c.name),
            toml_string(&c.def.path),
            c.def.line,
        ));
    }
    out
}

// ============================================================================
// [[ast]] candidates
// ============================================================================

/// One proposed `[[ast]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AstCandidate {
    pub id: String,
    pub pattern: String,
    pub lang: OracleLang,
    pub precision: PrecisionClass,
    /// Files the oracle matches in `lang` (for the reviewer).
    pub oracle_files: usize,
    /// Distinct files skim's `--ast <pattern>` returns in `lang`.
    pub skim_files: usize,
    /// Propose `expect_oracle_empty = true`: the oracle matches no file, so
    /// the entry guards skim's false positive (precision) only.
    pub expect_oracle_empty: bool,
}

/// An `[[ast]]` id: `<corpus>-ast-<pattern>-<lang>`. It names the pair, not
/// a position, so regenerating never renumbers an id the ledger or the
/// baseline holds.
pub fn ast_id(corpus: &str, pattern: &str, lang: OracleLang) -> String {
    format!("{corpus}-ast-{pattern}-{lang}")
}

/// The class `golden-gen` proposes: `hard` when `catalog` marks `pattern`
/// `exact` (its n-grams are a reliable subset of every occurrence), else
/// `ratchet`. The gate never calls this: the class is frozen in golden.
pub fn proposed_class(catalog: &[CatalogPattern], pattern: &str) -> PrecisionClass {
    let exact = catalog.iter().any(|p| p.name == pattern && p.exact);
    if exact {
        PrecisionClass::Hard
    } else {
        PrecisionClass::Ratchet
    }
}

/// Every covered `(pattern, language)` of `catalog` whose language has files
/// in the corpus's AST universe, ordered by `(pattern, language name)`.
fn present_pairs(
    catalog: &[CatalogPattern],
    answers: &OracleAnswers,
) -> Vec<(&'static str, OracleLang)> {
    let mut pairs: Vec<(&'static str, OracleLang)> = catalog_coverage(catalog)
        .into_iter()
        .filter_map(|(pattern, coverage)| match coverage {
            PatternCoverage::Covered { langs } => Some((pattern, langs)),
            PatternCoverage::Uncovered { .. } => None,
        })
        .flat_map(|(pattern, langs)| langs.into_iter().map(move |lang| (pattern, lang)))
        .filter(|&(_, lang)| answers.scored_files(lang) > 0)
        .collect();
    pairs.sort_by(|a, b| (a.0, a.1.as_str()).cmp(&(b.0, b.1.as_str())));
    pairs
}

/// Propose `[[ast]]` entries (see the module docs) for the patterns of
/// `catalog`. `skim` holds skim's `--ast <pattern>` full list per pattern
/// called; a pattern not in it counts as no rows.
///
/// # Errors
///
/// A covered pair the oracle has no answer for (an oracle bug).
pub fn generate_ast(
    corpus: &str,
    catalog: &[CatalogPattern],
    answers: &OracleAnswers,
    skim: &BTreeMap<String, ResultPage>,
) -> anyhow::Result<Vec<AstCandidate>> {
    let mut out = Vec::new();
    for (pattern, lang) in present_pairs(catalog, answers) {
        let oracle_files = answers.definition(pattern, lang)?.len();
        let skim_files = skim
            .get(pattern)
            .map_or(0, |page| distinct_files(&rows_in(page, lang).rows).len());
        if oracle_files == 0 && skim_files == 0 {
            continue;
        }
        out.push(AstCandidate {
            id: ast_id(corpus, pattern, lang),
            pattern: pattern.to_string(),
            lang,
            precision: proposed_class(catalog, pattern),
            oracle_files,
            skim_files,
            expect_oracle_empty: oracle_files == 0,
        });
    }
    Ok(out)
}

/// skim rows no candidate scores ([`unscored_in`]), per called pattern with
/// at least one, in pattern order: what the gate counts in
/// `structural.unscored_rows.<pattern>` once the proposal is frozen.
pub fn unscored_after<'s>(
    candidates: &[AstCandidate],
    skim: &'s BTreeMap<String, ResultPage>,
) -> Vec<(&'s str, Vec<&'s ResultRow>)> {
    let scored: BTreeSet<(&str, OracleLang)> = candidates
        .iter()
        .map(|c| (c.pattern.as_str(), c.lang))
        .collect();
    skim.iter()
        .map(|(pattern, page)| (pattern.as_str(), unscored_in(pattern, page, &scored)))
        .filter(|(_, rows)| !rows.is_empty())
        .collect()
}

/// TOML comment lines reporting [`unscored_after`] to the reviewer: each
/// pattern's count and a sample of `path:line` rows, or `none`.
pub fn render_unscored_comment(unscored: &[(&str, Vec<&ResultRow>)]) -> String {
    if unscored.is_empty() {
        return "# golden-gen: skim rows no proposed entry scores: none\n".to_string();
    }
    let mut out = String::from(
        "# golden-gen: skim rows no proposed entry scores (the gate counts them in \
         structural.unscored_rows.<pattern>):\n",
    );
    for (pattern, rows) in unscored {
        let located: Vec<String> = rows
            .iter()
            .map(|r| match r.line {
                Some(line) => format!("{}:{line}", r.path),
                None => r.path.clone(),
            })
            .collect();
        out.push_str(&format!(
            "#   {pattern} {}: {}\n",
            rows.len(),
            sample(located.iter().map(String::as_str))
        ));
    }
    out
}

/// TOML `[[ast]]` entries for `candidates`, each preceded by a review
/// comment with the oracle's and skim's file counts; a false-positive guard
/// also gets `expect_oracle_empty = true`.
pub fn render_ast_toml(candidates: &[AstCandidate]) -> String {
    let mut out = String::new();
    for c in candidates {
        out.push_str(&format!(
            "\n# golden-gen: oracle files {}; skim files {}\n\
             [[ast]]\nid = {}\npattern = {}\nlang = {}\nprecision = {}\n",
            c.oracle_files,
            c.skim_files,
            toml_string(&c.id),
            toml_string(&c.pattern),
            toml_string(c.lang.as_str()),
            toml_string(c.precision.as_str()),
        ));
        if c.expect_oracle_empty {
            out.push_str("expect_oracle_empty = true\n");
        }
    }
    out
}

/// A TOML basic string (quoted and escaped by the `toml` crate).
fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
mod tests {
    use super::*;
    use crate::scoreboard::golden::{IntegrityContext, Origin, check_integrity, parse_golden};
    use crate::scoreboard::test_support::{FixtureRepo, catalog};
    use crate::scoreboard::universe::GitIsolation;

    fn site(name: &str, path: &str, line: u32) -> DefinitionSite {
        DefinitionSite {
            name: name.to_string(),
            path: path.to_string(),
            line,
        }
    }

    /// `gt_files` that answers `n` for every name.
    fn every_name_has(n: usize) -> impl FnMut(&str) -> anyhow::Result<usize> {
        move |_| Ok(n)
    }

    // --- extractor routing and definition fields ---------------------------------

    #[test]
    fn extractor_language_is_extension_only_and_case_sensitive() {
        use rskim_core::Language;
        assert_eq!(extractor_language("src/lib.rs"), Some(Language::Rust));
        assert_eq!(extractor_language("a/b.py"), Some(Language::Python));
        assert_eq!(extractor_language("a/b.pyi"), Some(Language::Python));
        assert_eq!(extractor_language("cmd/main.go"), Some(Language::Go));
        for ts in ["a.ts", "a.mts", "a.cts"] {
            assert_eq!(extractor_language(ts), Some(Language::TypeScript), "{ts}");
        }
        for none in [
            "a.tsx",
            "a.js",
            "a.RS",
            "README.md",
            "Makefile",
            "a.rs.bak",
            ".rs",
        ] {
            assert_eq!(extractor_language(none), None, "{none}");
        }
    }

    #[test]
    fn only_definition_fields_count() {
        use rskim_core::Language;
        for lang in [
            Language::Rust,
            Language::Python,
            Language::Go,
            Language::TypeScript,
        ] {
            assert!(
                is_definition(lang, SearchField::FunctionSignature),
                "{lang:?}"
            );
            assert!(is_definition(lang, SearchField::TypeDefinition), "{lang:?}");
            assert!(!is_definition(lang, SearchField::ImportExport), "{lang:?}");
        }
        assert!(is_definition(Language::TypeScript, SearchField::SymbolName));
        assert!(!is_definition(Language::Rust, SearchField::SymbolName));
    }

    #[test]
    fn line_of_counts_newlines_before_the_offset() {
        let text = "a\nbc\n\ndef";
        assert_eq!(line_of(text, 0), Some(1));
        assert_eq!(line_of(text, 1), Some(1), "the newline itself is on line 1");
        assert_eq!(line_of(text, 2), Some(2));
        assert_eq!(line_of(text, 6), Some(4));
        assert_eq!(line_of(text, text.len()), Some(4));
        assert_eq!(line_of(text, text.len() + 1), None);
    }

    #[test]
    fn definition_sites_keep_definitions_and_drop_imports_and_impls() {
        let rust = "use std::collections::HashMap;\n\
                    pub struct Widget;\n\
                    impl Widget {\n    pub fn render_widget(&self) {}\n}\n";
        let ts = "import { Imported } from './x';\n\
                  export function exportedFn(): void {}\n\
                  export class Gadget {}\n";
        let sites = definition_sites([
            ("src/widget.rs", rust),
            ("web/gadget.ts", ts),
            ("web/view.tsx", "export function tsxOnly() {}\n"),
            ("README.md", "fn not_code() {}\n"),
        ])
        .unwrap();
        assert_eq!(
            sites,
            vec![
                site("Gadget", "web/gadget.ts", 3),
                site("Widget", "src/widget.rs", 2),
                site("exportedFn", "web/gadget.ts", 2),
                site("render_widget", "src/widget.rs", 4),
            ],
            "sorted; no HashMap/Imported (imports), no impl-Widget site, \
             one exportedFn site (the export re-emit is dropped), nothing from .tsx / .md"
        );
    }

    // --- selection ----------------------------------------------------------------

    #[test]
    fn a_name_defined_more_than_once_is_dropped() {
        let sites = [
            site("parse_config", "a.rs", 1),
            site("parse_config", "b.rs", 9),
            site("twice_in_one_file", "c.rs", 1),
            site("twice_in_one_file", "c.rs", 20),
            site("defined_once", "d.rs", 3),
        ];
        let got = select("skim", &sites, 20, every_name_has(5)).unwrap();
        let names: Vec<&str> = got.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["defined_once"]);
        assert_eq!(
            got[0].def,
            DefSite {
                path: "d.rs".to_string(),
                line: 3
            }
        );
    }

    #[test]
    fn names_shorter_than_six_bytes_are_dropped() {
        let sites = [site("parse", "a.rs", 1), site("parse_", "b.rs", 1)];
        let got = select("skim", &sites, 20, every_name_has(5)).unwrap();
        let names: Vec<&str> = got.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["parse_"]);
    }

    #[test]
    fn the_ground_truth_must_cover_two_to_sixty_files() {
        let sites = [
            site("only_itself", "a.rs", 1),
            site("just_enough", "b.rs", 1),
            site("upper_bound", "c.rs", 1),
            site("too_common", "d.rs", 1),
        ];
        let counts = BTreeMap::from([
            ("only_itself", 1),
            ("just_enough", 2),
            ("upper_bound", 60),
            ("too_common", 61),
        ]);
        let got = select("skim", &sites, 20, |name| Ok(counts[name])).unwrap();
        let mut names: Vec<&str> = got.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["just_enough", "upper_bound"]);
        assert!(got.iter().all(|c| c.gt_files == counts[c.name.as_str()]));
    }

    #[test]
    fn candidates_follow_the_corpus_scoped_sha256_order_and_stop_at_take() {
        let names = [
            "alpha_one",
            "bravo_two",
            "charlie_three",
            "delta_four",
            "echo_five",
        ];
        let sites: Vec<DefinitionSite> = names.iter().map(|n| site(n, "a.rs", 1)).collect();

        let got = select("skim", &sites, 3, every_name_has(5)).unwrap();
        assert_eq!(got.len(), 3);
        let keys: Vec<&str> = got.iter().map(|c| c.order_key.as_str()).collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "ascending sha256 order");

        let mut expected: Vec<(String, &str)> =
            names.iter().map(|n| (order_key("skim", n), *n)).collect();
        expected.sort();
        let want: Vec<&str> = expected.iter().take(3).map(|(_, n)| *n).collect();
        let got_names: Vec<&str> = got.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(got_names, want);

        // The corpus name is part of the key: another corpus orders differently.
        assert_ne!(
            order_key("skim", "alpha_one"),
            order_key("zod", "alpha_one")
        );
    }

    #[test]
    fn order_key_is_the_hex_sha256_of_corpus_colon_name() {
        // `printf 'skim:abc' | shasum -a 256`, computed outside this code.
        assert_eq!(
            order_key("skim", "abc"),
            "3adf64015c1fbfac7fd8025f0ad02a8f33cee6d3aae3cfe71ea6836e1758e45a"
        );
    }

    #[test]
    fn selection_is_independent_of_input_order() {
        let mut sites: Vec<DefinitionSite> = (0..30)
            .map(|i| site(&format!("symbol_{i:02}"), &format!("f{i}.rs"), i + 1))
            .collect();
        let forward = select("ripgrep", &sites, 20, every_name_has(3)).unwrap();
        sites.reverse();
        let backward = select("ripgrep", &sites, 20, every_name_has(3)).unwrap();
        assert_eq!(forward, backward);
        assert_eq!(forward.len(), 20);
    }

    #[test]
    fn ground_truth_is_computed_lazily_until_take_is_reached() {
        let sites: Vec<DefinitionSite> = (0..50)
            .map(|i| site(&format!("lazy_name_{i:02}"), "a.rs", 1))
            .collect();
        let mut calls = 0;
        let got = select("skim", &sites, 4, |_| {
            calls += 1;
            Ok(10)
        })
        .unwrap();
        assert_eq!(got.len(), 4);
        assert_eq!(calls, 4, "every name passes, so exactly `take` lookups");
    }

    #[test]
    fn a_ground_truth_error_is_propagated() {
        let sites = [site("failing_name", "a.rs", 1)];
        assert!(select("skim", &sites, 20, |_| anyhow::bail!("boom")).is_err());
    }

    // --- rendering ------------------------------------------------------------------

    #[test]
    fn rendered_entries_parse_as_generated_idents_with_sequential_ids() {
        let candidates = vec![
            Candidate {
                name: "check_staleness".to_string(),
                def: DefSite {
                    path: "src/a \"quoted\".rs".to_string(),
                    line: 7,
                },
                gt_files: 12,
                order_key: "0".repeat(64),
            },
            Candidate {
                name: "FileManifest".to_string(),
                def: DefSite {
                    path: "src/b.rs".to_string(),
                    line: 1,
                },
                gt_files: 3,
                order_key: "1".repeat(64),
            },
        ];
        let body = render_toml("skim", &candidates);
        let src = format!(
            "corpus = \"skim\"\ncommit = \"b8a0a79463382347820f1c2572bde37b68e87c76\"\n{body}"
        );
        let golden = parse_golden(&src).unwrap();
        let ids: Vec<&str> = golden.idents.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["skim-I01", "skim-I02"]);
        assert_eq!(golden.idents[0].query, "check_staleness");
        assert_eq!(golden.idents[0].def.path, "src/a \"quoted\".rs");
        assert_eq!(golden.idents[0].def.line, 7);
        assert!(golden.idents.iter().all(|e| e.origin == Origin::Generated));
        assert!(body.contains("and-GT files: 12"), "{body}");
    }

    // --- end to end over a fixture corpus ----------------------------------------------

    #[test]
    fn generate_over_a_universe_yields_integrity_clean_entries() {
        let repo = FixtureRepo::new();
        repo.write(
            "src/config.rs",
            "// config\npub fn load_settings() {}\npub struct SettingsStore;\n",
        );
        repo.write("src/main.rs", "fn main() { load_settings(); }\n");
        repo.write("src/other.rs", "// uses SettingsStore\n");
        repo.write("src/dup_a.rs", "pub fn duplicated_name() {}\n");
        repo.write("src/dup_b.rs", "pub fn duplicated_name() {}\n");
        repo.write("src/lonely.rs", "pub fn lonely_function() {}\n");
        let sha = repo.commit_all("init");
        let universe = Universe::compute(repo.root(), &GitIsolation::new(repo.home())).unwrap();

        let got = generate("skim", &universe, 20).unwrap();
        let mut names: Vec<&str> = got.iter().map(|c| c.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["SettingsStore", "load_settings"],
            "duplicated_name has two sites; lonely_function occurs in one file only"
        );

        let src = format!(
            "corpus = \"skim\"\ncommit = \"{sha}\"\n{}",
            render_toml("skim", &got)
        );
        let golden = parse_golden(&src).unwrap();
        let violations = check_integrity(
            &golden,
            &IntegrityContext {
                corpus: "skim",
                commit: &sha,
                universe: Some(&universe),
                ledger: &[],
                catalog: catalog(),
            },
        );
        assert!(violations.is_empty(), "{violations:?}");
    }

    // --- [[ast]] candidates --------------------------------------------------------

    fn ast_answers(files: &[(&str, &str)]) -> OracleAnswers {
        let oracle = rskim_oracle::structural::StructuralOracle::new().unwrap();
        OracleAnswers::compute(&oracle, files.iter().copied()).unwrap()
    }

    fn skim_rows(paths: &[&str]) -> ResultPage {
        ResultPage {
            rows: paths
                .iter()
                .map(|p| crate::scoreboard::types::ResultRow {
                    path: p.to_string(),
                    score: 1.0,
                    line: Some(1),
                    snippet: Vec::new(),
                })
                .collect(),
            has_more: false,
            verify_mode: crate::scoreboard::types::VerifyMode::Substring,
            degraded: Vec::new(),
        }
    }

    #[test]
    fn ast_ids_name_the_pattern_and_language_pair() {
        assert_eq!(
            ast_id("zod", "try-catch", OracleLang::Tsx),
            "zod-ast-try-catch-tsx"
        );
        assert_eq!(
            ast_id("skim", "rust-nested-loop", OracleLang::Rust),
            "skim-ast-rust-nested-loop-rust"
        );
    }

    #[test]
    fn the_proposed_class_follows_the_catalog_exact_flag() {
        assert_eq!(proposed_class(catalog(), "try-catch"), PrecisionClass::Hard);
        assert_eq!(
            proposed_class(catalog(), "try-catch-finally"),
            PrecisionClass::Hard
        );
        assert_eq!(
            proposed_class(catalog(), "rust-nested-loop"),
            PrecisionClass::Ratchet
        );
        assert_eq!(
            proposed_class(catalog(), "call-in-loop"),
            PrecisionClass::Ratchet
        );
        assert_eq!(
            proposed_class(catalog(), "no-such-pattern"),
            PrecisionClass::Ratchet
        );
        // The flag comes from the catalog passed in, not from skim's.
        let flipped = [CatalogPattern {
            name: "rust-nested-loop",
            exact: true,
            example: "",
        }];
        assert_eq!(
            proposed_class(&flipped, "rust-nested-loop"),
            PrecisionClass::Hard
        );
        assert_eq!(
            proposed_class(&flipped, "try-catch"),
            PrecisionClass::Ratchet
        );
    }

    #[test]
    fn ast_candidates_are_the_non_vacuous_pairs_of_the_corpus_languages() {
        let answers = ast_answers(&[
            (
                "src/nested.rs",
                "fn walk() {\n    for a in 0..2 {\n        for b in 0..2 {}\n    }\n}\n",
            ),
            ("web/a.ts", "try {\n  go();\n} catch (e) {}\n"),
            // A try/finally wrapping a separate try/catch: no single try
            // carries both clauses (the #546 shape).
            (
                "web/b.js",
                "try { try { a(); } catch (e) {} } finally { b(); }\n",
            ),
        ]);
        // skim returns the JS file for try-catch-finally although the oracle
        // does not match it: a skim-only entry is non-vacuous and proposed.
        let skim = BTreeMap::from([("try-catch-finally".to_string(), skim_rows(&["web/b.js"]))]);
        let got = generate_ast("skim", catalog(), &answers, &skim).unwrap();
        let pairs: Vec<(&str, &str, usize, usize)> = got
            .iter()
            .map(|c| {
                (
                    c.pattern.as_str(),
                    c.lang.as_str(),
                    c.oracle_files,
                    c.skim_files,
                )
            })
            .collect();
        assert!(
            pairs.contains(&("try-catch-finally", "javascript", 0, 1)),
            "{pairs:?}"
        );
        assert!(
            pairs.contains(&("rust-nested-loop", "rust", 1, 0)),
            "{pairs:?}"
        );
        assert!(
            pairs.contains(&("try-catch", "typescript", 1, 0)),
            "{pairs:?}"
        );
        assert!(
            pairs.contains(&("try-catch", "javascript", 1, 0)),
            "{pairs:?}"
        );
        assert!(
            !pairs.iter().any(|p| p.0 == "god-function"),
            "vacuous: neither the oracle nor skim finds one: {pairs:?}"
        );
        assert!(
            !pairs.iter().any(|p| p.1 == "tsx"),
            "no .tsx file in the corpus: {pairs:?}"
        );
        assert!(
            !pairs
                .iter()
                .any(|p| p.0 == "go-select" || p.0 == "python-try-except"),
            "no Go or Python file in the corpus: {pairs:?}"
        );
        let mut sorted = pairs.clone();
        sorted.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        assert_eq!(pairs, sorted, "ordered by (pattern, language name)");
        assert_eq!(
            generate_ast("skim", catalog(), &answers, &skim).unwrap(),
            got
        );

        // Exactly the entries the oracle matches nothing for are proposed as
        // false-positive guards.
        let guards: Vec<&str> = got
            .iter()
            .filter(|c| c.expect_oracle_empty)
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(guards, ["skim-ast-try-catch-finally-javascript"]);
        assert!(
            got.iter()
                .all(|c| c.expect_oracle_empty == (c.oracle_files == 0))
        );
    }

    #[test]
    fn rows_no_proposed_entry_scores_are_reported_per_pattern() {
        let answers = ast_answers(&[
            (
                "src/nested.rs",
                "fn walk() {\n    for a in 0..2 {\n        for b in 0..2 {}\n    }\n}\n",
            ),
            ("web/a.ts", "try {\n  go();\n} catch (e) {}\n"),
        ]);
        // skim is called for every catalog pattern. deep-nesting has no
        // oracle, god-function has no entry for Python, and the corpus has
        // no .tsx file, so no try-catch entry scores `web/b.tsx`.
        let skim = BTreeMap::from([
            (
                "rust-nested-loop".to_string(),
                skim_rows(&["src/nested.rs"]),
            ),
            (
                "deep-nesting".to_string(),
                skim_rows(&["src/nested.rs", "lib/C.java"]),
            ),
            ("god-function".to_string(), skim_rows(&["app.py"])),
            ("go-select".to_string(), skim_rows(&[])),
            (
                "try-catch".to_string(),
                skim_rows(&["web/a.ts", "web/b.tsx"]),
            ),
        ]);
        let candidates = generate_ast("skim", catalog(), &answers, &skim).unwrap();
        let unscored = unscored_after(&candidates, &skim);
        let got: Vec<(&str, Vec<&str>)> = unscored
            .iter()
            .map(|(p, rows)| (*p, rows.iter().map(|r| r.path.as_str()).collect()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("deep-nesting", vec!["src/nested.rs", "lib/C.java"]),
                ("god-function", vec!["app.py"]),
                ("try-catch", vec!["web/b.tsx"]),
            ],
            "patterns with no unscored row are left out"
        );

        let comment = render_unscored_comment(&unscored);
        assert!(comment.lines().all(|l| l.starts_with('#')), "{comment}");
        assert!(
            comment.contains("structural.unscored_rows.<pattern>"),
            "{comment}"
        );
        assert!(
            comment.contains("#   deep-nesting 2: src/nested.rs:1, lib/C.java:1"),
            "{comment}"
        );
        assert!(
            comment.contains("#   try-catch 1: web/b.tsx:1"),
            "{comment}"
        );
        assert_eq!(
            render_unscored_comment(&[]),
            "# golden-gen: skim rows no proposed entry scores: none\n"
        );
    }

    #[test]
    fn rendered_ast_candidates_are_integrity_clean_golden_entries() {
        let answers = ast_answers(&[("web/a.ts", "try {\n  go();\n} catch (e) {}\n")]);
        // skim's try-catch-finally row is a false positive: a guard entry.
        let skim = BTreeMap::from([("try-catch-finally".to_string(), skim_rows(&["web/a.ts"]))]);
        let got = generate_ast("skim", catalog(), &answers, &skim).unwrap();
        assert!(got.iter().any(|c| c.expect_oracle_empty));
        assert!(got.iter().any(|c| !c.expect_oracle_empty));
        let rendered = render_ast_toml(&got);
        assert_eq!(
            rendered.matches("expect_oracle_empty = true").count(),
            got.iter().filter(|c| c.expect_oracle_empty).count(),
            "{rendered}"
        );
        let golden = parse_golden(&format!(
            "corpus = \"skim\"\ncommit = \"b8a0a79463382347820f1c2572bde37b68e87c76\"\n{rendered}"
        ))
        .unwrap();
        assert_eq!(golden.asts.len(), got.len());
        for (entry, candidate) in golden.asts.iter().zip(&got) {
            assert_eq!(
                (
                    entry.id.as_str(),
                    entry.pattern.as_str(),
                    entry.lang,
                    entry.precision,
                    entry.expect_oracle_empty
                ),
                (
                    candidate.id.as_str(),
                    candidate.pattern.as_str(),
                    candidate.lang,
                    candidate.precision,
                    candidate.expect_oracle_empty
                )
            );
        }
        let violations = check_integrity(
            &golden,
            &IntegrityContext {
                corpus: "skim",
                commit: "b8a0a79463382347820f1c2572bde37b68e87c76",
                universe: None,
                ledger: &[],
                catalog: catalog(),
            },
        );
        assert!(violations.is_empty(), "{violations:?}");
    }
}
