//! The post-run hook: Claude Code's `PostToolUse` event for a Bash call.
//!
//! The command already ran. [`run_post_hook`] reads the captured `tool_response`
//! and, in auto permission mode, replaces `stdout` with a compressed view and
//! adds one skim marker line to `stderr`. It only transforms the text it was
//! given: it starts no process, opens no file, writes no analytics row, and
//! never returns a permission decision. Every path exits 0 with empty stderr;
//! a decline sends nothing, so the model gets the raw output.
//!
//! This module also owns the allow-list of commands whose output it may
//! compress. A command is on it when the rewrite engine matched one of the rule
//! prefixes in [`POST_COMPRESS_RULES`], or matched the `cat` handler on exactly
//! one code file.
//!
//! The list keys on the rule *prefix*, never on the rewrite target: `ruff check`
//! and bare `ruff` share one target, as do `golangci-lint run` and bare
//! `golangci-lint`, and only the prefix tells a lint run from a `--version` call.
//!
//! No member of the list routes through a tool whose output skim must mask
//! (`RewriteResult::masks_output`), and [`post_tool_for`] re-checks that at
//! lookup time so a future rule cannot add one by accident.

use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Map, Value};

use super::engine::{strip_env_vars, try_rewrite};
use super::handlers::CAT_HANDLER_PREFIX;
use super::hook::{AUTO_PERMISSION_MODE, command_gates, write_hook_response};
use super::types::RewriteResult;
use crate::cmd::hooks::claude::{PostToolUseInput, format_post_response};
use crate::cmd::lint::{biome, eslint, golangci, mypy, oxlint, rubocop, ruff, swiftlint};
use crate::cmd::session::AgentKind;
use crate::cmd::test::{cargo, pytest};
use crate::output::fidelity::{FidelityDecision, decide_with_notice};
use crate::output::{
    POST_VIEW_MARKER_PREFIXES, ParseResult, REWRITE_ORIGIN_ENV, lossy_view_marker,
    post_parsed_view_marker, strip_ansi_cow,
};
use crate::runner::CommandOutput;

/// Most characters of captured stdout the post path accepts. Claude Code keeps
/// output of about 30,000 characters or more in a file and gives the hook a
/// prefix, so a text this long is never the whole output.
const MAX_STDOUT_CHARS: usize = 29_000;

/// `tool_response` keys whose presence means the run is not a plain foreground
/// run whose `stdout` is the whole output.
const DECLINING_RESPONSE_KEYS: [&str; 3] = [
    "persistedOutputPath",
    "backgroundTaskId",
    "returnCodeInterpretation",
];

/// What the post-run hook does with the captured output of an allow-listed command.
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

/// The tool for a `cat` rewrite of exactly one tree-sitter code file.
///
/// The cat handler builds `[<origin tag>, skim, <file>, <mode>]` for one file.
/// More files add `--no-header`, and caller env vars add leading tokens, so
/// both give a different token count and are refused. A glob, brace or `$`
/// in the one argument can expand to several files in the shell, so it is
/// refused too. The cat rewrite also accepts Markdown and the serde data
/// formats; this path does not.
fn cat_post_tool(tokens: &[String]) -> Option<PostTool> {
    let [origin, skim, file, mode] = tokens else {
        return None;
    };
    if !origin.starts_with(REWRITE_ORIGIN_ENV)
        || skim != "skim"
        || file.contains(['*', '?', '[', '{', '$'])
        || !is_code_language(file)
    {
        return None;
    }
    match mode.as_str() {
        "--mode=structure" => Some(PostTool::Cat { structure: true }),
        "--mode=pseudo" => Some(PostTool::Cat { structure: false }),
        _ => None,
    }
}

/// Whether the extension of `path` names a tree-sitter code language: not
/// Markdown, and not JSON, YAML or TOML.
fn is_code_language(path: &str) -> bool {
    rskim_core::Language::from_path(Path::new(path))
        .is_some_and(|lang| lang != rskim_core::Language::Markdown && !lang.is_serde_based())
}

// ============================================================================
// The post path
// ============================================================================

