//! Tests for the line matcher: literal and regex reading, the case rules, the
//! whole-word rule and the size limits.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

fn spec(pattern: &str, kind: PatternKind, case: CaseMode, word: bool) -> MatcherSpec<'_> {
    MatcherSpec {
        pattern,
        kind,
        case,
        word,
    }
}

fn literal(pattern: &str) -> Matcher {
    Matcher::new(spec(
        pattern,
        PatternKind::Literal,
        CaseMode::Sensitive,
        false,
    ))
    .unwrap()
}

fn whole_word(pattern: &str) -> Matcher {
    Matcher::new(spec(
        pattern,
        PatternKind::Literal,
        CaseMode::Sensitive,
        true,
    ))
    .unwrap()
}

fn regex_of(pattern: &str) -> Matcher {
    Matcher::new(spec(
        pattern,
        PatternKind::Regex,
        CaseMode::Sensitive,
        false,
    ))
    .unwrap()
}

fn found(matcher: &Matcher, line: &str) -> Vec<Range<usize>> {
    matcher.matches(line).collect()
}

#[test]
fn a_literal_matches_itself_and_its_metacharacters_are_text() {
    let m = literal("foo(");
    assert!(m.is_match("call foo(1)"));
    assert!(!m.is_match("call foo 1"));
    assert_eq!(found(&literal("a.b"), "a.b axb"), vec![0..3]);
}

#[test]
fn every_match_on_a_line_is_found() {
    assert_eq!(found(&literal("ab"), "ab ab xab"), vec![0..2, 3..5, 7..9]);
}

#[test]
fn a_regex_reads_its_pattern() {
    let m = regex_of(r"fo+\(");
    assert!(m.is_match("call foooo(1)"));
    assert_eq!(found(&m, "fo( foo("), vec![0..3, 4..8]);
}

#[test]
fn an_empty_literal_is_an_error_and_an_empty_regex_matches_every_line() {
    let err = Matcher::new(spec("", PatternKind::Literal, CaseMode::Sensitive, false));
    assert_eq!(err.unwrap_err(), MatcherError::EmptyPattern);

    let m = regex_of("");
    assert!(m.is_match(""));
    assert!(m.is_match("anything"));
    // One empty match at each character boundary, and one at the end.
    assert_eq!(found(&m, "ab"), vec![0..0, 1..1, 2..2]);
}

#[test]
fn case_is_exact_unless_the_mode_says_otherwise() {
    let sensitive = literal("Foo");
    assert!(sensitive.is_match("a Foo b"));
    assert!(!sensitive.is_match("a foo b"));
    assert!(!sensitive.is_case_insensitive());

    let insensitive = Matcher::new(spec(
        "Foo",
        PatternKind::Literal,
        CaseMode::Insensitive,
        false,
    ))
    .unwrap();
    assert!(insensitive.is_match("a fOO b"));
    assert!(insensitive.is_case_insensitive());
}

#[test]
fn smart_case_ignores_case_only_when_the_pattern_has_no_uppercase() {
    let lower = Matcher::new(spec("foo", PatternKind::Literal, CaseMode::Smart, false)).unwrap();
    assert!(lower.is_match("FOO"));
    assert!(lower.is_case_insensitive());

    let mixed = Matcher::new(spec("Foo", PatternKind::Literal, CaseMode::Smart, false)).unwrap();
    assert!(!mixed.is_match("foo"));
    assert!(mixed.is_match("Foo"));
    assert!(!mixed.is_case_insensitive());
}

#[test]
fn a_whole_word_match_needs_a_non_word_character_on_both_sides() {
    let m = whole_word("foo");
    assert!(m.is_match("foo"));
    assert!(m.is_match("(foo)"));
    assert!(m.is_match("x.foo.y"));
    assert!(!m.is_match("foobar"));
    assert!(!m.is_match("barfoo"));
    assert!(!m.is_match("foo_bar"));
    assert!(!m.is_match("foo1"));
    assert!(!m.is_match("é_foo"));
}

#[test]
fn a_rejected_whole_word_match_does_not_hide_a_later_one() {
    let m = whole_word("foo");
    assert_eq!(found(&m, "foofoo foo"), vec![7..10]);
    // Overlapping candidates are all tried: the second `aa` starts one past the
    // first and is bounded.
    assert_eq!(found(&whole_word("aa"), "aaa aa"), vec![4..6]);
    // A rejected candidate may hold a valid one: the search goes on one
    // character past its start, not past its end.
    assert_eq!(found(&whole_word("a a"), "xa a a"), vec![3..6]);
}

