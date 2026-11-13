//! Tests of the flag parser of `skim search`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use super::*;

/// Every flag the query form accepts, in the spelling the help text shows.
const KNOWN_FLAGS: [&str; 30] = [
    "-i",
    "--ignore-case",
    "-S",
    "--smart-case",
    "-w",
    "--word-regexp",
    "-F",
    "--fixed-strings",
    "-E",
    "--regex",
    "-g",
    "--glob",
    "-t",
    "--type",
    "-l",
    "--files-with-matches",
    "-c",
    "--count",
    "-C",
    "--context",
    "-n",
    "--line-number",
    "--all",
    "--full",
    "--json",
    "--limit",
    "--offset",
    "--root",
    "-h",
    "--help",
];

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

fn query(parts: &[&str]) -> QueryFlags {
    match parse_flags(&args(parts)) {
        Ok(SearchAction::Query(flags)) => *flags,
        other => panic!("expected a query for {parts:?}, got {other:?}"),
    }
}

fn error(parts: &[&str]) -> String {
    match parse_flags(&args(parts)) {
        Err(e) => e.message().to_owned(),
        other => panic!("expected a usage error for {parts:?}, got {other:?}"),
    }
}

fn pattern_of(flags: &QueryFlags) -> (&str, bool) {
    match &flags.shape {
        QueryShape::Pattern { text, identifier } => (text.as_str(), *identifier),
        QueryShape::Words(w) => panic!("expected a pattern, got words {w:?}"),
    }
}

// ============================================================================
// Help and the help text
// ============================================================================

#[test]
fn no_arguments_print_help() {
    assert_eq!(parse_flags(&[]), Ok(SearchAction::Help));
}

#[test]
fn help_flags_print_help() {
    assert_eq!(parse_flags(&args(&["-h"])), Ok(SearchAction::Help));
    assert_eq!(parse_flags(&args(&["--help"])), Ok(SearchAction::Help));
    assert_eq!(
        parse_flags(&args(&["foo", "--help"])),
        Ok(SearchAction::Help)
    );
}

#[test]
fn help_is_not_taken_as_a_flag_value() {
    for flag in ["--limit", "--root", "-g", "-t", "-C"] {
        let message = error(&[flag, "--help"]);
        assert!(message.contains("needs a value"), "{flag}: {message}");
    }
}

#[test]
fn a_bad_flag_before_help_is_an_error_not_help() {
    let message = error(&["--bogus", "--help"]);
    assert!(message.contains("unknown flag"), "{message}");
    let message = error(&["--limit", "0", "--help"]);
    assert!(message.contains("--limit"), "{message}");
}

#[test]
fn the_help_text_documents_every_flag() {
    let removed = REMOVED_FLAGS.iter().map(|(flag, _)| flag);
    for flag in KNOWN_FLAGS.iter().chain(removed) {
        assert!(
            SEARCH_HELP_TEXT.contains(flag),
            "the help text does not mention {flag}"
        );
    }
    assert!(SEARCH_HELP_TEXT.contains("-j"), "the removed -j is listed");
}

#[test]
fn the_help_text_states_the_rule_for_hidden_and_secret_files() {
    assert!(SEARCH_HELP_TEXT.contains("hidden or"));
    assert!(SEARCH_HELP_TEXT.contains("may hold secrets"));
}

#[test]
fn the_help_text_states_that_the_rule_for_hidden_and_secret_files_can_change() {
    assert!(SEARCH_HELP_TEXT.contains("A later release can change this default"));
    assert!(SEARCH_HELP_TEXT.contains("One function and one constant"));
}

#[test]
fn the_help_text_states_that_list_modes_ignore_limit_and_offset() {
    assert!(SEARCH_HELP_TEXT.contains("--limit and --offset do"));
}

// ============================================================================
// Unknown, refused and removed flags
// ============================================================================