/// Run the `PostToolUse` hook for `input`, which the router read from stdin.
/// A decline, a bad input, a closed stdout and a panic all end the same way:
/// nothing on stdout, nothing on stderr, exit 0.
pub(super) fn run_post_hook(input: &Value, agent: AgentKind) -> ExitCode {
    // The default panic hook prints to stderr, which the hook protocol treats as
    // an error. Log the location only: the message can hold input text.
    panic::set_hook(Box::new(|info| {
        let place = info
            .location()
            .map(|at| format!(" at {}:{}", at.file(), at.line()))
            .unwrap_or_default();
        crate::cmd::hook_log::log_hook_warning(&format!("post hook panicked{place}"));
    }));
    if let Some(response) = guarded(|| post_response(input, agent)) {
        let _ = write_hook_response(&response);
    }
    ExitCode::SUCCESS
}

/// Run `compute`, turning a panic into "no response".
fn guarded(compute: impl FnOnce() -> Option<Value>) -> Option<Value> {
    panic::catch_unwind(AssertUnwindSafe(compute))
        .ok()
        .flatten()
}

/// The response for a `PostToolUse` input, or `None` for no output. Each gate
/// declines, cheapest first: agent, auto mode, input shape, the command gates the
/// Pre path shares, the captured result, the allow-list, tokenizer health, and
/// last the net-savings guard.
fn post_response(input: &Value, agent: AgentKind) -> Option<Value> {
    if agent != AgentKind::ClaudeCode {
        return None;
    }
    let call = PostToolUseInput::parse(input)?;
    if call.permission_mode != Some(AUTO_PERMISSION_MODE) {
        return None;
    }
    let (tokens, has_operator_chars) = command_gates(call.command).ok()?;
    if has_operator_chars || !command_is_plain(call.command, &tokens) {
        return None;
    }
    let raw = captured_stdout(call.tool_response)?;
    let rewrite = try_rewrite(&tokens)?;
    let tool = post_tool_for(&rewrite)?;
    if !crate::tokens::cl100k_is_ready() {
        return None;
    }
    let (served, marker) = match tool {
        // The cat handler copied its one file argument to the third token.
        PostTool::Cat { structure } => cat_view(raw, rewrite.tokens.get(2)?, structure)?,
        // The label is the rule prefix, a fixed string from the rule table.
        parser => parsed_view(parser, &rewrite.matched_prefix?.join(" "), raw)?,
    };
    // The guard charges the marker to the view: a view that saves less than its
    // own marker costs gives no output.
    if served.trim().is_empty()
        || decide_with_notice(raw, &served, Some(&marker)) != FidelityDecision::Keep
    {
        return None;
    }
    format_post_response(call.tool_response, &served, &marker)
}

/// Command shapes declined on top of the shared gates. A background `&`, a
/// redirect or a substitution means the captured text may not be the plain
/// output of one command; an env prefix or `SKIM_PASSTHROUGH` asks for raw
/// output. Text checks only: no process runs and no env var is read.
fn command_is_plain(command: &str, tokens: &[&str]) -> bool {
    !command.contains(['&', '>', '<'])
        && !command.contains("$(")
        && !command.contains("SKIM_PASSTHROUGH")
        && strip_env_vars(tokens) == 0
}

/// The captured stdout, when `response` is a finished foreground run whose
/// stdout is the whole output, is clean text, and holds no skim marker.
fn captured_stdout(response: &Map<String, Value>) -> Option<&str> {
    let is_false = |key: &str| response.get(key) == Some(&Value::Bool(false));
    let finished = is_false("interrupted")
        && is_false("isImage")
        && !DECLINING_RESPONSE_KEYS
            .iter()
            .any(|key| response.contains_key(*key));
    let stdout = response.get("stdout")?.as_str()?;
    let stderr = match response.get("stderr") {
        None => "",
        Some(value) => value.as_str()?,
    };
    let has_marker = |text: &str| {
        POST_VIEW_MARKER_PREFIXES
            .iter()
            .any(|prefix| text.contains(prefix))
    };
    let clean = !stdout.trim().is_empty()
        && !stdout.contains(['\0', '\u{FFFD}'])
        && stdout.chars().count() < MAX_STDOUT_CHARS;
    (finished && clean && !has_marker(stdout) && !has_marker(stderr)).then_some(stdout)
}