#[test]
fn a_whole_word_boundary_reads_non_ascii_letters_and_digits_as_word_characters() {
    let m = whole_word("cafe");
    assert!(!m.is_match("cafeé"));
    assert!(
        !m.is_match("é cafe٣"),
        "an Arabic-Indic digit is a word character"
    );
    assert!(m.is_match("café cafe!"));
}

fn word_matcher(pattern: &str, kind: PatternKind, case: CaseMode) -> Matcher {
    Matcher::new(spec(pattern, kind, case, true)).unwrap()
}

/// Assert each `(line, expected)` pair against `matcher.is_match`.
fn assert_lines(matcher: &Matcher, table: &[(&str, bool)]) {
    for &(line, expected) in table {
        assert_eq!(matcher.is_match(line), expected, "line {line:?}");
    }
}

#[test]
fn a_whole_word_alternation_may_take_its_longer_branch() {
    // Frozen from `rg -nw 'foo|foobar'` over these lines: `foo` fails the word
    // rule at the start of `foobar`, and the longer branch is still tried.
    let m = word_matcher("foo|foobar", PatternKind::Regex, CaseMode::Sensitive);
    assert_lines(
        &m,
        &[
            ("foobar", true),
            ("foo", true),
            ("foo bar", true),
            ("xfoo foo_", false),
        ],
    );
    assert_eq!(found(&m, "foobar"), vec![0..6]);
    assert_eq!(found(&m, "foo foobar"), vec![0..3, 4..10]);
}

#[test]
fn a_literal_with_a_bar_is_text_under_the_whole_word_rule() {
    // Frozen from `rg -nwF 'foo|foobar'` over `foobar`: no match.
    let m = word_matcher("foo|foobar", PatternKind::Literal, CaseMode::Sensitive);
    assert!(!m.is_match("foobar"));
    assert!(m.is_match("x foo|foobar y"));
}

#[test]
fn a_pattern_that_starts_with_a_non_word_character_reads_the_character_before_it() {
    // Frozen from `rg -nwF -e '-foo'` (and `git grep -nwF`): the rule reads the
    // character before the match, not the first character of the pattern.
    let m = word_matcher("-foo", PatternKind::Literal, CaseMode::Sensitive);
    assert_lines(
        &m,
        &[
            ("-foo", true),
            ("a -foo b", true),
            ("x-foo", false),
            ("-foobar", false),
            ("-foo_", false),
            ("-foo-", true),
            (".-foo", true),
        ],
    );
}

#[test]
fn a_pattern_that_ends_with_a_non_word_character_reads_the_character_after_it() {
    // Frozen from `rg -nwF -e 'foo('` (and `git grep -nwF`).
    let m = word_matcher("foo(", PatternKind::Literal, CaseMode::Sensitive);
    assert_lines(
        &m,
        &[
            ("foo(", true),
            ("foo(x", false),
            ("a foo( b", true),
            ("xfoo(", false),
            ("foo(1)", false),
            ("foo( ", true),
            ("(foo(", true),
        ],
    );
}

#[test]
fn the_whole_word_rule_reads_the_word_characters_of_rg() {
    // Frozen from `rg -nwF -e cafe`: letters, digits, marks, connector
    // punctuation and joiners all count as word characters.
    let m = whole_word("cafe");
    assert_lines(
        &m,
        &[
            ("café cafe!", true),
            ("cafe", true),
            ("cafeé", false),
            ("cafe٣", false),
            ("cafe\u{301}", false),
            ("cafe\u{203f}", false),
            ("cafe\u{200d}", false),
            ("é_cafe", false),
        ],
    );
    assert_eq!(found(&m, "café cafe!"), vec![6..10]);
}

#[test]
fn the_whole_word_rule_holds_under_every_pattern_mode() {
    let insensitive = word_matcher("FOO|FOOBAR", PatternKind::Regex, CaseMode::Insensitive);
    assert_lines(
        &insensitive,
        &[("a fooBar b", true), ("fooBARx", false), ("Foo", true)],
    );
    assert_eq!(found(&insensitive, "fooBAR"), vec![0..6]);

    let smart_lower = word_matcher("foo", PatternKind::Literal, CaseMode::Smart);
    assert_lines(&smart_lower, &[("a FOO b", true), ("FOOD", false)]);

    let smart_mixed = word_matcher("Foo", PatternKind::Literal, CaseMode::Smart);
    assert_lines(&smart_mixed, &[("a Foo b", true), ("a foo b", false)]);

    let flags_in_pattern = word_matcher("(?i)foo|foobar", PatternKind::Regex, CaseMode::Sensitive);
    assert_lines(&flags_in_pattern, &[("FOOBAR", true), ("FOOBARS", false)]);
}