#[test]
fn flags_outside_the_closed_set_exit_with_the_unknown_flag_message() {
    let refused = [
        "--pre",
        "--pre-glob",
        "--hostname-bin",
        "-z",
        "--search-zip",
        "-P",
        "--pcre2",
        "--engine",
        "-L",
        "--follow",
        "--hidden",
        "--no-ignore",
        "--no-ignore-vcs",
        "-u",
        "-r",
        "--replace",
        "-A",
        "-B",
        "-v",
        "-o",
        "-m",
        "-a",
        "--text",
        "-f",
        "--file",
        "--threads",
        "--sort",
        "--type-add",
        "--ignore-file",
        "--files",
    ];
    for flag in refused {
        let message = error(&[flag, "needle"]);
        assert_eq!(
            message,
            format!("unknown flag {flag:?}; skim search --help lists the flags"),
            "{flag}"
        );
    }
}

#[test]
fn a_refused_flag_with_an_attached_value_is_unknown() {
    let message = error(&["--pre=/tmp/script", "needle"]);
    assert!(message.starts_with("unknown flag"), "{message}");
}

#[test]
fn a_joined_group_with_an_unknown_letter_names_the_group() {
    let message = error(&["-iz", "needle"]);
    assert_eq!(
        message,
        "unknown flag \"-iz\"; skim search --help lists the flags"
    );
    let message = error(&["-iwzF", "needle"]);
    assert!(message.contains("\"-iwzF\""), "{message}");
}

#[test]
fn removed_flag_message_names_flag_and_replacement() {
    let cases = [
        ("--ast", "use ast-grep for structural search"),
        ("--hot", "use skim heatmap for churn and fix density"),
        ("--cold", "use skim heatmap for churn and fix density"),
        ("--risky", "use skim heatmap for churn and fix density"),
        (
            "--blast-radius",
            "use skim heatmap, or git log --name-only -- FILE, for co-change",
        ),
        ("--weights", "there is no fused ranking"),
        ("--phrase", "use -F 'exact words' or -E for a regex"),
        ("--near", "use -F 'exact words' or -E for a regex"),
        (
            "--build",
            "skim search has no index; each query reads the files",
        ),
        (
            "--rebuild",
            "skim search has no index; each query reads the files",
        ),
        (
            "--update",
            "skim search has no index; each query reads the files",
        ),
        (
            "--stats",
            "skim search has no index; each query reads the files",
        ),
        (
            "--install-hooks",
            "skim init and skim init --uninstall remove old search hooks",
        ),
        (
            "--remove-hooks",
            "skim init and skim init --uninstall remove old search hooks",
        ),
        ("--lang", "use -t TYPE"),
    ];
    assert_eq!(
        cases.len(),
        REMOVED_FLAGS.len(),
        "every removed flag is checked"
    );
    for (flag, advice) in cases {
        assert_eq!(
            removed_flag_message(flag),
            Some(format!("{flag} was removed in 3.0.0; {advice}"))
        );
    }
    assert_eq!(
        removed_flag_message("-j"),
        Some("-j was removed in 3.0.0; use --json".to_owned())
    );
}

#[test]
fn removed_flag_message_does_not_echo_an_attached_value() {
    let message = removed_flag_message("--ast=\x1b[2Jevil").unwrap();
    assert!(!message.contains('\x1b'), "{message:?}");
    assert!(!message.contains("evil"), "{message:?}");
}

#[test]
fn removed_flag_message_ignores_lookalikes() {
    for arg in [
        "--astx",
        "--ast-grep",
        "--hotspot",
        "--language",
        "-jx",
        "ast",
    ] {
        assert_eq!(removed_flag_message(arg), None, "{arg}");
    }
}

#[test]
fn a_removed_flag_ends_parsing_before_a_later_error() {
    assert_eq!(
        parse_flags(&args(&["--ast", "--limit", "0", "--bogus"])),
        Ok(SearchAction::Removed(
            "--ast was removed in 3.0.0; use ast-grep for structural search".to_owned()
        ))
    );
}

