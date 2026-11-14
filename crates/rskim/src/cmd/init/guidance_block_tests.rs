//! Tests for the shape of a guidance file (guidance_block.rs).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{
    BlockStatus, CURSOR_RULE_FRONTMATTER, GuidanceFormat, GuidanceHooks, Marker, block_status,
    find_skim_section, skim_shaped, splice, strip_section, strip_skim_section, whole_file,
};
use crate::cmd::init::helpers::guidance_content;

const FRONTMATTER: &str = CURSOR_RULE_FRONTMATTER;

fn markdown() -> GuidanceFormat {
    GuidanceFormat::markdown(GuidanceHooks::Hook)
}

fn rule_file() -> GuidanceFormat {
    GuidanceFormat::cursor_rule(GuidanceHooks::Hook)
}

fn block(version: &str) -> String {
    guidance_content(version, GuidanceHooks::Hook)
}

fn semver(text: &str) -> Marker {
    Marker::Semver(text.to_string())
}

// ============================================================================
// find_skim_section
// ============================================================================

#[test]
fn an_end_marker_in_front_of_the_block_does_not_end_it() {
    let text = "<!-- skim-end -->\nnotes\n<!-- skim-start v1.0.0 -->\nbody\n<!-- skim-end -->\n";

    let (start, end) = find_skim_section(text).unwrap();

    assert_eq!(
        &text[start..end],
        "<!-- skim-start v1.0.0 -->\nbody\n<!-- skim-end -->"
    );
}

// ============================================================================
// block_status: the version is compared whole
// ============================================================================

#[test]
fn a_newer_block_and_an_older_block_are_both_stale() {
    // v3.0.10 contains v3.0.1 as a prefix; neither is the other's version.
    let newer = format!("# Mine\n\n{}\n", block("3.0.10"));
    let older = format!("# Mine\n\n{}\n", block("3.0.1"));

    assert_eq!(
        block_status(&newer, &block("3.0.1"), &markdown()),
        BlockStatus::Stale {
            marker: semver("3.0.10")
        }
    );
    assert_eq!(
        block_status(&older, &block("3.0.10"), &markdown()),
        BlockStatus::Stale {
            marker: semver("3.0.1")
        }
    );
}

#[test]
fn a_block_with_the_same_version_and_the_same_text_is_current() {
    let text = format!("# Mine\n\n{}\n\n# More of mine\n", block("3.0.1"));

    assert_eq!(
        block_status(&text, &block("3.0.1"), &markdown()),
        BlockStatus::Current
    );
}

#[test]
fn a_block_with_the_same_version_and_other_text_is_changed() {
    let edited = block("3.0.1").replace("Skim earns its cost", "Skim earns its keep");
    let text = format!("# Mine\n\n{edited}\n");

    assert_eq!(
        block_status(&text, &block("3.0.1"), &markdown()),
        BlockStatus::Changed
    );
}

#[test]
fn a_start_marker_with_no_end_marker_is_broken() {
    let cut = block("3.0.1").replace("<!-- skim-end -->", "");

    assert_eq!(
        block_status(&cut, &block("3.0.1"), &markdown()),
        BlockStatus::Broken
    );
}

#[test]
fn an_end_marker_before_the_start_marker_is_broken() {
    let text = "<!-- skim-end -->\nsome content\n<!-- skim-start v1.0.0 -->\n";

    assert_eq!(
        block_status(text, &block("3.0.1"), &markdown()),
        BlockStatus::Broken
    );
}

#[test]
fn text_with_no_start_marker_is_absent() {
    assert_eq!(
        block_status("# Mine\n", &block("3.0.1"), &markdown()),
        BlockStatus::Absent
    );
    assert_eq!(
        block_status("", &block("3.0.1"), &markdown()),
        BlockStatus::Absent
    );
}

#[test]
fn a_tag_after_the_numbers_is_part_of_the_version() {
    let text = format!("{}\n", block("3.0.1-rc.1"));

    assert_eq!(
        block_status(&text, &block("3.0.1"), &markdown()),
        BlockStatus::Stale {
            marker: semver("3.0.1-rc.1")
        }
    );
}