#[test]
fn a_pattern_with_an_unbalanced_group_is_an_error_under_the_whole_word_rule() {
    // Without the check of the bare pattern, `a)|(b` would compile inside the
    // wrapper as a different pattern.
    for pattern in ["a)|(b", "(", "a)"] {
        let err = Matcher::new(spec(pattern, PatternKind::Regex, CaseMode::Sensitive, true));
        assert!(
            matches!(err, Err(MatcherError::Invalid { .. })),
            "{pattern:?}: {err:?}"
        );
    }
}

#[test]
fn a_whole_word_pattern_in_extended_mode_may_end_with_a_comment() {
    let m = word_matcher(
        "(?x) foo # a comment",
        PatternKind::Regex,
        CaseMode::Sensitive,
    );
    assert_lines(&m, &[("a foo b", true), ("foobar", false)]);
}

#[test]
fn a_whole_word_search_of_a_long_line_gives_the_same_answer_as_a_short_one() {
    // 120 KB on one line. Every `a` follows a word character, so no match is a
    // whole word, and the answer is the empty one.
    let line = "ba ".repeat(40_000);
    let m = word_matcher("a.*", PatternKind::Regex, CaseMode::Sensitive);
    assert!(!m.is_match(&line));
    assert!(found(&m, &line).is_empty());

    // One match from the first bounded `a` to the end of the line.
    let line = format!(" a{line}");
    assert_eq!(found(&m, &line), vec![1..line.len()]);

    // Many matches, each found by one step.
    let line = "foobar ".repeat(17_000);
    let m = word_matcher("foo|foobar", PatternKind::Regex, CaseMode::Sensitive);
    let matches = found(&m, &line);
    assert_eq!(matches.len(), 17_000);
    assert_eq!(matches.first(), Some(&(0..6)));
    assert_eq!(matches.last(), Some(&(16_999 * 7..16_999 * 7 + 6)));
}

/// The production text of `matcher.rs` without its comment lines and its test
/// module.
fn matcher_production_code() -> String {
    let source = include_str!("matcher.rs");
    let end = source
        .find("#[cfg(test)]")
        .expect("matcher.rs ends with its test module");
    source[..end]
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_whole_word_rule_is_in_the_pattern_and_not_a_check_after_each_match() {
    // The cost of a per-match check cannot show in an answer: a line of n bytes
    // takes n searches of n bytes and gives the same matches. So this test
    // polices the construct. The word rule is two half word boundaries around
    // the pattern, and no code reads the characters around a match.
    let code = matcher_production_code();
    for required in ["start-half", "end-half"] {
        assert!(code.contains(required), "matcher.rs has no {required}");
    }
    for forbidden in ["fn bounded", "is_word_char", "next_back", "WORD_CHAR_CLASS"] {
        assert!(
            !code.contains(forbidden),
            "matcher.rs checks the characters around a match again: {forbidden}"
        );
    }
}

#[test]
fn only_a_case_sensitive_literal_has_a_prefilter() {
    let m = literal("needle");
    assert!(m.text_may_match("a needle here"));
    assert!(!m.text_may_match("a nail here"));

    let insensitive = Matcher::new(spec(
        "needle",
        PatternKind::Literal,
        CaseMode::Insensitive,
        false,
    ))
    .unwrap();
    assert!(insensitive.text_may_match("NO NEEDLE-LIKE TEXT"));
    assert!(regex_of("needle").text_may_match("no match here"));
}

#[test]
fn a_pattern_that_does_not_compile_names_the_pattern_and_one_cause_line() {
    let err = Matcher::new(spec("(", PatternKind::Regex, CaseMode::Sensitive, false)).unwrap_err();
    match err {
        MatcherError::Invalid { pattern, detail } => {
            assert_eq!(pattern, "(");
            assert!(!detail.contains('\n'), "{detail:?}");
            assert!(detail.contains("unclosed group"), "{detail:?}");
        }
        other => panic!("expected Invalid, got {other:?}"),
    }
}

#[test]
fn a_pattern_past_the_size_limit_is_a_compile_error() {
    let err = Matcher::new(spec(
        "((a{1000}){1000}){1000}",
        PatternKind::Regex,
        CaseMode::Sensitive,
        false,
    ))
    .unwrap_err();
    assert!(matches!(err, MatcherError::Invalid { .. }), "{err:?}");
}

#[test]
fn a_pattern_nested_past_the_limit_is_a_compile_error() {
    let deep = format!("{}a{}", "(".repeat(300), ")".repeat(300));
    let err = Matcher::new(spec(&deep, PatternKind::Regex, CaseMode::Sensitive, false));
    assert!(matches!(err, Err(MatcherError::Invalid { .. })), "{err:?}");
}