#[test]
fn a_removed_flag_after_the_pattern_is_refused() {
    assert!(matches!(
        parse_flags(&args(&["needle", "--hot"])),
        Ok(SearchAction::Removed(_))
    ));
}

#[test]
fn a_removed_flag_name_after_double_dash_is_the_pattern() {
    let flags = query(&["--", "--ast"]);
    assert_eq!(pattern_of(&flags), ("--ast", false));
}

// ============================================================================
// Flags, values, joined groups
// ============================================================================

#[test]
fn short_boolean_flags_can_join() {
    let flags = query(&["-iwF", "needle"]);
    assert_eq!(flags.case, CaseMode::Insensitive);
    assert!(flags.word);
    assert!(!flags.regex);
    let flags = query(&["-lw", "needle"]);
    assert_eq!(flags.list, ListMode::Files);
    assert!(flags.word);
}

#[test]
fn ignore_case_wins_over_smart_case() {
    assert_eq!(query(&["-S", "x"]).case, CaseMode::Smart);
    assert_eq!(query(&["-Si", "x"]).case, CaseMode::Insensitive);
    assert_eq!(query(&["-iS", "x"]).case, CaseMode::Insensitive);
    assert_eq!(query(&["x"]).case, CaseMode::Sensitive);
}

#[test]
fn value_flags_take_the_next_argument_or_an_attached_value() {
    assert_eq!(query(&["-C3", "x"]).context, 3);
    assert_eq!(query(&["-C", "3", "x"]).context, 3);
    assert_eq!(query(&["--context=3", "x"]).context, 3);
    assert_eq!(query(&["--context", "3", "x"]).context, 3);
    assert_eq!(query(&["-iC3", "x"]).context, 3);
    assert_eq!(query(&["-iC", "3", "x"]).context, 3);
    assert_eq!(query(&["-g", "*.rs", "x"]).globs, vec!["*.rs"]);
    assert_eq!(query(&["-g*.rs", "x"]).globs, vec!["*.rs"]);
    assert_eq!(query(&["--glob=*.rs", "x"]).globs, vec!["*.rs"]);
    assert_eq!(query(&["-t", "rust", "x"]).types, vec!["rust"]);
    assert_eq!(query(&["--type=rust", "x"]).types, vec!["rust"]);
    assert_eq!(
        query(&["--root", "dir", "x"]).root,
        Some(PathBuf::from("dir"))
    );
    assert_eq!(query(&["--root=dir", "x"]).root, Some(PathBuf::from("dir")));
}

#[test]
fn numbers_out_of_range_exit_2() {
    for bad in [
        &["-C", "101", "x"][..],
        &["-C", "-1", "x"],
        &["-C", "abc", "x"],
        &["-C=3", "x"],
        &["--context=", "x"],
        &["--limit", "0", "x"],
        &["--limit", "1000001", "x"],
        &["--limit", "ten", "x"],
        &["--offset", "4294967296", "x"],
        &["--offset", "-1", "x"],
        &["--offset=", "x"],
    ] {
        let message = error(bad);
        assert!(
            message.contains("needs a whole number"),
            "{bad:?}: {message}"
        );
    }
}

#[test]
fn numbers_at_the_edges_are_accepted() {
    assert_eq!(query(&["-C", "0", "x"]).context, 0);
    assert_eq!(query(&["-C", "100", "x"]).context, 100);
    assert_eq!(query(&["--limit", "1", "x"]).limit, Some(1));
    assert_eq!(query(&["--limit", "1000000", "x"]).limit, Some(1_000_000));
    assert_eq!(query(&["--offset", "0", "x"]).offset, 0);
    assert_eq!(
        query(&["--offset", "4294967295", "x"]).offset,
        4_294_967_295
    );
}

#[test]
fn a_value_flag_without_a_value_is_an_error() {
    for flag in ["-g", "-t", "-C", "--limit", "--offset", "--root", "--glob"] {
        let message = error(&["x", flag]);
        assert!(message.contains("needs a value"), "{flag}: {message}");
    }
}

