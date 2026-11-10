//! The allow-list of commands whose captured output the post-run hook may compress.
//!
//! This module is the one owner of that list. A command is on it when the
//! rewrite engine matched one of the rule prefixes in [`POST_COMPRESS_RULES`],
//! or matched the `cat` handler on exactly one code file.
//!
//! The list keys on the rule *prefix*, never on the rewrite target: `ruff check`
//! and bare `ruff` share one target, as do `golangci-lint run` and bare
//! `golangci-lint`, and only the prefix tells a lint run from a `--version` call.
//!
//! No member of the list routes through a tool whose output skim must mask
//! (`RewriteResult::masks_output`), and [`post_tool_for`] re-checks that at
//! lookup time so a future rule cannot add one by accident.

use super::handlers::CAT_HANDLER_PREFIX;
use super::types::RewriteResult;
use crate::output::REWRITE_ORIGIN_ENV;

/// What the post-run hook does with the captured output of an allow-listed command.
// Used by the post path from Phase 3 on; Phase 3 removes this allow.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PostTool {
    CargoTest,
    Pytest,
    Eslint,
    RuffCheck,
    Mypy,
    Golangci,
    Rubocop,
    Oxlint,
    /// `biome check` and `biome lint`, with or without `npx`.
    BiomeCheck,
    Swiftlint,
    /// One code file read with `cat`. `structure` is true for declaration files
    /// (`.d.ts`, `.d.mts`, `.d.cts`, `.pyi`) and false for every other code file.
    Cat {
        structure: bool,
    },
}

/// The rule prefixes the post-run hook compresses, each with its tool.
///
/// Every prefix is also a prefix in the rewrite rule table; a unit test pins that.
// Used by `post_tool_for`, which the post path calls from Phase 3 on; Phase 3
// removes this allow.
#[allow(dead_code)]
pub(super) const POST_COMPRESS_RULES: &[(&[&str], PostTool)] = &[
    (&["cargo", "test"], PostTool::CargoTest),
    (&["pytest"], PostTool::Pytest),
    (&["python", "-m", "pytest"], PostTool::Pytest),
    (&["python3", "-m", "pytest"], PostTool::Pytest),
    (&["eslint"], PostTool::Eslint),
    (&["npx", "eslint"], PostTool::Eslint),
    (&["ruff", "check"], PostTool::RuffCheck),
    (&["mypy"], PostTool::Mypy),
    (&["python", "-m", "mypy"], PostTool::Mypy),
    (&["python3", "-m", "mypy"], PostTool::Mypy),
    (&["golangci-lint", "run"], PostTool::Golangci),
    (&["rubocop"], PostTool::Rubocop),
    (&["bundle", "exec", "rubocop"], PostTool::Rubocop),
    (&["oxlint"], PostTool::Oxlint),
    (&["npx", "oxlint"], PostTool::Oxlint),
    (&["biome", "check"], PostTool::BiomeCheck),
    (&["biome", "lint"], PostTool::BiomeCheck),
    (&["npx", "biome", "check"], PostTool::BiomeCheck),
    (&["npx", "biome", "lint"], PostTool::BiomeCheck),
    (&["swiftlint"], PostTool::Swiftlint),
    (&["bundle", "exec", "swiftlint"], PostTool::Swiftlint),
];

/// The tool the post-run hook uses for `result`, or `None` when the command is
/// not on the allow-list.
///
/// Fails closed: a result that masks output, has no matched prefix, or is a
/// `cat` result in a layout this module does not know gets `None`.
// Called by the post path from Phase 3 on; Phase 3 removes this allow.
#[allow(dead_code)]
pub(super) fn post_tool_for(result: &RewriteResult) -> Option<PostTool> {
    if result.masks_output {
        return None;
    }
    let prefix = result.matched_prefix?;
    if prefix == CAT_HANDLER_PREFIX {
        return cat_post_tool(&result.tokens);
    }
    POST_COMPRESS_RULES
        .iter()
        .find(|(allowed, _)| *allowed == prefix)
        .map(|(_, tool)| *tool)
}

