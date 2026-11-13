//! `golden-gen`: candidate `[[ident]]` entries (#203) for a corpus's golden
//! file.
//!
//! The output is a proposal. It is reviewed, pasted into
//! `golden/<corpus>.toml` and frozen there; the committed golden file is the
//! source of truth and CI never regenerates it.
//!
//! Selection (deterministic, no RNG):
//! 1. [`definition_sites`]: the role oracle's declared names
//!    ([`rskim_oracle::roles::definition_names`]) over every oracle-universe
//!    file whose extension the oracle has a grammar for ([`oracle_language`]).
//! 2. [`select`]: names with exactly one definition site in the universe,
//!    at least [`MIN_NAME_BYTES`] long, whose `and` ground truth covers
//!    [`GT_FILES_MIN`]`..=`[`GT_FILES_MAX`] files, ordered by
//!    `sha256("<corpus>:<name>")`, first [`GENERATED_PER_CORPUS`] taken.
//!
//! `def.line` is the line the role oracle gives for the declared name, so
//! golden integrity's def-line check holds by construction. Nothing here
//! reads skim's output or scores skim.

use std::collections::BTreeMap;

use anyhow::Context;
use rskim_oracle::roles::definition_names;
use rskim_oracle::structural::{LangClass, OracleLang, classify};

use crate::scoreboard::golden::{DefSite, hex_sha256};
use crate::scoreboard::oracle::{LexicalQuery, ground_truth};
use crate::scoreboard::universe::TextUniverse;

/// Generated identifier entries per corpus.
pub const GENERATED_PER_CORPUS: usize = 20;

/// Shortest name kept (bytes): shorter names are substrings of too much.
pub const MIN_NAME_BYTES: usize = 6;

/// Fewest `and`-ground-truth files for a kept name (the definition file plus
/// at least one other, so ranking has something to rank against).
pub const GT_FILES_MIN: usize = 2;

/// Most `and`-ground-truth files for a kept name.
pub const GT_FILES_MAX: usize = 60;

/// One definition found by the role oracle.
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

/// The role oracle's language for `path`, by extension only
/// (case-sensitive): the languages the oracle has role queries for.
pub fn oracle_language(path: &str) -> Option<OracleLang> {
    match classify(path) {
        LangClass::Oracle(lang) => Some(lang),
        LangClass::Unscored { .. } | LangClass::NotIndexed { .. } => None,
    }
}

/// Every definition site in `files` (`(path, text)` pairs), sorted.
///
/// # Errors
///
/// A role-oracle failure on a file, naming the path.
pub fn definition_sites<'a>(
    files: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> anyhow::Result<Vec<DefinitionSite>> {
    let mut sites = Vec::new();
    for (path, text) in files {
        let Some(lang) = oracle_language(path) else {
            continue;
        };
        let names = definition_names(lang, text)
            .with_context(|| format!("{path}: the role oracle failed"))?;
        sites.extend(names.into_iter().map(|(name, line)| DefinitionSite {
            name,
            path: path.to_string(),
            line,
        }));
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
pub fn generate(
    corpus: &str,
    universe: &TextUniverse,
    take: usize,
) -> anyhow::Result<Vec<Candidate>> {
    let sites = definition_sites(universe.files())?;
    select(corpus, &sites, take, |name| {
        let query =
            LexicalQuery::new(name, None).with_context(|| format!("oracle query for {name:?}"))?;
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

/// A TOML basic string (quoted and escaped by the `toml` crate).
fn toml_string(s: &str) -> String {
    toml::Value::String(s.to_string()).to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
mod tests {
    use super::*;
    use crate::scoreboard::golden::{IntegrityContext, Origin, check_integrity, parse_golden};
    use crate::scoreboard::test_support::FixtureRepo;
    use crate::scoreboard::universe::{GitIsolation, text_universe};

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

    // --- language routing and definitions ------------------------------------------

    #[test]
    fn oracle_language_is_extension_only_and_case_sensitive() {
        assert_eq!(oracle_language("src/lib.rs"), Some(OracleLang::Rust));
        assert_eq!(oracle_language("a/b.py"), Some(OracleLang::Python));
        assert_eq!(oracle_language("cmd/main.go"), Some(OracleLang::Go));
        assert_eq!(oracle_language("a.ts"), Some(OracleLang::TypeScript));
        assert_eq!(oracle_language("a.tsx"), Some(OracleLang::Tsx));
        assert_eq!(oracle_language("a.js"), Some(OracleLang::JavaScript));
        for none in ["a.RS", "README.md", "Makefile", "a.rs.bak", "a.json"] {
            assert_eq!(oracle_language(none), None, "{none}");
        }
    }

    #[test]
    fn definition_sites_keep_declarations_and_drop_imports_and_uses() {
        let rust = "use std::collections::HashMap;\n\
                    pub struct Widget;\n\
                    impl Widget {\n    pub fn render_widget(&self) {}\n}\n\
                    fn caller() { render_widget(); }\n";
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
                site("caller", "src/widget.rs", 6),
                site("exportedFn", "web/gadget.ts", 2),
                site("render_widget", "src/widget.rs", 4),
                site("tsxOnly", "web/view.tsx", 1),
            ],
            "sorted; no HashMap or Imported (imports), no call of render_widget, \
             nothing from README.md"
        );
    }

    #[test]
    fn a_file_the_oracle_cannot_parse_as_its_language_is_still_listed_by_what_parses() {
        // The role oracle reads error nodes like tree-sitter does: the
        // declaration before the damage is still found.
        let sites = definition_sites([("a.rs", "fn intact_one() {}\nfn broken( {\n")]).unwrap();
        assert!(sites.iter().any(|s| s.name == "intact_one"), "{sites:?}");
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
        let text = text_universe(repo.root(), &GitIsolation::new(repo.home())).unwrap();

        let got = generate("skim", &text, 20).unwrap();
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
                universe: Some(&text),
                ledger: &[],
            },
        );
        assert!(violations.is_empty(), "{violations:?}");
    }
}