#[test]
fn an_empty_filter_or_root_value_is_an_error() {
    assert!(error(&["-g", "", "x"]).contains("not empty"));
    assert!(error(&["-t", "", "x"]).contains("not empty"));
    assert!(error(&["--root", "", "x"]).contains("not empty"));
}

#[test]
fn a_switch_with_an_attached_value_is_an_error() {
    let message = error(&["--json=1", "x"]);
    assert!(message.contains("takes no value"), "{message}");
}

#[test]
fn line_number_flag_changes_nothing() {
    let with = query(&["-n", "x"]);
    let without = query(&["x"]);
    assert_eq!(with, without);
}

#[test]
fn filters_are_bounded() {
    let mut parts: Vec<String> = Vec::new();
    for n in 0..MAX_FILTERS {
        parts.push("-g".to_owned());
        parts.push(format!("g{n}"));
    }
    parts.push("x".to_owned());
    assert!(matches!(parse_flags(&parts), Ok(SearchAction::Query(_))));

    let mut over = parts.clone();
    over.insert(0, "rust".to_owned());
    over.insert(0, "-t".to_owned());
    match parse_flags(&over) {
        Err(e) => assert_eq!(e.message(), "at most 64 -g/-t filters"),
        other => panic!("expected the filter bound to fire, got {other:?}"),
    }
}

// ============================================================================
// Conflicts
// ============================================================================

#[test]
fn conflicting_flag_pairs_exit_2() {
    let cases: [(&[&str], &str); 11] = [
        (&["-l", "-c", "x"], "-l and -c cannot be combined"),
        (&["-lc", "x"], "-l and -c cannot be combined"),
        (&["-l", "-C", "1", "x"], "-l cannot be combined with -C"),
        (&["-c", "-C", "0", "x"], "-c cannot be combined with -C"),
        (&["-l", "--all", "x"], "-l cannot be combined with --all"),
        (&["-c", "--full", "x"], "-c cannot be combined with --full"),
        (&["-l", "--json", "x"], "-l cannot be combined with --json"),
        (&["-c", "--json", "x"], "-c cannot be combined with --json"),
        (&["-F", "-E", "x"], "-F and -E cannot be combined"),
        (&["-FE", "x"], "-F and -E cannot be combined"),
        (
            &["-l", "two words"],
            "-l and -c take one pattern, not a list of words; \
             use -F for exact words or -E for a regex",
        ),
    ];
    for (parts, expected) in cases {
        assert_eq!(error(parts), expected, "{parts:?}");
    }
}

#[test]
fn list_modes_accept_limit_and_offset() {
    let flags = query(&["-l", "--limit", "3", "--offset", "2", "x"]);
    assert_eq!(flags.list, ListMode::Files);
    assert_eq!(flags.limit, Some(3));
    assert_eq!(flags.offset, 2);
}

// ============================================================================
// The pattern
// ============================================================================

#[test]
fn a_bare_identifier_gives_the_labelled_view() {
    for ident in ["launchWorker", "_x", "$el", "run_parsed_command", "Größe"] {
        let flags = query(&[ident]);
        assert_eq!(pattern_of(&flags), (ident, true), "{ident}");
    }
}

#[test]
fn other_single_arguments_are_not_bare_identifiers() {
    for text in ["foo(", "a.b", "1abc", "foo-bar", "a::b"] {
        let flags = query(&[text]);
        assert_eq!(pattern_of(&flags), (text, false), "{text}");
    }
}

#[test]
fn an_explicit_pattern_is_never_a_bare_identifier() {
    for flag in ["-E", "-F"] {
        let flags = query(&[flag, "launchWorker"]);
        assert_eq!(pattern_of(&flags), ("launchWorker", false), "{flag}");
    }
    assert!(query(&["-E", "launchWorker"]).regex);
}