#[test]
fn a_marker_that_is_not_a_version_is_stale_and_unrecognized() {
    let text = "<!-- skim-start vnext -->\nbody\n<!-- skim-end -->\n";

    assert_eq!(
        block_status(text, &block("3.0.1"), &markdown()),
        BlockStatus::Stale {
            marker: Marker::Unrecognized("vnext -->".to_string())
        }
    );
}

#[test]
fn a_long_unrecognized_marker_is_cut_to_a_fixed_length() {
    let text = format!(
        "<!-- skim-start v{} -->\nbody\n<!-- skim-end -->\n",
        "9".repeat(500)
    );

    let BlockStatus::Stale {
        marker: Marker::Unrecognized(shown),
    } = block_status(&text, &block("3.0.1"), &markdown())
    else {
        panic!("a 500-digit version is not a version");
    };

    assert_eq!(shown.chars().count(), 64);
}

// ============================================================================
// block_status: a file that skim owns
// ============================================================================

#[test]
fn a_skim_shaped_owned_file_is_current_only_when_it_is_byte_equal() {
    let expected = block("3.0.1");
    let exact = whole_file(&rule_file(), &expected);
    let without_final_newline = format!("{FRONTMATTER}{expected}");
    let doubled = format!("{FRONTMATTER}{exact}");

    assert_eq!(
        block_status(&exact, &expected, &rule_file()),
        BlockStatus::Current
    );
    assert_eq!(
        block_status(&without_final_newline, &expected, &rule_file()),
        BlockStatus::Changed
    );
    assert_eq!(
        block_status(&doubled, &expected, &rule_file()),
        BlockStatus::Changed
    );
}

#[test]
fn an_owned_file_with_user_text_and_a_current_block_is_current() {
    let expected = block("3.0.1");
    let text = format!("{FRONTMATTER}My own notes.\n\n{expected}\n\nMore notes.\n");

    assert_eq!(
        block_status(&text, &expected, &rule_file()),
        BlockStatus::Current
    );
}

// ============================================================================
// skim_shaped
// ============================================================================

#[test]
fn copies_of_the_frontmatter_a_block_and_blank_lines_are_skim_shaped() {
    let b = block("3.0.1");

    assert!(skim_shaped(&format!("{FRONTMATTER}{b}\n"), &rule_file()));
    assert!(skim_shaped(
        &format!("{FRONTMATTER}{FRONTMATTER}{FRONTMATTER}{b}\n\n\n"),
        &rule_file()
    ));
    assert!(skim_shaped(&b, &rule_file()));
}

#[test]
fn text_of_the_user_makes_a_file_not_skim_shaped() {
    let b = block("3.0.1");

    assert!(!skim_shaped(
        &format!("{FRONTMATTER}notes\n\n{b}\n"),
        &rule_file()
    ));
    assert!(!skim_shaped(
        &format!("{FRONTMATTER}{b}\n\nnotes\n"),
        &rule_file()
    ));
    assert!(!skim_shaped(
        &format!("{FRONTMATTER}{b}\n{b}\n"),
        &rule_file()
    ));
    assert!(!skim_shaped("notes\n", &rule_file()));
    assert!(!skim_shaped(&format!("{FRONTMATTER}\n"), &rule_file()));
}

#[test]
fn a_markdown_file_with_only_the_block_is_skim_shaped() {
    let b = block("3.0.1");

    assert!(skim_shaped(&format!("{b}\n"), &markdown()));
    assert!(!skim_shaped(&format!("# Mine\n\n{b}\n"), &markdown()));
    // The frontmatter of a rule file is text of the user in a Markdown file.
    assert!(!skim_shaped(&format!("{FRONTMATTER}{b}\n"), &markdown()));
}

// ============================================================================
// splice
// ============================================================================

