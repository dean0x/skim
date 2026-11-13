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