#[test]
fn whole_word_alone_keeps_a_bare_identifier() {
    let flags = query(&["-w", "launchWorker"]);
    assert_eq!(pattern_of(&flags), ("launchWorker", true));
    assert!(flags.word);
}

#[test]
fn words_are_split_on_ascii_whitespace_and_deduplicated() {
    let flags = query(&["alpha  beta\tgamma\nalpha"]);
    assert_eq!(
        flags.shape,
        QueryShape::Words(vec!["alpha".into(), "beta".into(), "gamma".into()])
    );
}

#[test]
fn one_word_after_the_split_is_a_pattern() {
    let flags = query(&["  launchWorker "]);
    assert_eq!(pattern_of(&flags), ("launchWorker", true));
    let flags = query(&["foo foo"]);
    assert_eq!(pattern_of(&flags), ("foo", true));
}

#[test]
fn explicit_flags_keep_whitespace_in_the_pattern() {
    let flags = query(&["-F", "two words"]);
    assert_eq!(pattern_of(&flags), ("two words", false));
    let flags = query(&["-E", "two words"]);
    assert_eq!(pattern_of(&flags), ("two words", false));
}

#[test]
fn a_non_ascii_space_does_not_split() {
    let flags = query(&["a\u{00a0}b"]);
    assert_eq!(pattern_of(&flags), ("a\u{00a0}b", false));
}

#[test]
fn the_word_count_is_bounded() {
    let sixteen: Vec<String> = (0..MAX_WORDS).map(|n| format!("w{n}")).collect();
    let flags = query(&[sixteen.join(" ").as_str()]);
    assert!(matches!(flags.shape, QueryShape::Words(ref w) if w.len() == MAX_WORDS));

    let seventeen: Vec<String> = (0..=MAX_WORDS).map(|n| format!("w{n}")).collect();
    assert_eq!(
        error(&[seventeen.join(" ").as_str()]),
        "at most 16 words; use -E for a longer pattern"
    );
}

#[test]
fn repeated_words_do_not_count_twice() {
    let many = vec!["same"; 100].join(" ");
    assert_eq!(pattern_of(&query(&[many.as_str()])), ("same", true));
}

#[test]
fn empty_patterns_exit_2() {
    assert_eq!(error(&[""]), "the pattern is empty");
    assert_eq!(error(&["-E", ""]), "the pattern is empty");
    assert_eq!(error(&["   "]), "the pattern is empty");
}

#[test]
fn a_missing_pattern_exits_2() {
    let message = error(&["--json"]);
    assert!(message.starts_with("no pattern given"), "{message}");
    let message = error(&["-i", "--limit", "3"]);
    assert!(message.starts_with("no pattern given"), "{message}");
}

#[test]
fn a_second_pattern_exits_2() {
    assert_eq!(
        error(&["foo", "bar"]),
        "one pattern only; quote words as one argument; use -g to filter paths"
    );
}

#[test]
fn a_number_after_the_old_limit_flag_gets_its_own_message() {
    assert_eq!(
        error(&["foo", "-n", "5"]),
        "-n no longer sets the limit; use --limit N"
    );
    // Only a number that follows -n: a plain second word keeps the general message.
    assert!(error(&["foo", "5"]).starts_with("one pattern only"));
}

#[test]
fn double_dash_makes_the_next_argument_the_pattern() {
    assert_eq!(pattern_of(&query(&["--", "-Werror"])), ("-Werror", false));
    assert_eq!(pattern_of(&query(&["-i", "--", "->"])), ("->", false));
    assert_eq!(pattern_of(&query(&["--", "--"])), ("--", false));
}

#[test]
fn a_second_pattern_after_double_dash_exits_2() {
    assert!(error(&["--", "a", "b"]).starts_with("one pattern only"));
}

#[test]
fn a_bare_dash_is_a_pattern() {
    assert_eq!(pattern_of(&query(&["-"])), ("-", false));
}

// ============================================================================
// The query flags together
// ============================================================================