#[test]
fn splicing_into_a_skim_shaped_owned_file_leaves_exactly_one_frontmatter() {
    let old = block("3.0.0");
    let new = block("3.0.1");
    let text = format!("{FRONTMATTER}{FRONTMATTER}{old}\n");

    let spliced = splice(&text, &new, &rule_file()).unwrap();

    assert_eq!(spliced, format!("{FRONTMATTER}{new}\n"));
    assert_eq!(spliced.matches("alwaysApply: true").count(), 1);
}

#[test]
fn a_skim_shaped_owned_file_is_rewritten_whole_with_no_blank_lines_after_the_block() {
    let old = block("3.0.0");
    let new = block("3.0.1");
    let text = format!("{FRONTMATTER}{old}\n\n\n");

    let spliced = splice(&text, &new, &rule_file()).unwrap();

    assert_eq!(spliced, whole_file(&rule_file(), &new));
}

#[test]
fn splicing_into_an_owned_file_with_user_text_keeps_every_byte_of_that_text() {
    let old = block("3.0.0");
    let new = block("3.0.1");
    let text = format!(
        "{FRONTMATTER}My notes\n  with  spacing\t\n\n{FRONTMATTER}{old}\n\nAfter the block\n\n\n"
    );

    let spliced = splice(&text, &new, &rule_file()).unwrap();

    assert_eq!(
        spliced,
        format!("{FRONTMATTER}My notes\n  with  spacing\t\n\n{new}\n\nAfter the block\n\n\n")
    );
}

#[test]
fn splicing_drops_the_copies_that_stand_in_front_of_the_block_and_keeps_the_first() {
    let old = block("3.0.0");
    let new = block("3.0.1");
    let text = format!("{FRONTMATTER}{FRONTMATTER}{old}\n\nAfter the block\n");

    let spliced = splice(&text, &new, &rule_file()).unwrap();

    assert_eq!(spliced, format!("{FRONTMATTER}{new}\n\nAfter the block\n"));
}

#[test]
fn splicing_keeps_one_blank_line_between_the_user_text_and_the_block() {
    let old = block("3.0.0");
    let new = block("3.0.1");
    // The text of the user ends with one newline, and skim's copy follows it.
    let text = format!("{FRONTMATTER}My notes\n{FRONTMATTER}{old}\n\nAfter\n");

    let spliced = splice(&text, &new, &rule_file()).unwrap();

    assert_eq!(
        spliced,
        format!("{FRONTMATTER}My notes\n\n{new}\n\nAfter\n")
    );
}

#[test]
fn splicing_into_a_markdown_file_changes_only_the_block() {
    let old = block("3.0.0");
    let new = block("3.0.1");
    let text = format!("# Header\n\n{old}\n\n# Footer\n");

    let spliced = splice(&text, &new, &markdown()).unwrap();

    assert_eq!(spliced, format!("# Header\n\n{new}\n\n# Footer\n"));
}

#[test]
fn splicing_a_file_with_no_block_gives_nothing() {
    assert_eq!(splice("# Mine\n", &block("3.0.1"), &markdown()), None);
    assert_eq!(splice("# Mine\n", &block("3.0.1"), &rule_file()), None);
}

// ============================================================================
// strip_section
// ============================================================================

#[test]
fn stripping_an_owned_file_with_user_text_keeps_the_text_and_one_frontmatter() {
    let b = block("3.0.1");
    let text = format!("{FRONTMATTER}{FRONTMATTER}{b}\n\nMy notes\n");

    let stripped = strip_section(&text, Some(FRONTMATTER)).unwrap();

    assert_eq!(
        stripped,
        format!("{}\n\nMy notes\n", FRONTMATTER.trim_end())
    );
}

#[test]
fn stripping_a_user_file_keeps_the_text_around_the_block() {
    let b = block("3.0.1");
    let text = format!("# Header\n\n{b}\n\n# Footer\n");

    assert_eq!(
        strip_skim_section(&text).unwrap(),
        "# Header\n\n# Footer\n".to_string()
    );
    assert_eq!(strip_skim_section("# Mine\n"), None);
}
