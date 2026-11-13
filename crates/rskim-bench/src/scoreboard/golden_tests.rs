//! Unit tests for `golden.rs` (co-located file, `#[path]`-included).

use super::*;
use crate::scoreboard::test_support::FixtureRepo;
use crate::scoreboard::universe::{GitIsolation, text_universe};

const SHA: &str = "b8a0a79463382347820f1c2572bde37b68e87c76";

/// The schema example from the design, comments included.
const DESIGN_EXAMPLE: &str = r#"
corpus = "skim"
commit = "b8a0a79463382347820f1c2572bde37b68e87c76"   # must equal corpora.toml

[[ident]]                  # definition-ranking queries
id = "skim-L01"
query = "check_staleness"
def = { path = "crates/rskim/src/cmd/search/staleness.rs", line = 321 }
origin = "seed"            # seed | curated | generated

[[concept]]                # ranking queries; relevance declared up front, never from skim output
id = "skim-C01"
query = "build lock"
relevant = '(?i)build[_\s-]*lock'

[[lexical]]                # recall/precision edge cases
id = "skim-X07"
query = "-D warnings"
category = "punct"         # substr | short | punct | case | lang | zero-hit
# lang = "toml"            # optional: passed as -t and applied by the oracle's own type table

[[pagination]]
id = "skim-G001"
query = "elision marker"
limits = [3, 7, 20]        # full count must be <= min(limits) x (MAX_PAGES - 1), else exit 2

[[prefix]]                 # limit=N must equal the first N rows of the full list
id = "skim-F001"
query = "fn"
limits = [5, 20]
"#;

fn golden(src: &str) -> GoldenFile {
    parse_golden(src).unwrap()
}