#[test]
fn defaults_of_a_plain_query() {
    let flags = query(&["needle"]);
    assert_eq!(flags.case, CaseMode::Sensitive);
    assert_eq!(flags.list, ListMode::Lines);
    assert_eq!(flags.context, 0);
    assert_eq!(flags.limit, None);
    assert_eq!(flags.offset, 0);
    assert!(!flags.all && !flags.full && !flags.json && !flags.word && !flags.regex);
    assert!(flags.globs.is_empty() && flags.types.is_empty());
    assert_eq!(flags.root, None);
}

#[test]
fn flags_may_follow_the_pattern() {
    let flags = query(&["needle", "--json", "--limit", "5", "-i"]);
    assert!(flags.json);
    assert_eq!(flags.limit, Some(5));
    assert_eq!(flags.case, CaseMode::Insensitive);
}

#[test]
fn counts_mode_is_read() {
    assert_eq!(query(&["-c", "x"]).list, ListMode::Counts);
    assert_eq!(query(&["--count", "x"]).list, ListMode::Counts);
    assert_eq!(query(&["--files-with-matches", "x"]).list, ListMode::Files);
}

// ============================================================================
// The commands of the old index
// ============================================================================

#[test]
fn an_old_index_command_is_refused_with_its_replacement() {
    for flag in [
        "--build",
        "--rebuild",
        "--update",
        "--stats",
        "--install-hooks",
        "--remove-hooks",
    ] {
        for extra in [&[][..], &["--root", "dir", "--json"][..], &["needle"][..]] {
            let mut parts = vec![flag];
            parts.extend_from_slice(extra);
            match parse_flags(&args(&parts)) {
                Ok(SearchAction::Removed(message)) => {
                    assert!(
                        message.starts_with(&format!("{flag} was removed in 3.0.0; ")),
                        "{parts:?}: {message}"
                    );
                }
                other => panic!("{parts:?}: expected a refusal, got {other:?}"),
            }
        }
    }
}

#[test]
fn an_old_index_command_name_after_double_dash_is_the_pattern() {
    let flags = query(&["--", "--rebuild"]);
    assert_eq!(pattern_of(&flags), ("--rebuild", false));
}

// ============================================================================
// Errors in JSON mode
// ============================================================================

#[test]
fn wants_json_reads_the_flag_before_double_dash() {
    assert!(wants_json(&args(&["--limit", "0", "--json"])));
    assert!(wants_json(&args(&["--json", "--bogus"])));
    assert!(!wants_json(&args(&["--", "--json"])));
    assert!(!wants_json(&args(&["x"])));
}

#[test]
fn the_usage_error_text_has_the_program_prefix() {
    let e = parse_flags(&args(&["--bogus"])).unwrap_err();
    assert!(e.to_string().starts_with("skim search: unknown flag"));
    assert!(!e.message().starts_with("skim search:"));
}

// ============================================================================
// The word "index" is an ordinary pattern
// ============================================================================

#[test]
fn the_bareword_index_is_a_pattern_not_a_build() {
    let flags = query(&["index"]);
    assert_eq!(pattern_of(&flags), ("index", true));
}

#[test]
fn index_followed_by_help_prints_the_help() {
    assert_eq!(
        parse_flags(&args(&["index", "--help"])),
        Ok(SearchAction::Help)
    );
}

#[test]
fn every_listed_flag_is_accepted_by_the_parser() {
    for flag in KNOWN_FLAGS {
        let parsed = match flag {
            "-h" | "--help" => parse_flags(&args(&[flag])),
            "-g" | "--glob" | "-t" | "--type" | "--root" => parse_flags(&args(&[flag, "v", "x"])),
            "-C" | "--context" | "--limit" | "--offset" => parse_flags(&args(&[flag, "1", "x"])),
            // `-c` and `-l` conflict with other output flags; the flag alone is enough.
            _ => parse_flags(&args(&[flag, "x"])),
        };
        assert!(
            parsed.is_ok(),
            "{flag} is listed but the parser refuses it: {parsed:?}"
        );
    }
}