/// The view of a one-file `cat` and its marker. The language comes from the
/// path the command named. The file is never opened: the text is what `cat`
/// printed, so a control byte inside a string literal survives.
fn cat_view(raw: &str, path: &str, structure: bool) -> Option<(String, String)> {
    let (mode, name) = if structure {
        (rskim_core::Mode::Structure, "structure")
    } else {
        (rskim_core::Mode::Pseudo, "pseudo")
    };
    let served = rskim_core::transform_auto(raw, Path::new(path), mode).ok()?;
    Some((served, lossy_view_marker(Some("cat"), name, 1, 1)?))
}

/// The parsed summary of a tool's output and its marker. The parser gets the
/// text without escape sequences, as the spawn path gives it. Only a Full or
/// Degraded parse is served: a Passthrough parse means the output was not understood.
fn parsed_view(tool: PostTool, label: &str, raw: &str) -> Option<(String, String)> {
    let output = CommandOutput {
        stdout: strip_ansi_cow(raw).into_owned(),
        stderr: String::new(),
        exit_code: None,
        duration: Duration::ZERO,
    };
    let served = match tool {
        PostTool::CargoTest => kept(cargo::parse_impl(&output, false)),
        PostTool::Pytest => kept(pytest::parse(&output.stdout)),
        PostTool::Eslint => kept(eslint::parse_impl(&output)),
        PostTool::RuffCheck => kept(ruff::parse_check_impl(&output)),
        PostTool::Mypy => kept(mypy::parse_impl(&output)),
        PostTool::Golangci => kept(golangci::parse_impl(&output)),
        PostTool::Rubocop => kept(rubocop::parse_impl(&output)),
        PostTool::Oxlint => kept(oxlint::parse_impl(&output)),
        PostTool::BiomeCheck => kept(biome::parse_check_impl(&output)),
        PostTool::Swiftlint => kept(swiftlint::parse_impl(&output)),
        PostTool::Cat { .. } => None,
    }?;
    let marker = post_parsed_view_marker(label, raw.len(), served.len());
    Some((served, marker))
}