fn ctx<'a>(ledger: &'a [LedgerRef<'a>]) -> IntegrityContext<'a> {
    IntegrityContext {
        corpus: "skim",
        commit: SHA,
        universe: None,
        ledger,
    }
}

fn violations_for(src: &str) -> Vec<IntegrityViolation> {
    check_integrity(&golden(src), &ctx(&[]))
}

/// Whether a violation names `id`.
fn flagged(violations: &[IntegrityViolation], id: &str) -> bool {
    violations.iter().any(|v| v.id.as_deref() == Some(id))
}

fn header() -> String {
    format!("corpus = \"skim\"\ncommit = \"{SHA}\"\n")
}

// --- schema ---------------------------------------------------------------

#[test]
fn the_design_example_parses_and_passes_schema_integrity() {
    let g = golden(DESIGN_EXAMPLE);
    assert_eq!((g.corpus.as_str(), g.commit.as_str()), ("skim", SHA));
    assert_eq!(g.idents[0].def.line, 321);
    assert_eq!(g.idents[0].origin, Origin::Seed);
    assert_eq!(g.lexicals[0].category, LexicalCategory::Punct);
    assert_eq!(g.paginations[0].limits, vec![3, 7, 20]);
    assert_eq!(g.prefixes[0].query, "fn");
    assert_eq!(
        check_integrity(&g, &ctx(&[])),
        Vec::<IntegrityViolation>::new()
    );
}

#[test]
fn misspelled_or_unknown_fields_are_rejected() {
    for body in [
        "[[pagination]]\nid = \"skim-G1\"\nquery = \"q\"\nlimit = [3]\n",
        // A flag list, a match mode and a window are gone with the old arms.
        "[[pagination]]\nid = \"skim-G1\"\nquery = \"q\"\nflags = []\nlimits = [3]\n",
        "[[lexical]]\nid = \"skim-X1\"\nquery = \"q\"\ncategory = \"punct\"\nmode = \"and\"\n",
        "[[lexical]]\nid = \"skim-X1\"\nquery = \"q\"\ncategory = \"punct\"\nnear = 3\n",
        "[[lexical]]\nid = \"skim-X1\"\nquery = \"q\"\ncategory = \"typo\"\n",
        // A prefix entry always has a query.
        "[[prefix]]\nid = \"skim-F1\"\nlimits = [5]\n",
        "[[prefix]]\nid = \"skim-F1\"\nquery = \"q\"\nflags = [\"--hot\"]\nlimits = [5]\n",
        "[[ident]]\nid = \"skim-L1\"\nquery = \"q\"\ndef = { path = \"a.rs\", line = 1 }\norigin = \"llm\"\n",
        "[[ast]]\nid = \"skim-A1\"\npattern = \"try-catch\"\nlang = \"rust\"\nprecision = \"hard\"\n",
        "[[widget]]\nid = \"skim-W1\"\n",
    ] {
        assert!(
            parse_golden(&format!("{}{body}", header())).is_err(),
            "{body}"
        );
    }
}

#[test]
fn ident_origins_are_seed_curated_or_generated() {
    for (raw, want) in [
        ("seed", Origin::Seed),
        ("curated", Origin::Curated),
        ("generated", Origin::Generated),
    ] {
        let g = golden(&format!(
            "{}[[ident]]\nid = \"skim-L1\"\nquery = \"q\"\n\
             def = {{ path = \"a.rs\", line = 1 }}\norigin = \"{raw}\"\n",
            header()
        ));
        assert_eq!(g.idents[0].origin, want, "{raw}");
    }
}

#[test]
fn lexical_categories_are_kebab_case() {
    let g = golden(&format!(
        "{}[[lexical]]\nid = \"skim-X20\"\nquery = \"zzq\"\ncategory = \"zero-hit\"\n",
        header()
    ));
    assert_eq!(g.lexicals[0].category, LexicalCategory::ZeroHit);
    assert_eq!(g.lexicals[0].lang, None);
}

#[test]
fn entry_kinds_are_indexed_by_id() {
    let g = golden(DESIGN_EXAMPLE);
    assert_eq!(g.entry_kind("skim-L01"), Some(EntryKind::Ident));
    assert_eq!(g.entry_kind("skim-G001"), Some(EntryKind::Pagination));
    assert_eq!(g.entry_kind("skim-F001"), Some(EntryKind::Prefix));
    assert_eq!(g.entry_kind("nope"), None);
    assert_eq!(g.ids().count(), 5);
}

// --- [[refs]] / [[clash]] / [[stakes]] / concept `first` ---------------------

/// One entry of each frozen-answer table, in the shape the design shows.
const NEW_TABLES: &str = r#"
[[refs]]
id = "skim-R01"
query = "run"
def = { path = "src/a.rs", line = 1 }
sites = [
  { path = "src/a.rs", line = 1, role = "def" },
  { path = "src/a.rs", line = 2, role = "call" },
]
origin = "curated"

[[clash]]
id = "skim-D01"
query = "run"
defs = [ { path = "src/a.rs", line = 1 }, { path = "src/b.rs", line = 1 } ]
calls = [
  { path = "src/a.rs", line = 2, def = 0 },
  { path = "src/c.rs", line = 3, def = "unassigned" },
]
split = "none"
origin = "curated"

[[stakes]]
id = "skim-S01"
query = "qzvx_absent_7713"
flags = []
expect = "exit"
exit = 1
"#;

fn refs(id: &str, query: &str, def: (&str, u32), sites: &[(&str, u32, &str)]) -> String {
    let sites: Vec<String> = sites
        .iter()
        .map(|(p, l, r)| format!("{{ path = \"{p}\", line = {l}, role = \"{r}\" }}"))
        .collect();
    format!(
        "[[refs]]\nid = \"{id}\"\nquery = \"{query}\"\n\
         def = {{ path = \"{}\", line = {} }}\nsites = [{}]\norigin = \"curated\"\n",
        def.0,
        def.1,
        sites.join(", ")
    )
}

/// `calls` are `(path, line, def)` with `def` already in TOML form
/// (`0` or `"unassigned"`).
fn clash(
    id: &str,
    query: &str,
    defs: &[(&str, u32)],
    calls: &[(&str, u32, &str)],
    split: &str,
) -> String {
    let defs: Vec<String> = defs
        .iter()
        .map(|(p, l)| format!("{{ path = \"{p}\", line = {l} }}"))
        .collect();
    let calls: Vec<String> = calls
        .iter()
        .map(|(p, l, d)| format!("{{ path = \"{p}\", line = {l}, def = {d} }}"))
        .collect();
    format!(
        "[[clash]]\nid = \"{id}\"\nquery = \"{query}\"\ndefs = [{}]\ncalls = [{}]\n\
         split = \"{split}\"\norigin = \"curated\"\n",
        defs.join(", "),
        calls.join(", ")
    )
}

fn stakes(id: &str, query: &str, flags: &str, expect: &str, exit: Option<u8>) -> String {
    let exit = exit.map(|c| format!("exit = {c}\n")).unwrap_or_default();
    format!(
        "[[stakes]]\nid = \"{id}\"\nquery = \"{query}\"\nflags = {flags}\nexpect = \"{expect}\"\n{exit}"
    )
}

fn message_of(violations: &[IntegrityViolation], id: &str) -> String {
    violations
        .iter()
        .filter(|v| v.id.as_deref() == Some(id))
        .map(|v| v.message.as_str())
        .collect::<Vec<_>>()
        .join(" | ")
}

#[test]
fn the_new_tables_parse_with_every_field_and_pass_schema_integrity() {
    let g = golden(&format!("{}{NEW_TABLES}", header()));
    assert_eq!(g.refs[0].query, "run");
    assert_eq!(g.refs[0].def.line, 1);
    assert_eq!(g.refs[0].sites[1].role, SiteRole::Call);
    assert_eq!(g.refs[0].origin, Origin::Curated);
    assert_eq!(g.clashes[0].defs.len(), 2);
    assert_eq!(g.clashes[0].split, ClashSplit::None);
    assert_eq!(g.stakes[0].expect, StakesExpect::Exit);
    assert_eq!(g.stakes[0].exit, Some(1));
    assert!(g.stakes[0].flags.is_empty());
    assert_eq!(check_integrity(&g, &ctx(&[])), Vec::new());
}

#[test]
fn every_site_role_name_loads() {
    for (raw, want) in [
        ("def", SiteRole::Def),
        ("call", SiteRole::Call),
        ("ref", SiteRole::Ref),
        ("import", SiteRole::Import),
        ("comment", SiteRole::Comment),
        ("string", SiteRole::String),
        ("text", SiteRole::Text),
    ] {
        let g = golden(&format!(
            "{}{}",
            header(),
            refs("skim-R1", "run", ("a.rs", 1), &[("a.rs", 1, raw)])
        ));
        assert_eq!(g.refs[0].sites[0].role, want, "{raw}");
    }
}

#[test]
fn a_call_def_is_an_index_or_the_word_unassigned() {
    let g = golden(&format!("{}{NEW_TABLES}", header()));
    assert_eq!(g.clashes[0].calls[0].def, CallDef::Index(0));
    assert_eq!(
        g.clashes[0].calls[1].def,
        CallDef::Unassigned(Unassigned::Unassigned)
    );
}

#[test]
fn typos_and_unknown_values_in_the_new_tables_do_not_load() {
    let bad = [
        // `site` for `sites`.
        "[[refs]]\nid = \"skim-R1\"\nquery = \"q\"\ndef = { path = \"a.rs\", line = 1 }\n\
         site = []\norigin = \"curated\"\n"
            .to_string(),
        // unknown role.
        refs("skim-R1", "q", ("a.rs", 1), &[("a.rs", 1, "callee")]),
        // a role on the clash call.
        "[[clash]]\nid = \"skim-D1\"\nquery = \"q\"\ndefs = []\n\
         calls = [{ path = \"a.rs\", line = 1, def = 0, role = \"call\" }]\n\
         split = \"none\"\norigin = \"curated\"\n"
            .to_string(),
        // a call def that is neither an index nor "unassigned".
        clash(
            "skim-D1",
            "q",
            &[("a.rs", 1), ("b.rs", 1)],
            &[("a.rs", 2, "\"first\"")],
            "none",
        ),
        clash("skim-D1", "q", &[("a.rs", 1)], &[], "approx"),
        // an `origin` on stakes (the table has none).
        format!(
            "{}origin = \"curated\"\n",
            stakes("skim-S1", "q", "[]", "lines", None)
        ),
        stakes("skim-S1", "q", "[]", "bytes", None),
        // `exit` out of range for an exit code.
        stakes("skim-S1", "q", "[]", "exit", Some(1)).replace("exit = 1", "exit = 300"),
    ];
    for body in bad {
        assert!(
            parse_golden(&format!("{}{body}", header())).is_err(),
            "{body}"
        );
    }
}

#[test]
fn the_new_entries_are_indexed_by_id_and_share_the_id_rules() {
    let g = golden(&format!("{}{NEW_TABLES}", header()));
    assert_eq!(g.entry_kind("skim-R01"), Some(EntryKind::Refs));
    assert_eq!(g.entry_kind("skim-D01"), Some(EntryKind::Clash));
    assert_eq!(g.entry_kind("skim-S01"), Some(EntryKind::Stakes));
    assert_eq!(g.ids().count(), 3);

    let v = violations_for(&format!(
        "{}{}{}",
        header(),
        refs("skim-R1", "run", ("a.rs", 1), &[("a.rs", 1, "def")]),
        stakes("skim-R1", "q", "[]", "lines", None),
    ));
    assert!(
        message_of(&v, "skim-R1").contains("used by 2 entries"),
        "{v:?}"
    );
    let v = violations_for(&format!(
        "{}{}",
        header(),
        stakes("zod-S1", "q", "[]", "lines", None)
    ));
    assert!(flagged(&v, "zod-S1"), "wrong corpus prefix: {v:?}");
}

// --- concept `first` ---------------------------------------------------------

#[test]
fn a_concept_without_first_still_loads_so_old_golden_files_stay_valid() {
    let g = golden(DESIGN_EXAMPLE);
    assert_eq!(g.concepts[0].first, None);
    assert!(check_integrity(&g, &ctx(&[])).is_empty());
}

#[test]
fn a_concept_first_lists_the_acceptable_first_files() {
    let g = golden(&format!(
        "{}[[concept]]\nid = \"skim-C1\"\nquery = \"build lock\"\nrelevant = \"lock\"\n\
         first = [\"src/a.rs\", \"src/b.rs\"]\n",
        header()
    ));
    assert_eq!(
        g.concepts[0].first,
        Some(vec!["src/a.rs".to_string(), "src/b.rs".to_string()])
    );
    assert!(check_integrity(&g, &ctx(&[])).is_empty());
}

#[test]
fn an_empty_or_blank_concept_first_is_a_violation() {
    for first in ["[]", "[\"\"]", "[\"a.rs\", \" \"]", "[\"a.rs\", \"a.rs\"]"] {
        let v = violations_for(&format!(
            "{}[[concept]]\nid = \"skim-C1\"\nquery = \"q\"\nrelevant = \"q\"\nfirst = {first}\n",
            header()
        ));
        assert!(flagged(&v, "skim-C1"), "first = {first}: {v:?}");
    }
}

// --- [[clash]] schema rules --------------------------------------------------

fn clash_violations(body: &str) -> Vec<IntegrityViolation> {
    violations_for(&format!("{}{body}", header()))
}

#[test]
fn a_clash_needs_two_defs_a_call_def_in_range_and_no_split() {
    let one_def = clash_violations(&clash(
        "skim-D1",
        "run",
        &[("a.rs", 1)],
        &[("a.rs", 2, "\"unassigned\"")],
        "none",
    ));
    assert!(
        message_of(&one_def, "skim-D1").contains("2 or more"),
        "{one_def:?}"
    );

    let out_of_range = clash_violations(&clash(
        "skim-D2",
        "run",
        &[("a.rs", 1), ("b.rs", 1)],
        &[("a.rs", 2, "2")],
        "none",
    ));
    assert!(
        message_of(&out_of_range, "skim-D2").contains("out of range"),
        "{out_of_range:?}"
    );

    let exact = clash_violations(&clash(
        "skim-D3",
        "run",
        &[("a.rs", 1), ("b.rs", 1)],
        &[("a.rs", 2, "0")],
        "exact",
    ));
    assert!(message_of(&exact, "skim-D3").contains("split"), "{exact:?}");
}

#[test]
fn a_valid_clash_has_no_violation_and_an_index_may_name_the_last_def() {
    let v = clash_violations(&clash(
        "skim-D1",
        "run",
        &[("a.rs", 1), ("b.rs", 1)],
        &[("a.rs", 2, "1"), ("a.rs", 3, "\"unassigned\"")],
        "none",
    ));
    assert!(v.is_empty(), "{v:?}");
}

#[test]
fn a_clash_with_a_repeated_def_or_call_site_is_a_violation() {
    let repeated_def = clash_violations(&clash(
        "skim-D1",
        "run",
        &[("a.rs", 1), ("a.rs", 1)],
        &[("a.rs", 2, "\"unassigned\"")],
        "none",
    ));
    assert!(
        message_of(&repeated_def, "skim-D1").contains("def a.rs:1 is listed twice"),
        "{repeated_def:?}"
    );
    let repeated_call = clash_violations(&clash(
        "skim-D2",
        "run",
        &[("a.rs", 1), ("b.rs", 1)],
        &[("a.rs", 2, "0"), ("a.rs", 2, "1")],
        "none",
    ));
    assert!(
        message_of(&repeated_call, "skim-D2").contains("call a.rs:2 is listed twice"),
        "{repeated_call:?}"
    );
}

#[test]
fn a_clash_entry_needs_a_call() {
    let v = clash_violations(&clash(
        "skim-D1",
        "run",
        &[("a.rs", 1), ("b.rs", 1)],
        &[],
        "none",
    ));
    assert!(message_of(&v, "skim-D1").contains("no calls"), "{v:?}");
}

// --- [[refs]] schema rules ---------------------------------------------------

#[test]
fn a_refs_entry_needs_sites_that_are_distinct_and_include_its_def() {
    let no_sites = clash_violations(&refs("skim-R1", "run", ("a.rs", 1), &[]));
    assert!(
        message_of(&no_sites, "skim-R1").contains("no sites"),
        "{no_sites:?}"
    );

    let duplicate = clash_violations(&refs(
        "skim-R2",
        "run",
        ("a.rs", 1),
        &[("a.rs", 1, "def"), ("a.rs", 1, "call")],
    ));
    assert!(
        message_of(&duplicate, "skim-R2").contains("is listed twice"),
        "{duplicate:?}"
    );

    let def_missing =
        clash_violations(&refs("skim-R3", "run", ("a.rs", 1), &[("a.rs", 2, "call")]));
    assert!(
        message_of(&def_missing, "skim-R3").contains("def is not one of the sites"),
        "{def_missing:?}"
    );

    let ok = clash_violations(&refs(
        "skim-R4",
        "run",
        ("a.rs", 1),
        &[("a.rs", 1, "def"), ("a.rs", 2, "call")],
    ));
    assert!(ok.is_empty(), "{ok:?}");
}

#[test]
fn a_refs_entry_needs_a_call_site() {
    let def_only = clash_violations(&refs("skim-R1", "run", ("a.rs", 1), &[("a.rs", 1, "def")]));
    assert!(
        message_of(&def_only, "skim-R1").contains("no call site"),
        "{def_only:?}"
    );

    let other_roles = clash_violations(&refs(
        "skim-R2",
        "run",
        ("a.rs", 1),
        &[
            ("a.rs", 1, "def"),
            ("a.rs", 2, "import"),
            ("a.rs", 3, "comment"),
        ],
    ));
    assert!(
        message_of(&other_roles, "skim-R2").contains("no call site"),
        "a ref, import or comment is not a call: {other_roles:?}"
    );
}

#[test]
fn the_refs_and_clash_queries_must_be_bare_identifiers() {
    let sites = [("a.rs", 1, "def"), ("a.rs", 2, "call")];
    for query in ["", "two words", "a-b", "a.b", "run()", "$run"] {
        let r = clash_violations(&refs("skim-R1", query, ("a.rs", 1), &sites));
        assert!(flagged(&r, "skim-R1"), "refs {query:?}: {r:?}");
        let c = clash_violations(&clash(
            "skim-D1",
            query,
            &[("a.rs", 1), ("b.rs", 1)],
            &[("a.rs", 2, "\"unassigned\"")],
            "none",
        ));
        assert!(flagged(&c, "skim-D1"), "clash {query:?}: {c:?}");
    }
    for query in ["run", "_run", "run2", "Run_Fast"] {
        let r = clash_violations(&refs("skim-R1", query, ("a.rs", 1), &sites));
        assert!(r.is_empty(), "refs {query:?}: {r:?}");
    }
}

#[test]
fn a_refs_def_on_line_zero_is_a_violation() {
    let v = clash_violations(&refs("skim-R1", "run", ("a.rs", 0), &[("a.rs", 1, "def")]));
    assert!(
        message_of(&v, "skim-R1").contains("def.line must be >= 1"),
        "{v:?}"
    );
}

#[test]
fn a_refs_site_on_line_zero_is_a_violation() {
    let v = clash_violations(&refs(
        "skim-R1",
        "run",
        ("a.rs", 1),
        &[("a.rs", 1, "def"), ("a.rs", 0, "call")],
    ));
    assert!(
        message_of(&v, "skim-R1").contains("every site line must be >= 1"),
        "{v:?}"
    );
}

#[test]
fn a_clash_def_on_line_zero_is_a_violation() {
    let v = clash_violations(&clash(
        "skim-D1",
        "run",
        &[("a.rs", 0), ("b.rs", 1)],
        &[("a.rs", 2, "1")],
        "none",
    ));
    assert!(
        message_of(&v, "skim-D1").contains("every def and call line must be >= 1"),
        "{v:?}"
    );
}

#[test]
fn a_clash_call_on_line_zero_is_a_violation() {
    let v = clash_violations(&clash(
        "skim-D1",
        "run",
        &[("a.rs", 1), ("b.rs", 1)],
        &[("a.rs", 0, "1")],
        "none",
    ));
    assert!(
        message_of(&v, "skim-D1").contains("every def and call line must be >= 1"),
        "{v:?}"
    );
}

// --- [[stakes]] schema rules -------------------------------------------------

#[test]
fn a_stakes_exit_is_required_for_expect_exit_and_forbidden_otherwise() {
    let v = clash_violations(&stakes("skim-S1", "q", "[]", "exit", None));
    assert!(
        message_of(&v, "skim-S1").contains("requires `exit`"),
        "{v:?}"
    );
    for expect in ["lines", "files", "counts"] {
        let v = clash_violations(&stakes("skim-S2", "q", "[]", expect, Some(1)));
        assert!(flagged(&v, "skim-S2"), "{expect}: {v:?}");
        let ok = clash_violations(&stakes("skim-S3", "q", "[]", expect, None));
        assert!(ok.is_empty(), "{expect}: {ok:?}");
    }
}

#[test]
fn a_stakes_exit_other_than_0_1_or_2_is_refused_without_a_text_universe() {
    for exit in [3, 101, 255] {
        let v = clash_violations(&stakes("skim-S1", "q", "[]", "exit", Some(exit)));
        let message = message_of(&v, "skim-S1");
        assert!(
            message.contains(&format!(
                "exit = {exit} cannot be judged by the regex oracle"
            )),
            "exit {exit}: {v:?}"
        );
    }
    for exit in [0, 1] {
        let ok = clash_violations(&stakes("skim-S2", "q", "[]", "exit", Some(exit)));
        assert!(ok.is_empty(), "exit {exit}: {ok:?}");
    }
}

#[test]
fn a_stakes_query_must_not_be_blank() {
    let v = clash_violations(&stakes("skim-S1", "  ", "[]", "lines", None));
    assert!(flagged(&v, "skim-S1"), "{v:?}");
}

#[test]
fn stakes_flags_must_come_from_the_closed_set_with_their_values() {
    for flags in [
        "[]",
        "[\"-i\"]",
        "[\"-w\", \"-F\"]",
        "[\"-S\", \"-n\"]",
        "[\"-C\", \"2\"]",
        "[\"-g\", \"*.rs\", \"-t\", \"rust\"]",
    ] {
        let v = clash_violations(&stakes("skim-S1", "q", flags, "lines", None));
        assert!(v.is_empty(), "{flags}: {v:?}");
    }
    for flags in [
        "[\"-I\"]",
        "[\"--hidden\"]",
        "[\"pattern\"]",
        "[\"-g\"]",
        "[\"-t\"]",
        "[\"-C\"]",
    ] {
        let v = clash_violations(&stakes("skim-S1", "q", flags, "lines", None));
        assert!(flagged(&v, "skim-S1"), "{flags}: {v:?}");
    }
}

#[test]
fn a_stakes_entry_must_leave_the_flags_the_runner_sets_to_the_runner() {
    for flags in [
        "[\"--json\"]",
        "[\"--limit\", \"5\"]",
        "[\"--offset\", \"1\"]",
        "[\"--root\", \"/x\"]",
        "[\"-h\"]",
    ] {
        let v = clash_violations(&stakes("skim-S1", "q", flags, "lines", None));
        assert!(
            message_of(&v, "skim-S1").contains("set by the scoreboard runner"),
            "{flags}: {v:?}"
        );
    }
}

#[test]
fn a_stakes_query_with_whitespace_is_a_word_list_unless_it_is_literal_or_a_regex() {
    let v = clash_violations(&stakes("skim-S1", "two words", "[]", "lines", None));
    assert!(message_of(&v, "skim-S1").contains("list of words"), "{v:?}");
    for flags in ["[\"-F\"]", "[\"-E\"]"] {
        let ok = clash_violations(&stakes("skim-S2", "two words", flags, "lines", None));
        assert!(ok.is_empty(), "{flags}: {ok:?}");
    }
}

#[test]
fn a_stakes_glob_or_type_the_oracle_does_not_copy_is_refused() {
    for flags in [
        "[\"-g\", \"*.{rs,toml}\"]",
        "[\"-g\", \"[ab].rs\"]",
        "[\"-g\", \"\"]",
        "[\"-t\", \"cpp\"]",
        "[\"-t\", \"Rust\"]",
    ] {
        let v = clash_violations(&stakes("skim-S1", "q", flags, "lines", None));
        assert!(flagged(&v, "skim-S1"), "{flags}: {v:?}");
    }
}

#[test]
fn list_and_count_flags_pair_with_their_own_expectation() {
    for (flags, expect, loads) in [
        ("[\"-l\"]", "files", true),
        ("[\"-l\"]", "exit", true),
        ("[\"-l\"]", "lines", false),
        ("[\"-l\"]", "counts", false),
        ("[\"-c\"]", "counts", true),
        ("[\"-c\"]", "exit", true),
        ("[\"-c\"]", "files", false),
        ("[\"-c\"]", "lines", false),
    ] {
        let exit = (expect == "exit").then_some(0);
        let v = clash_violations(&stakes("skim-S1", "q", flags, expect, exit));
        assert_eq!(v.is_empty(), loads, "{flags} {expect}: {v:?}");
    }
}

#[test]
fn an_exit_2_entry_needs_a_query_or_flag_set_the_search_rejects() {
    for (query, flags) in [
        ("run", "[\"-F\", \"-E\"]"),
        ("run", "[\"-l\", \"-c\"]"),
        ("run", "[\"-l\", \"-C\", \"2\"]"),
        ("run", "[\"-c\", \"--all\"]"),
        ("run", "[\"-l\", \"--full\"]"),
        ("ru(n", "[\"-E\"]"),
    ] {
        let rejected = clash_violations(&stakes("skim-S1", query, flags, "exit", Some(2)));
        assert!(rejected.is_empty(), "{query} {flags}: {rejected:?}");
        // The same entry cannot claim a result.
        let as_lines = clash_violations(&stakes("skim-S2", query, flags, "lines", None));
        assert!(
            flagged(&as_lines, "skim-S2"),
            "{query} {flags}: {as_lines:?}"
        );
    }
    let v = clash_violations(&stakes("skim-S3", "run", "[\"-i\"]", "exit", Some(2)));
    assert!(
        message_of(&v, "skim-S3").contains("the oracle reads a valid query"),
        "{v:?}"
    );
}

// --- ledger pairing for the new kinds ----------------------------------------

#[test]
fn a_lexical_check_in_the_ledger_cannot_name_a_refs_clash_or_stakes_entry() {
    let g = golden(&format!("{}{NEW_TABLES}", header()));
    for id in ["skim-R01", "skim-D01", "skim-S01"] {
        let refs = [LedgerRef {
            check: CheckId::LexicalRecall,
            id,
        }];
        assert!(flagged(&check_integrity(&g, &ctx(&refs)), id), "{id}");
    }
}

// --- flags -----------------------------------------------------------------

#[test]
fn a_lexical_lang_becomes_a_dash_t_flag_with_the_rg_type_token() {
    for (lang, token) in [
        ("javascript", "js"),
        ("typescript", "ts"),
        ("tsx", "ts"),
        ("python", "py"),
        ("rust", "rust"),
        ("json", "json"),
        ("markdown", "md"),
        ("toml", "toml"),
    ] {
        let flags = QueryFlags::new(Some(lang)).unwrap();
        assert_eq!(flags.to_args(), vec!["-t", token], "{lang}");
    }
    assert!(QueryFlags::new(None).unwrap().to_args().is_empty());
    assert!(QueryFlags::new(Some("haskell")).is_err());
}

#[test]
fn lexical_entries_expand_to_cli_flags_and_an_oracle_query() {
    let g = golden(&format!(
        "{}[[lexical]]\nid = \"skim-P04\"\nquery = \"auto refresh stale\"\n\
         category = \"lang\"\nlang = \"rust\"\n",
        header()
    ));
    let e = &g.lexicals[0];
    assert_eq!(e.flags().unwrap().to_args(), vec!["-t", "rust"]);
    let q = e.oracle_query().unwrap();
    assert!(q.matches("src/a.rs", "auto x refresh y stale"));
    assert!(
        !q.matches("docs/a.md", "auto x refresh y stale"),
        "the type filter drops other files"
    );
    assert!(
        !q.matches("src/a.rs", "auto x refresh"),
        "every word must occur"
    );
}

// --- integrity: schema-level ------------------------------------------------

#[test]
fn corpus_and_commit_must_match_corpora_toml() {
    let g = golden(DESIGN_EXAMPLE);
    let wrong_commit = IntegrityContext {
        commit: "0123456789abcdef0123456789abcdef01234567",
        ..ctx(&[])
    };
    assert!(!check_integrity(&g, &wrong_commit).is_empty());
    let wrong_corpus = IntegrityContext {
        corpus: "zod",
        ..ctx(&[])
    };
    assert!(!check_integrity(&g, &wrong_corpus).is_empty());
}

#[test]
fn ids_must_be_unique_across_kinds_and_prefixed_with_the_corpus() {
    let v = violations_for(&format!(
        "{}[[concept]]\nid = \"skim-D1\"\nquery = \"a\"\nrelevant = \"a\"\n\
         [[lexical]]\nid = \"skim-D1\"\nquery = \"b\"\ncategory = \"substr\"\n\
         [[lexical]]\nid = \"zod-X1\"\nquery = \"c\"\ncategory = \"substr\"\n",
        header()
    ));
    assert!(flagged(&v, "skim-D1"), "{v:?}");
    assert!(flagged(&v, "zod-X1"), "{v:?}");
}

#[test]
fn each_malformed_entry_is_reported_and_nothing_short_circuits() {
    let v = violations_for(&format!(
        "{}\
         [[ident]]\nid = \"skim-L1\"\nquery = \" \"\ndef = {{ path = \"a.rs\", line = 1 }}\norigin = \"seed\"\n\
         [[ident]]\nid = \"skim-L2\"\nquery = \"x\"\ndef = {{ path = \"a.rs\", line = 0 }}\norigin = \"seed\"\n\
         [[concept]]\nid = \"skim-C1\"\nquery = \"a\"\nrelevant = \"(unclosed\"\n\
         [[lexical]]\nid = \"skim-X1\"\nquery = \"a\"\ncategory = \"lang\"\nlang = \"haskell\"\n\
         [[lexical]]\nid = \"skim-X2\"\nquery = \"a\"\ncategory = \"lang\"\n\
         [[lexical]]\nid = \"skim-X3\"\nquery = \"  \"\ncategory = \"substr\"\n\
         [[pagination]]\nid = \"skim-G1\"\nquery = \"a\"\nlimits = []\n\
         [[pagination]]\nid = \"skim-G2\"\nquery = \"a\"\nlimits = [0, 5]\n\
         [[prefix]]\nid = \"skim-F1\"\nquery = \"fn\"\nlimits = []\n\
         [[prefix]]\nid = \"skim-F2\"\nquery = \" \"\nlimits = [5]\n\
         [[stakes]]\nid = \"skim-S1\"\nquery = \"a\"\nflags = [\"--hot\"]\nexpect = \"lines\"\n",
        header()
    ));
    for id in [
        "skim-L1", "skim-L2", "skim-C1", "skim-X1", "skim-X2", "skim-X3", "skim-G1", "skim-G2",
        "skim-F1", "skim-F2", "skim-S1",
    ] {
        assert!(flagged(&v, id), "{id} not flagged: {v:?}");
    }
}

// --- integrity: ledger ------------------------------------------------------

#[test]
fn ledger_entries_must_name_an_existing_id_their_check_applies_to() {
    let g = golden(DESIGN_EXAMPLE);
    let ok = [LedgerRef {
        check: CheckId::PaginationHasMoreHonest,
        id: "skim-G001",
    }];
    assert!(check_integrity(&g, &ctx(&ok)).is_empty());

    let unknown_id = [LedgerRef {
        check: CheckId::PaginationComplete,
        id: "skim-G999",
    }];
    assert!(flagged(
        &check_integrity(&g, &ctx(&unknown_id)),
        "skim-G999"
    ));

    let wrong_kind = [LedgerRef {
        check: CheckId::PaginationComplete,
        id: "skim-L01",
    }];
    assert!(flagged(&check_integrity(&g, &ctx(&wrong_kind)), "skim-L01"));
}

// --- integrity against the corpus universe ------------------------------------

fn universe_of(repo: &FixtureRepo) -> TextUniverse {
    text_universe(repo.root(), &GitIsolation::new(repo.home())).unwrap()
}

fn corpus() -> (FixtureRepo, String, TextUniverse) {
    let repo = FixtureRepo::new();
    repo.write(
        "src/staleness.rs",
        "// header\npub fn check_staleness() {}\nfn other() {}\n",
    );
    repo.write("notes.txt", "check_staleness mentioned in a notes file\n");
    for i in 0..5 {
        repo.write(&format!("src/m{i}.rs"), "// shared marker\n");
    }
    let sha = repo.commit_all("init");
    let universe = universe_of(&repo);
    (repo, sha, universe)
}

fn check_against(universe: &TextUniverse, sha: &str, body: &str) -> Vec<IntegrityViolation> {
    let src = format!("corpus = \"skim\"\ncommit = \"{sha}\"\n{body}");
    let ctx = IntegrityContext {
        corpus: "skim",
        commit: sha,
        universe: Some(universe),
        ledger: &[],
    };
    check_integrity(&golden(&src), &ctx)
}

fn ident(path: &str, line: u32) -> String {
    format!(
        "[[ident]]\nid = \"skim-L01\"\nquery = \"check_staleness\"\n\
         def = {{ path = \"{path}\", line = {line} }}\norigin = \"seed\"\n"
    )
}

#[test]
fn a_def_line_that_contains_the_query_passes() {
    let (_repo, sha, u) = corpus();
    assert!(check_against(&u, &sha, &ident("src/staleness.rs", 2)).is_empty());
}

#[test]
fn def_line_drift_is_a_violation() {
    let (_repo, sha, u) = corpus();
    for line in [1, 3, 99] {
        let v = check_against(&u, &sha, &ident("src/staleness.rs", line));
        assert!(flagged(&v, "skim-L01"), "line {line}: {v:?}");
    }
}

#[test]
fn a_def_must_be_a_file_of_the_text_universe() {
    let (_repo, sha, u) = corpus();
    let v = check_against(&u, &sha, &ident("src/missing.rs", 1));
    assert!(
        message_of(&v, "skim-L01").contains("not in the oracle's universe"),
        "{v:?}"
    );
    // A text file with no language is in the one universe, so its line counts.
    assert!(check_against(&u, &sha, &ident("notes.txt", 1)).is_empty());
}

#[test]
fn pagination_bound_is_min_limit_times_max_pages_minus_one() {
    assert_eq!(
        pagination_bound(&[7, 3, 20]),
        Some(3 * u64::from(MAX_PAGES - 1))
    );
    assert_eq!(pagination_bound(&[]), None);
    assert!(within_pagination_bound(63, &[1, 5]));
    assert!(!within_pagination_bound(64, &[1, 5]));
    assert!(!within_pagination_bound(1, &[]));
}

#[test]
fn pagination_ground_truth_must_fit_the_page_bound() {
    // 64 matching files: over the bound for limit 1 (63), within it for 2.
    let repo = FixtureRepo::new();
    for i in 0..64 {
        repo.write(&format!("src/m{i:02}.rs"), "// shared marker\n");
    }
    let sha = repo.commit_all("init");
    let u = universe_of(&repo);
    let entry = |limits: &str| {
        format!("[[pagination]]\nid = \"skim-G1\"\nquery = \"shared marker\"\nlimits = {limits}\n")
    };

    assert!(flagged(
        &check_against(&u, &sha, &entry("[1, 7]")),
        "skim-G1"
    ));
    assert!(check_against(&u, &sha, &entry("[2, 7]")).is_empty());
}

#[test]
fn a_word_list_pagination_needs_three_files_that_hold_every_word() {
    let (_repo, sha, u) = corpus();
    let entry = |query: &str| {
        format!("[[pagination]]\nid = \"skim-G1\"\nquery = \"{query}\"\nlimits = [2, 5]\n")
    };
    // Five files hold both words.
    assert!(check_against(&u, &sha, &entry("shared marker")).is_empty());
    // One file holds both words: the word view would add files with only one.
    let v = check_against(&u, &sha, &entry("check_staleness mentioned"));
    assert!(
        message_of(&v, "skim-G1").contains("the word view adds files"),
        "{v:?}"
    );
    // A single word has no word view: two files are enough.
    assert!(check_against(&u, &sha, &entry("check_staleness")).is_empty());
}

#[test]
fn a_zero_hit_entry_with_ground_truth_hits_is_a_violation() {
    let (_repo, sha, u) = corpus();
    let v = check_against(
        &u,
        &sha,
        "[[lexical]]\nid = \"skim-Z1\"\nquery = \"shared marker\"\ncategory = \"zero-hit\"\n\
         [[lexical]]\nid = \"skim-Z2\"\nquery = \"no_such_token_anywhere\"\ncategory = \"zero-hit\"\n",
    );
    assert!(flagged(&v, "skim-Z1"), "{v:?}");
    assert!(!flagged(&v, "skim-Z2"), "{v:?}");
}

#[test]
fn a_zero_hit_entry_reads_the_type_filter_too() {
    let (_repo, sha, u) = corpus();
    // `shared marker` is in .rs files only: under `-t md` nothing matches.
    let entry = |lang: &str| {
        format!(
            "[[lexical]]\nid = \"skim-Z1\"\nquery = \"shared marker\"\ncategory = \"zero-hit\"\n\
             lang = \"{lang}\"\n"
        )
    };
    assert!(check_against(&u, &sha, &entry("markdown")).is_empty());
    assert!(flagged(&check_against(&u, &sha, &entry("rust")), "skim-Z1"));
}

// --- whole-word integrity against the corpus universe ------------------------

/// `run` is a whole word on a.rs lines 1, 2 and 3 and b.rs line 1; lines
/// that hold it only inside a longer word are a.rs:4 and b.rs:2.
fn word_corpus() -> (FixtureRepo, String, TextUniverse) {
    let repo = FixtureRepo::new();
    repo.write(
        "src/a.rs",
        "pub fn run() {}\nfn helper() { run(); }\n// rerun is not run\nlet runner = 1;\n",
    );
    repo.write("src/b.rs", "pub fn run(x: u32) {}\nfn rerun_it() {}\n");
    let sha = repo.commit_all("init");
    let universe = universe_of(&repo);
    (repo, sha, universe)
}

fn word_violations(body: &str) -> Vec<IntegrityViolation> {
    let (_repo, sha, u) = word_corpus();
    check_against(&u, &sha, body)
}

#[test]
fn a_def_line_that_holds_the_query_only_inside_a_longer_word_is_a_violation() {
    let (_repo, sha, u) = word_corpus();
    let ident = |path: &str, line: u32| {
        format!(
            "[[ident]]\nid = \"skim-L01\"\nquery = \"run\"\n\
             def = {{ path = \"{path}\", line = {line} }}\norigin = \"seed\"\n"
        )
    };
    assert!(check_against(&u, &sha, &ident("src/a.rs", 1)).is_empty());
    for (path, line) in [("src/a.rs", 4), ("src/b.rs", 2)] {
        let v = check_against(&u, &sha, &ident(path, line));
        assert!(
            message_of(&v, "skim-L01").contains("does not contain"),
            "{path}:{line}: {v:?}"
        );
    }
}

#[test]
fn a_refs_site_set_equal_to_the_whole_word_lines_passes() {
    let v = word_violations(&refs(
        "skim-R1",
        "run",
        ("src/a.rs", 1),
        &[
            ("src/a.rs", 1, "def"),
            ("src/a.rs", 2, "call"),
            ("src/a.rs", 3, "comment"),
            ("src/b.rs", 1, "def"),
        ],
    ));
    assert!(v.is_empty(), "{v:?}");
}

#[test]
fn a_refs_site_set_that_misses_a_whole_word_line_is_a_violation() {
    let v = word_violations(&refs(
        "skim-R1",
        "run",
        ("src/a.rs", 1),
        &[
            ("src/a.rs", 1, "def"),
            ("src/a.rs", 2, "call"),
            ("src/b.rs", 1, "def"),
        ],
    ));
    let message = message_of(&v, "skim-R1");
    assert!(
        message.contains("missing") && message.contains("src/a.rs:3"),
        "{v:?}"
    );
}

#[test]
fn a_refs_site_on_a_line_without_the_whole_word_is_a_violation() {
    let v = word_violations(&refs(
        "skim-R1",
        "run",
        ("src/a.rs", 1),
        &[
            ("src/a.rs", 1, "def"),
            ("src/a.rs", 2, "call"),
            ("src/a.rs", 3, "comment"),
            ("src/a.rs", 4, "ref"),
            ("src/b.rs", 1, "def"),
        ],
    ));
    let message = message_of(&v, "skim-R1");
    assert!(
        message.contains("not a whole-word line") && message.contains("src/a.rs:4"),
        "{v:?}"
    );
}

#[test]
fn a_refs_site_outside_the_universe_is_a_violation() {
    let v = word_violations(&refs(
        "skim-R1",
        "run",
        ("src/a.rs", 1),
        &[
            ("src/a.rs", 1, "def"),
            ("src/a.rs", 2, "call"),
            ("src/a.rs", 3, "comment"),
            ("src/b.rs", 1, "def"),
            ("src/gone.rs", 7, "call"),
        ],
    ));
    assert!(message_of(&v, "skim-R1").contains("src/gone.rs:7"), "{v:?}");
}

#[test]
fn a_refs_def_that_is_not_a_whole_word_line_is_a_violation() {
    let v = word_violations(&refs(
        "skim-R1",
        "run",
        ("src/a.rs", 4),
        &[
            ("src/a.rs", 1, "def"),
            ("src/a.rs", 2, "call"),
            ("src/a.rs", 3, "comment"),
            ("src/a.rs", 4, "def"),
            ("src/b.rs", 1, "def"),
        ],
    ));
    let message = message_of(&v, "skim-R1");
    assert!(
        message.contains("src/a.rs:4 does not contain \"run\" as a whole word"),
        "{v:?}"
    );
}

#[test]
fn clash_defs_and_calls_must_be_whole_word_lines() {
    let ok = word_violations(&clash(
        "skim-D1",
        "run",
        &[("src/a.rs", 1), ("src/b.rs", 1)],
        &[("src/a.rs", 2, "0"), ("src/a.rs", 3, "\"unassigned\"")],
        "none",
    ));
    assert!(ok.is_empty(), "{ok:?}");

    let bad_call = word_violations(&clash(
        "skim-D2",
        "run",
        &[("src/a.rs", 1), ("src/b.rs", 1)],
        &[("src/a.rs", 4, "\"unassigned\"")],
        "none",
    ));
    assert!(
        message_of(&bad_call, "skim-D2").contains("src/a.rs:4"),
        "{bad_call:?}"
    );

    let bad_def = word_violations(&clash(
        "skim-D3",
        "run",
        &[("src/a.rs", 1), ("src/b.rs", 2)],
        &[("src/a.rs", 2, "\"unassigned\"")],
        "none",
    ));
    assert!(
        message_of(&bad_def, "skim-D3").contains("src/b.rs:2"),
        "{bad_def:?}"
    );

    let gone = word_violations(&clash(
        "skim-D4",
        "run",
        &[("src/a.rs", 1), ("src/gone.rs", 1)],
        &[("src/a.rs", 2, "\"unassigned\"")],
        "none",
    ));
    assert!(flagged(&gone, "skim-D4"), "{gone:?}");
}

#[test]
fn a_concept_first_path_must_be_in_the_universe() {
    let concept = |first: &str| {
        format!(
            "[[concept]]\nid = \"skim-C1\"\nquery = \"run\"\nrelevant = \"run\"\nfirst = {first}\n"
        )
    };
    assert!(word_violations(&concept("[\"src/a.rs\", \"src/b.rs\"]")).is_empty());
    let v = word_violations(&concept("[\"src/a.rs\", \"src/gone.rs\"]"));
    assert!(message_of(&v, "skim-C1").contains("src/gone.rs"), "{v:?}");
}

// --- the text universe and the stakes exit rule -------------------------------

/// `run` is a whole word in `src/a.rs:1`, `src/a.rs:2` (a call) and
/// `LICENSE:1`, a file with no language that the one text universe still
/// reads.
fn licensed_corpus() -> (FixtureRepo, String, TextUniverse) {
    let repo = FixtureRepo::new();
    repo.write("src/a.rs", "pub fn run() {}\nfn go() { run(); }\n");
    repo.write("LICENSE", "you may run it\n");
    let sha = repo.commit_all("init");
    let universe = universe_of(&repo);
    (repo, sha, universe)
}

#[test]
fn refs_sites_are_judged_against_every_tracked_text_file() {
    let (_repo, sha, u) = licensed_corpus();
    assert_eq!(u.len(), 2);
    let with_license = refs(
        "skim-R1",
        "run",
        ("src/a.rs", 1),
        &[
            ("src/a.rs", 1, "def"),
            ("src/a.rs", 2, "call"),
            ("LICENSE", 1, "text"),
        ],
    );
    assert!(check_against(&u, &sha, &with_license).is_empty());

    let without = refs(
        "skim-R2",
        "run",
        ("src/a.rs", 1),
        &[("src/a.rs", 1, "def"), ("src/a.rs", 2, "call")],
    );
    let v = check_against(&u, &sha, &without);
    assert!(
        message_of(&v, "skim-R2").contains("LICENSE:1"),
        "a file with no language still owes a site: {v:?}"
    );
}

#[test]
fn a_stakes_exit_must_agree_with_the_oracle_hit_count() {
    let (_repo, sha, u) = word_corpus();
    let check = |query: &str, flags: &str, exit: u8| {
        check_against(
            &u,
            &sha,
            &stakes("skim-S1", query, flags, "exit", Some(exit)),
        )
    };
    // No match: exit 1 holds, exit 0 does not.
    assert!(check("qzvx_absent", "[]", 1).is_empty());
    let v = check("qzvx_absent", "[]", 0);
    assert!(
        message_of(&v, "skim-S1").contains("finds no matching line"),
        "{v:?}"
    );
    // A match: exit 0 holds, exit 1 does not, and the message names a site.
    assert!(check("run", "[]", 0).is_empty());
    let v = check("run", "[]", 1);
    assert!(message_of(&v, "skim-S1").contains("src/a.rs:1"), "{v:?}");
    // A query the search accepts cannot claim exit 2, with or without a universe.
    let v = check("run", "[]", 2);
    assert!(
        message_of(&v, "skim-S1").contains("the oracle reads a valid query"),
        "{v:?}"
    );
}

#[test]
fn a_stakes_exit_is_not_judged_without_a_text_universe() {
    let src = format!(
        "{}{}",
        header(),
        stakes("skim-S1", "run", "[]", "exit", Some(1))
    );
    assert!(check_integrity(&golden(&src), &ctx(&[])).is_empty());
}

#[test]
fn stakes_flags_change_what_the_oracle_counts_as_a_match() {
    let (_repo, sha, u) = word_corpus();
    let exit_of = |query: &str, flags: &str| -> u8 {
        // The exit that holds is the one the check lets through.
        let holds = |exit: u8| {
            check_against(
                &u,
                &sha,
                &stakes("skim-S1", query, flags, "exit", Some(exit)),
            )
            .is_empty()
        };
        match (holds(0), holds(1)) {
            (true, false) => 0,
            (false, true) => 1,
            other => unreachable!("exactly one exit must hold: {other:?}"),
        }
    };
    // Case: the default is exact, `-i` ignores case, `-S` ignores it only
    // for an all-lowercase query, and `-i` wins over `-S`.
    assert_eq!(exit_of("RUN", "[]"), 1);
    assert_eq!(exit_of("RUN", "[\"-i\"]"), 0);
    assert_eq!(exit_of("run", "[\"-S\"]"), 0);
    assert_eq!(exit_of("RUN", "[\"-S\"]"), 1);
    assert_eq!(exit_of("RUN", "[\"-i\", \"-S\"]"), 0);
    // Whole word: `ru` is inside `run` but is no word of its own.
    assert_eq!(exit_of("ru", "[]"), 0);
    assert_eq!(exit_of("ru", "[\"-w\"]"), 1);
    // Regex: `-E` reads the query as a pattern, `-F` and the default as text.
    assert_eq!(exit_of("ru[n]", "[]"), 1);
    assert_eq!(exit_of("ru[n]", "[\"-F\"]"), 1);
    assert_eq!(exit_of("ru[n]", "[\"-E\"]"), 0);
    // Display flags change nothing.
    assert_eq!(exit_of("run", "[\"-n\", \"-C\", \"2\"]"), 0);
}

#[test]
fn stakes_file_filters_decide_which_files_the_hit_count_reads() {
    let repo = FixtureRepo::new();
    repo.write("src/a.rs", "pub fn run() {}\n");
    repo.write("docs/guide.md", "run it\n");
    let sha = repo.commit_all("init");
    let u = universe_of(&repo);
    let holds = |flags: &str, exit: u8| {
        check_against(
            &u,
            &sha,
            &stakes("skim-S1", "run", flags, "exit", Some(exit)),
        )
        .is_empty()
    };
    // Each filter that keeps a file with a match leaves exit 0 standing.
    for flags in [
        "[\"-t\", \"rust\"]",
        "[\"-t\", \"md\"]",
        "[\"-g\", \"*.md\"]",
        "[\"-g\", \"!docs/\"]",
    ] {
        assert!(holds(flags, 0), "{flags} keeps a file with a match");
        assert!(!holds(flags, 1), "{flags} keeps a file with a match");
    }
    // Each filter that keeps no file with a match makes exit 1 stand.
    for flags in [
        "[\"-t\", \"py\"]",
        "[\"-g\", \"*.toml\"]",
        "[\"-g\", \"*.rs\", \"-t\", \"md\"]",
        "[\"-g\", \"*.md\", \"-g\", \"!docs/\"]",
    ] {
        assert!(holds(flags, 1), "{flags} keeps no file with a match");
        assert!(!holds(flags, 0), "{flags} keeps no file with a match");
    }
}

#[test]
fn contradicting_or_unreadable_stakes_queries_are_violations() {
    let (_repo, sha, u) = word_corpus();
    for (query, flags) in [("run", "[\"-F\", \"-E\"]"), ("ru(n", "[\"-E\"]")] {
        let v = check_against(&u, &sha, &stakes("skim-S1", query, flags, "lines", None));
        assert!(flagged(&v, "skim-S1"), "{query} {flags}: {v:?}");
    }
}

/// A `[[stakes]]` recall entry (`lines`, `files` or `counts`) whose query the
/// oracle finds on no line has an empty truth, so any output would pass its
/// checks. Each expectation is one rule.
fn stakes_recall_violation(expect: &str, flags: &str, query: &str) -> Vec<IntegrityViolation> {
    let (_repo, sha, u) = word_corpus();
    check_against(&u, &sha, &stakes("skim-S1", query, flags, expect, None))
}

#[test]
fn a_stakes_lines_entry_with_no_matching_line_is_a_violation() {
    assert!(stakes_recall_violation("lines", "[]", "run").is_empty());
    let v = stakes_recall_violation("lines", "[]", "qzvx_absent");
    assert!(
        message_of(&v, "skim-S1").contains("finds no matching line"),
        "{v:?}"
    );
}

#[test]
fn a_stakes_files_entry_with_no_matching_file_is_a_violation() {
    assert!(stakes_recall_violation("files", "[\"-l\"]", "run").is_empty());
    let v = stakes_recall_violation("files", "[\"-l\"]", "qzvx_absent");
    assert!(
        message_of(&v, "skim-S1").contains("finds no matching line"),
        "{v:?}"
    );
}

#[test]
fn a_stakes_counts_entry_with_no_matching_line_is_a_violation() {
    assert!(stakes_recall_violation("counts", "[\"-c\"]", "run").is_empty());
    let v = stakes_recall_violation("counts", "[\"-c\"]", "qzvx_absent");
    assert!(
        message_of(&v, "skim-S1").contains("finds no matching line"),
        "{v:?}"
    );
}

#[test]
fn a_stakes_recall_entry_whose_file_filter_keeps_no_match_is_a_violation() {
    // `run` is in .rs files only: under `-t py` the oracle finds no line.
    let v = stakes_recall_violation("lines", "[\"-t\", \"py\"]", "run");
    assert!(
        message_of(&v, "skim-S1").contains("finds no matching line"),
        "{v:?}"
    );
}

// --- loading and hashing --------------------------------------------------------

#[test]
fn load_golden_hashes_the_raw_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("skim.toml");
    std::fs::write(&path, DESIGN_EXAMPLE).unwrap();

    let loaded = load_golden(&path).unwrap();
    assert_eq!(loaded.file, golden(DESIGN_EXAMPLE));
    assert_eq!(loaded.sha256, hex_sha256(DESIGN_EXAMPLE.as_bytes()));
    assert_eq!(loaded.sha256.len(), 64);

    std::fs::write(&path, format!("{DESIGN_EXAMPLE}# trailing comment\n")).unwrap();
    let edited = load_golden(&path).unwrap();
    assert_eq!(edited.file, loaded.file, "same entries");
    assert_ne!(
        edited.sha256, loaded.sha256,
        "any byte edit changes the hash"
    );
}

#[test]
fn load_golden_reports_the_path_on_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("broken.toml");
    std::fs::write(&path, "corpus = ").unwrap();
    let err = load_golden(&path).unwrap_err();
    assert!(format!("{err:#}").contains("broken.toml"), "{err:#}");
}

#[test]
fn golden_set_hash_is_order_independent_and_content_sensitive() {
    let a = LoadedGolden {
        file: golden(DESIGN_EXAMPLE),
        sha256: "a".repeat(64),
    };
    let mut b = a.clone();
    b.file.corpus = "zod".to_string();
    b.sha256 = "b".repeat(64);

    let ab = golden_set_sha256([&a, &b]);
    assert_eq!(ab, golden_set_sha256([&b, &a]));
    let mut b2 = b.clone();
    b2.sha256 = "c".repeat(64);
    assert_ne!(ab, golden_set_sha256([&a, &b2]));
    assert_eq!(ab.len(), 64);
}