/// The tool for a `cat` rewrite of exactly one code file.
///
/// The cat handler builds `[<origin tag>, skim, <file>, <mode>]` for one file.
/// More files add `--no-header`, and caller env vars add leading tokens, so
/// both give a different token count and are refused.
fn cat_post_tool(tokens: &[String]) -> Option<PostTool> {
    let [origin, skim, _file, mode] = tokens else {
        return None;
    };
    if !origin.starts_with(REWRITE_ORIGIN_ENV) || skim != "skim" {
        return None;
    }
    match mode.as_str() {
        "--mode=structure" => Some(PostTool::Cat { structure: true }),
        "--mode=pseudo" => Some(PostTool::Cat { structure: false }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::super::compound::{split_compound, try_rewrite_compound};
    use super::super::engine::{rewrites_to_masking_tool, try_rewrite};
    use super::super::rules;
    use super::super::types::{CompoundSplitResult, RewriteCategory};
    use super::*;

    /// The rewrite the engine finds for a simple command line.
    fn rewrite_of(command: &str) -> Option<RewriteResult> {
        let tokens: Vec<&str> = command.split_whitespace().collect();
        try_rewrite(&tokens)
    }

    fn post_tool_of(command: &str) -> Option<PostTool> {
        rewrite_of(command).and_then(|result| post_tool_for(&result))
    }

    /// A result with the given pieces, for the shapes the engine never builds.
    fn result_with(
        tokens: &[&str],
        masks_output: bool,
        matched_prefix: Option<&'static [&'static str]>,
    ) -> RewriteResult {
        RewriteResult {
            tokens: tokens.iter().map(|t| (*t).to_string()).collect(),
            category: RewriteCategory::Read,
            masks_output,
            matched_prefix,
        }
    }

    // ------------------------------------------------------------------
    // The allow-list against the rewrite rule table
    // ------------------------------------------------------------------

    #[test]
    fn every_allow_list_prefix_is_a_rewrite_rule_prefix() {
        for (prefix, _) in POST_COMPRESS_RULES {
            assert!(
                rules::all_rules().any(|rule| rule.prefix == *prefix),
                "{prefix:?} is on the allow-list but is not a rewrite rule prefix"
            );
        }
    }

    /// The cat handler's marker is told apart from a rule prefix by value, so no
    /// rule may carry it.
    #[test]
    fn cat_handler_marker_is_not_a_rewrite_rule_prefix() {
        assert!(rules::all_rules().all(|rule| rule.prefix != CAT_HANDLER_PREFIX));
    }

    #[test]
    fn allow_list_names_each_prefix_once() {
        for (index, (prefix, _)) in POST_COMPRESS_RULES.iter().enumerate() {
            let repeats = POST_COMPRESS_RULES[index + 1..]
                .iter()
                .filter(|(other, _)| other == prefix)
                .count();
            assert_eq!(repeats, 0, "{prefix:?} is on the allow-list twice");
        }
    }

    #[test]
    fn no_allow_list_prefix_rewrites_to_a_masking_tool() {
        for (prefix, _) in POST_COMPRESS_RULES {
            for rule in rules::all_rules().filter(|rule| rule.prefix == *prefix) {
                assert!(
                    !rewrites_to_masking_tool(rule.rewrite_to),
                    "{prefix:?} rewrites to {:?}, a tool whose output skim must mask",
                    rule.rewrite_to
                );
            }
        }
    }

    /// A command built from a prefix reaches that prefix's rule, and not a
    /// more specific rule that shadows it.
    #[test]
    fn every_allow_list_prefix_reaches_its_own_rule() {
        for (prefix, tool) in POST_COMPRESS_RULES {
            let command = prefix.join(" ");
            let result = rewrite_of(&command)
                .unwrap_or_else(|| panic!("{command:?} must match a rewrite rule"));
            assert_eq!(result.matched_prefix, Some(*prefix), "{command:?}");
            assert!(!result.masks_output, "{command:?} must not mask output");
            assert_eq!(post_tool_for(&result), Some(*tool), "{command:?}");
        }
    }

    // ------------------------------------------------------------------
    // Lookup by matched prefix
    // ------------------------------------------------------------------

    #[test]
    fn lookup_gives_the_tool_for_every_allow_listed_command_form() {
        let expected = [
            ("cargo test", PostTool::CargoTest),
            ("cargo test --lib parser::", PostTool::CargoTest),
            ("pytest", PostTool::Pytest),
            ("pytest -x tests/", PostTool::Pytest),
            ("python -m pytest tests/", PostTool::Pytest),
            ("python3 -m pytest tests/", PostTool::Pytest),
            ("eslint src/", PostTool::Eslint),
            ("npx eslint src/", PostTool::Eslint),
            ("ruff check src/", PostTool::RuffCheck),
            ("mypy src/", PostTool::Mypy),
            ("python -m mypy src/", PostTool::Mypy),
            ("python3 -m mypy src/", PostTool::Mypy),
            ("golangci-lint run ./...", PostTool::Golangci),
            ("rubocop app/", PostTool::Rubocop),
            ("bundle exec rubocop app/", PostTool::Rubocop),
            ("oxlint src/", PostTool::Oxlint),
            ("npx oxlint src/", PostTool::Oxlint),
            ("biome check src/", PostTool::BiomeCheck),
            ("biome lint src/", PostTool::BiomeCheck),
            ("npx biome check src/", PostTool::BiomeCheck),
            ("npx biome lint src/", PostTool::BiomeCheck),
            ("swiftlint lint", PostTool::Swiftlint),
            ("bundle exec swiftlint", PostTool::Swiftlint),
        ];
        for (command, tool) in expected {
            assert_eq!(post_tool_of(command), Some(tool), "{command:?}");
        }
    }

    /// Commands that share a rewrite target with an allow-listed prefix, or that
    /// the engine rewrites for another tool, stay off the list.
    #[test]
    fn lookup_gives_none_for_commands_off_the_allow_list() {
        for command in [
            "ruff --version",
            "ruff format src/",
            "ruff format --check src/",
            "golangci-lint",
            "golangci-lint linters",
            "biome format src/",
            "biome ci src/",
            "npx biome format src/",
            "cargo build",
            "cargo nextest run",
            "cargo clippy",
            "git status",
            "npx vitest",
            "npx jest",
            "head -5 src/lib.rs",
            "tail src/lib.rs",
        ] {
            assert!(rewrite_of(command).is_some(), "{command:?} must rewrite");
            assert_eq!(post_tool_of(command), None, "{command:?}");
        }
    }

    #[test]
    fn lookup_gives_none_for_a_compound_rewrite() {
        let CompoundSplitResult::Compound(segments) = split_compound("cargo test && cargo build")
        else {
            panic!("the command must split at &&");
        };
        let result = try_rewrite_compound(&segments).expect("a segment must rewrite");
        assert_eq!(post_tool_for(&result), None);
    }

    #[test]
    fn lookup_gives_none_for_the_masking_tools() {
        for command in ["env", "printenv HOME", "curl https://example.com"] {
            let result = rewrite_of(command).unwrap_or_else(|| panic!("{command:?} must rewrite"));
            assert!(result.masks_output, "{command:?}");
            assert_eq!(post_tool_for(&result), None, "{command:?}");
        }
    }

    /// An allow-listed prefix on a result that masks output is never served.
    #[test]
    fn lookup_gives_none_when_a_result_masks_output() {
        let result = result_with(&["skim", "test", "cargo"], true, Some(&["cargo", "test"]));
        assert_eq!(post_tool_for(&result), None);
    }

    // ------------------------------------------------------------------
    // cat
    // ------------------------------------------------------------------

    #[test]
    fn lookup_gives_pseudo_for_one_code_file() {
        for command in ["cat src/lib.rs", "cat app.ts", "cat -s tool.py"] {
            assert_eq!(
                post_tool_of(command),
                Some(PostTool::Cat { structure: false }),
                "{command:?}"
            );
        }
    }

    #[test]
    fn lookup_gives_structure_for_one_declaration_file() {
        for command in [
            "cat types/index.d.ts",
            "cat types/index.d.mts",
            "cat types/index.d.cts",
            "cat stubs/api.pyi",
        ] {
            assert_eq!(
                post_tool_of(command),
                Some(PostTool::Cat { structure: true }),
                "{command:?}"
            );
        }
    }

    #[test]
    fn lookup_gives_none_for_cat_of_several_files() {
        for command in ["cat a.rs b.rs", "cat a.d.ts b.d.ts"] {
            assert!(rewrite_of(command).is_some(), "{command:?} must rewrite");
            assert_eq!(post_tool_of(command), None, "{command:?}");
        }
    }

    #[test]
    fn lookup_gives_none_for_cat_with_an_env_prefix() {
        let command = "FOO=1 cat src/lib.rs";
        assert!(rewrite_of(command).is_some(), "{command:?} must rewrite");
        assert_eq!(post_tool_of(command), None);
    }

    /// A cat result whose layout or mode the owner does not know is refused.
    #[test]
    fn lookup_gives_none_for_a_cat_result_it_cannot_read() {
        let origin = format!("{REWRITE_ORIGIN_ENV}=cat");
        for tokens in [
            vec![origin.as_str(), "skim", "a.rs", "--mode=full"],
            vec![origin.as_str(), "skim", "a.rs"],
            vec!["skim", "a.rs", "--mode=pseudo"],
            vec!["FOO=1", "skim", "a.rs", "--mode=pseudo"],
            vec![origin.as_str(), "other", "a.rs", "--mode=pseudo"],
            vec![origin.as_str(), "skim", "a.rs", "b.rs", "--mode=pseudo"],
        ] {
            let result = result_with(&tokens, false, Some(CAT_HANDLER_PREFIX));
            assert_eq!(post_tool_for(&result), None, "{tokens:?}");
        }
    }
}