/// The text of a Full or Degraded parse, and `None` for any other tier.
fn kept<T: AsRef<str>>(parsed: ParseResult<T>) -> Option<String> {
    matches!(parsed, ParseResult::Full(_) | ParseResult::Degraded(..))
        .then(|| parsed.content().to_owned())
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
        for command in [
            "cat a.rs b.rs",
            "cat a.d.ts b.d.ts",
            "cat src/*.rs",
            "cat src/{a,b}.rs",
            "cat src/?.rs",
            "cat src/[ab].rs",
            "cat $F.rs",
        ] {
            assert!(rewrite_of(command).is_some(), "{command:?} must rewrite");
            assert_eq!(post_tool_of(command), None, "{command:?}");
        }
    }

    /// Prose and data formats are not code: the cat rewrite accepts them, the
    /// post path does not.
    #[test]
    fn lookup_gives_none_for_cat_of_prose_and_data_files() {
        for command in [
            "cat README.md",
            "cat package.json",
            "cat ci.yaml",
            "cat ci.yml",
            "cat Cargo.toml",
        ] {
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

    // ------------------------------------------------------------------
    // The post path
    // ------------------------------------------------------------------

    /// A passing libtest run, long enough that its summary and the marker
    /// together are smaller than it.
    fn cargo_stdout() -> String {
        let mut text = String::from("running 40 tests\n");
        for n in 0..40 {
            text.push_str(&format!("test parser::tests::case_{n} ... ok\n"));
        }
        text.push_str(
            "\ntest result: ok. 40 passed; 0 failed; 0 ignored; 0 measured; \
             0 filtered out; finished in 0.01s\n",
        );
        text
    }

    /// A Claude Code `PostToolUse` input in auto mode for a finished run.
    fn call(command: &str, stdout: &str) -> Value {
        serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "permission_mode": "auto",
            "tool_input": { "command": command },
            "tool_response": {
                "stdout": stdout,
                "stderr": "   Compiling skim v1.0.0\n",
                "interrupted": false,
                "isImage": false,
                "noOutputExpected": false,
            },
        })
    }

    fn baseline() -> Value {
        call("cargo test", &cargo_stdout())
    }

    fn respond(input: &Value) -> Option<Value> {
        post_response(input, AgentKind::ClaudeCode)
    }

    fn updated(response: &Value) -> &Value {
        &response["hookSpecificOutput"]["updatedToolOutput"]
    }

    /// A module header, then non-doc comments, which pseudo mode removes, and an
    /// ESC byte in a literal.
    fn rust_source() -> String {
        let steps: String = (0..12)
            .map(|n| {
                format!(
                    "// A comment that explains step {n} at some length.\n\
                     fn step_{n}() -> &'static str {{ \"\u{1b}[31m\" }}\n"
                )
            })
            .collect();
        format!("//! Module header.\n\n{steps}")
    }

    #[test]
    fn a_passing_cargo_test_run_gets_the_summary_and_one_marker_line() {
        // Escape sequences in the captured text must not reach the parser or the view.
        let colored = cargo_stdout().replace("ok", "\u{1b}[32mok\u{1b}[0m");
        let response = respond(&call("cargo test", &colored)).expect("must compress");
        let keys: Vec<_> = response.as_object().expect("object").keys().collect();
        assert_eq!(keys, ["hookSpecificOutput"]);
        assert_eq!(
            response["hookSpecificOutput"]["hookEventName"],
            "PostToolUse"
        );
        let stdout = updated(&response)["stdout"].as_str().expect("stdout");
        assert!(
            stdout.contains("40") && stdout.len() < colored.len(),
            "{stdout:?}"
        );
        assert!(!stdout.contains('\u{1b}'), "{stdout:?}");
        let stderr = updated(&response)["stderr"].as_str().expect("stderr");
        assert!(
            stderr.starts_with("   Compiling skim v1.0.0\n[skim] compressed view of 'cargo test'"),
            "{stderr:?}"
        );
        assert_eq!(stderr.lines().count(), 2, "one marker line: {stderr:?}");
        assert_eq!(updated(&response)["noOutputExpected"], false);
    }

    #[test]
    fn each_gate_declines_when_one_field_of_the_baseline_changes() {
        use serde_json::json;
        let marked = |text: &str| format!("{text}{}", cargo_stdout());
        let cases: Vec<(&str, Option<Value>)> = vec![
            ("/permission_mode", Some(json!("default"))),
            ("/permission_mode", None),
            ("/hook_event_name", Some(json!("PostToolUseFailure"))),
            ("/tool_name", Some(json!("Read"))),
            ("/tool_input/command", Some(json!(7))),
            ("/tool_response", Some(json!("text"))),
            ("/tool_response/stdout", Some(json!(7))),
            ("/tool_response/stdout", None),
            ("/tool_response/stdout", Some(json!(" \n\t"))),
            ("/tool_response/stdout", Some(json!(marked("\0")))),
            ("/tool_response/stdout", Some(json!(marked("\u{FFFD}")))),
            (
                "/tool_response/stdout",
                Some(json!("x\n".repeat(MAX_STDOUT_CHARS))),
            ),
            (
                "/tool_response/stdout",
                Some(json!(marked("[skim] compressed view of 'x'\n"))),
            ),
            (
                "/tool_response/stdout",
                Some(json!(marked("[skim] transformed view (cat"))),
            ),
            (
                "/tool_response/stderr",
                Some(json!("[skim] compressed view of 'x'")),
            ),
            ("/tool_response/stderr", Some(json!(null))),
            ("/tool_response/interrupted", Some(json!(true))),
            ("/tool_response/interrupted", None),
            ("/tool_response/isImage", Some(json!(true))),
            ("/tool_response/isImage", None),
            (
                "/tool_response/persistedOutputPath",
                Some(json!("/tmp/out.txt")),
            ),
            ("/tool_response/backgroundTaskId", Some(json!("b1"))),
            (
                "/tool_response/returnCodeInterpretation",
                Some(json!("note")),
            ),
        ];
        for (pointer, value) in cases {
            let mut input = baseline();
            let (parent, key) = pointer.rsplit_once('/').expect("pointer");
            let object = input.pointer_mut(parent).and_then(Value::as_object_mut);
            match (&value, object.expect("parent object")) {
                (Some(value), object) => object.insert(key.to_string(), value.clone()),
                (None, object) => object.remove(key),
            };
            assert!(respond(&input).is_none(), "{pointer} = {value:?}");
        }
    }

    #[test]
    fn stdout_is_accepted_below_the_char_limit_and_declined_at_it() {
        let base = cargo_stdout();
        for (chars, compresses) in [(MAX_STDOUT_CHARS - 1, true), (MAX_STDOUT_CHARS, false)] {
            let padded = format!("{base}{}", " ".repeat(chars - base.len()));
            assert_eq!(
                respond(&call("cargo test", &padded)).is_some(),
                compresses,
                "{chars}"
            );
        }
    }

    #[test]
    fn commands_outside_the_plain_allow_listed_shapes_are_declined() {
        for command in [
            "cargo test && cargo build",
            "cargo test | tee out.txt",
            "cargo test &",
            "cargo test > out.txt",
            "cargo test < in.txt",
            "cargo test 2>&1",
            "cargo test $(date)",
            "cargo test\nls",
            "FOO=1 cargo test",
            "SKIM_PASSTHROUGH=1 cargo test",
            "cargo test SKIM_PASSTHROUGH",
            "env",
            "curl https://example.com",
            "git status",
            "cat a.rs b.rs",
            "cat notes.txt",
        ] {
            assert!(
                respond(&call(command, &cargo_stdout())).is_none(),
                "{command:?}"
            );
        }
        for agent in [AgentKind::CodexCli, AgentKind::GeminiCli, AgentKind::Cursor] {
            assert!(post_response(&baseline(), agent).is_none(), "{agent:?}");
        }
    }

    #[test]
    fn cat_of_one_code_file_compresses_the_given_text_and_never_opens_the_path() {
        let source = rust_source();
        let response = respond(&call("cat no/such/dir/lib.rs", &source)).expect("must compress");
        let stdout = updated(&response)["stdout"].as_str().expect("stdout");
        assert!(stdout.len() < source.len());
        assert!(!stdout.contains("A comment that explains"), "{stdout:?}");
        assert!(
            stdout.contains('\u{1b}'),
            "an ESC byte in a literal survives"
        );
        let stderr = updated(&response)["stderr"].as_str().expect("stderr");
        assert!(
            stderr.contains("[skim] transformed view (cat \u{2192} skim --mode=pseudo)"),
            "{stderr:?}"
        );
    }

    #[test]
    fn cat_view_uses_the_mode_the_allow_list_chose() {
        let source = "export function f(a: number): number {\n  return a + 1;\n}\n";
        let (served, marker) = cat_view(source, "types/f.d.ts", true).expect("structure view");
        assert!(!served.contains("return a + 1"), "{served:?}");
        assert!(marker.contains("--mode=structure"), "{marker:?}");
        assert!(
            cat_view(source, "notes.txt", false).is_none(),
            "unknown language"
        );
    }

    /// The guard charges the marker: a view smaller than raw, but not by the
    /// marker's own cost, gives no output.
    #[test]
    fn a_view_that_saves_less_than_its_marker_costs_gives_no_output() {
        let source = "fn main() {}\n// a short comment below the code\nfn other() {}\n";
        let (served, marker) = cat_view(source, "main.rs", false).expect("view");
        assert!(served.len() < source.len(), "the view alone is smaller");
        assert!(
            served.len() + marker.len() >= source.len(),
            "the marker eats the saving"
        );
        assert!(respond(&call("cat main.rs", source)).is_none());
    }

    #[test]
    fn an_output_no_parser_understands_gives_no_view_and_a_panic_gives_no_response() {
        assert!(parsed_view(PostTool::CargoTest, "cargo test", "no tests here\n").is_none());
        assert!(parsed_view(PostTool::Pytest, "pytest", "hello\n").is_none());
        assert!(guarded(|| panic!("boom")).is_none());
        assert!(guarded(|| Some(serde_json::json!(1))).is_some());
    }
}
